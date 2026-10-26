//! Facts read straight from the syntax tree: durable writes (for write
//! pairing), RETURN codes and the Db2 for i commit level. Routine kind,
//! handlers and transaction behaviour come from the semantic IR
//! (`crate::ir`).

use sqldiff_syntax::{ast, SyntaxKind, SyntaxNode};

use crate::artifact::Analyzed;
use crate::classify::{is_temp_target, nested_write, stmt_kind, write_target, StmtKind};

#[derive(Debug, Clone)]
pub struct Write {
    pub node: SyntaxNode,
    pub kind: StmtKind,
    pub target: String,
    /// MERGE, or INSERT ... ON CONFLICT.
    pub upsert: bool,
}

/// Writes to tables that hold durable state (temporary tables excluded), in
/// document order.
pub fn writes(a: &Analyzed) -> Vec<Write> {
    let d = a.dialect();
    let mut out = Vec::new();
    for node in a.root.descendants() {
        if node.kind() != SyntaxKind::STATEMENT {
            continue;
        }
        let kind = stmt_kind(&node, d);
        if d == sqldiff_syntax::Dialect::Tsql {
            if let Some(target) = history_table(&node) {
                out.push(Write {
                    node,
                    kind: StmtKind::Insert,
                    target,
                    upsert: false,
                });
                continue;
            }
        }
        let direct = kind.is_write();
        let (kind, parts) = if direct {
            match write_target(&node) {
                Some(parts) => (kind, parts),
                None => continue,
            }
        } else if kind == StmtKind::Select {
            match nested_write(&node) {
                Some(w) => w,
                None => continue,
            }
        } else {
            continue;
        };
        if is_temp_target(&parts) {
            continue;
        }
        out.push(Write {
            upsert: kind == StmtKind::Merge || (direct && has_on_conflict(&node)),
            node,
            kind,
            target: parts.last().cloned().unwrap_or_default(),
        });
    }
    out
}

/// T-SQL `CREATE TABLE … WITH (SYSTEM_VERSIONING = ON (HISTORY_TABLE = h))`:
/// every UPDATE and DELETE of the table writes the old row into `h`. That
/// write is declared here, where the converted table's history trigger
/// writes it, so the two pair up.
fn history_table(node: &SyntaxNode) -> Option<String> {
    let toks: Vec<_> = ast::sig_tokens(node).collect();
    let up = |i: usize| toks.get(i).map(|t| t.text().to_ascii_uppercase());
    if up(0).as_deref() != Some("CREATE") || up(1).as_deref() != Some("TABLE") {
        return None;
    }
    let at = (0..toks.len()).find(|&i| up(i).as_deref() == Some("HISTORY_TABLE"))?;
    if toks.get(at + 1).map(|t| t.text()) != Some("=") {
        return None;
    }
    ast::qualified_name_at(&toks, at + 2).pop()
}

/// `BEGIN ATOMIC` (Db2 compound, SQL-standard function body).
pub fn is_atomic_block(node: &SyntaxNode) -> bool {
    if node.kind() != SyntaxKind::BLOCK {
        return false;
    }
    let toks: Vec<String> = ast::header_tokens(node)
        .iter()
        .take(4)
        .map(|t| t.text().to_ascii_uppercase())
        .collect();
    toks.iter()
        .position(|t| t == "BEGIN")
        .is_some_and(|i| toks.get(i + 1).is_some_and(|t| t == "ATOMIC"))
}

fn has_on_conflict(node: &SyntaxNode) -> bool {
    let words: Vec<String> = ast::sig_tokens(node)
        .map(|t| t.text().to_ascii_uppercase())
        .collect();
    words.windows(2).any(|w| w[0] == "ON" && w[1] == "CONFLICT")
}

pub fn return_code(node: &SyntaxNode) -> Option<String> {
    let toks: Vec<_> = ast::sig_tokens(node)
        .skip(1)
        .filter(|t| t.kind() != SyntaxKind::SEMICOLON)
        .collect();
    match toks.as_slice() {
        [n] if n.kind() == SyntaxKind::NUMBER => Some(n.text().to_string()),
        [m, n] if m.text() == "-" && n.kind() == SyntaxKind::NUMBER => {
            Some(format!("-{}", n.text()))
        }
        _ => None,
    }
}

/// `COMMIT = *NONE` in a Db2 for i `SET OPTION` clause or statement.
pub fn find_commit_level(a: &Analyzed) -> Option<(String, usize)> {
    let toks: Vec<_> = ast::sig_tokens(&a.root).collect();
    for w in toks.windows(4) {
        if w[0].text().eq_ignore_ascii_case("COMMIT")
            && w[1].text() == "="
            && w[2].text() == "*"
            && w[3].kind() == SyntaxKind::IDENT
        {
            let level = format!("*{}", w[3].text().to_ascii_uppercase());
            return Some((level, w[0].text_range().start().into()));
        }
    }
    None
}
