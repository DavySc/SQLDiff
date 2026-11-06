//! The plan of a tree conversion: which file defines and uses which object,
//! the files grouped and ordered so that what a file uses comes first, and
//! the choice of an increment (a set of files to check in one run).
//!
//! The plan is a scheduling aid only. It never decides what counts as
//! converted: every check still binds against the catalogs of the whole tree
//! and a file nobody checked (in this run or by a matching stored result)
//! counts as not converted. A use the scan misses (a name inside dynamic
//! SQL, a name written in a form this does not read) can therefore put a
//! file later than ideal, never make a result wrong; a use it invents (a
//! column named like a table) can only join groups that would otherwise be
//! apart.
//!
//! Definitions come from the catalog loader (tables, views, routines,
//! triggers) and a scan for `CREATE TYPE`. Uses are the identifier chains of
//! the file's tokens: `schema.name` (the last two parts of a longer chain)
//! matches an object exactly, a bare `name` matches an object in the
//! dialect's default schema. Comments and strings hold no identifiers.
//! Nothing here names a corpus.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqldiff_core::catalog::{self, ddl, RoutineKind, TableKind};
use sqldiff_core::Dialect;
use sqldiff_syntax::{ast, SyntaxKind, SyntaxNode};

use crate::tree::Input;

/// Version of `plan.json`.
pub const FORMAT: u32 = 1;

/// Where the plan is written, below the output directory.
pub const FILE: &str = ".sqldiff/plan.json";

/// What a file defines, in the order a deployment needs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Type,
    Table,
    View,
    Function,
    Procedure,
    Trigger,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Type => "type",
            Kind::Table => "table",
            Kind::View => "view",
            Kind::Function => "function",
            Kind::Procedure => "procedure",
            Kind::Trigger => "trigger",
        }
    }
}

/// An object a file defines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Defined {
    pub kind: Kind,
    /// `schema.name`, lower case, the default schema filled in.
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanFile {
    pub path: String,
    pub bytes: usize,
    pub lines: usize,
    pub defines: Vec<Defined>,
    /// Objects other files define that this file names, `schema.name`.
    pub uses: Vec<String>,
    /// Index into `Plan::groups`.
    pub group: usize,
}

/// Files that must be checked together because they use each other in a
/// cycle, or one file alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    /// Indices into `Plan::files`, by path.
    pub files: Vec<usize>,
    /// More than one file: they use each other in a cycle.
    pub cyclic: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub format: u32,
    pub from: String,
    /// Hash of every file's path and text: a plan is only valid for the tree
    /// it was made from.
    pub tree_hash: String,
    /// By path.
    pub files: Vec<PlanFile>,
    /// In plan order: a group comes after every group it uses.
    pub groups: Vec<Group>,
}

fn default_schema(dialect: Dialect) -> &'static str {
    catalog::default_schema(dialect).unwrap_or("")
}

fn key(schema: Option<&str>, name: &str, dialect: Dialect) -> String {
    format!(
        "{}.{}",
        schema.unwrap_or(default_schema(dialect)).to_lowercase(),
        name.to_lowercase()
    )
}

