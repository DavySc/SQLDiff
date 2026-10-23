//! Two directory trees paired object by object (`sqldiff tree`, the GUI
//! review queue): pack `ignore`, `shared`, `dialect_map`, pairing rules and
//! explicit pairs, then one engine diff per pair with pack policy applied.

use std::collections::BTreeMap;
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

/// Engine diff with pack policy: limit rules, overrides, declared assumptions.
pub fn diff(a: &Artifact, b: &Artifact, cfg: &Resolved) -> Report {
    let opts = Options {
        excerpt_lines: cfg.excerpt_lines,
        ..Options::default()
    };
    with_policy(sqldiff_core::diff(a, b, &opts), a, Some(b), cfg)
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
