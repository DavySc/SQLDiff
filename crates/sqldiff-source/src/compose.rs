//! Pack-driven template and composition input for the core (design
//! sections 3 layer 4 and 5.3): marker syntax and template syntax from the
//! merged policy, adapter compositions converted to the core model,
//! template lookup for check mode, and parameter sources merged by the
//! pack's collision rule.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sqldiff_core::composition::{self, Composed, Composition, Markers, Part, PartCheck, Problem};
use sqldiff_core::{Artifact, Options, Report};
use sqldiff_syntax::template::{self, Collision, Compiled};

use crate::pack::{Policy, TemplateSyntax};
use crate::protocol;
use crate::{Error, Result, Source};

/// Marker syntaxes of every `[[composition]]` entry, in order.
pub fn markers(policy: &Policy) -> Result<Vec<Markers>> {
    policy
        .composition
        .iter()
        .map(|c| Markers::new(&c.name, &c.begin, &c.end).map_err(Error::new))
        .collect()
}

/// Read mode from pack markers; `None` when the text has no markers.
pub fn read(text: &str, policy: &Policy) -> Result<Option<Composition>> {
    Ok(composition::read(text, &markers(policy)?))
}

pub fn syntax_def(t: &TemplateSyntax) -> template::TemplateSyntax {
    template::TemplateSyntax {
        open: t.open.clone(),
        close: t.close.clone(),
        name_pattern: t.name_pattern.clone(),
        special_forms: t
            .special_forms
            .iter()
            .map(|f| (f.name.clone(), f.pattern.clone()))
            .collect(),
    }
}

pub fn compile(t: &TemplateSyntax) -> Result<Compiled> {
    Compiled::new(&syntax_def(t)).map_err(Error::new)
}

/// An adapter's persisted composition. Its spans are the fragments; the
/// adapter reports no separate marker lines.
pub fn from_protocol(text: &str, c: &protocol::Composition) -> Composition {
    let parts = c
        .parts
        .iter()
        .map(|p| Part {
            name: p.name.clone(),
            template: p.template.clone(),
            parameters: p.parameters.clone(),
            start: p.start,
            end: p.end,
            body_start: p.start,
            body_end: p.end,
            template_hash: p.template_hash.clone(),
        })
        .collect();
    Composition::from_spans(text, parts, c.stale)
}

/// Templates by name from a directory: the first file whose name without
/// extensions equals the template name.
pub struct TemplateDir {
    files: BTreeMap<String, PathBuf>,
}

impl TemplateDir {
    pub fn new(dir: &Path) -> Result<TemplateDir> {
        let mut files = BTreeMap::new();
        let rd =
            std::fs::read_dir(dir).map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
        let mut paths: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        paths.sort();
        for p in paths {
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let stem = name.split('.').next().unwrap_or(name).to_string();
            files.entry(stem).or_insert(p);
        }
        Ok(TemplateDir { files })
    }

    pub fn get(&self, name: &str) -> Option<String> {
        self.files
            .get(name)
            .and_then(|p| std::fs::read_to_string(p).ok())
    }
}

/// Merges extra parameter sources into every part by the pack's
/// `parameter_sources` order and `collision` rule. A part's own parameters
/// are the source `composition`. Conflicts become composition problems.
pub fn merge_parameters(
    comp: &mut Composition,
    extra: &[(&str, &BTreeMap<String, String>)],
    t: &TemplateSyntax,
) {
    let rule: Collision = t
        .collision
        .as_deref()
        .and_then(|c| c.parse().ok())
        .unwrap_or_default();
    let mut problems = Vec::new();
    for p in &mut comp.parts {
        let own = p.parameters.clone();
        let mut sources = vec![("composition", &own)];
        sources.extend(extra.iter().copied());
        let (merged, conflicts) = template::merge_parameters(&sources, &t.parameter_sources, rule);
        p.parameters = merged;
        for c in conflicts {
            let vals: Vec<String> = c
                .values
                .iter()
                .map(|(s, v)| format!("{s} = `{v}`"))
                .collect();
            problems.push(Problem {
                offset: p.start,
                message: format!(
                    "part '{}': parameter sources disagree on {} ({})",
                    p.name,
                    c.name,
                    vals.join(", ")
                ),
            });
        }
    }
    comp.problems.extend(problems);
}