/// The identifier chains of a parsed file, each as unquoted parts.
fn chains(root: &SyntaxNode) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    // Whether the last significant token was a part (a dot may follow) or a
    // dot (a part must follow).
    let mut after_part = false;
    let mut after_dot = false;
    for t in root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
    {
        let k = t.kind();
        if k.is_trivia() {
            continue;
        }
        let part = matches!(k, SyntaxKind::IDENT | SyntaxKind::QUOTED_IDENT);
        if part {
            if !after_dot && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cur.push(ast::unquote(t.text()));
            after_part = true;
            after_dot = false;
        } else if k == SyntaxKind::DOT && after_part {
            after_part = false;
            after_dot = true;
        } else {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            after_part = false;
            after_dot = false;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `CREATE TYPE name` statements: the catalog does not hold types.
fn types(root: &SyntaxNode, dialect: Dialect) -> Vec<String> {
    let toks: Vec<_> = root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| !t.kind().is_trivia())
        .collect();
    let mut out = Vec::new();
    for i in 0..toks.len().saturating_sub(2) {
        let is = |j: usize, w: &str| {
            toks[j].kind() == SyntaxKind::IDENT && toks[j].text().eq_ignore_ascii_case(w)
        };
        if !(is(i, "create") && is(i + 1, "type")) {
            continue;
        }
        let mut parts = Vec::new();
        let mut j = i + 2;
        while let Some(t) = toks.get(j) {
            if !matches!(t.kind(), SyntaxKind::IDENT | SyntaxKind::QUOTED_IDENT) {
                break;
            }
            parts.push(ast::unquote(t.text()));
            if toks.get(j + 1).map(|d| d.kind()) == Some(SyntaxKind::DOT) {
                j += 2;
            } else {
                break;
            }
        }
        if let Some(name) = parts.pop() {
            out.push(key(parts.pop().as_deref(), &name, dialect));
        }
    }
    out
}

struct Scan {
    defines: Vec<Defined>,
    /// Every identifier chain, reduced to the object keys it could mean.
    mentions: BTreeSet<String>,
    /// The bare-name keys: only objects in the default schema.
    bare: BTreeSet<String>,
}

fn scan(text: &str, dialect: Dialect) -> Scan {
    let root = sqldiff_syntax::parse(text, dialect).syntax();
    let mut cat = catalog::Catalog::default();
    ddl::load_tree(&mut cat, &root, dialect);
    let mut defines = Vec::new();
    for t in &cat.tables {
        let kind = match t.kind {
            TableKind::Table => Kind::Table,
            TableKind::View => Kind::View,
            TableKind::Temporary | TableKind::Variable => continue,
        };
        defines.push(Defined {
            kind,
            name: key(t.schema.as_deref(), &t.name, dialect),
        });
    }
    for r in &cat.routines {
        let kind = match r.kind {
            RoutineKind::Function => Kind::Function,
            RoutineKind::Procedure => Kind::Procedure,
            RoutineKind::Trigger => Kind::Trigger,
        };
        defines.push(Defined {
            kind,
            name: key(r.schema.as_deref(), &r.name, dialect),
        });
    }
    for t in &cat.triggers {
        defines.push(Defined {
            kind: Kind::Trigger,
            name: key(t.schema.as_deref(), &t.name, dialect),
        });
    }
    for name in types(&root, dialect) {
        defines.push(Defined {
            kind: Kind::Type,
            name,
        });
    }
    defines.sort_by(|a, b| (a.kind, &a.name).cmp(&(b.kind, &b.name)));
    defines.dedup();
    let mut mentions = BTreeSet::new();
    let mut bare = BTreeSet::new();
    for c in chains(&root) {
        match c.as_slice() {
            [] => {}
            [n] => {
                bare.insert(key(None, n, dialect));
            }
            [.., s, n] => {
                mentions.insert(key(Some(s), n, dialect));
            }
        }
    }
    Scan {
        defines,
        mentions,
        bare,
    }
}

/// Lowest in the plan order first: what a file defines decides where it
/// stands among files that do not depend on each other.
fn rank(f: &PlanFile) -> u8 {
    f.defines
        .iter()
        .map(|d| d.kind as u8)
        .min()
        // Scripts and data files: nothing defined, run last.
        .unwrap_or(Kind::Trigger as u8 + 1)
}

/// Strongly connected components of a graph with edges `from -> to`, in an
/// order where every component comes after the ones it points to is not
/// guaranteed; the caller orders them. Iterative: a tree can have chains of
/// thousands of files.
fn components(edges: &[BTreeSet<usize>]) -> Vec<Vec<usize>> {
    let n = edges.len();
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut next = 0usize;
    let mut out = Vec::new();
    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        // (node, iterator position into its sorted successors)
        let mut call: Vec<(usize, Vec<usize>, usize)> =
            vec![(root, edges[root].iter().copied().collect(), 0)];
        index[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on[root] = true;
        while let Some((v, succ, pos)) = call.last_mut() {
            let v = *v;
            if *pos < succ.len() {
                let w = succ[*pos];
                *pos += 1;
                if index[w] == usize::MAX {
                    index[w] = next;
                    low[w] = next;
                    next += 1;
                    stack.push(w);
                    on[w] = true;
                    call.push((w, edges[w].iter().copied().collect(), 0));
                } else if on[w] {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                call.pop();
                if let Some((p, _, _)) = call.last() {
                    low[*p] = low[*p].min(low[v]);
                }
                if low[v] == index[v] {
                    let mut c = Vec::new();
                    loop {
                        let w = stack.pop().unwrap();
                        on[w] = false;
                        c.push(w);
                        if w == v {
                            break;
                        }
                    }
                    c.sort_unstable();
                    out.push(c);
                }
            }
        }
    }
    out
}

impl Plan {
    /// Plans `inputs` (source files in `from`). Deterministic: the same tree
    /// gives the same plan.
    pub fn build(inputs: &[Input], from: Dialect) -> Plan {
        let mut order: Vec<usize> = (0..inputs.len()).collect();
        order.sort_by(|&a, &b| inputs[a].rel.cmp(&inputs[b].rel));
        let mut hasher = Sha256::new();
        let mut scans = Vec::new();
        for &i in &order {
            let inp = &inputs[i];
            hasher.update((inp.rel.len() as u64).to_le_bytes());
            hasher.update(inp.rel.as_bytes());
            hasher.update((inp.text.len() as u64).to_le_bytes());
            hasher.update(inp.text.as_bytes());
            scans.push(scan(&inp.text, from));
        }
        let tree_hash = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        // Who defines what.
        let mut definers: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (f, s) in scans.iter().enumerate() {
            for d in &s.defines {
                definers.entry(d.name.as_str()).or_default().push(f);
            }
        }
        let dflt = format!("{}.", default_schema(from).to_lowercase());
        let mut uses: Vec<BTreeSet<&str>> = Vec::new();
        let mut edges: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); scans.len()];
        for (f, s) in scans.iter().enumerate() {
            let mut u: BTreeSet<&str> = BTreeSet::new();
            for m in s.mentions.iter().chain(s.bare.iter()) {
                // A bare name means the default schema only.
                if s.bare.contains(m) && !s.mentions.contains(m) && !m.starts_with(&dflt) {
                    continue;
                }
                if let Some((name, ds)) = definers.get_key_value(m.as_str()) {
                    let others: Vec<usize> = ds.iter().copied().filter(|&d| d != f).collect();
                    if !others.is_empty() {
                        u.insert(*name);
                        edges[f].extend(others);
                    }
                }
            }
            uses.push(u);
        }
        let mut files: Vec<PlanFile> = order
            .iter()
            .zip(scans.iter())
            .zip(uses.iter())
            .map(|((&i, s), u)| PlanFile {
                path: inputs[i].rel.clone(),
                bytes: inputs[i].text.len(),
                lines: inputs[i].text.matches('\n').count() + 1,
                defines: s.defines.clone(),
                uses: u.iter().map(|s| s.to_string()).collect(),
                group: 0,
            })
            .collect();
        // Strongly connected components, then a topological order of the
        // components: dependencies first, ties by rank then path.
        let comps = components(&edges);
        let mut comp_of = vec![0usize; files.len()];
        for (c, members) in comps.iter().enumerate() {
            for &m in members {
                comp_of[m] = c;
            }
        }
        let mut deps: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); comps.len()];
        let mut users: Vec<Vec<usize>> = vec![Vec::new(); comps.len()];
        for (f, es) in edges.iter().enumerate() {
            for &t in es {
                if comp_of[f] != comp_of[t] && deps[comp_of[f]].insert(comp_of[t]) {
                    users[comp_of[t]].push(comp_of[f]);
                }
            }
        }
        let keys: Vec<(u8, usize)> = comps
            .iter()
            .map(|c| (c.iter().map(|&m| rank(&files[m])).min().unwrap_or(0), c[0]))
            .collect();
        let sort_key = |c: usize| keys[c];
        let mut waiting: Vec<usize> = deps.iter().map(|d| d.len()).collect();
        let mut ready: BinaryHeap<Reverse<(u8, usize, usize)>> = BinaryHeap::new();
        for (c, w) in waiting.iter().enumerate() {
            if *w == 0 {
                let (r, p) = sort_key(c);
                ready.push(Reverse((r, p, c)));
            }
        }
        let mut groups = Vec::with_capacity(comps.len());
        while let Some(Reverse((_, _, c))) = ready.pop() {
            let g = groups.len();
            for &m in &comps[c] {
                files[m].group = g;
            }
            groups.push(Group {
                files: comps[c].clone(),
                cyclic: comps[c].len() > 1,
            });
            for &u in &users[c] {
                waiting[u] -= 1;
                if waiting[u] == 0 {
                    let (r, p) = sort_key(u);
                    ready.push(Reverse((r, p, u)));
                }
            }
        }
        debug_assert_eq!(groups.len(), comps.len());
        Plan {
            format: FORMAT,
            from: from.display_name().to_string(),
            tree_hash,
            files,
            groups,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("plan serializes")
    }

    /// A stored plan, if it was made for exactly this tree.
    pub fn matches(&self, inputs: &[Input], from: Dialect) -> bool {
        Plan::build(inputs, from).tree_hash == self.tree_hash
    }
}

