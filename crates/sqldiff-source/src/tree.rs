//! Two directory trees paired object by object (`sqldiff tree`, the GUI
//! review queue): pack `ignore`, `shared`, `dialect_map`, pairing rules and
//! explicit pairs, then one engine diff per pair with pack policy applied.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use sqldiff_core::{Artifact, Dialect, Options, Report};

use crate::compose::Placeholders;
use crate::protocol::{ObjectFilter, ObjectInfo};
use crate::{pairing, policy, Error, FsSource, Resolved, Result, Source};

/// Builds an artifact. Dialect: the given one, else `dialect_map` or the path
/// convention. Version: `dialect_map`, else the configured target version.
pub fn artifact(
    display: String,
    rel: &str,
    dialect: Option<Dialect>,
    text: String,
    cfg: &Resolved,
) -> Result<Artifact> {
    let (found, version) = crate::classify(rel, cfg);
    let d = dialect.or(found).ok_or_else(|| {
        Error(format!(
            "{display}: dialect unknown; give a dialect flag or map it with dialect_map"
        ))
    })?;
    let mut a = Artifact::new(display, d, text);
    a.version = match version {
        Some(v) if found == Some(d) => v,
        _ => cfg.dialect_version(d),
    };
    Ok(a)
}

/// Path with `/` separators, as `dialect_map` globs and pairing see it.
pub fn slashed(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
        .replace("//", "/")
}

/// Reads a file as an artifact (see [`artifact`]).
pub fn read(path: &Path, dialect: Option<Dialect>, cfg: &Resolved) -> Result<Artifact> {
    let text =
        std::fs::read_to_string(path).map_err(|e| Error(format!("{}: {e}", path.display())))?;
    artifact(
        path.display().to_string(),
        &slashed(path),
        dialect,
        text,
        cfg,
    )
}

/// Engine diff with composition (pack markers) and pack policy: limit
/// rules, overrides, declared assumptions.
pub fn diff(a: &Artifact, b: &Artifact, cfg: &Resolved) -> Report {
    diff_with(a, b, cfg, &cfg.options())
}

/// One side read as a template: the template, its SQL fragment, the
/// artifact the analyses read (the fragment's text, same path, dialect and
/// version) and what decided which names are placeholders. The diff, the
/// GUI's pair view, what-if, overlay and redaction all start from this, so
/// they read the same SQL.
pub struct TemplateArtifact {
    pub template: sqldiff_syntax::template::Template,
    pub fragment: sqldiff_syntax::template::Derived,
    pub analysed: Artifact,
    pub basis: sqldiff_core::composition::PlaceholderBasis,
}

impl TemplateArtifact {
    pub fn new(
        a: &Artifact,
        known: Option<&BTreeSet<String>>,
        placeholders: &Placeholders,
    ) -> TemplateArtifact {
        let (template, basis) = placeholders.parse(&a.text, a.dialect, known);
        let fragment = template.fragment();
        let mut analysed = a.clone();
        analysed.text = fragment.text.clone();
        TemplateArtifact {
            template,
            fragment,
            analysed,
            basis,
        }
    }

    /// The way from fragment results back to the template.
    pub fn side(&self) -> sqldiff_core::composition::TemplateSide<'_> {
        sqldiff_core::composition::TemplateSide::new(&self.template, &self.fragment)
    }
}

/// Template-level diff: the SQL fragments of two templates in the pack's
/// `[template]` syntax, with locations and witnesses in the templates.
///
/// `known` holds each side's parameter names when its source knows them.
/// [`Placeholders`] decides what is a placeholder; where a name is SQL or
/// only a placeholder by pattern, the report says so (`template.unresolved`,
/// an unknown reason, or a declared assumption).
pub fn template_report(
    a: &Artifact,
    b: &Artifact,
    known: [Option<&BTreeSet<String>>; 2],
    placeholders: &Placeholders,
    opts: &Options,
) -> Report {
    use sqldiff_core::composition::{remap_to_template, template_notes};
    use sqldiff_core::model::Side;

    let ta = TemplateArtifact::new(a, known[0], placeholders);
    let tb = TemplateArtifact::new(b, known[1], placeholders);
    let mut r = sqldiff_core::diff(&ta.analysed, &tb.analysed, opts);
    for (side, t, art) in [(Side::A, &ta, a), (Side::B, &tb, b)] {
        remap_to_template(
            &mut r,
            side,
            &t.template,
            &t.fragment,
            art.dialect,
            opts.excerpt_lines,
        );
        template_notes(
            &mut r,
            side,
            &t.template,
            &art.path,
            art.dialect,
            t.basis,
            placeholders.delimiters(),
            opts.excerpt_lines,
        );
    }
    r
}

