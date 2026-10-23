//! Flat significant-token views over CST nodes, used where the parser keeps
//! expressions flat (catalog DDL reading and binding).

use sqldiff_syntax::{ast, SyntaxKind, SyntaxNode, SyntaxToken};

use crate::catalog::Ident;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tok {
    pub kind: SyntaxKind,
    pub text: String,
    pub start: usize,
    pub end: usize,
}

impl Tok {
    pub fn from_token(t: &SyntaxToken) -> Self {
        Tok {
            kind: t.kind(),
            text: t.text().to_string(),
            start: t.text_range().start().into(),
            end: t.text_range().end().into(),
        }
    }

    pub fn is_kw(&self, kw: &str) -> bool {
        self.kind == SyntaxKind::IDENT && self.text.eq_ignore_ascii_case(kw)
    }

    pub fn is_any_kw(&self, kws: &[&str]) -> bool {
        self.kind == SyntaxKind::IDENT && kws.iter().any(|k| self.text.eq_ignore_ascii_case(k))
    }

    pub fn upper(&self) -> String {
        self.text.to_ascii_uppercase()
    }

    pub fn is(&self, kind: SyntaxKind) -> bool {
        self.kind == kind
    }

    pub fn is_name(&self) -> bool {
        matches!(self.kind, SyntaxKind::IDENT | SyntaxKind::QUOTED_IDENT)
    }

    pub fn is_op(&self, op: &str) -> bool {
        self.kind == SyntaxKind::OP && self.text == op
    }

    pub fn ident(&self) -> Ident {
        match self.kind {
            SyntaxKind::QUOTED_IDENT => Ident::new(ast::unquote(&self.text), true),
            _ => Ident::plain(self.text.clone()),
        }
    }
}

/// Significant tokens of a whole subtree.
pub fn of_node(node: &SyntaxNode) -> Vec<Tok> {
    ast::sig_tokens(node).map(|t| Tok::from_token(&t)).collect()
}

/// Significant tokens of a node without its statement children.
pub fn header(node: &SyntaxNode) -> Vec<Tok> {
    ast::header_tokens(node)
        .iter()
        .map(Tok::from_token)
        .collect()
}

/// Index of the parenthesis closing the one at `open`, or the last index.
pub fn close_paren(t: &[Tok], open: usize) -> usize {
    let mut depth = 0usize;
    for (i, x) in t.iter().enumerate().skip(open) {
        match x.kind {
            SyntaxKind::L_PAREN => depth += 1,
            SyntaxKind::R_PAREN => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
    }
    // Unclosed (incomplete input): one past the end, so `t[open + 1..close]`
    // is the rest and `close + 1` is past every token.
    t.len()
}

/// Splits `t` at top-level commas.
pub fn split_commas(t: &[Tok]) -> Vec<&[Tok]> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, x) in t.iter().enumerate() {
        match x.kind {
            SyntaxKind::L_PAREN | SyntaxKind::L_BRACKET => depth += 1,
            SyntaxKind::R_PAREN | SyntaxKind::R_BRACKET => depth = depth.saturating_sub(1),
            SyntaxKind::COMMA if depth == 0 => {
                out.push(&t[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < t.len() {
        out.push(&t[start..]);
    }
    out
}

/// Reads a qualified name (`a.b.c`, Db2 `lib/file`) at `i`; returns the parts
/// and the index after the name.
pub fn qualified(t: &[Tok], mut i: usize) -> (Vec<Ident>, usize) {
    let mut parts = Vec::new();
    while i < t.len() && t[i].is_name() {
        parts.push(t[i].ident());
        i += 1;
        let sep = t
            .get(i)
            .is_some_and(|n| n.kind == SyntaxKind::DOT || n.is_op("/"));
        if sep && t.get(i + 1).is_some_and(Tok::is_name) {
            i += 1;
        } else {
            break;
        }
    }
    (parts, i)
}

/// Position of the first top-level keyword `kw` at or after `from`.
pub fn find_kw(t: &[Tok], from: usize, kw: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, x) in t.iter().enumerate().skip(from) {
        match x.kind {
            SyntaxKind::L_PAREN => depth += 1,
            SyntaxKind::R_PAREN => depth = depth.saturating_sub(1),
            _ if depth == 0 && x.is_kw(kw) => return Some(i),
            _ => {}
        }
    }
    None
}

pub fn texts(t: &[Tok]) -> Vec<&str> {
    t.iter().map(|x| x.text.as_str()).collect()
}