/// Which files an increment holds. Criteria add up; none given means the
/// whole tree (today's behaviour).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Select {
    /// The next N groups, in plan order, that still have a file not checked.
    pub next: Option<usize>,
    /// Files defining an object in one of these schemas.
    pub schemas: Vec<String>,
    /// An object (`schema.name` or a bare name) and everything it uses that
    /// is not checked yet.
    pub objects: Vec<String>,
    /// Paths matching a glob (`*` within a segment, `**` across segments,
    /// `?` one character).
    pub paths: Vec<String>,
    /// Every file that is not checked.
    pub changed: bool,
}

impl Select {
    pub fn is_empty(&self) -> bool {
        *self == Select::default()
    }
}

/// Glob match of `/`-separated paths, case-sensitive.
pub fn glob(pattern: &str, path: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match p.split_first() {
            None => s.is_empty(),
            Some((b'*', rest)) => {
                if let Some(rest2) = rest.strip_prefix(b"*") {
                    // `**` crosses `/`; `**/` also matches nothing.
                    let rest2 = rest2.strip_prefix(b"/").unwrap_or(rest2);
                    (0..=s.len()).any(|i| go(rest2, &s[i..]))
                } else {
                    (0..=s.len())
                        .take_while(|&i| i == 0 || s[i - 1] != b'/')
                        .any(|i| go(rest, &s[i..]))
                }
            }
            Some((b'?', rest)) => s.first().is_some_and(|&c| c != b'/') && go(rest, &s[1..]),
            Some((&c, rest)) => s.first() == Some(&c) && go(rest, &s[1..]),
        }
    }
    go(pattern.as_bytes(), path.as_bytes())
}

