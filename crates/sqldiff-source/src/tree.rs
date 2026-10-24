//! Two directory trees paired object by object (`sqldiff tree`, the GUI
//! review queue): pack `ignore`, `shared`, `dialect_map`, pairing rules and
//! explicit pairs, then one engine diff per pair with pack policy applied.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use sqldiff_core::{Artifact, Dialect, Options, Report};

use crate::protocol::ObjectFilter;
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

/// Template-level diff: the SQL fragments of two templates in the pack's
/// `[template]` syntax, with locations and witnesses in the templates.
///
/// `known` holds each side's parameter names when its source knows them; the
/// pack's `known_parameters` are added to a given list. With a list, only its
/// names are placeholders and any other delimited name stays SQL (reported
/// as `template.unresolved`), since `[name]` is also a quoted identifier in
/// T-SQL. Without one the name pattern decides, and where the delimiters can
/// quote names the report says which names that made placeholders. A pack
/// with `placeholders = "pattern"` declares the pattern as the generator's
/// convention: lists are not used and the report records an assumption.
pub fn template_report(
    a: &Artifact,
    b: &Artifact,
    known: [Option<&BTreeSet<String>>; 2],
    cfg: &Resolved,
    opts: &Options,
) -> Result<Report> {
    use sqldiff_core::composition::{
        delimiters_quote_names, remap_to_template, template_notes, PlaceholderBasis,
    };
    use sqldiff_core::model::Side;
    use sqldiff_syntax::template::Compiled;
    use sqldiff_syntax::template::Template;

    let t = cfg
        .policy
        .template
        .as_ref()
        .ok_or_else(|| Error::new("no [template] syntax in the loaded packs or config"))?;
    let syntax = crate::compose::compile(t)?;
    // Where the delimiters are not SQL, a delimited name cannot be a quoted
    // identifier, so the pack may allow a wider name pattern there.
    let unquoted = match &t.unquoted_name_pattern {
        Some(p) => {
            let mut def = crate::compose::syntax_def(t);
            def.name_pattern = p.clone();
            Some(Compiled::new(&def).map_err(Error::new)?)
        }
        None => None,
    };
    let syntax_for = |d: Dialect| {
        if delimiters_quote_names(&t.open, d) {
            &syntax
        } else {
            unquoted.as_ref().unwrap_or(&syntax)
        }
    };
    let by_pattern = t.placeholders.as_deref() == Some("pattern");
    let with_pack = |k: Option<&BTreeSet<String>>| {
        k.filter(|_| !by_pattern).map(|k| {
            k.iter()
                .chain(t.known_parameters.iter())
                .cloned()
                .collect::<BTreeSet<String>>()
        })
    };
    let basis = |k: &Option<BTreeSet<String>>| match (k, by_pattern) {
        (_, true) => PlaceholderBasis::DeclaredPattern,
        (Some(_), false) => PlaceholderBasis::Parameters,
        (None, false) => PlaceholderBasis::Pattern,
    };
    let (ka, kb) = (with_pack(known[0]), with_pack(known[1]));
    let ta = Template::parse(&a.text, syntax_for(a.dialect), ka.as_ref());
    let tb = Template::parse(&b.text, syntax_for(b.dialect), kb.as_ref());
    let (fa, fb) = (ta.fragment(), tb.fragment());
    let mut xa = a.clone();
    xa.text = fa.text.clone();
    let mut xb = b.clone();
    xb.text = fb.text.clone();
    let mut r = sqldiff_core::diff(&xa, &xb, opts);
    for (side, tpl, frag, art, k) in [(Side::A, &ta, &fa, a, &ka), (Side::B, &tb, &fb, b, &kb)] {
        remap_to_template(&mut r, side, tpl, frag, art.dialect, opts.excerpt_lines);
        template_notes(
            &mut r,
            side,
            tpl,
            &art.path,
            art.dialect,
            basis(k),
            (&t.open, &t.close),
            opts.excerpt_lines,
        );
    }
    Ok(r)
}

/// [`template_report`] with pack policy (limit rules, overrides, declared
/// assumptions), as [`diff`] adds it to an engine diff.
pub fn template_diff(
    a: &Artifact,
    b: &Artifact,
    known: [Option<&BTreeSet<String>>; 2],
    cfg: &Resolved,
    opts: &Options,
) -> Result<Report> {
    Ok(with_policy(
        template_report(a, b, known, cfg, opts)?,
        a,
        Some(b),
        cfg,
    ))
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
        let load = |path: &Path, id: &str, d| {
            let text = std::fs::read_to_string(path)
                .map_err(|e| Error(format!("{}: {e}", path.display())))?;
            artifact(path.display().to_string(), id, d, text, cfg)
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
}

impl Side {
    /// Lists a directory with pack `ignore` and `dialect_map`, skipping shared
    /// artifacts, and keys every object by its pairing rule.
    fn load(dir: &Path, dialect: Option<Dialect>, cfg: &Resolved) -> Result<Side> {
        let src = FsSource::new(dir, cfg)?;
        let objects = src.list_objects(&ObjectFilter::default())?;
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut no_dialect = Vec::new();
        for o in objects {
            if cfg.is_shared(&o.id) {
                continue;
            }
            let Some(d) = dialect.or_else(|| o.dialect.as_deref().and_then(|d| d.parse().ok()))
            else {
                no_dialect.push(o.id);
                continue;
            };
            let rule = cfg.pairing_rule(&o.id, Some(d));
            groups
                .entry(pairing::key(&o.name, &rule))
                .or_default()
                .push(o.id);
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
        })
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
    for (k, ia) in &sa.by_key {
        match sb.by_key.get(k) {
            Some(ib) => ids.push((ia.clone(), ib.clone())),
            None => only_a.push(sa.display(ia)),
        }
    }
    for (k, ib) in &sb.by_key {
        if !sa.by_key.contains_key(k) {
            only_b.push(sb.display(ib));
        }
    }
    let ambiguous = [(&sa, "A"), (&sb, "B")]
        .iter()
        .flat_map(|(s, side)| {
            s.ambiguous.iter().map(move |(k, ids)| Ambiguous {
                side,
                key: k.clone(),
                ids: ids.iter().map(|i| s.display(i)).collect(),
            })
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
                path_a: sa.dir.join(&a),
                path_b: sb.dir.join(&b),
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
