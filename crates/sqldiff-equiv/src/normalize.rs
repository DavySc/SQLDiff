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

/// Index pairs of a longest common subsequence.
pub fn lcs_pairs<T: PartialEq>(a: &[T], b: &[T]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < n && j < m {
        if a[i] == b[j] {
            out.push((i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
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
