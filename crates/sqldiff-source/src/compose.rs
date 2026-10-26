//! Pack-driven template and composition input for the core (design
//! sections 3 layer 4 and 5.3): marker syntax and template syntax from the
//! merged policy, adapter compositions converted to the core model,
//! template lookup for check mode, and parameter sources merged by the
//! pack's collision rule.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use regex::Regex;
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
        ignore_case: t.ignore_case,
    }
}

pub fn compile(t: &TemplateSyntax) -> Result<Compiled> {
    Compiled::new(&syntax_def(t)).map_err(Error::new)
}

/// What decides whether a delimited name in a template is a placeholder:
/// the pack's `[template]` rules, compiled once and shared by every command
/// (templates-and-composition.md, "Placeholder or quoted name?").
///
/// A neutral scanner finds every delimited token, so no rule can miss a name
/// because another rule's pattern did not capture it; the rules then decide:
///
/// | Mode | Placeholder | Unresolved (reported) | SQL |
/// |---|---|---|---|
/// | `parameters`, with a list | listed | matches the pattern, or glued | the rest |
/// | `parameters`, no list | matches the pattern, or glued | — | the rest |
/// | `pattern` (declared convention) | matches the pattern as written, listed, or glued | — | the rest |
///
/// "The pattern" is `unquoted_name_pattern` in dialects whose delimiters are
/// not SQL, else `name_pattern`; it matches in any case with `ignore_case`,
/// except under the declared convention, which is about how names are
/// written. "Glued" ([`template::Candidate::glued`]) counts only where the
/// delimiters also quote names. The pack's `known_parameters` join any list.
#[derive(Debug, Clone)]
pub struct Placeholders {
    scanner: Compiled,
    pattern: Regex,
    convention: Regex,
    unquoted: Option<Regex>,
    by_convention: bool,
    ignore_case: bool,
    known_parameters: Vec<String>,
    open: String,
    close: String,
}

impl Placeholders {
    pub fn new(t: &TemplateSyntax) -> Result<Placeholders> {
        let anchored = |p: &str, ci: bool| {
            Regex::new(&format!("^(?{}:{p})$", if ci { "i" } else { "" }))
                .map_err(|e| Error::new(format!("template name pattern `{p}`: {e}")))
        };
        // Any run of characters other than white space and the delimiters'
        // own characters.
        let delims: String = t
            .open
            .chars()
            .chain(t.close.chars())
            .map(|c| regex::escape(&c.to_string()))
            .collect();
        let mut def = syntax_def(t);
        def.name_pattern = format!("[^\\s{delims}]+");
        Ok(Placeholders {
            scanner: Compiled::new(&def).map_err(Error::new)?,
            pattern: anchored(&t.name_pattern, t.ignore_case)?,
            convention: anchored(&t.name_pattern, false)?,
            unquoted: t
                .unquoted_name_pattern
                .as_deref()
                .map(|p| anchored(p, t.ignore_case))
                .transpose()?,
            by_convention: t.placeholders.as_deref() == Some("pattern"),
            ignore_case: t.ignore_case,
            known_parameters: t.known_parameters.clone(),
            open: t.open.clone(),
            close: t.close.clone(),
        })
    }

    /// Whether names match parameters in any letter case (kept in upper case).
    pub fn ignore_case(&self) -> bool {
        self.ignore_case
    }

    pub fn delimiters(&self) -> (&str, &str) {
        (&self.open, &self.close)
    }