impl Plan {
    /// The files of the increment `sel` names, as a mask over `files`.
    /// `checked[i]` says file `i` has a stored result that matches (it needs
    /// no check). Fails when a schema, object or path matches nothing.
    pub fn select(&self, sel: &Select, checked: &[bool]) -> Result<Vec<bool>, String> {
        assert_eq!(checked.len(), self.files.len());
        if sel.is_empty() {
            return Ok(vec![true; self.files.len()]);
        }
        let mut on = vec![false; self.files.len()];
        if sel.changed {
            for (i, c) in checked.iter().enumerate() {
                on[i] |= !c;
            }
        }
        if let Some(n) = sel.next {
            let mut left = n;
            for g in &self.groups {
                if left == 0 {
                    break;
                }
                if g.files.iter().all(|&f| checked[f]) {
                    continue;
                }
                for &f in &g.files {
                    on[f] = true;
                }
                left -= 1;
            }
        }
        for s in &sel.schemas {
            let prefix = format!("{}.", s.to_lowercase());
            let mut any = false;
            for (i, f) in self.files.iter().enumerate() {
                if f.defines.iter().any(|d| d.name.starts_with(&prefix)) {
                    on[i] = true;
                    any = true;
                }
            }
            if !any {
                return Err(format!("--schema {s}: no file defines an object in it"));
            }
        }
        for p in &sel.paths {
            let mut any = false;
            for (i, f) in self.files.iter().enumerate() {
                if glob(p, &f.path) {
                    on[i] = true;
                    any = true;
                }
            }
            if !any {
                return Err(format!("--path {p}: no file matches"));
            }
        }
        for o in &sel.objects {
            let want = o.to_lowercase();
            let hit = |name: &str| {
                name == want || (!want.contains('.') && name.rsplit('.').next() == Some(&want))
            };
            let seeds: Vec<usize> = self
                .files
                .iter()
                .enumerate()
                .filter(|(_, f)| f.defines.iter().any(|d| hit(&d.name)))
                .map(|(i, _)| i)
                .collect();
            if seeds.is_empty() {
                return Err(format!("--object {o}: no file defines it"));
            }
            // The object's group(s), then every group they use that is not
            // checked yet (a group depends on the groups of what it uses).
            let mut definer: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
            for (i, f) in self.files.iter().enumerate() {
                for d in &f.defines {
                    definer.entry(d.name.as_str()).or_default().push(i);
                }
            }
            let mut todo: Vec<usize> = seeds.iter().map(|&s| self.files[s].group).collect();
            let mut seen: BTreeSet<usize> = BTreeSet::new();
            while let Some(g) = todo.pop() {
                if !seen.insert(g) {
                    continue;
                }
                for &f in &self.groups[g].files {
                    on[f] = true;
                    for u in &self.files[f].uses {
                        for &d in definer.get(u.as_str()).into_iter().flatten() {
                            let dg = self.files[d].group;
                            if dg != g && !self.groups[dg].files.iter().all(|&x| checked[x]) {
                                todo.push(dg);
                            }
                        }
                    }
                }
            }
        }
        Ok(on)
    }

