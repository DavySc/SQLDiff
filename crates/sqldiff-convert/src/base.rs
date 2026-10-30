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
//! object gets its own verdict. A base object whose statement is worse than
//! converted under assumptions (a decision, not proven, `differs`, `not
//! converted`), or that a statement altering it or putting an index or a
//! trigger on it leaves so, is suspect, and so is every object whose
//! statement uses a suspect one. Suspect objects stay on side B (one the
//! rules wrote no definition for stands in as its source definition), and
//! every unit of the tree that uses one carries `ASM-BASE-SUSPECT`, an
//! assumption a person accepts once it is checked.
//!
//! When the target's own base DDL is at hand (`--catalog-b FILE`, in the
//! target dialect), side B holds exactly that instead: nothing is converted
//! or flagged, and a base object it lacks does not bind on side B.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
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
    read_files(paths, "--catalog", "source")
}

/// Reads the target's base catalog files (`--catalog-b`), as `read` does.
/// They need the source's (`a`): side A needs the base objects too.
pub fn read_b(paths: &[PathBuf], a: &[PathBuf], no_catalog: bool) -> Result<Vec<File>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    if no_catalog {
        return Err("--catalog-b and --no-catalog do not go together: --no-catalog checks each file alone, without any catalog".into());
    }
    if a.is_empty() {
        return Err("--catalog-b needs --catalog: the source side binds against the base objects too, in the source dialect".into());
    }
    read_files(paths, "--catalog-b", "target")
}

