//! Base catalogs (`sqldiff convert --catalog FILE`): DDL files in the source
//! dialect that describe objects which exist already but are not part of the
//! tree being converted, such as the install an upgrade script set alters
//! and queries. They are bound against, and give the rules and the check
//! their facts (columns, keys, types, routine signatures), but are not
//! converted into the output tree, nor listed among its files.
//!
//! Side A (the source catalog) holds every base object, later files winning
//! over earlier ones, and the tree's own objects over those: an object the
//! tree defines replaces every base object of that name (all overloads of a
//! routine).
//!
//! Side B (the target catalog) holds the base objects as the same rules
//! convert them, converted with the base catalogs alone, and the tree's
//! drafts over those. Each top-level statement of a base file is converted
//! and checked on its own, as a tree file is (diffed against its conversion,
//! and the conversion checked on its own against the base catalogs), so each
//! object gets its own verdict. A base object whose statement is `differs` or
//! `not converted` is left out of side B, so every statement that uses it
//! fails to bind there and is `differs`. A statement that alters a table, or
//! creates an index or a trigger on it, counts for that table. Leaving an
//! object out can make others that use it fail too (a view over it, a
//! routine that calls it), so this repeats until nothing more is left out.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use sqldiff_core::catalog::builtins::Helper;
use sqldiff_core::catalog::{ddl, default_schema, key, Catalog, Ident, RoutineKind, TableKind};
use sqldiff_core::{Dialect, Options};
use sqldiff_syntax::{SyntaxKind, SyntaxNode};

use crate::{Conversion, Status};

/// One base catalog file as read.
pub struct File {
    /// The path as given.
    pub path: String,
    pub text: String,
}

/// Reads the base catalog files. Fails on a file that is missing, not text,
/// given twice or a snapshot bundle, and when `no_catalog` asks to check
/// each file alone: base catalogs are catalogs.
pub fn read(paths: &[PathBuf], no_catalog: bool) -> Result<Vec<File>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    if no_catalog {
        return Err("--catalog and --no-catalog do not go together: --no-catalog checks each file alone, without any catalog".into());
    }
    let mut seen = HashSet::new();
    let mut files = Vec::new();
    for p in paths {
        let path = p.display().to_string();
        if !seen.insert(path.clone()) {
            return Err(format!("{path}: given twice as --catalog"));
        }
        if p.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        {
            return Err(format!(
                "{path}: a base catalog for a conversion is DDL in the source dialect; snapshot bundles (.json) are not read here"
            ));
        }
        if !p.is_file() {
            return Err(format!("{path}: not a file"));
        }
        let bytes = std::fs::read(p).map_err(|e| format!("{path}: {e}"))?;
        let text = crate::tree::decode(&bytes)
            .ok_or_else(|| format!("{path}: not UTF-8 or UTF-16 text"))?;
        files.push(File { path, text });
    }
    Ok(files)
}

/// One hash over the contents of the base catalogs, in order (CRLF read as
/// LF, paths left out): what a review decision is tied to. `None` without
/// base catalogs.
pub fn hash(files: &[File]) -> Option<String> {
    use sha2::{Digest, Sha256};
    if files.is_empty() {
        return None;
    }
    let mut h = Sha256::new();
    for f in files {
        h.update(crate::review::file_hash(&f.text).as_bytes());
        h.update([0]);
    }
    Some(sqldiff_core::model::hex(&h.finalize()))
}

/// What the report says about one base catalog file.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogFile {
    pub path: String,
    /// `review::file_hash` of its text.
    pub sha256: String,
    /// Tables and views, and routines, it defines.
    pub tables: usize,
    pub routines: usize,
}

/// A base object whose conversion is not plainly converted.
#[derive(Debug, Clone, Serialize)]
pub struct ObjectReport {
    /// `table`, `view`, `procedure`, `function`, `trigger`.
    pub kind: String,
    pub name: String,
    /// The base catalog file and the 1-based line of the statement that
    /// defines it.
    pub file: String,
    pub line: usize,
    /// The worst status of the statements that define or change it.
    pub status: Status,
    /// Whether it is in the target catalog. When it is not, every statement
    /// that uses it is `differs`.
    pub in_target: bool,
    pub rules: Vec<String>,
    pub reason: String,
}

/// A base statement that defines no object and did not convert.
#[derive(Debug, Clone, Serialize)]
pub struct StatementReport {
    pub file: String,
    pub line: usize,
    pub status: Status,
    pub rules: Vec<String>,
    pub reason: String,
}

