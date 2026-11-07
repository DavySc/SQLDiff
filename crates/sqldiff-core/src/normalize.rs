//! Token normalization for comparison (whitespace, comments, case, quoting).

use sha2::{Digest, Sha256};
use sqldiff_syntax::{ast, Dialect, IdentFolding, SyntaxKind, SyntaxNode, SyntaxToken};

use crate::model::hex;

/// A significant token in comparable form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormTok {
    /// Normalized under dialect rules plus the default assumptions
    /// (T-SQL identifiers compared case-insensitively).
    pub loose: String,
    /// Normalized under dialect rules only.
    pub strict: String,
    pub raw: String,
}

/// Words that are keywords in at least one dialect; their case never matters.
const KEYWORDS: &[&str] = &[
    "ADD",
    "ALL",
    "ALTER",
    "AND",
    "ANY",
    "AS",
    "ASC",
    "ATOMIC",
    "BEGIN",
    "BETWEEN",
    "BREAK",
    "BY",
    "CALL",
    "CASCADE",
    "CASE",
    "CAST",
    "CATCH",
    "CHECK",
    "CLOSE",
    "COALESCE",
    "COLUMN",
    "COMMIT",
    "CONSTRAINT",
    "CONTINUE",
    "CREATE",
    "CROSS",
    "CURRENT",
    "CURSOR",
    "DECLARE",
    "DEFAULT",
    "DELETE",
    "DESC",
    "DISTINCT",
    "DO",
    "DROP",
    "ELSE",
    "ELSEIF",
    "ELSIF",
    "END",
    "ESCAPE",
    "EXCEPT",
    "EXCEPTION",
    "EXEC",
    "EXECUTE",
    "EXISTS",
    "EXIT",
    "FETCH",
    "FOR",
    "FOREIGN",
    "FROM",
    "FULL",
    "FUNCTION",
    "GOTO",
    "GRANT",
    "GROUP",
    "HANDLER",
    "HAVING",
    "IF",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INTERSECT",
    "INTO",
    "IS",
    "ITERATE",
    "JOIN",
    "KEY",
    "LANGUAGE",
    "LEAVE",
    "LEFT",
    "LIKE",
    "LIMIT",
    "LOOP",
    "MATCHED",
    "MERGE",
    "NOCOUNT",
    "NOT",
    "NULL",
    "OF",
    "OFF",
    "OFFSET",
    "ON",
    "OPEN",
    "OPTION",
    "OR",
    "ORDER",
    "OUT",
    "OUTER",
    "OUTPUT",
    "PERFORM",
    "PRIMARY",
    "PRINT",
    "PROC",
    "PROCEDURE",
    "RAISE",
    "RAISERROR",
    "REFERENCES",
    "REPEAT",
    "REPLACE",
    "RESIGNAL",
    "RETURN",
    "RETURNING",
    "RETURNS",
    "RIGHT",
    "ROLLBACK",
    "ROW",
    "ROWS",
    "SAVE",
    "SAVEPOINT",
    "SELECT",
    "SET",
    "SIGNAL",
    "SQLEXCEPTION",
    "SQLSTATE",
    "SQLWARNING",
    "TABLE",
    "THEN",
    "THROW",
    "TOP",
    "TRAN",
    "TRANSACTION",
    "TRIGGER",
    "TRUNCATE",
    "TRY",
    "UNDO",
    "UNION",
    "UNIQUE",
    "UNTIL",
    "UPDATE",
    "USING",
    "VALUES",
    "VIEW",
    "WHEN",
    "WHERE",
    "WHILE",
    "WITH",
    "XACT_ABORT",
    "INT",
    "INTEGER",
    "VARCHAR",
    "NVARCHAR",
    "CHAR",
    "DECIMAL",
    "NUMERIC",
    "BIGINT",
    "SMALLINT",
    "DATE",
    "TIME",
    "TIMESTAMP",
    "TEXT",
    "BIT",
    "BOOLEAN",
    "IN",
    "INOUT",
];

/// The keywords [`is_keyword`] knows, upper case.
pub fn keywords() -> &'static [&'static str] {
    KEYWORDS
}

