//! LIKE patterns of T-SQL and PostgreSQL as one form, so a converted
//! pattern can be written and compared.
//!
//! Checked on SQL Server 2022 and PostgreSQL 18 (2026-10-29):
//! - Both: `%` is any string, `_` any one character.
//! - T-SQL: `[c]` is the character `c`, so `[[]`, `[_]`, `[%]` are literals;
//!   `\` and a `]` outside brackets are literals. `[]` and `[]]` match
//!   nothing, an unclosed `[a` matches nothing, `[^]` matches `^` and any
//!   other character, and `-` inside brackets is a range or, at an end, a
//!   literal whose order follows the collation: all refused here, as are
//!   classes of several characters and negated ones.
//! - T-SQL's `_` under a non-_SC collation matches no character outside the
//!   BMP (`N'a😀' LIKE N'a_'` and `N'a__'` are both false); PostgreSQL's
//!   matches one code point. The comparison notes `ASM-TSQL-LEN-BMP`.
//! - PostgreSQL: `\` escapes the next character (the default ESCAPE), a
//!   trailing lone `\` is refused; `[` is a literal.
//!
//! Case, trailing spaces and collation equivalences (T-SQL ignores trailing
//! spaces of a varchar or char value, not of nvarchar or of the pattern,
//! and matches up to letter case under its default collation) are left to
//! the comparison, which treats them under `ASM-CI-COLLATION`.

use sqldiff_syntax::Dialect;

use super::ast::Expr;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Any,
    One,
    Lit(char),
}

fn tsql(p: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let mut it = p.chars();
    while let Some(c) = it.next() {
        match c {
            '%' => out.push(Tok::Any),
            '_' => out.push(Tok::One),
            '[' => {
                let mut class = String::new();
                loop {
                    match it.next() {
                        Some(']') => break,
                        Some(x) => class.push(x),
                        None => {
                            return Err("a `[` without its `]` (matches nothing in T-SQL)".into())
                        }
                    }
                }
                let mut cs = class.chars();
                match (cs.next(), cs.next()) {
                    (None, _) => return Err("an empty `[]` class (matches nothing in T-SQL)".into()),
                    (Some('^'), _) => {
                        return Err("a negated `[^…]` class has no PostgreSQL LIKE form".into())
                    }
                    (Some('-'), None) => out.push(Tok::Lit('-')),
                    (Some(x), None) => out.push(Tok::Lit(x)),
                    (Some(_), Some(_)) => {
                        return Err(format!(
                            "the class `[{class}]` (several characters or a range) has no PostgreSQL LIKE form"
                        ))
                    }
                }
            }
            x => out.push(Tok::Lit(x)),
        }
    }
    Ok(out)
}

fn postgres(p: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let mut it = p.chars();
    while let Some(c) = it.next() {
        match c {
            '%' => out.push(Tok::Any),
            '_' => out.push(Tok::One),
            '\\' => match it.next() {
                Some(x) => out.push(Tok::Lit(x)),
                None => return Err("a pattern that ends in a lone `\\`".into()),
            },
            x => out.push(Tok::Lit(x)),
        }
    }
    Ok(out)
}

/// The pattern as PostgreSQL LIKE text with the default `\` escape.
fn render(ts: &[Tok]) -> String {
    let mut s = String::new();
    for t in ts {
        match t {
            Tok::Any => s.push('%'),
            Tok::One => s.push('_'),
            Tok::Lit(c @ ('%' | '_' | '\\')) => {
                s.push('\\');
                s.push(*c);
            }
            Tok::Lit(c) => s.push(*c),
        }
    }
    s
}

/// A T-SQL LIKE pattern (the string value) as a PostgreSQL LIKE pattern
/// that matches the same strings, or why there is none.
pub fn tsql_to_postgres(p: &str) -> Result<String, String> {
    tsql(p).map(|t| render(&t))
}

/// The pattern in one form for both dialects (PostgreSQL's), when it is
/// read exactly.
pub fn canonical(p: &str, d: Dialect) -> Option<String> {
    match d {
        Dialect::Tsql => tsql(p).ok(),
        Dialect::Postgres => postgres(p).ok(),
        Dialect::Db2i => None,
    }
    .map(|t| render(&t))
}

/// The value x of a pattern that matches x itself with `%` its only
/// wildcard, each dialect's way: T-SQL `REPLACE(REPLACE(x, '[', '[[]'), '_',
/// '[_]')` (Ola Hallengren's idiom: `[[]` and `[_]` are the literal
/// characters, `\` is a literal anyway) and PostgreSQL `replace(replace(x,
/// '\', '\\'), '_', '\_')` (its escape character escaped first).
pub fn escaped_value(p: &Expr, d: Dialect) -> Option<&Expr> {
    let (first, second) = match d {
        Dialect::Tsql => (("[", "[[]"), ("_", "[_]")),
        Dialect::Postgres => (("\\", "\\\\"), ("_", "\\_")),
        Dialect::Db2i => return None,
    };
    fn replace<'e>(e: &'e Expr, (from, to): (&str, &str)) -> Option<&'e Expr> {
        let Expr::Call(c) = e else { return None };
        let plain = matches!(&c.name[..], [n] if n.eq_ignore_ascii_case("replace"))
            && !(c.distinct || c.over || c.star || c.within_group)
            && c.order.is_empty();
        match &c.args[..] {
            [x, Expr::Str(f), Expr::Str(t)] if plain && f == from && t == to => Some(x),
            _ => None,
        }
    }
    replace(replace(p, second)?, first)
}

/// Whether a pattern in the form `canonical` gives has a `_` wildcard.
pub fn steps_one(canonical: &str) -> bool {
    postgres(canonical).is_ok_and(|t| t.contains(&Tok::One))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tsql_patterns_become_postgres_ones() {
        for (t, p) in [
            ("%[%]%", "%\\%%"),
            ("a[_]b", "a\\_b"),
            ("a[[]b", "a[b"),
            ("%\\%", "%\\\\%"),
            ("_:\\%", "_:\\\\%"),
            ("a]b", "a]b"),
            ("[-]", "-"),
            ("[\\]", "\\\\"),
            ("https://%/%", "https://%/%"),
            ("", ""),
        ] {
            assert_eq!(tsql_to_postgres(t).as_deref(), Ok(p), "{t}");
            assert_eq!(canonical(p, Dialect::Postgres).as_deref(), Some(p), "{p}");
        }
        for t in ["[]", "[]]", "[a", "[^a]", "[^]", "[ab]", "[a-c]", "[a-]"] {
            assert!(tsql_to_postgres(t).is_err(), "{t}");
        }
        assert_eq!(canonical("a\\", Dialect::Postgres), None);
        // PostgreSQL's `\x` is the literal x.
        assert_eq!(canonical("a\\b", Dialect::Postgres).as_deref(), Some("ab"));
        assert!(steps_one("a_%"));
        assert!(!steps_one("a\\_%"));
    }
}