/// The base catalogs, ready for a conversion.
pub struct Base {
    /// Every base object in the source dialect.
    pub source: Catalog,
    /// The base objects as converted, without those that did not convert.
    pub target: Catalog,
    pub files: Vec<CatalogFile>,
    /// Base objects that did not convert (left out of the target), then
    /// those in the target that are not plainly converted.
    pub objects: Vec<ObjectReport>,
    /// Statements that define no object and did not convert.
    pub statements: Vec<StatementReport>,
    pub hash: Option<String>,
}

/// The name an object is known by, for matching across files and sides:
/// letter case as the dialect compares, and the dialect's default schema for
/// an unqualified name. Without a default schema (Db2 for i), an unqualified
/// name may be any schema's.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Key {
    routine: bool,
    schema: Option<String>,
    name: String,
}

impl Key {
    fn new(d: Dialect, routine: bool, schema: Option<&str>, name: &str) -> Key {
        let schema = match schema {
            Some(s) => Some(key(d, s)),
            None => default_schema(d).map(|s| key(d, &Ident::plain(s).stored(d))),
        };
        Key {
            routine,
            schema,
            name: key(d, name),
        }
    }

    fn of_parts(d: Dialect, parts: &[Ident]) -> Option<Key> {
        let (name, schema) = match parts {
            [n] => (n, None),
            [s, n] => (n, Some(s.stored(d))),
            _ => return None,
        };
        Some(Key::new(d, false, schema.as_deref(), &name.stored(d)))
    }

    fn same(&self, o: &Key) -> bool {
        self.routine == o.routine
            && self.name == o.name
            && (self.schema == o.schema || self.schema.is_none() || o.schema.is_none())
    }
}

struct Object {
    key: Key,
    kind: &'static str,
    name: String,
}

/// A top-level statement of a base file.
struct Node {
    start: usize,
    end: usize,
    line: usize,
    /// What it creates.
    defines: Vec<Object>,
    /// The table it alters or puts an index or a trigger on.
    touches: Option<Key>,
}

fn nodes(text: &str, d: Dialect) -> Vec<Node> {
    let root = sqldiff_syntax::parse(text, d).syntax();
    let mut out = Vec::new();
    for node in root.descendants() {
        let top = node
            .parent()
            .is_some_and(|p| matches!(p.kind(), SyntaxKind::SOURCE_FILE | SyntaxKind::BATCH));
        if !top || !matches!(node.kind(), SyntaxKind::STATEMENT | SyntaxKind::ROUTINE) {
            continue;
        }
        let r = node.text_range();
        let (start, end) = (usize::from(r.start()), usize::from(r.end()));
        // The node alone: what it defines, whatever came before it.
        let mut c = Catalog::default();
        ddl::load_tree(&mut c, &node, d);
        let mut defines: Vec<Object> = c
            .tables
            .iter()
            .map(|t| Object {
                key: Key::new(d, false, t.schema.as_deref(), &t.name),
                kind: match t.kind {
                    TableKind::View => "view",
                    _ => "table",
                },
                name: t.display(),
            })
            .collect();
        defines.extend(c.routines.iter().map(|r| Object {
            key: Key::new(d, true, r.schema.as_deref(), &r.name),
            kind: match r.kind {
                RoutineKind::Procedure => "procedure",
                RoutineKind::Function => "function",
                RoutineKind::Trigger => "trigger",
            },
            name: r.display(),
        }));
        out.push(Node {
            start,
            end,
            line: text[..start].matches('\n').count() + 1,
            defines,
            touches: touched(&node, d),
        });
    }
    out
}

/// The table `ALTER TABLE t …`, `CREATE … INDEX … ON t` or
/// `CREATE … TRIGGER … ON t` is about.
fn touched(node: &SyntaxNode, d: Dialect) -> Option<Key> {
    use sqldiff_core::tokens::{find_kw, of_node, qualified};
    let t = of_node(node);
    let first = t.first()?;
    if first.is_kw("ALTER") && t.get(1).is_some_and(|x| x.is_kw("TABLE")) {
        let mut i = 2;
        if t.get(i).is_some_and(|x| x.is_kw("IF")) {
            i += 2;
        }
        if t.get(i).is_some_and(|x| x.is_kw("ONLY")) {
            i += 1;
        }
        return Key::of_parts(d, &qualified(&t, i).0);
    }
    if first.is_kw("CREATE") {
        let kind = t
            .iter()
            .take(8)
            .position(|x| x.is_any_kw(&["INDEX", "TRIGGER"]))?;
        let on = find_kw(&t, kind + 1, "ON")?;
        return Key::of_parts(d, &qualified(&t, on + 1).0);
    }
    None
}