/// [`template_report`] with compositions and pack policy (limit rules,
/// overrides, declared assumptions), as [`diff`] adds them to an engine diff.
/// Compiles the placeholder rules for one call; use [`template_diff_with`]
/// to reuse them.
pub fn template_diff(
    a: &Artifact,
    b: &Artifact,
    known: [Option<&BTreeSet<String>>; 2],
    cfg: &Resolved,
    opts: &Options,
) -> Result<Report> {
    let placeholders = placeholders(cfg)?;
    Ok(template_diff_with(a, b, known, &placeholders, cfg, opts))
}

/// The config's placeholder rules, compiled.
pub fn placeholders(cfg: &Resolved) -> Result<Placeholders> {
    let t = cfg
        .policy
        .template
        .as_ref()
        .ok_or_else(|| Error::new("no [template] syntax in the loaded packs or config"))?;
    Placeholders::new(t)
}

/// [`diff_with`] for two templates: [`template_report`], then compositions
/// and pack policy.
pub fn template_diff_with(
    a: &Artifact,
    b: &Artifact,
    known: [Option<&BTreeSet<String>>; 2],
    placeholders: &Placeholders,
    cfg: &Resolved,
    opts: &Options,
) -> Report {
    let mut r = template_report(a, b, known, placeholders, opts);
    crate::compose::apply(&mut r, a, Some(b), &cfg.policy, None, opts);
    with_policy(r, a, Some(b), cfg)
}

/// [`diff`] with explicit engine options (e.g. scenario flips).
pub fn diff_with(a: &Artifact, b: &Artifact, cfg: &Resolved, opts: &Options) -> Report {
    let mut r = sqldiff_core::diff(a, b, opts);
    crate::compose::apply(&mut r, a, Some(b), &cfg.policy, None, opts);
    with_policy(r, a, Some(b), cfg)
}

/// Pack policy on an engine report.
pub fn with_policy(mut r: Report, a: &Artifact, b: Option<&Artifact>, cfg: &Resolved) -> Report {
    policy::add_limits(&mut r, a, b, cfg);
    policy::apply(&mut r, cfg);
    r
}

/// One paired object: ids relative to each tree root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TreePair {
    pub id_a: String,
    pub id_b: String,
    pub path_a: PathBuf,
    pub path_b: PathBuf,
}

impl TreePair {
    /// The object fragments of the two ids, for ids that name one object of
    /// a file with several.
    pub fn fragments(&self) -> (Option<&str>, Option<&str>) {
        (
            fragment(&self.path_a, &self.id_a),
            fragment(&self.path_b, &self.id_b),
        )
    }
}

fn fragment<'i>(path: &Path, id: &'i str) -> Option<&'i str> {
    match crate::split::split_id(id) {
        (_, Some(f)) if !path.ends_with(id) => Some(f),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ambiguous {
    pub side: &'static str,
    pub key: String,
    pub ids: Vec<String>,
}

/// The pairing of two trees.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Tree {
    pub pairs: Vec<TreePair>,
    pub only_a: Vec<String>,
    pub only_b: Vec<String>,
    pub ambiguous: Vec<Ambiguous>,
    pub no_dialect: Vec<String>,
    #[serde(skip)]
    pub dialect_a: Option<Dialect>,
    #[serde(skip)]
    pub dialect_b: Option<Dialect>,
}

impl Tree {
    /// Reads both sides of a pair.
    pub fn read(&self, p: &TreePair, cfg: &Resolved) -> Result<(Artifact, Artifact)> {
        let load = |path: &Path, id: &str, d: Option<Dialect>| {
            let mut text = std::fs::read_to_string(path)
                .map_err(|e| Error(format!("{}: {e}", path.display())))?;
            // One object of a file with several (see `split`).
            let frag = fragment(path, id);
            let rel = if frag.is_some() {
                crate::split::split_id(id).0
            } else {
                id
            };
            let mut object = None;
            if let Some(frag) = frag {
                let dialect = d
                    .or(crate::classify(rel, cfg).0)
                    .ok_or_else(|| Error(format!("{}: dialect unknown", path.display())))?;
                let (t, name) = crate::split::object(&text, dialect, frag)
                    .ok_or_else(|| Error(format!("{}: no object `{frag}`", path.display())))?;
                text = t;
                object = Some(name);
            }
            let mut a = artifact(path.display().to_string(), rel, d, text, cfg)?;
            a.object = object;
            Ok::<_, Error>(a)
        };
        Ok((
            load(&p.path_a, &p.id_a, self.dialect_a)?,
            load(&p.path_b, &p.id_b, self.dialect_b)?,
        ))
    }
}

