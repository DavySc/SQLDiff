//! Owned token slices over the CST, with the small helpers the rules need.

use sqldiff_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};
use SyntaxKind::*;

#[derive(Debug, Clone)]
pub(crate) struct T {
    pub k: SyntaxKind,
    pub s: String,
}

impl T {
    pub fn sig(&self) -> bool {
        !self.k.is_trivia() && self.k != INFERRED_TERMINATOR
    }

    /// An unquoted word, compared case-insensitively.
    pub fn is(&self, w: &str) -> bool {
        self.k == IDENT && self.s.eq_ignore_ascii_case(w)
    }

    pub fn up(&self) -> String {
        self.s.to_ascii_uppercase()
    }
}

/// All tokens of a node, trivia included, in source order.
pub(crate) fn tokens(n: &SyntaxNode) -> Vec<T> {
    n.descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .map(|t| T {
            k: t.kind(),
            s: t.text().to_string(),
        })
        .collect()
}

/// Indices of significant tokens.
pub(crate) fn sig(ts: &[T]) -> Vec<usize> {
    (0..ts.len()).filter(|&i| ts[i].sig()).collect()
}

pub(crate) fn next_sig(ts: &[T], i: usize) -> Option<usize> {
    (i + 1..ts.len()).find(|&j| ts[j].sig())
}

pub(crate) fn prev_sig(ts: &[T], i: usize) -> Option<usize> {
    (0..i).rev().find(|&j| ts[j].sig())
}

/// Index of the `)` matching the `(` at `open`.
pub(crate) fn matching(ts: &[T], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (j, t) in ts.iter().enumerate().skip(open) {
        match t.k {
            L_PAREN => depth += 1,
            R_PAREN => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
    }
    None
}

/// Paren depth before each token.
pub(crate) fn depths(ts: &[T]) -> Vec<i32> {
    let mut d = 0;
    ts.iter()
        .map(|t| {
            let here = d;
            match t.k {
                L_PAREN => d += 1,
                R_PAREN => d -= 1,
                _ => {}
            }
            if t.k == R_PAREN {
                d
            } else {
                here
            }
        })
        .collect()
}

/// Splits at top-level commas. An all-trivia input gives no parts.
pub(crate) fn split_commas(ts: &[T]) -> Vec<&[T]> {
    if !ts.iter().any(T::sig) {
        return Vec::new();
    }
    let ds = depths(ts);
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, t) in ts.iter().enumerate() {
        if t.k == COMMA && ds[i] == 0 {
            parts.push(&ts[start..i]);
            start = i + 1;
        }
    }
    parts.push(&ts[start..]);
    parts
}

/// Top-level index of the first significant word `w` at or after `from`.
pub(crate) fn find_word(ts: &[T], from: usize, w: &str) -> Option<usize> {
    let ds = depths(ts);
    (from..ts.len()).find(|&i| ds[i] == 0 && ts[i].is(w))
}

/// Concatenated text with leading and trailing trivia removed.
pub(crate) fn trimmed(ts: &[T]) -> &[T] {
    let s = ts.iter().position(T::sig).unwrap_or(ts.len());
    let e = ts.iter().rposition(T::sig).map_or(s, |i| i + 1);
    &ts[s..e.max(s)]
}

/// `ItemId` → `item_id`.
pub(crate) fn snake(name: &str) -> String {
    let mut out = String::new();
    let cs: Vec<char> = name.chars().collect();
    for (i, &c) in cs.iter().enumerate() {
        if c.is_uppercase() && i > 0 {
            let prev = cs[i - 1];
            let next_lower = cs.get(i + 1).is_some_and(|n| n.is_lowercase());
            if prev.is_lowercase() || prev.is_ascii_digit() || (prev.is_uppercase() && next_lower) {
                out.push('_');
            }
        }
        out.extend(c.to_lowercase());
    }
    out
}