/// A unit's rules and reason, as the report gives them.
fn why(c: &Conversion, u: &crate::Unit) -> (Vec<String>, String) {
    let worst: Vec<&crate::Note> = u.notes.iter().filter(|n| n.status == u.status).collect();
    let rules = if worst.is_empty() {
        vec!["engine".to_string()]
    } else {
        worst.iter().map(|n| n.rule.clone()).collect()
    };
    let mut reason = worst.first().map(|n| n.text.clone()).unwrap_or_default();
    if reason.is_empty() {
        reason = u
            .findings
            .iter()
            .filter_map(|&i| c.report.findings.get(i))
            .chain(
                u.output_findings
                    .iter()
                    .filter_map(|&i| c.output_check.findings.get(i)),
            )
            .find(|f| f.verdict == sqldiff_core::Verdict::Different || crate::gates(f))
            .map(|f| format!("{}: {}", f.rule, f.witness))
            .unwrap_or_default();
    }
    (rules, reason)
}

/// One node's verdict in one round.
#[derive(Clone)]
struct Seen {
    status: Status,
    rules: Vec<String>,
    reason: String,
}

/// Converts the base catalogs and builds both sides' base catalogs.
///
/// Every top-level statement of a base file is converted and checked on its
/// own (with the catalogs of all base files), so each object gets its own
/// verdict and the engine compares each `CREATE TABLE` by itself.
pub fn prepare(
    files: &[File],
    from: Dialect,
    to: Dialect,
    helpers: &[Helper],
    jobs: usize,
    hooks: &crate::tree::Hooks,
) -> Result<Base, String> {
    let names: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    let source = crate::tree::catalog(
        files.iter().map(|f| f.text.as_str()),
        from,
        Path::new(&format!("base catalogs {}", names.join(", "))),
        None,
    );
    let target_origin = format!("base catalogs {} (converted)", names.join(", "));
    let all: Vec<Vec<Node>> = crate::tree::parallel(jobs, files, |f| nodes(&f.text, from));
    // One piece per statement, in file and statement order.
    let pieces: Vec<(usize, usize)> = all
        .iter()
        .enumerate()
        .flat_map(|(f, ns)| (0..ns.len()).map(move |n| (f, n)))
        .collect();
    let text = |&(f, n): &(usize, usize)| &files[f].text[all[f][n].start..all[f][n].end];
    // Converted with the base catalogs alone.
    let drafts = crate::tree::parallel(jobs, &pieces, |p| {
        crate::rewrite_in(text(p), from, to, Some(&source))
    });
    let outputs: Vec<String> = drafts.iter().map(|d| d.output().to_string()).collect();
    let source = Arc::new(source);
    let target_of = |left_out: &BTreeSet<usize>| {
        crate::tree::catalog(
            outputs
                .iter()
                .enumerate()
                .filter(|(i, _)| !left_out.contains(i))
                .map(|(_, o)| o.as_str()),
            to,
            Path::new(&target_origin),
            None,
        )
    };

    // Round one: each statement's own verdict, the check a tree file gets.
    hooks.stop_if_cancelled()?;
    let opts = Options {
        helpers: helpers.to_vec(),
        facts_a: vec![Arc::clone(&source)],
        facts_b: vec![Arc::new(target_of(&BTreeSet::new()))],
        ..Options::default()
    };
    let work = std::sync::Mutex::new(drafts.into_iter().map(Some).collect::<Vec<_>>());
    let each: Vec<usize> = (0..pieces.len()).collect();
    let own: Vec<Seen> = crate::tree::parallel(jobs, &each, |&i| {
        let d = work.lock().unwrap()[i].take().unwrap();
        let c = crate::check(text(&pieces[i]), d, &opts);
        let mut s = Seen {
            status: Status::Converted,
            rules: Vec::new(),
            reason: String::new(),
        };
        for u in &c.units {
            if u.status > s.status {
                let (rules, reason) = why(&c, u);
                s = Seen {
                    status: u.status,
                    rules,
                    reason,
                };
            }
        }
        if !c.unplaced.is_empty() && s.status < Status::Differs {
            s = Seen {
                status: Status::Differs,
                rules: c
                    .unplaced
                    .iter()
                    .filter_map(|&i| c.output_check.findings.get(i))
                    .map(|f| f.rule.clone())
                    .collect(),
                reason: "The output does not check outside any unit".into(),
            };
        }
        s
    });
    let mut first_fail: HashMap<usize, Seen> = HashMap::new();
    let mut failed: Vec<usize> = (0..pieces.len())
        .filter(|&i| own[i].status >= Status::Differs)
        .collect();
    for &i in &failed {
        first_fail.insert(i, own[i].clone());
    }
    // Left-out statements, and left-out objects with the verdict that left
    // each out.
    let mut left_out: BTreeSet<usize> = BTreeSet::new();
    let mut excluded: Vec<(Key, Seen)> = Vec::new();
    let keys = |i: usize| {
        let node = &all[pieces[i].0][pieces[i].1];
        node.defines.iter().map(|o| &o.key).chain(&node.touches)
    };
    let target = loop {
        for &i in &failed {
            for k in keys(i) {
                if !excluded.iter().any(|(x, _)| x == k) {
                    excluded.push((k.clone(), first_fail[&i].clone()));
                }
            }
            left_out.insert(i);
        }
        // Every statement on a left-out object goes with it, and so does
        // whatever else that statement defines.
        loop {
            let mut grew = false;
            for i in 0..pieces.len() {
                if left_out.contains(&i) {
                    continue;
                }
                let Some(cause) = keys(i).find_map(|k| {
                    excluded
                        .iter()
                        .find(|(x, _)| x.same(k))
                        .map(|(_, s)| s.clone())
                }) else {
                    continue;
                };
                left_out.insert(i);
                for k in keys(i) {
                    if !excluded.iter().any(|(x, _)| x == k) {
                        excluded.push((k.clone(), cause.clone()));
                    }
                }
                grew = true;
            }
            if !grew {
                break;
            }
        }
        let target = target_of(&left_out);
        if failed.is_empty() {
            break target;
        }
        // Later rounds: only what leaving objects out changes, whether the
        // rest of the converted base still binds.
        hooks.stop_if_cancelled()?;
        let opts = Options {
            helpers: helpers.to_vec(),
            facts_a: vec![Arc::new(target)],
            ..Options::default()
        };
        let rest: Vec<usize> = (0..pieces.len())
            .filter(|i| !left_out.contains(i))
            .collect();
        let found = crate::tree::parallel(jobs, &rest, |&i| {
            let b = sqldiff_core::Artifact::new("converted.sql", to, outputs[i].as_str());
            sqldiff_core::check(&b, &opts)
                .findings
                .into_iter()
                .find(crate::gates)
        });
        failed.clear();
        for (&i, f) in rest.iter().zip(found) {
            if let Some(f) = f {
                first_fail.insert(
                    i,
                    Seen {
                        status: Status::Differs,
                        rules: vec![f.rule.clone()],
                        reason: format!(
                            "The converted base does not check as {} once the base objects that did not convert are left out: {}",
                            to.display_name(),
                            f.witness
                        ),
                    },
                );
                failed.push(i);
            }
        }
    };

    // The report: each object once, at its last definition.
    let mut defs: Vec<(usize, &Object)> = Vec::new();
    let mut statements = Vec::new();
    for (i, &(f, n)) in pieces.iter().enumerate() {
        let node = &all[f][n];
        for o in &node.defines {
            defs.retain(|(_, x)| x.key != o.key);
            defs.push((i, o));
        }
        if node.defines.is_empty() && node.touches.is_none() {
            if let Some(s) = first_fail.get(&i) {
                statements.push(StatementReport {
                    file: files[f].path.clone(),
                    line: node.line,
                    status: s.status,
                    rules: s.rules.clone(),
                    reason: s.reason.clone(),
                });
            }
        }
    }
    let mut objects: Vec<(usize, ObjectReport)> = Vec::new();
    for (i, o) in defs {
        let (f, n) = pieces[i];
        let cause = excluded.iter().find(|(x, _)| x.same(&o.key));
        // Left out: the failure that left it out; else its own verdict.
        let seen = match cause {
            Some((_, s)) => s.clone(),
            None if own[i].status > Status::Converted => own[i].clone(),
            None => continue,
        };
        objects.push((
            f,
            ObjectReport {
                kind: o.kind.to_string(),
                name: o.name.clone(),
                file: files[f].path.clone(),
                line: all[f][n].line,
                status: seen.status,
                in_target: cause.is_none(),
                rules: seen.rules,
                reason: seen.reason,
            },
        ));
    }
    // Left out first, then in the order of the files and lines.
    objects.sort_by(|(fa, a), (fb, b)| {
        a.in_target
            .cmp(&b.in_target)
            .then(fa.cmp(fb))
            .then(a.line.cmp(&b.line))
    });
    let objects = objects.into_iter().map(|(_, o)| o).collect();
    let catalog_files = files
        .iter()
        .zip(&all)
        .map(|(f, ns)| {
            let defs = ns.iter().flat_map(|n| &n.defines);
            CatalogFile {
                path: f.path.clone(),
                sha256: crate::review::file_hash(&f.text),
                tables: defs.clone().filter(|o| !o.key.routine).count(),
                routines: defs.filter(|o| o.key.routine).count(),
            }
        })
        .collect();
    Ok(Base {
        source: Arc::try_unwrap(source).unwrap_or_else(|a| (*a).clone()),
        target,
        files: catalog_files,
        objects,
        statements,
        hash: hash(files),
    })
}