/// One side of a tree: its objects by pairing key.
struct Side {
    dir: PathBuf,
    by_key: BTreeMap<String, String>,
    /// Keys held by several objects; none of them is paired.
    ambiguous: BTreeMap<String, Vec<String>>,
    no_dialect: Vec<String>,
    dialects: BTreeMap<String, Dialect>,
    infos: BTreeMap<String, ObjectInfo>,
}

impl Side {
    /// Lists a directory with pack `ignore` and `dialect_map`, skipping shared
    /// artifacts, and keys every object by its pairing rule.
    fn load(dir: &Path, dialect: Option<Dialect>, cfg: &Resolved) -> Result<Side> {
        let src = FsSource::new(dir, cfg)?.with_dialect(dialect);
        let objects = src.list_objects(&ObjectFilter::default())?;
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut no_dialect = Vec::new();
        let mut dialects = BTreeMap::new();
        let mut infos = BTreeMap::new();
        for o in objects {
            if cfg.is_shared(&o.id) {
                continue;
            }
            let Some(d) = dialect.or_else(|| o.dialect.as_deref().and_then(|d| d.parse().ok()))
            else {
                no_dialect.push(o.id);
                continue;
            };
            dialects.insert(o.id.clone(), d);
            let rule = cfg.pairing_rule(&o.id, Some(d));
            groups
                .entry(pairing::key(&o.name, &rule))
                .or_default()
                .push(o.id.clone());
            infos.insert(o.id.clone(), o);
        }
        let (unique, ambiguous): (BTreeMap<_, _>, BTreeMap<_, _>) =
            groups.into_iter().partition(|(_, ids)| ids.len() == 1);
        Ok(Side {
            dir: dir.to_path_buf(),
            by_key: unique
                .into_iter()
                .map(|(k, mut ids)| (k, ids.remove(0)))
                .collect(),
            ambiguous,
            no_dialect,
            dialects,
            infos,
        })
    }

    /// Takes every object with this key out, unique or ambiguous.
    fn take_key(&mut self, key: &str) -> Vec<String> {
        let mut ids = self.ambiguous.remove(key).unwrap_or_default();
        ids.extend(self.by_key.remove(key));
        ids
    }

    /// The one object the file defines (see [`crate::defined_name`]).
    fn defined(&self, id: &str) -> Option<String> {
        let text = std::fs::read_to_string(self.dir.join(id)).ok()?;
        crate::defined_name(&text, *self.dialects.get(id)?)
    }

    fn display(&self, id: &str) -> String {
        self.dir.join(id).display().to_string()
    }

    /// Takes an object out by id, wherever it is keyed.
    fn take(&mut self, id: &str) -> bool {
        let key = self
            .by_key
            .iter()
            .find(|(_, v)| v.as_str() == id)
            .map(|(k, _)| k.clone());
        key.is_some_and(|k| self.by_key.remove(&k).is_some())
    }
}

