//! Files that hold several objects (schema dumps, migration scripts) split
//! into one object per CREATE, so they pair object by object.
//!
//! Each object keeps its CREATE plus the statements that name it (indexes,
//! ALTER TABLE, COMMENT ON, GRANT). Other statements (SET, DROP, GO-batch
//! settings) go with the next object, or with the last one at the end of the
//! file. An object's text is the whole file with everything else blanked out,
//! newlines kept, so line numbers and offsets stay those of the file.

use sqldiff_core::Dialect;
use sqldiff_syntax::{ast as cst, SyntaxKind, SyntaxNode};

/// One object of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// Id fragment: the folded name, `~2`, `~3` for repeats.
    pub fragment: String,
    /// The object name as written, without schema and quotes.
    pub name: String,
    /// The name with its schema as written (quotes removed).
    pub qualified: String,
    pub kind: String,
    /// Byte ranges of the statements that make up the object.
    pub ranges: Vec<(usize, usize)>,
}

impl Part {
    /// The object in the form of [`crate::defined_name`], for tables, views
    /// and routines.
    pub fn defines(&self) -> Option<String> {
        let ns = match self.kind.as_str() {
            "table" | "view" => "relation",
            "procedure" | "function" => "routine",
            _ => return None,
        };
        let mut parts = self.qualified.rsplit('.');
        let name = parts.next()?;
        let schema = parts.next().unwrap_or("");
        Some(format!(
            "{ns}:{}.{}",
            schema.to_lowercase(),
            name.to_lowercase()
        ))
    }
}

/// What a top-level statement does to which object.
enum Role {
    Creates { kind: String, parts: Vec<String> },
    Names(String),
    Other,
}

const MODIFIERS: &[&str] = &[
    "OR",
    "REPLACE",
    "ALTER",
    "UNIQUE",
    "CLUSTERED",
    "NONCLUSTERED",
    "TEMP",
    "TEMPORARY",
    "GLOBAL",
    "LOCAL",
    "UNLOGGED",
    "MATERIALIZED",
    "RECURSIVE",
    "CONSTRAINT",
    "DEFINER",
    "COLUMNSTORE",
];

fn kind_of(word: &str) -> Option<&'static str> {
    Some(match word {
        "TABLE" => "table",
        "VIEW" => "view",
        "PROCEDURE" | "PROC" => "procedure",
        "FUNCTION" => "function",
        "TRIGGER" => "trigger",
        "SEQUENCE" => "sequence",
        "TYPE" => "type",
        "INDEX" => "index",
        _ => return None,
    })
}

/// The parts of the qualified name starting at token `i`, unquoted.
fn qname_at(toks: &[(SyntaxKind, String)], mut i: usize) -> Vec<String> {
    let mut parts = Vec::new();
    while let Some((k, t)) = toks.get(i) {
        match k {
            SyntaxKind::IDENT | SyntaxKind::QUOTED_IDENT => parts.push(cst::unquote(t)),
            _ => break,
        }
        if toks.get(i + 1).is_some_and(|(k, _)| *k == SyntaxKind::DOT) {
            i += 2;
        } else {
            break;
        }
    }
    parts
}

/// The last part of the qualified name starting at token `i`.
fn name_at(toks: &[(SyntaxKind, String)], i: usize) -> Option<String> {
    qname_at(toks, i).pop()
}