/// What a side's catalog is called in binding reasons when base catalogs
/// are used: `what` and the files.
pub fn origin(what: &str, files: &[CatalogFile]) -> String {
    let names: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    format!("{what} and base catalogs {}", names.join(", "))
}

/// The tree's objects (`texts`, loaded in order) over the `base` catalog: an
/// object the tree defines replaces every base object of that name, all
/// overloads of a routine included, and statements in the tree that alter a
/// base table change it. With a stand-in `stem` (templates), the tree's
/// objects are read as `tree::catalog` reads them.
pub fn over<'a>(
    base: &Catalog,
    texts: impl Iterator<Item = &'a str>,
    d: Dialect,
    origin: String,
    stem: Option<&str>,
) -> Catalog {
    let mut cat = base.clone();
    cat.origin = origin;
    let own = match stem {
        None => {
            let mut own = Catalog::default();
            for t in texts {
                let root = sqldiff_syntax::parse(t, d).syntax();
                ddl::load_tree(&mut own, &root, d);
                ddl::load_tree(&mut cat, &root, d);
            }
            own
        }
        Some(_) => {
            let own = crate::tree::catalog(texts, d, Path::new(""), stem);
            cat.extend(d, &own);
            own
        }
    };
    shadow(&mut cat, &own, d);
    cat
}