fn read_files(paths: &[PathBuf], flag: &str, side: &str) -> Result<Vec<File>, String> {
    let mut seen = HashSet::new();
    let mut files = Vec::new();
    for p in paths {
        let path = p.display().to_string();
        if !seen.insert(path.clone()) {
            return Err(format!("{path}: given twice as {flag}"));
        }
        if p.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        {
            return Err(format!(
                "{path}: a base catalog for a conversion is DDL in the {side} dialect; snapshot bundles (.json) are not read here"
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

/// One hash over the contents of the base catalogs, in order, the target's
/// (`b`) after the source's (CRLF read as LF, paths left out): what a review
/// decision is tied to. `None` without base catalogs.
pub fn hash(files: &[File], b: &[File]) -> Option<String> {
    use sha2::{Digest, Sha256};
    if files.is_empty() {
        return None;
    }
    let mut h = Sha256::new();
    for f in files {
        h.update(crate::review::file_hash(&f.text).as_bytes());
        h.update([0]);
    }
    if !b.is_empty() {
        h.update(b"catalog-b");
        h.update([0]);
    }
    for f in b {
        h.update(crate::review::file_hash(&f.text).as_bytes());
        h.update([0]);
    }
    Some(sqldiff_core::model::hex(&h.finalize()))
}

/// What the report says about one base catalog file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogFile {
    pub path: String,
    /// `review::file_hash` of its text.
    pub sha256: String,
    /// Tables and views, and routines, it defines.
    pub tables: usize,
    pub routines: usize,
}

/// A base object whose conversion is not plainly converted.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Whether it is suspect: its own conversion is worse than converted
    /// under assumptions, or it uses a suspect base object. Every statement
    /// in the tree that uses it carries `ASM-BASE-SUSPECT`.
    pub suspect: bool,
    pub rules: Vec<String>,
    pub reason: String,
}

/// A base statement that defines no object and did not convert.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// The base objects as converted, without those that did not convert;
    /// or, with `files_b`, those files' objects.
    pub target: Catalog,
    pub files: Vec<CatalogFile>,
    /// The target's base catalog files (`--catalog-b`), if given.
    pub files_b: Vec<CatalogFile>,
    /// Suspect base objects, then the others that are not plainly
    /// converted.
    pub objects: Vec<ObjectReport>,
    /// Statements that define no object and did not convert.
    pub statements: Vec<StatementReport>,
    pub hash: Option<String>,
    suspects: Vec<Suspect>,
}

/// A suspect base object, for flagging what uses it.
struct Suspect {
    key: Key,
    /// How the note names it.
    label: String,
}

/// The rule a statement using a suspect base object carries.
pub const BASE_SUSPECT: &str = "ASM-BASE-SUSPECT";

/// The name an object is known by, for matching across files and sides:
/// letter case as the dialect compares, and the dialect's default schema for
/// an unqualified name. Without a default schema (Db2 for i), an unqualified
/// name may be any schema's.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Key {
    pub(crate) routine: bool,
    pub(crate) schema: Option<String>,
    pub(crate) name: String,
}

impl Key {
    pub(crate) fn new(d: Dialect, routine: bool, schema: Option<&str>, name: &str) -> Key {
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

    pub(crate) fn of_parts(d: Dialect, parts: &[Ident]) -> Option<Key> {
        let (name, schema) = match parts {
            [n] => (n, None),
            [s, n] => (n, Some(s.stored(d))),
            _ => return None,
        };
        Some(Key::new(d, false, schema.as_deref(), &name.stored(d)))
    }

    /// The same name, whether a table or a routine: what a reference in a
    /// statement, which does not say which, can mean.
    pub(crate) fn same_name(&self, o: &Key) -> bool {
        self.name == o.name
            && (self.schema == o.schema || self.schema.is_none() || o.schema.is_none())
    }

    fn same(&self, o: &Key) -> bool {
        self.routine == o.routine
            && self.name == o.name
            && (self.schema == o.schema || self.schema.is_none() || o.schema.is_none())
    }
}

pub(crate) struct Object {
    pub(crate) key: Key,
    pub(crate) kind: &'static str,
    pub(crate) name: String,
}

/// A top-level statement of a base file.
pub(crate) struct Node {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) line: usize,
    /// What it creates.
    pub(crate) defines: Vec<Object>,
    /// The table it alters or puts an index or a trigger on.
    touches: Option<Key>,
}

pub(crate) fn nodes(text: &str, d: Dialect) -> Vec<Node> {
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

/// Every name in `text` that may name a table, view or routine, with its
/// offset: each dotted name by every two adjacent parts (`s.t.c` names the
/// table `s.t` and the column `c` of `t`), a name alone by itself. Columns
/// and aliases are among them; a column named like a suspect object flags
/// too (a false alarm, never a miss).
pub(crate) fn refs_at(text: &str, d: Dialect) -> Vec<(usize, Key)> {
    let root = sqldiff_syntax::parse(text, d).syntax();
    refs_in(&sqldiff_core::tokens::of_node(&root), d)
}

/// `refs_at` over tokens already read.
pub(crate) fn refs_in(t: &[sqldiff_core::tokens::Tok], d: Dialect) -> Vec<(usize, Key)> {
    use sqldiff_core::tokens::qualified;
    let mut out = Vec::new();
    let mut i = 0;
    while i < t.len() {
        let after_dot = i > 0 && t[i - 1].kind == SyntaxKind::DOT;
        if !t[i].is_name() || after_dot {
            i += 1;
            continue;
        }
        let (parts, next) = qualified(t, i);
        let pairs: Vec<&[Ident]> = match parts.len() {
            0 | 1 => vec![&parts[..]],
            _ => parts.windows(2).collect(),
        };
        for p in pairs {
            if let Some(k) = Key::of_parts(d, p) {
                out.push((t[i].start, k));
            }
        }
        i = next.max(i + 1);
    }
    out
}

fn refs(text: &str, d: Dialect) -> Vec<Key> {
    refs_at(text, d).into_iter().map(|(_, k)| k).collect()
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
///
/// With the target's own base catalogs (`b`), side B is those, as given.
pub fn prepare(
    files: &[File],
    b: &[File],
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
    if !b.is_empty() {
        let names_b: Vec<&str> = b.iter().map(|f| f.path.as_str()).collect();
        let target = crate::tree::catalog(
            b.iter().map(|f| f.text.as_str()),
            to,
            Path::new(&format!("base catalogs {}", names_b.join(", "))),
            None,
        );
        return Ok(Base {
            source,
            target,
            files: catalog_files(files, from),
            files_b: catalog_files(b, to),
            objects: Vec::new(),
            statements: Vec::new(),
            hash: hash(files, b),
            suspects: Vec::new(),
        });
    }
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
    let mut target = crate::tree::catalog(
        outputs.iter().map(String::as_str),
        to,
        Path::new(&target_origin),
        None,
    );

    // An object the rules wrote no definition for (its statement not
    // converted, kept as a comment) stands in the target catalog as its
    // source definition, so what uses it binds and is flagged, not refused.
    let named = |c: &Catalog, k: &Key| {
        c.tables.iter().any(|t| key(to, &t.name) == k.name)
            || c.routines.iter().any(|r| key(to, &r.name) == k.name)
    };
    let mut stand_in = Catalog::default();
    for p in &pieces {
        let node = &all[p.0][p.1];
        if node.defines.iter().any(|o| !named(&target, &o.key)) {
            let root = sqldiff_syntax::parse(text(p), from).syntax();
            ddl::load_tree(&mut stand_in, &root, from);
        }
    }
    target.extend(to, &stand_in);

    // Round one: each statement's own verdict, the check a tree file gets.
    hooks.stop_if_cancelled()?;
    let opts = Options {
        helpers: helpers.to_vec(),
        facts_a: vec![Arc::clone(&source)],
        facts_b: vec![Arc::new(target.clone())],
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
    // Suspect: what a statement that does not plainly convert defines or
    // changes, and then what a statement using a suspect object defines.
    let refs_of: Vec<Vec<Key>> = crate::tree::parallel(jobs, &pieces, |p| refs(text(p), from));
    let keys = |i: usize| {
        let node = &all[pieces[i].0][pieces[i].1];
        node.defines.iter().map(|o| &o.key).chain(&node.touches)
    };
    let mut suspect: Vec<(Key, Seen)> = Vec::new();
    let mut failed: HashSet<usize> = HashSet::new();
    for (i, seen) in own.iter().enumerate() {
        if seen.status > Status::Assumptions {
            failed.insert(i);
            for k in keys(i) {
                if !suspect.iter().any(|(x, _)| x == k) {
                    suspect.push((k.clone(), seen.clone()));
                }
            }
        }
    }
    loop {
        let mut grew = false;
        for (i, found) in refs_of.iter().enumerate() {
            let mine: Vec<&Key> = keys(i).collect();
            let Some((via, cause)) = found.iter().find_map(|r| {
                if mine.iter().any(|k| k.same_name(r)) {
                    return None;
                }
                suspect.iter().find(|(x, _)| x.same_name(r))
            }) else {
                continue;
            };
            let cause = Seen {
                status: cause.status,
                rules: cause.rules.clone(),
                reason: format!(
                    "Uses `{}`, a suspect base object: {}",
                    via.name, cause.reason
                ),
            };
            for k in mine {
                if !suspect.iter().any(|(x, _)| x == k) {
                    suspect.push((k.clone(), cause.clone()));
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }

    for (k, seen) in &mut suspect {
        if named(&stand_in, k) {
            seen.reason.push_str(
                " (no converted definition: its source definition stands in the target catalog)",
            );
        }
    }

    // The report: each object once, at its last definition.
    let mut defs: Vec<(usize, &Object)> = Vec::new();
    let mut statements = Vec::new();
    for (i, &(f, n)) in pieces.iter().enumerate() {
        let node = &all[f][n];
        for o in &node.defines {
            defs.retain(|(_, x)| x.key != o.key);
            defs.push((i, o));
        }
        if node.defines.is_empty() && node.touches.is_none() && failed.contains(&i) {
            statements.push(StatementReport {
                file: files[f].path.clone(),
                line: node.line,
                status: own[i].status,
                rules: own[i].rules.clone(),
                reason: own[i].reason.clone(),
            });
        }
    }
    let mut objects: Vec<(usize, ObjectReport)> = Vec::new();
    let mut suspects = Vec::new();
    for (i, o) in defs {
        let (f, n) = pieces[i];
        let cause = suspect.iter().find(|(x, _)| x.same(&o.key));
        // Suspect: what made it suspect; else its own verdict.
        let seen = match cause {
            Some((_, s)) => s.clone(),
            None if own[i].status > Status::Converted => own[i].clone(),
            None => continue,
        };
        if cause.is_some() {
            suspects.push(Suspect {
                key: o.key.clone(),
                label: format!(
                    "{} `{}` ({} line {}: {})",
                    o.kind,
                    o.name,
                    files[f].path,
                    all[f][n].line,
                    seen.status.label()
                ),
            });
        }
        objects.push((
            f,
            ObjectReport {
                kind: o.kind.to_string(),
                name: o.name.clone(),
                file: files[f].path.clone(),
                line: all[f][n].line,
                status: seen.status,
                suspect: cause.is_some(),
                rules: seen.rules,
                reason: seen.reason,
            },
        ));
    }
    // Suspect first, then in the order of the files and lines.
    objects.sort_by(|(fa, a), (fb, b)| {
        b.suspect
            .cmp(&a.suspect)
            .then(fa.cmp(fb))
            .then(a.line.cmp(&b.line))
    });
    let objects = objects.into_iter().map(|(_, o)| o).collect();
    Ok(Base {
        source: Arc::try_unwrap(source).unwrap_or_else(|a| (*a).clone()),
        target,
        files: summaries(files, &all),
        files_b: Vec::new(),
        objects,
        statements,
        hash: hash(files, &[]),
        suspects,
    })
}

/// What the report says about each of `files`, in dialect `d`.
fn catalog_files(files: &[File], d: Dialect) -> Vec<CatalogFile> {
    let all: Vec<Vec<Node>> = files.iter().map(|f| nodes(&f.text, d)).collect();
    summaries(files, &all)
}

fn summaries(files: &[File], all: &[Vec<Node>]) -> Vec<CatalogFile> {
    files
        .iter()
        .zip(all)
        .map(|(f, ns)| {
            let defs = ns.iter().flat_map(|n| &n.defines);
            CatalogFile {
                path: f.path.clone(),
                sha256: crate::review::file_hash(&f.text),
                tables: defs.clone().filter(|o| !o.key.routine).count(),
                routines: defs.filter(|o| o.key.routine).count(),
            }
        })
        .collect()
}

impl Base {
    /// Flags every unit of the tree (`files`: path and text, converted into
    /// `convs`) that uses a suspect base object with `ASM-BASE-SUSPECT`. An
    /// object the tree defines replaces the base object of its name; a tree
    /// object whose statement uses a suspect object is suspect itself.
    pub fn flag<'c>(
        &self,
        files: &[(&str, &str)],
        convs: impl Iterator<Item = &'c mut Conversion>,
        d: Dialect,
    ) {
        if self.suspects.is_empty() {
            return;
        }
        let tree: Vec<Vec<Node>> = files.iter().map(|(_, t)| nodes(t, d)).collect();
        let own: Vec<&Key> = tree
            .iter()
            .flatten()
            .flat_map(|n| n.defines.iter().map(|o| &o.key))
            .collect();
        let mut set: Vec<(Key, String)> = self
            .suspects
            .iter()
            .filter(|s| !own.iter().any(|k| k.same(&s.key)))
            .map(|s| (s.key.clone(), s.label.clone()))
            .collect();
        loop {
            let mut grew = false;
            for ((path, text), ns) in files.iter().zip(&tree) {
                for n in ns {
                    let mine: Vec<&Key> = n.defines.iter().map(|o| &o.key).collect();
                    let via = refs(&text[n.start..n.end], d).into_iter().find_map(|r| {
                        if mine.iter().any(|k| k.same_name(&r)) {
                            return None;
                        }
                        set.iter()
                            .find(|(x, _)| x.same_name(&r))
                            .map(|(x, _)| x.name.clone())
                    });
                    let Some(via) = via else { continue };
                    for o in &n.defines {
                        if !set.iter().any(|(x, _)| *x == o.key) {
                            set.push((
                                o.key.clone(),
                                format!(
                                    "{} `{}` ({path} line {}: uses `{via}`)",
                                    o.kind, o.name, n.line
                                ),
                            ));
                            grew = true;
                        }
                    }
                }
            }
            if !grew {
                break;
            }
        }
        for ((_, text), c) in files.iter().zip(convs) {
            let found = refs_at(text, d);
            let mut changed = false;
            for u in &mut c.units {
                let mut hit: Vec<&str> = Vec::new();
                for (at, r) in &found {
                    if *at < u.src.start || *at >= u.src.end {
                        continue;
                    }
                    for (x, label) in &set {
                        if x.same_name(r) && !hit.contains(&label.as_str()) {
                            hit.push(label);
                        }
                    }
                }
                if hit.is_empty() {
                    continue;
                }
                u.notes.push(crate::Note {
                    status: Status::Assumptions,
                    rule: BASE_SUSPECT.into(),
                    text: format!(
                        "Uses base objects whose own conversion is not plain: {}. The target catalog holds the rules' conversion of them, which may not be what the target has; the base catalogs report says why. Accept this once that is checked (a column or alias named like one of them also counts).",
                        hit.join("; ")
                    ),
                });
                u.status = u.status.max(Status::Assumptions);
                changed = true;
            }
            if changed {
                crate::recount(c);
            }
        }
    }

    /// The files side B's base objects come from: the target's own when
    /// given, else the converted source's.
    pub fn target_files(&self) -> &[CatalogFile] {
        if self.files_b.is_empty() {
            &self.files
        } else {
            &self.files_b
        }
    }
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