fn role(node: &SyntaxNode) -> Role {
    let toks: Vec<(SyntaxKind, String)> = cst::sig_tokens(node)
        .take(40)
        .map(|t| (t.kind(), t.text().to_string()))
        .collect();
    let up = |i: usize| {
        toks.get(i)
            .map(|t| t.1.to_ascii_uppercase())
            .unwrap_or_default()
    };
    let skip_if_exists = |mut i: usize| {
        if up(i) == "IF" && up(i + 1) == "NOT" && up(i + 2) == "EXISTS" {
            i += 3;
        } else if up(i) == "IF" && up(i + 1) == "EXISTS" {
            i += 2;
        }
        i
    };
    match up(0).as_str() {
        "CREATE" => {
            let mut i = 1;
            while MODIFIERS.contains(&up(i).as_str()) {
                i += 1;
            }
            let Some(kind) = kind_of(&up(i)) else {
                return Role::Other;
            };
            let i = skip_if_exists(i + 1);
            if kind == "index" {
                // CREATE INDEX [name] ON table: part of the table.
                return match (i..toks.len()).find(|&j| up(j) == "ON") {
                    Some(on) => {
                        let at = if up(on + 1) == "ONLY" { on + 2 } else { on + 1 };
                        name_at(&toks, at).map_or(Role::Other, Role::Names)
                    }
                    None => Role::Other,
                };
            }
            let parts = qname_at(&toks, i);
            if parts.is_empty() {
                return Role::Other;
            }
            Role::Creates {
                kind: kind.to_string(),
                parts,
            }
        }
        "ALTER" if kind_of(&up(1)).is_some() => {
            let mut i = skip_if_exists(2);
            if up(i) == "ONLY" {
                i += 1;
            }
            name_at(&toks, i).map_or(Role::Other, Role::Names)
        }
        "COMMENT" if up(1) == "ON" => {
            let column = up(2) == "COLUMN";
            let i = 3;
            // COMMENT ON COLUMN t.c: the table is the part before the last.
            let mut parts = Vec::new();
            let mut j = i;
            while let Some((k, t)) = toks.get(j) {
                if !matches!(k, SyntaxKind::IDENT | SyntaxKind::QUOTED_IDENT) {
                    break;
                }
                parts.push(cst::unquote(t));
                if toks.get(j + 1).is_some_and(|(k, _)| *k == SyntaxKind::DOT) {
                    j += 2;
                } else {
                    break;
                }
            }
            if column {
                parts.pop();
            }
            parts.pop().map_or(Role::Other, Role::Names)
        }
        "GRANT" | "REVOKE" => match (1..toks.len()).find(|&j| up(j) == "ON") {
            Some(on) => {
                let at = if kind_of(&up(on + 1)).is_some() {
                    on + 2
                } else {
                    on + 1
                };
                name_at(&toks, at).map_or(Role::Other, Role::Names)
            }
            None => Role::Other,
        },
        _ => Role::Other,
    }
}

/// Id fragment of a name: folded, without the characters ids use (`#`, `/`, `.`, `@`).
fn fragment_of(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| {
            if matches!(c, '#' | '/' | '.' | '@' | '~') {
                '_'
            } else {
                c
            }
        })
        .collect()
}

fn units(node: &SyntaxNode, out: &mut Vec<SyntaxNode>) {
    for c in cst::structural_children(node) {
        match c.kind() {
            SyntaxKind::SOURCE_FILE | SyntaxKind::BATCH => units(&c, out),
            _ => out.push(c),
        }
    }
}

/// The objects of a file; empty when it holds fewer than two.
pub fn parts(text: &str, d: Dialect) -> Vec<Part> {
    let root = sqldiff_syntax::parse(text, d).syntax();
    let mut nodes = Vec::new();
    units(&root, &mut nodes);
    let mut out: Vec<Part> = Vec::new();
    let mut pending: Vec<(usize, usize)> = Vec::new();
    for n in &nodes {
        let r = cst::sig_range(n);
        match role(n) {
            Role::Creates { kind, parts } => {
                let mut ranges = std::mem::take(&mut pending);
                ranges.push(r);
                let name = parts.last().cloned().unwrap_or_default();
                out.push(Part {
                    fragment: fragment_of(&name),
                    qualified: parts.join("."),
                    name,
                    kind,
                    ranges,
                });
            }
            Role::Names(name) => {
                let folded = fragment_of(&name);
                match out.iter_mut().rev().find(|p| p.fragment == folded) {
                    Some(p) => p.ranges.push(r),
                    None => pending.push(r),
                }
            }
            Role::Other => pending.push(r),
        }
    }
    if out.len() < 2 {
        return Vec::new();
    }
    if let Some(last) = out.last_mut() {
        last.ranges.append(&mut pending);
    }
    // Repeated names (overloads): number them in file order.
    let mut seen: std::collections::HashMap<String, usize> = Default::default();
    for p in &mut out {
        let k = seen.entry(p.fragment.clone()).or_insert(0);
        *k += 1;
        if *k > 1 {
            p.fragment = format!("{}~{k}", p.fragment);
        }
        p.ranges.sort();
    }
    out
}