/// A table's key as `Catalog::put_table` compares it.
type Exact = (Option<String>, String);

/// Removes from `cat` every table and routine with a name `own` defines
/// that is not `own`'s own.
fn shadow(cat: &mut Catalog, own: &Catalog, d: Dialect) {
    let exact = |s: &Option<String>, n: &str| (s.as_deref().map(|s| key(d, s)), key(d, n));
    let mut tables: HashMap<String, Vec<(Key, Exact)>> = HashMap::new();
    for t in &own.tables {
        let k = Key::new(d, false, t.schema.as_deref(), &t.name);
        tables
            .entry(k.name.clone())
            .or_default()
            .push((k, exact(&t.schema, &t.name)));
    }
    cat.tables.retain(|t| {
        let Some(list) = tables.get(&key(d, &t.name)) else {
            return true;
        };
        let k = Key::new(d, false, t.schema.as_deref(), &t.name);
        let e = exact(&t.schema, &t.name);
        !list.iter().any(|(o, _)| o.same(&k)) || list.iter().any(|(_, oe)| *oe == e)
    });
    let mut routines: HashMap<String, Vec<Key>> = HashMap::new();
    let mut mine: HashSet<String> = HashSet::new();
    for r in &own.routines {
        let k = Key::new(d, true, r.schema.as_deref(), &r.name);
        routines.entry(k.name.clone()).or_default().push(k);
        mine.insert(serde_json::to_string(r).unwrap_or_default());
    }
    cat.routines.retain(|r| {
        let Some(list) = routines.get(&key(d, &r.name)) else {
            return true;
        };
        let k = Key::new(d, true, r.schema.as_deref(), &r.name);
        !list.iter().any(|o| o.same(&k))
            || mine.contains(&serde_json::to_string(r).unwrap_or_default())
    });
}