pub fn is_keyword(upper: &str) -> bool {
    KEYWORDS.contains(&upper)
}

pub fn norm_token(tok: &SyntaxToken, dialect: Dialect) -> NormTok {
    let raw = tok.text().to_string();
    let folding = dialect.lexical().folding;
    let fold = |s: &str, quoted: bool| -> (String, String) {
        match (folding, quoted) {
            (IdentFolding::Lower, false) => (s.to_lowercase(), s.to_lowercase()),
            (IdentFolding::Upper, false) => (s.to_uppercase(), s.to_uppercase()),
            (IdentFolding::Lower | IdentFolding::Upper, true) => (s.to_string(), s.to_string()),
            (IdentFolding::Collation, _) => (s.to_uppercase(), s.to_string()),
        }
    };
    let (loose, strict) = match tok.kind() {
        SyntaxKind::IDENT => {
            let upper = raw.to_uppercase();
            if is_keyword(&upper) {
                (upper.clone(), upper)
            } else {
                fold(&raw, false)
            }
        }
        SyntaxKind::QUOTED_IDENT => fold(&ast::unquote(&raw), true),
        SyntaxKind::VARIABLE => fold(&raw, false),
        SyntaxKind::STRING => {
            let q = raw.find('\'').unwrap_or(0);
            let s = format!("{}{}", raw[..q].to_uppercase(), &raw[q..]);
            (s.clone(), s)
        }
        SyntaxKind::NUMBER => (raw.to_lowercase(), raw.to_lowercase()),
        _ => (raw.clone(), raw.clone()),
    };
    NormTok { loose, strict, raw }
}

/// Comparable tokens of a subtree; terminators are dropped (structure is in the tree).
pub fn norm_tokens(node: &SyntaxNode, dialect: Dialect) -> Vec<NormTok> {
    norm_of(ast::sig_tokens(node), dialect)
}

pub fn norm_of(tokens: impl Iterator<Item = SyntaxToken>, dialect: Dialect) -> Vec<NormTok> {
    tokens
        .filter(|t| t.kind() != SyntaxKind::SEMICOLON)
        .map(|t| norm_token(&t, dialect))
        .collect()
}

pub fn hash_strs<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0x1f]);
    }
    hex(&h.finalize()[..8])
}

pub fn loose_hash(toks: &[NormTok]) -> String {
    hash_strs(toks.iter().map(|t| t.loose.as_str()))
}

pub fn strict_hash(toks: &[NormTok]) -> String {
    hash_strs(toks.iter().map(|t| t.strict.as_str()))
}

