//! Owned token slices over the CST, with the small helpers the rules need.

use sqldiff_syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};
use SyntaxKind::*;

#[derive(Debug, Clone)]
pub(crate) struct T {
    pub k: SyntaxKind,
    pub s: String,
    /// Byte range in the input text. A token a rule made up has an empty
    /// range at the place it stands for.
    pub pos: (usize, usize),
    /// The rule that made this token up, if one did: the expression walk
    /// records it as a step where it writes the token.
    pub made: Option<&'static str>,
}

impl T {
    /// A token rule `rule` makes up, standing at input offset `at`.
    pub fn made(k: SyntaxKind, s: impl Into<String>, at: usize, rule: &'static str) -> T {
        T {
            k,
            s: s.into(),
            pos: (at, at),
            made: Some(rule),
        }
    }

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
        .map(owned)
        .collect()
}

/// One CST token as the owned `T` that [`tokens`] makes of it.
fn owned(t: SyntaxToken) -> T {
    let r = t.text_range();
    T {
        k: t.kind(),
        s: t.text().to_string(),
        pos: (r.start().into(), r.end().into()),
        made: None,
    }
}

/// Whether `one` holds for a significant token of `n`, or `two` for two
/// significant tokens that follow each other (trivia between them does not
/// separate them). It answers what `any` and `windows(2)` over
/// `tokens(n)` filtered to [`T::sig`] answer, but owns at most two tokens at
/// a time instead of a copy of the whole subtree.
pub(crate) fn any_sig(
    n: &SyntaxNode,
    one: impl Fn(&T) -> bool,
    two: impl Fn(&T, &T) -> bool,
) -> bool {
    let mut prev: Option<T> = None;
    for t in n
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .map(owned)
        .filter(T::sig)
    {
        if one(&t) || prev.as_ref().is_some_and(|p| two(p, &t)) {
            return true;
        }
        prev = Some(t);
    }
    false
}

/// How many tokens `tokens(n)` returns, trivia included, without building
/// the list: it walks the same elements and keeps none of them.
pub(crate) fn count_tokens(n: &SyntaxNode) -> usize {
    n.descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .count()
}

/// The upper-cased text of the first `k` significant tokens of `n` (fewer
/// when `n` has fewer), the same words `sig(&tokens(n))` yields in order, and
/// without reading the tokens after them.
pub(crate) fn lead_words(n: &SyntaxNode, k: usize) -> Vec<String> {
    n.descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(|t| !t.kind().is_trivia() && t.kind() != INFERRED_TERMINATOR)
        .take(k)
        .map(|t| t.text().to_ascii_uppercase())
        .collect()
}

/// Whether the first significant token of `n` is the unquoted word `w`,
/// without reading the tokens after it (`tokens(n)` copies the whole
/// subtree, which for a loop nested in a loop nested in a loop … is the
/// whole rest of the input at every level).
pub(crate) fn starts_with_word(n: &SyntaxNode, w: &str) -> bool {
    n.descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .find(|t| !t.kind().is_trivia() && t.kind() != INFERRED_TERMINATOR)
        .is_some_and(|t| t.kind() == IDENT && t.text().eq_ignore_ascii_case(w))
}

/// The input bytes a token slice covers, from its first to its last
/// significant token (made-up tokens count by the place they stand for).
/// `None` for a slice with no significant token.
pub(crate) fn span(ts: &[T]) -> Option<(usize, usize)> {
    let mut sig = ts.iter().filter(|t| t.sig());
    let first = sig.next()?;
    let (mut s, mut e) = first.pos;
    for t in sig {
        s = s.min(t.pos.0);
        e = e.max(t.pos.1);
    }
    Some((s, e))
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

#[cfg(test)]
mod tests {
    use super::starts_with_word;

    #[test]
    fn first_significant_word_ignores_trivia_and_case() {
        let root = sqldiff_syntax::parse(
            "  /* c */ while 1=1 select 1;",
            sqldiff_syntax::Dialect::Tsql,
        )
        .syntax();
        assert!(starts_with_word(&root, "WHILE"));
        assert!(!starts_with_word(&root, "SELECT"));
    }
}