    /// Splits a template written in `dialect`, with the source's parameter
    /// names when it gives them, and says what decided.
    pub fn parse(
        &self,
        text: &str,
        dialect: sqldiff_core::Dialect,
        list: Option<&BTreeSet<String>>,
    ) -> (template::Template, composition::PlaceholderBasis) {
        use composition::PlaceholderBasis as Basis;
        use template::{Candidate, Decision};
        let quoting = composition::delimiters_quote_names(&self.open, dialect);
        let list: Option<BTreeSet<String>> = list.map(|l| {
            l.iter()
                .chain(self.known_parameters.iter())
                .cloned()
                .collect()
        });
        let listed = |n: &str| {
            list.as_ref()
                .is_some_and(|l| template::listed(l, n, self.ignore_case))
        };
        let pattern = |n: &str| match &self.unquoted {
            Some(u) if !quoting => u.is_match(n),
            _ => self.pattern.is_match(n),
        };
        let glued = |c: &Candidate| quoting && c.glued(&self.open, &self.close);
        let decide = |c: &Candidate| {
            let n = c.name;
            let placeholder = if self.by_convention {
                let by_name = if quoting {
                    self.convention.is_match(n)
                } else {
                    pattern(n)
                };
                by_name || listed(n) || glued(c)
            } else if list.is_some() {
                if !listed(n) {
                    return if pattern(n) || glued(c) {
                        Decision::Unresolved
                    } else {
                        Decision::Sql
                    };
                }
                true
            } else {
                pattern(n) || glued(c)
            };
            if placeholder {
                Decision::Placeholder
            } else {
                Decision::Sql
            }
        };
        let basis = match (self.by_convention, &list) {
            (true, _) => Basis::DeclaredPattern,
            (false, Some(_)) => Basis::Parameters,
            (false, None) => Basis::Pattern,
        };
        (
            template::Template::parse_with(text, &self.scanner, &decide),
            basis,
        )
    }
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
    pub root: PathBuf,
    files: BTreeMap<String, PathBuf>,
    /// Parameter values given for check mode, as parameter source `cli`.
    pub params: BTreeMap<String, String>,
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
        Ok(TemplateDir {
            root: dir.to_path_buf(),
            files,
            params: BTreeMap::new(),
        })
    }

    pub fn with_params(mut self, params: BTreeMap<String, String>) -> Self {
        self.params = params;
        self
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

/// The shared step for every engine report built from files (CLI `diff`,
/// `check` and `tree`, the GUI, the review pipeline): read mode over the
/// pack's markers on each side, then [`annotate`]. Problems never abort the
/// report; they become unknown reasons.
pub fn apply(
    report: &mut Report,
    a: &Artifact,
    b: Option<&Artifact>,
    policy: &Policy,
    templates: Option<&TemplateDir>,
    opts: &Options,
) {
    let res = (|| {
        let ca = read(&a.text, policy)?;
        let cb = match b {
            Some(b) => read(&b.text, policy)?,
            None => None,
        };
        annotate(
            report,
            (a, ca.as_ref()),
            b.map(|b| (b, cb.as_ref())),
            policy,
            templates,
            opts,
        )
    })();
    if let Err(e) = res {
        unknown(report, &e.0);
    }
}

/// [`apply`] for objects of a source: the adapter's persisted composition
/// when it has one, else pack markers. A failing `get_composition` makes
/// the pair unknown instead of aborting the run.
pub fn apply_source(
    report: &mut Report,
    src: &dyn Source,
    a: (&str, &Artifact),
    b: (&str, &Artifact),
    policy: &Policy,
    opts: &Options,
) {
    let res = (|| {
        let ca = for_object(src, a.0, &a.1.text, policy)?;
        let cb = for_object(src, b.0, &b.1.text, policy)?;
        annotate(
            report,
            (a.1, ca.as_ref()),
            Some((b.1, cb.as_ref())),
            policy,
            None,
            opts,
        )
    })();
    if let Err(e) = res {
        unknown(report, &e.0);
    }
}

fn unknown(report: &mut Report, why: &str) {
    report
        .unknown_reasons
        .push(format!("composition not read: {why}"));
    report.verdict = report.verdict.max(sqldiff_core::Verdict::Unknown);
}

/// Runs read mode (and check mode with `templates`) on both sides and adds
/// the composition findings and wrapper facts to an engine report. Check
/// mode that was asked for but cannot run is an unknown reason, never
/// skipped silently.
pub fn annotate(
    report: &mut Report,
    a: (&Artifact, Option<&Composition>),
    b: Option<(&Artifact, Option<&Composition>)>,
    policy: &Policy,
    templates: Option<&TemplateDir>,
    opts: &Options,
) -> Result<()> {
    let none = a.1.is_none() && b.map_or(true, |b| b.1.is_none());
    if let Some(dir) = templates {
        let why = if policy.template.is_none() {
            Some("no [template] syntax in the loaded packs or config".to_string())
        } else if none {
            Some(if policy.composition.is_empty() {
                "no [[composition]] markers in the loaded packs or config and no adapter composition".to_string()
            } else {
                "no composition markers found in the input".to_string()
            })
        } else {
            None
        };
        if let Some(why) = why {
            unknown(
                report,
                &format!(
                    "check mode against {} did not run: {why}",
                    dir.root.display()
                ),
            );
            return Ok(());
        }
    }
    if none {
        return Ok(());
    }
    // Check mode merges the extra parameter sources into every part.
    let merged = |c: Option<&Composition>| -> Option<Composition> {
        let (c, dir, t) = (c?, templates?, policy.template.as_ref()?);
        let mut c = c.clone();
        merge_parameters(&mut c, &[("cli", &dir.params)], t);
        Some(c)
    };
    let (ma, mb) = (merged(a.1), b.and_then(|b| merged(b.1)));
    let ca = ma.as_ref().or(a.1);
    let cb = b.and_then(|b| mb.as_ref().or(b.1));
    let checks_a = match ca {
        Some(c) => check(&a.0.text, c, policy, templates)?,
        None => Vec::new(),
    };
    let checks_b = match (b, cb) {
        (Some((art, _)), Some(c)) => check(&art.text, c, policy, templates)?,
        _ => Vec::new(),
    };
    let side_a = Composed {
        artifact: a.0,
        composition: ca,
        checks: &checks_a,
    };
    let side_b = b.map(|(art, _)| Composed {
        artifact: art,
        composition: cb,
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
        // The body is the whole line with its own line ending, LF or CRLF.
        let body = &text[c.parts[1].body_start..c.parts[1].body_end];
        let line = body
            .strip_suffix("\r\n")
            .or_else(|| body.strip_suffix('\n'));
        assert_eq!(line, Some("  SET XACT_ABORT ON;"), "{body:?}");
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
            known_parameters: vec![],
            ignore_case: false,
            placeholders: None,
            unquoted_name_pattern: None,
            include: None,
            values: Default::default(),
            fragments: vec![],
            pairs: vec![],
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