/// Length of the longest common subsequence of two sequences.
pub fn lcs_len<T: PartialEq>(a: &[T], b: &[T]) -> usize {
    let mut prev = vec![0usize; b.len() + 1];
    let mut cur = vec![0usize; b.len() + 1];
    for x in a {
        for (j, y) in b.iter().enumerate() {
            cur[j + 1] = if x == y {
                prev[j] + 1
            } else {
                cur[j].max(prev[j + 1])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// The most cells of the table `lcs_pairs` fills (4 bytes each): past it the
/// table would grow with the product of the lengths, and a statement of
/// 20 000 tokens already needs gigabytes.
const LCS_CELLS: usize = 16_000_000;

/// The most cells (the product of the lengths, once the common start and end
/// are matched) the alignment works through exactly: about a minute of work
/// in a release build. Past it the middle stays unmatched.
const LCS_WORK: usize = 4_000_000_000;

thread_local! {
    static LCS_WORK_LIMIT: std::cell::Cell<usize> = const { std::cell::Cell::new(LCS_WORK) };
}

/// Runs `f` with the exact alignment bound ([`LCS_WORK`]) set to `limit` on
/// this thread, so a test can reach the inexact path with small inputs.
#[doc(hidden)]
pub fn with_alignment_bound<R>(limit: usize, f: impl FnOnce() -> R) -> R {
    let old = LCS_WORK_LIMIT.with(|c| c.replace(limit));
    let r = f();
    LCS_WORK_LIMIT.with(|c| c.set(old));
    r
}

/// Index pairs of a longest common subsequence, and whether they are one.
///
/// Exact at every size up to [`LCS_WORK`]. While the table fits
/// [`LCS_CELLS`] it is the table walk (so the pairs there are the same as
/// they always were); past that the common start and end are matched and the
/// middle is aligned by Hirschberg's method, which needs memory in the order
/// of the shorter length. Beyond the bound the middle stays unmatched and
/// the flag is false: the pairs are then a common subsequence but not a
/// longest one. The answer depends only on the inputs.
pub fn lcs_pairs_checked<T: PartialEq>(a: &[T], b: &[T]) -> (Vec<(usize, usize)>, bool) {
    let (n, m) = (a.len(), b.len());
    if n.saturating_mul(m) <= LCS_CELLS {
        return (lcs_table(a, b, 0, 0), true);
    }
    let pre = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..]
        .iter()
        .rev()
        .zip(b[pre..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (ma, mb) = (&a[pre..n - suf], &b[pre..m - suf]);
    let mut out: Vec<(usize, usize)> = (0..pre).map(|i| (i, i)).collect();
    let cells = ma.len().saturating_mul(mb.len());
    let mut exact = true;
    if cells <= LCS_CELLS {
        out.extend(lcs_table(ma, mb, pre, pre));
    } else if cells <= LCS_WORK_LIMIT.with(|c| c.get()) {
        hirschberg(ma, mb, pre, pre, &mut out);
    } else {
        exact = false;
    }
    out.extend((0..suf).map(|k| (n - suf + k, m - suf + k)));
    (out, exact)
}

/// [`lcs_pairs_checked`] for callers that only show the pairs: past the
/// bound they get the partial alignment.
pub fn lcs_pairs<T: PartialEq>(a: &[T], b: &[T]) -> Vec<(usize, usize)> {
    lcs_pairs_checked(a, b).0
}

/// The pairs of a longest common subsequence, or `None` when the lists are
/// past the bound and only part of them could be aligned. For callers that
/// report what is left over as added, removed or moved.
pub fn lcs_pairs_exact<T: PartialEq>(a: &[T], b: &[T]) -> Option<Vec<(usize, usize)>> {
    let (p, exact) = lcs_pairs_checked(a, b);
    exact.then_some(p)
}

/// LCS length of `a` against every prefix of `b` (`b.len() + 1` values), read
/// forward or, with `rev`, over both slices reversed.
fn lcs_row<T: PartialEq>(a: &[T], b: &[T], rev: bool) -> Vec<u32> {
    let m = b.len();
    let mut prev = vec![0u32; m + 1];
    let mut cur = vec![0u32; m + 1];
    for i in 0..a.len() {
        let x = if rev { &a[a.len() - 1 - i] } else { &a[i] };
        for j in 0..m {
            let y = if rev { &b[m - 1 - j] } else { &b[j] };
            cur[j + 1] = if x == y {
                prev[j] + 1
            } else {
                cur[j].max(prev[j + 1])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev
}

/// Appends the pairs of a longest common subsequence of `a` and `b`, offset
/// by `(oa, ob)`, in order: split `a` in half, find where `b` is best cut
/// from the scores of both halves, and align each side on its own.
fn hirschberg<T: PartialEq>(a: &[T], b: &[T], oa: usize, ob: usize, out: &mut Vec<(usize, usize)>) {
    if a.is_empty() || b.is_empty() {
        return;
    }
    if a.len().saturating_mul(b.len()) <= LCS_CELLS / 16 {
        out.extend(lcs_table(a, b, oa, ob));
        return;
    }
    let mid = a.len() / 2;
    let left = lcs_row(&a[..mid], b, false);
    let right = lcs_row(&a[mid..], b, true);
    let m = b.len();
    // The first cut of `b` with the best total; the same on every run.
    let mut cut = 0;
    let mut best = 0;
    for j in 0..=m {
        let total = left[j] + right[m - j];
        if total > best {
            best = total;
            cut = j;
        }
    }
    hirschberg(&a[..mid], &b[..cut], oa, ob, out);
    hirschberg(&a[mid..], &b[cut..], oa + mid, ob + cut, out);
}

/// The table walk of [`lcs_pairs`] over the whole of both slices; the pairs
/// are offset by `(oa, ob)`.
fn lcs_table<T: PartialEq>(a: &[T], b: &[T], oa: usize, ob: usize) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let w = m + 1;
    let mut dp = vec![0u32; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * w + j] = if a[i] == b[j] {
                dp[(i + 1) * w + j + 1] + 1
            } else {
                dp[(i + 1) * w + j].max(dp[i * w + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < n && j < m {
        if a[i] == b[j] {
            out.push((oa + i, ob + j));
            i += 1;
            j += 1;
        } else if dp[(i + 1) * w + j] >= dp[i * w + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// Dice similarity over the token LCS, in [0, 1].
pub fn similarity(a: &[NormTok], b: &[NormTok]) -> f64 {
    const CAP: usize = 400;
    let a: Vec<&str> = a.iter().take(CAP).map(|t| t.loose.as_str()).collect();
    let b: Vec<&str> = b.iter().take(CAP).map(|t| t.loose.as_str()).collect();
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    2.0 * lcs_len(&a, &b) as f64 / (a.len() + b.len()) as f64
}

/// Changed hunks between two token sequences, as (removed, added) raw text runs.
pub fn token_hunks(a: &[NormTok], b: &[NormTok]) -> Vec<(Vec<String>, Vec<String>)> {
    let ka: Vec<&str> = a.iter().map(|t| t.loose.as_str()).collect();
    let kb: Vec<&str> = b.iter().map(|t| t.loose.as_str()).collect();
    let pairs = lcs_pairs(&ka, &kb);
    let mut hunks = Vec::new();
    let (mut i, mut j) = (0, 0);
    for (pi, pj) in pairs.into_iter().chain(std::iter::once((a.len(), b.len()))) {
        if pi > i || pj > j {
            hunks.push((
                a[i..pi].iter().map(|t| t.raw.clone()).collect(),
                b[j..pj].iter().map(|t| t.raw.clone()).collect(),
            ));
        }
        i = pi + 1;
        j = pj + 1;
    }
    hunks
}

#[cfg(test)]
mod lcs_tests {
    use super::*;

    fn is_common(a: &[u32], b: &[u32], p: &[(usize, usize)]) -> bool {
        p.windows(2).all(|w| w[0].0 < w[1].0 && w[0].1 < w[1].1)
            && p.iter().all(|&(i, j)| a[i] == b[j])
    }

    #[test]
    fn small_inputs_get_a_longest_common_subsequence() {
        let a: Vec<u32> = vec![1, 2, 3, 4, 5, 2, 3];
        let b: Vec<u32> = vec![2, 3, 9, 4, 2, 3, 5];
        let p = lcs_pairs(&a, &b);
        assert!(is_common(&a, &b, &p));
        assert_eq!(p.len(), lcs_len(&a, &b));
    }

    /// Two 20 000-token statements that differ in one token must not build a
    /// table of 400 million cells: the common start and end are matched.
    #[test]
    fn long_inputs_with_one_change_are_matched_around_it() {
        let a: Vec<u32> = (0..20_000).collect();
        let mut b = a.clone();
        b[10_000] = 99_999;
        let p = lcs_pairs(&a, &b);
        assert!(is_common(&a, &b, &p));
        assert_eq!(p.len(), 19_999);
    }

    /// Past the table size the answer is still a longest common subsequence
    /// (checked against the table on inputs just past it) and the same every
    /// time.
    #[test]
    fn past_the_table_size_the_alignment_is_still_longest() {
        // Half of each side's items are shared, in order, with gaps.
        let a: Vec<u32> = (0..4_200).map(|i| (i * 7) % 1_000).collect();
        let b: Vec<u32> = (0..4_100).map(|i| (i * 11 + 3) % 1_000).collect();
        assert!(a.len() * b.len() > LCS_CELLS);
        let p = lcs_pairs(&a, &b);
        assert!(is_common(&a, &b, &p));
        assert_eq!(p.len(), lcs_len(&a, &b));
        assert_eq!(p, lcs_pairs(&a, &b));
    }
}