/// The composition of an object: the adapter's persisted composition if it
/// has one, else read mode over pack markers.
pub fn for_object(
    src: &dyn Source,
    id: &str,
    text: &str,
    policy: &Policy,
) -> Result<Option<Composition>> {
    if let Some(c) = src.get_composition(id)? {
        return Ok(Some(from_protocol(text, &c)));
    }
    read(text, policy)
}

/// Check mode for one side, when templates and a template syntax are available.
pub fn check(
    text: &str,
    comp: &Composition,
    policy: &Policy,
    templates: Option<&TemplateDir>,
) -> Result<Vec<PartCheck>> {
    let (Some(dir), Some(t)) = (templates, &policy.template) else {
        return Ok(Vec::new());
    };
    let syntax = compile(t)?;
    Ok(composition::check(text, comp, &syntax, &|name| {
        dir.get(name)
    }))
}

/// Runs read mode (and check mode with `templates`) on both sides and adds
/// the composition findings and wrapper facts to an engine report.
pub fn annotate(
    report: &mut Report,
    a: (&Artifact, Option<&Composition>),
    b: Option<(&Artifact, Option<&Composition>)>,
    policy: &Policy,
    templates: Option<&TemplateDir>,
    opts: &Options,
) -> Result<()> {
    if a.1.is_none() && b.map_or(true, |b| b.1.is_none()) {
        return Ok(());
    }
    let checks_a = match a.1 {
        Some(c) => check(&a.0.text, c, policy, templates)?,
        None => Vec::new(),
    };
    let checks_b = match b.and_then(|b| b.1.map(|c| (b.0, c))) {
        Some((art, c)) => check(&art.text, c, policy, templates)?,
        None => Vec::new(),
    };
    let side_a = Composed {
        artifact: a.0,
        composition: a.1,
        checks: &checks_a,
    };
    let side_b = b.map(|(art, c)| Composed {
        artifact: art,
        composition: c,
        checks: &checks_b,
    });
    composition::annotate(report, side_a, side_b, opts);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::Pack;

    #[test]
    fn corpus_pack_reads_m23() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
        let pack = Pack::load(&root.join("packs/synthetic-generator.toml")).unwrap();
        let text = std::fs::read_to_string(
            root.join("mutations/m23-composed-fragment-dropped/original.tsql.sql"),
        )
        .unwrap();
        let c = read(&text, &pack.policy).unwrap().unwrap();
        let names: Vec<&str> = c.parts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["proc-header", "session-options", "body", "proc-footer"]
        );
        assert_eq!(c.parts[1].template.as_deref(), Some("xact-abort"));
        assert_eq!(
            &text[c.parts[1].body_start..c.parts[1].body_end],
            "  SET XACT_ABORT ON;\n"
        );
        compile(pack.policy.template.as_ref().unwrap()).unwrap();
    }

    #[test]
    fn parameter_conflicts_are_problems() {
        let t = TemplateSyntax {
            open: "{{".into(),
            close: "}}".into(),
            name_pattern: "[A-Z]+".into(),
            special_forms: vec![],
            parameter_sources: vec!["composition".into(), "cli".into()],
            collision: Some("error".into()),
        };
        let mut c = Composition::from_spans(
            "x",
            vec![Part {
                name: "p".into(),
                template: None,
                parameters: [("T".to_string(), "a".to_string())].into(),
                start: 0,
                end: 1,
                body_start: 0,
                body_end: 1,
                template_hash: None,
            }],
            false,
        );
        let cli: BTreeMap<String, String> = [
            ("T".to_string(), "b".to_string()),
            ("U".to_string(), "c".to_string()),
        ]
        .into();
        merge_parameters(&mut c, &[("cli", &cli)], &t);
        assert_eq!(c.parts[0].parameters["U"], "c");
        assert_eq!(c.problems.len(), 1, "{:?}", c.problems);
        assert!(c.problems[0]
            .message
            .contains("composition = `a`, cli = `b`"));
    }
}
