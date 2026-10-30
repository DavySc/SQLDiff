//! Typed helpers over the untyped CST.

use crate::kind::SyntaxKind::{self, *};
use crate::kind::{SyntaxElement, SyntaxNode, SyntaxToken};

/// Significant (non-trivia, non-synthetic) tokens in a subtree.
pub fn sig_tokens(node: &SyntaxNode) -> impl Iterator<Item = SyntaxToken> {
    node.descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(|t| is_significant(t.kind()))
}

pub fn is_significant(kind: SyntaxKind) -> bool {
    !kind.is_trivia() && kind != INFERRED_TERMINATOR
}

/// The first `n` significant tokens, upper-cased.
pub fn first_words(node: &SyntaxNode, n: usize) -> Vec<String> {
    sig_tokens(node)
        .take(n)
        .map(|t| t.text().to_ascii_uppercase())
        .collect()
}

/// Whether the first significant token is the word `kw`.
pub fn starts_with_kw(node: &SyntaxNode, kw: &str) -> bool {
    sig_tokens(node)
        .next()
        .is_some_and(|t| t.kind() == IDENT && t.text().eq_ignore_ascii_case(kw))
}

/// Child nodes that are statements or statement lists.
pub fn structural_children(node: &SyntaxNode) -> Vec<SyntaxNode> {
    node.children()
        .filter(|c| c.kind().is_unit() || c.kind().is_container())
        .collect()
}

/// Significant tokens of a node excluding its statement children: the
/// node's own keywords plus conditions and labels.
pub fn header_tokens(node: &SyntaxNode) -> Vec<SyntaxToken> {
    let mut out = Vec::new();
    collect_header(node, &mut out);
    out
}

fn collect_header(node: &SyntaxNode, out: &mut Vec<SyntaxToken>) {
    for el in node.children_with_tokens() {
        match el {
            rowan::NodeOrToken::Token(t) => {
                if is_significant(t.kind()) {
                    out.push(t);
                }
            }
            rowan::NodeOrToken::Node(n) => {
                if !(n.kind().is_unit() || n.kind().is_container()) {
                    collect_header(&n, out);
                }
            }
        }
    }
}

/// Whether the statement ended without an explicit `;` (T-SQL).
pub fn ends_with_inferred_terminator(node: &SyntaxNode) -> bool {
    node.last_token()
        .is_some_and(|t| t.kind() == INFERRED_TERMINATOR)
        || node
            .children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .any(|t| t.kind() == INFERRED_TERMINATOR)
}

/// Text range of the significant part of a node (without leading/trailing trivia).
pub fn sig_range(node: &SyntaxNode) -> (usize, usize) {
    let mut toks = sig_tokens(node);
    let Some(first) = toks.next() else {
        let r = node.text_range();
        return (r.start().into(), r.end().into());
    };
    let last = toks.last().unwrap_or_else(|| first.clone());
    (
        first.text_range().start().into(),
        last.text_range().end().into(),
    )
}

/// Name of a routine (`CREATE PROCEDURE a.b ...`), parts unquoted and joined with `.`.
pub fn routine_name(node: &SyntaxNode) -> Option<String> {
    let parts = routine_name_parts(node);
    (!parts.is_empty()).then(|| parts.join("."))
}

/// The routine's name parts as its header writes them, unquoted
/// (`[dbo].[p]` → `dbo`, `p`); empty when there is none.
pub fn routine_name_parts(node: &SyntaxNode) -> Vec<String> {
    let toks: Vec<SyntaxToken> = sig_tokens(node).collect();
    let Some(i) = toks.iter().position(|t| {
        t.kind() == IDENT
            && ["PROCEDURE", "PROC", "FUNCTION", "TRIGGER", "VIEW"]
                .iter()
                .any(|k| t.text().eq_ignore_ascii_case(k))
    }) else {
        return Vec::new();
    };
    qualified_name_at(&toks, i + 1)
}

/// Reads a qualified name (`a.b.c`, `lib/file`) starting at `toks[i]`.
pub fn qualified_name_at(toks: &[SyntaxToken], mut i: usize) -> Vec<String> {
    let mut parts = Vec::new();
    while i < toks.len() {
        let t = &toks[i];
        match t.kind() {
            IDENT | VARIABLE => parts.push(t.text().to_string()),
            QUOTED_IDENT => parts.push(unquote(t.text())),
            _ => break,
        }
        i += 1;
        match toks.get(i) {
            Some(n) if n.kind() == DOT || (n.kind() == OP && n.text() == "/") => i += 1,
            _ => break,
        }
    }
    parts
}

/// Removes identifier quoting: `"a""b"` → `a"b`, `[a]]b]` → `a]b`.
pub fn unquote(text: &str) -> String {
    let text = text.strip_prefix("U&").unwrap_or(text);
    if let Some(inner) = text.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
        inner.replace("]]", "]")
    } else if let Some(inner) = text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
        inner.replace("\"\"", "\"")
    } else {
        text.to_string()
    }
}

/// Indented dump of the tree, for debugging and `sqldiff parse --tree`.
pub fn dump(node: &SyntaxNode) -> String {
    let mut out = String::new();
    dump_into(node, 0, &mut out);
    out
}

fn dump_into(node: &SyntaxNode, depth: usize, out: &mut String) {
    use std::fmt::Write;
    let _ = writeln!(
        out,
        "{:indent$}{:?}@{:?}",
        "",
        node.kind(),
        node.text_range(),
        indent = depth * 2
    );
    for el in node.children_with_tokens() {
        match el {
            rowan::NodeOrToken::Node(n) => dump_into(&n, depth + 1, out),
            rowan::NodeOrToken::Token(t) => {
                if !t.kind().is_trivia() {
                    let _ = writeln!(
                        out,
                        "{:indent$}{:?} {:?}",
                        "",
                        t.kind(),
                        t.text(),
                        indent = (depth + 1) * 2
                    );
                }
            }
        }
    }
}