    /// How much of the plan `checked` covers.
    pub fn progress(&self, checked: &[bool]) -> Progress {
        let mut p = Progress {
            files: self.files.len(),
            groups: self.groups.len(),
            ..Progress::default()
        };
        for (i, f) in self.files.iter().enumerate() {
            p.lines += f.lines;
            p.bytes += f.bytes;
            if checked[i] {
                p.files_checked += 1;
                p.lines_checked += f.lines;
                p.bytes_checked += f.bytes;
            }
        }
        p.groups_checked = self
            .groups
            .iter()
            .filter(|g| g.files.iter().all(|&f| checked[f]))
            .count();
        p.next_group = self
            .groups
            .iter()
            .position(|g| !g.files.iter().all(|&f| checked[f]));
        p
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub files: usize,
    pub files_checked: usize,
    pub groups: usize,
    pub groups_checked: usize,
    pub lines: usize,
    pub lines_checked: usize,
    pub bytes: usize,
    pub bytes_checked: usize,
    /// Index of the first group (in plan order) with a file not checked.
    pub next_group: Option<usize>,
}

impl Progress {
    /// `3 of 10 files (30%), 1200 of 9000 lines (13%), 2 of 7 groups`.
    pub fn line(&self) -> String {
        let pct = |a: usize, b: usize| a.saturating_mul(100).checked_div(b).unwrap_or(100);
        format!(
            "{} of {} files ({}%), {} of {} lines ({}%), {} of {} groups checked",
            self.files_checked,
            self.files,
            pct(self.files_checked, self.files),
            self.lines_checked,
            self.lines,
            pct(self.lines_checked, self.lines),
            self.groups_checked,
            self.groups,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inp(rel: &str, text: &str) -> Input {
        Input {
            rel: rel.into(),
            text: text.into(),
        }
    }

    fn tree() -> Vec<Input> {
        vec![
            inp("z_load.sql", "INSERT INTO dbo.Orders (Id) VALUES (1);\n"),
            inp(
                "proc/p.sql",
                "CREATE PROCEDURE dbo.P AS BEGIN SELECT * FROM [dbo].[V]; END",
            ),
            inp("v.sql", "CREATE VIEW dbo.V AS SELECT Id FROM Orders"),
            inp("t.sql", "CREATE TABLE dbo.Orders (Id int);"),
        ]
    }

    #[test]
    fn dependencies_come_first() {
        let p = Plan::build(&tree(), Dialect::Tsql);
        let order: Vec<&str> = p
            .groups
            .iter()
            .map(|g| p.files[g.files[0]].path.as_str())
            .collect();
        assert_eq!(order, ["t.sql", "v.sql", "proc/p.sql", "z_load.sql"]);
        assert!(p.groups.iter().all(|g| !g.cyclic));
    }

    #[test]
    fn records_definitions_and_uses() {
        let p = Plan::build(&tree(), Dialect::Tsql);
        let v = p.files.iter().find(|f| f.path == "v.sql").unwrap();
        assert_eq!(v.defines[0].kind, Kind::View);
        assert_eq!(v.defines[0].name, "dbo.v");
        assert_eq!(v.uses, ["dbo.orders"]);
        let pr = p.files.iter().find(|f| f.path == "proc/p.sql").unwrap();
        assert_eq!(pr.uses, ["dbo.v"], "bracketed names are read");
    }

    #[test]
    fn a_cycle_is_one_group() {
        let t = vec![
            inp("a.sql", "CREATE PROCEDURE dbo.A AS EXEC dbo.B"),
            inp("b.sql", "CREATE PROCEDURE dbo.B AS EXEC dbo.A"),
            inp("c.sql", "CREATE PROCEDURE dbo.C AS EXEC dbo.A"),
        ];
        let p = Plan::build(&t, Dialect::Tsql);
        assert_eq!(p.groups.len(), 2);
        assert!(p.groups[0].cyclic);
        assert_eq!(p.groups[0].files.len(), 2);
        assert_eq!(p.files[p.groups[1].files[0]].path, "c.sql");
    }

    #[test]
    fn comments_and_strings_use_nothing() {
        let t = vec![
            inp("t.sql", "CREATE TABLE dbo.Orders (Id int);"),
            inp(
                "p.sql",
                "CREATE PROCEDURE dbo.P AS SELECT 'dbo.Orders' -- dbo.Orders\n/* dbo.Orders */",
            ),
        ];
        let p = Plan::build(&t, Dialect::Tsql);
        assert!(p.files.iter().all(|f| f.uses.is_empty()));
    }

    #[test]
    fn other_schemas_are_not_a_bare_name() {
        let t = vec![
            inp("t.sql", "CREATE TABLE sales.Orders (Id int);"),
            inp("p.sql", "CREATE PROCEDURE dbo.P AS SELECT Orders FROM x"),
        ];
        let p = Plan::build(&t, Dialect::Tsql);
        assert!(p.files.iter().all(|f| f.uses.is_empty()));
    }

    #[test]
    fn types_are_defined() {
        let t = vec![
            inp("ty.sql", "CREATE TYPE dbo.Ids AS TABLE (Id int);"),
            inp(
                "p.sql",
                "CREATE PROCEDURE dbo.P @x dbo.Ids READONLY AS SELECT 1",
            ),
        ];
        let p = Plan::build(&t, Dialect::Tsql);
        let by = |n: &str| p.files.iter().find(|f| f.path == n).unwrap();
        assert_eq!(by("ty.sql").defines[0].kind, Kind::Type);
        assert_eq!(by("p.sql").uses, ["dbo.ids"]);
    }

    #[test]
    fn the_plan_is_deterministic_and_tied_to_the_tree() {
        let a = Plan::build(&tree(), Dialect::Tsql);
        let mut rev = tree();
        rev.reverse();
        let b = Plan::build(&rev, Dialect::Tsql);
        assert_eq!(a.to_json(), b.to_json());
        let mut changed = tree();
        changed[0].text.push(' ');
        assert!(a.matches(&tree(), Dialect::Tsql));
        assert!(!a.matches(&changed, Dialect::Tsql));
    }

    #[test]
    fn selects_increments() {
        let p = Plan::build(&tree(), Dialect::Tsql);
        let none = vec![false; 4];
        let names = |m: &[bool]| -> Vec<&str> {
            p.groups
                .iter()
                .flat_map(|g| g.files.iter())
                .filter(|&&f| m[f])
                .map(|&f| p.files[f].path.as_str())
                .collect()
        };
        let all = p.select(&Select::default(), &none).unwrap();
        assert!(all.iter().all(|&b| b));
        let next = |n| Select {
            next: Some(n),
            ..Select::default()
        };
        assert_eq!(
            names(&p.select(&next(2), &none).unwrap()),
            ["t.sql", "v.sql"]
        );
        // The first group is checked already: the next 2 start after it.
        let mut done = none.clone();
        let t = p.files.iter().position(|f| f.path == "t.sql").unwrap();
        done[t] = true;
        assert_eq!(
            names(&p.select(&next(2), &done).unwrap()),
            ["v.sql", "proc/p.sql"]
        );
        let obj = Select {
            objects: vec!["P".into()],
            ..Select::default()
        };
        assert_eq!(
            names(&p.select(&obj, &none).unwrap()),
            ["t.sql", "v.sql", "proc/p.sql"]
        );
        // Used objects already checked are left out.
        assert_eq!(
            names(&p.select(&obj, &done).unwrap()),
            ["v.sql", "proc/p.sql"]
        );
        let schema = Select {
            schemas: vec!["DBO".into()],
            ..Select::default()
        };
        assert_eq!(names(&p.select(&schema, &none).unwrap()).len(), 3);
        let path = Select {
            paths: vec!["proc/**".into()],
            ..Select::default()
        };
        assert_eq!(names(&p.select(&path, &none).unwrap()), ["proc/p.sql"]);
        let changed = Select {
            changed: true,
            ..Select::default()
        };
        assert_eq!(names(&p.select(&changed, &done).unwrap()).len(), 3);
        assert!(p
            .select(
                &Select {
                    objects: vec!["nope".into()],
                    ..Select::default()
                },
                &none
            )
            .is_err());
    }

    #[test]
    fn globs() {
        assert!(glob("*.sql", "a.sql"));
        assert!(!glob("*.sql", "d/a.sql"));
        assert!(glob("**/*.sql", "d/e/a.sql"));
        assert!(glob("**/*.sql", "a.sql"));
        assert!(glob("d/?.sql", "d/a.sql"));
        assert!(!glob("d/?.sql", "d/ab.sql"));
    }

    #[test]
    fn progress_counts_lines_and_groups() {
        let p = Plan::build(&tree(), Dialect::Tsql);
        let mut done = vec![false; 4];
        let t = p.files.iter().position(|f| f.path == "t.sql").unwrap();
        done[t] = true;
        let pr = p.progress(&done);
        assert_eq!((pr.files, pr.files_checked), (4, 1));
        assert_eq!((pr.groups, pr.groups_checked), (4, 1));
        assert_eq!(pr.next_group, Some(1));
        assert!(pr.line().starts_with("1 of 4 files (25%)"));
    }
}