/// The file text with everything outside `ranges` blanked (newlines kept).
pub fn blank_outside(text: &str, ranges: &[(usize, usize)]) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, c) in text.char_indices() {
        let keep = c == '\n' || c == '\r' || ranges.iter().any(|&(s, e)| s <= i && i < e);
        if keep {
            out.push(c);
        } else {
            for _ in 0..c.len_utf8() {
                out.push(' ');
            }
        }
    }
    out
}

/// `path#fragment` → (`path`, `fragment`).
pub fn split_id(id: &str) -> (&str, Option<&str>) {
    match id.rsplit_once('#') {
        Some((path, frag)) if !frag.contains(['/', '.']) && !path.is_empty() => (path, Some(frag)),
        _ => (id, None),
    }
}

/// The file of an id under `dir` and the object fragment; a file whose name
/// contains `#` is still read whole.
pub fn locate<'i>(dir: &std::path::Path, id: &'i str) -> (std::path::PathBuf, Option<&'i str>) {
    let whole = dir.join(id);
    if whole.is_file() {
        return (whole, None);
    }
    let (rel, frag) = split_id(id);
    (dir.join(rel), frag)
}

/// The text of one object of a file, and its qualified name.
pub fn object(text: &str, d: Dialect, fragment: &str) -> Option<(String, String)> {
    parts(text, d)
        .into_iter()
        .find(|p| p.fragment == fragment)
        .map(|p| (blank_outside(text, &p.ranges), p.qualified))
}

/// The text of one object of a file.
pub fn object_text(text: &str, d: Dialect, fragment: &str) -> Option<String> {
    object(text, d, fragment).map(|o| o.0)
}

/// The qualified name of the first object a text creates.
pub fn created(text: &str, d: Dialect) -> Option<String> {
    let root = sqldiff_syntax::parse(text, d).syntax();
    let mut nodes = Vec::new();
    units(&root, &mut nodes);
    nodes.iter().find_map(|n| match role(n) {
        Role::Creates { parts, .. } => Some(parts.join(".")),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_schema_file() {
        let text = "SET search_path = public;\nCREATE TABLE film (id int);\nCREATE INDEX ix ON film (id);\nCOMMENT ON COLUMN film.id IS 'x';\nCREATE TABLE actor (id int);\nALTER TABLE ONLY actor ADD PRIMARY KEY (id);\nCREATE VIEW film_list AS SELECT id FROM film;\n";
        let p = parts(text, Dialect::Postgres);
        let names: Vec<_> = p
            .iter()
            .map(|p| (p.fragment.as_str(), p.kind.as_str(), p.ranges.len()))
            .collect();
        assert_eq!(
            names,
            [
                ("film", "table", 4),
                ("actor", "table", 2),
                ("film_list", "view", 1)
            ]
        );
        let film = object_text(text, Dialect::Postgres, "film").unwrap();
        assert_eq!(film.len(), text.len());
        assert!(film.contains("CREATE INDEX ix ON film"));
        assert!(!film.contains("actor"));
        assert_eq!(film.lines().count(), text.lines().count());
    }

    #[test]
    fn single_objects_and_overloads() {
        assert!(parts(
            "CREATE TABLE t (a int);\nINSERT INTO t VALUES (1);",
            Dialect::Postgres
        )
        .is_empty());
        let text = "CREATE FUNCTION f(a int) RETURNS int AS $$ SELECT a $$ LANGUAGE sql;\nCREATE FUNCTION f(a text) RETURNS text AS $$ SELECT a $$ LANGUAGE sql;\n";
        let p = parts(text, Dialect::Postgres);
        assert_eq!(
            p.iter().map(|p| p.fragment.as_str()).collect::<Vec<_>>(),
            ["f", "f~2"]
        );
        let ts = "CREATE TABLE [Sales].[Orders] (id int);\nGO\nCREATE PROCEDURE Sales.GetOrder @Id int AS SELECT id FROM Sales.Orders WHERE id = @Id;\nGO\n";
        let p = parts(ts, Dialect::Tsql);
        assert_eq!(
            p.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["Orders", "GetOrder"]
        );
        assert_eq!(split_id("a/b.sql#orders"), ("a/b.sql", Some("orders")));
        assert_eq!(split_id("a/b.sql"), ("a/b.sql", None));
    }
}