/// Pairs the SQL files of two trees.
pub fn pair_dirs(
    dir_a: &Path,
    dir_b: &Path,
    da: Option<Dialect>,
    db: Option<Dialect>,
    cfg: &Resolved,
) -> Result<Tree> {
    let mut sa = Side::load(dir_a, da, cfg)?;
    let mut sb = Side::load(dir_b, db, cfg)?;
    let mut ids: Vec<(String, String)> = Vec::new();
    // Explicit pack pairs first: `a` relative to DIR_A, `b` relative to DIR_B.
    for e in &cfg.policy.pairing.pairs {
        for (x, y) in [(&e.a, &e.b), (&e.b, &e.a)] {
            if sa.by_key.values().any(|v| v == x) && sb.by_key.values().any(|v| v == y) {
                sa.take(x);
                sb.take(y);
                ids.push((x.clone(), y.clone()));
                break;
            }
        }
    }
    let mut only_a = Vec::new();
    let mut only_b = Vec::new();
    // Keys several files share on a side (a table and a view both named
    // `Cities.sql` in different schema folders): pair by the one
    // schema-qualified object each file defines, where that is unique on
    // both sides. The rest stay ambiguous, or unpaired when one is left.
    let shared_keys: BTreeSet<String> = sa
        .ambiguous
        .keys()
        .chain(sb.ambiguous.keys())
        .cloned()
        .collect();
    let mut ambiguous_left: Vec<(&'static str, String, Vec<String>)> = Vec::new();
    for k in shared_keys {
        let (ca, cb) = (sa.take_key(&k), sb.take_key(&k));
        let named = |side: &Side, ids: &[String]| -> BTreeMap<String, Vec<String>> {
            let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for id in ids {
                if let Some(n) = side.defined(id) {
                    m.entry(n).or_default().push(id.clone());
                }
            }
            m
        };
        let (na, nb) = (named(&sa, &ca), named(&sb, &cb));
        let mut done: BTreeSet<String> = BTreeSet::new();
        for (name, xs) in &na {
            if let (1, Some(ys)) = (xs.len(), nb.get(name)) {
                if ys.len() == 1 {
                    ids.push((xs[0].clone(), ys[0].clone()));
                    done.insert(xs[0].clone());
                    done.insert(ys[0].clone());
                }
            }
        }
        let la: Vec<String> = ca.into_iter().filter(|i| !done.contains(i)).collect();
        let lb: Vec<String> = cb.into_iter().filter(|i| !done.contains(i)).collect();
        for (side, left, s, only) in [("A", la, &sa, &mut only_a), ("B", lb, &sb, &mut only_b)] {
            match left.len() {
                0 => {}
                1 => only.push(s.display(&left[0])),
                _ => ambiguous_left.push((side, k.clone(), left)),
            }
        }
    }
    let mut left_a = Vec::new();
    for (k, ia) in &sa.by_key {
        match sb.by_key.get(k) {
            Some(ib) => ids.push((ia.clone(), ib.clone())),
            None => left_a.push(&sa.infos[ia]),
        }
    }
    let left_b: Vec<&ObjectInfo> = sb
        .by_key
        .iter()
        .filter(|(k, _)| !sa.by_key.contains_key(*k))
        .map(|(_, ib)| &sb.infos[ib])
        .collect();
    // A file split on one side only: its object pairs with the other
    // side's whole file (see `pairing::split_matches`).
    let rule = |o: &ObjectInfo, id: &str| {
        cfg.pairing_rule(id, o.dialect.as_deref().and_then(|d| d.parse().ok()))
    };
    let split = pairing::split_matches(
        &left_a,
        &left_b,
        |o| pairing::key(&o.name, &rule(o, &o.id)),
        |o, rel| pairing::key(&crate::walk::object_name(rel), &rule(o, rel)),
    );
    let matched: BTreeSet<&str> = split
        .iter()
        .flat_map(|(a, b)| [a.id.as_str(), b.id.as_str()])
        .collect();
    ids.extend(split.iter().map(|(a, b)| (a.id.clone(), b.id.clone())));
    for o in left_a.iter().filter(|o| !matched.contains(o.id.as_str())) {
        only_a.push(sa.display(&o.id));
    }
    for o in left_b.iter().filter(|o| !matched.contains(o.id.as_str())) {
        only_b.push(sb.display(&o.id));
    }
    let ambiguous = ambiguous_left
        .into_iter()
        .map(|(side, key, ids)| {
            let s = if side == "A" { &sa } else { &sb };
            Ambiguous {
                side,
                key,
                ids: ids.iter().map(|i| s.display(i)).collect(),
            }
        })
        .collect();
    let no_dialect = sa
        .no_dialect
        .iter()
        .map(|i| sa.display(i))
        .chain(sb.no_dialect.iter().map(|i| sb.display(i)))
        .collect();
    Ok(Tree {
        pairs: ids
            .into_iter()
            .map(|(a, b)| TreePair {
                path_a: crate::split::locate(&sa.dir, &a).0,
                path_b: crate::split::locate(&sb.dir, &b).0,
                id_a: a,
                id_b: b,
            })
            .collect(),
        only_a,
        only_b,
        ambiguous,
        no_dialect,
        dialect_a: da,
        dialect_b: db,
    })
}
