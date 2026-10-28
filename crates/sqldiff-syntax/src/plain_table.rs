//! Plain tables: the `CREATE TABLE` statements whose every fact the proof
//! checker's C3a slice can take as given (`Sqldiff.Mirror.CatTable` in
//! formal/lean). A table is read only when its definition is nothing but
//! `int` columns, their nullability and a primary key; anything else (a
//! default, identity, computed column, other constraint, collation, table
//! option, temporary table, `IF NOT EXISTS`, a type other than `int`) is
//! refused, never skipped.
//!
//! T-SQL makes a column without `NULL` or `NOT NULL` nullable or not
//! depending on the session (`ANSI_NULL_DFLT_ON`, "SET ANSI_NULL_DFLT_ON
//! (Transact-SQL)"), so a T-SQL column outside the key must say which.

use crate::abstract_syntax::{tokens, Parser, K};
use crate::{Dialect, SyntaxKind, SyntaxNode};

/// A plain table as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlainTable {
    /// The name's parts as written (a quoted part as the abstract syntax
    /// keeps it: `"Name` when its case matters in PostgreSQL).
    pub name: Vec<String>,
    /// In declaration order.
    pub cols: Vec<PlainCol>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlainCol {
    pub name: String,
    pub not_null: bool,
    /// A column of the primary key (NOT NULL in any case).
    pub key: bool,
}

/// The name as the dialect compares it: T-SQL ignores case (for ASCII,
/// under the collations the slice admits), PostgreSQL folds unquoted names.
fn same(d: Dialect, a: &str, b: &str) -> bool {
    match d {
        Dialect::Tsql => a.eq_ignore_ascii_case(b),
        _ => fold(a) == fold(b),
    }
}

fn fold(n: &str) -> String {
    if n.starts_with('"') {
        n.to_string()
    } else {
        n.to_lowercase()
    }
}

/// `PRIMARY KEY [CLUSTERED | NONCLUSTERED]`, after `[CONSTRAINT name]`.
fn primary_key(p: &mut Parser, d: Dialect) -> Result<bool, String> {
    let at = p.i;
    if p.eat_kw("CONSTRAINT") {
        p.ident()?;
    }
    if p.eat_kw("PRIMARY") {
        p.expect_kw("KEY")?;
        if d == Dialect::Tsql && !p.eat_kw("CLUSTERED") {
            p.eat_kw("NONCLUSTERED");
        }
        Ok(true)
    } else {
        p.i = at;
        Ok(false)
    }
}

/// The plain table a statement defines, or why it is not one.
pub fn read(n: &SyntaxNode, d: Dialect) -> Result<PlainTable, String> {
    if n.kind() != SyntaxKind::STATEMENT {
        return Err("not a statement".into());
    }
    let t = tokens(n);
    let mut p = Parser::new(&t, d);
    p.expect_kw("CREATE")?;
    p.expect_kw("TABLE")?;
    if p.is_kw("IF") {
        return Err("CREATE TABLE IF NOT EXISTS may keep another definition".into());
    }
    let name = p.qname()?;
    if name.iter().any(|x| x.starts_with(['#', '@'])) {
        return Err("a temporary table".into());
    }
    p.expect_kind(K::LParen)?;
    let mut cols: Vec<PlainCol> = Vec::new();
    let mut key: Option<Vec<String>> = None;
    loop {
        if primary_key(&mut p, d)? {
            if key.is_some() {
                return Err("two primary keys".into());
            }
            p.expect_kind(K::LParen)?;
            let mut k = vec![p.ident()?];
            while p.eat_kind(K::Comma) {
                k.push(p.ident()?);
            }
            p.expect_kind(K::RParen)?;
            key = Some(k);
        } else {
            let col = p.ident()?;
            match p.peek() {
                Some(x) if x.kind == K::Word && (x.up == "INT" || x.up == "INTEGER") => p.i += 1,
                _ => return Err(p.err("only int columns are plain")),
            }
            let (mut null, mut not_null, mut pk) = (false, false, false);
            loop {
                if p.eat_kw("NOT") {
                    p.expect_kw("NULL")?;
                    if not_null {
                        return Err(p.err("NOT NULL twice"));
                    }
                    not_null = true;
                } else if p.eat_kw("NULL") {
                    if null {
                        return Err(p.err("NULL twice"));
                    }
                    null = true;
                } else if primary_key(&mut p, d)? {
                    if pk || key.is_some() {
                        return Err("two primary keys".into());
                    }
                    pk = true;
                } else {
                    break;
                }
            }
            if null && (not_null || pk) {
                return Err(format!("column {col} is both NULL and NOT NULL"));
            }
            if d == Dialect::Tsql && !(null || not_null || pk) {
                return Err(format!(
                    "column {col} does not say NULL or NOT NULL (ANSI_NULL_DFLT_ON decides)"
                ));
            }
            if pk {
                key = Some(vec![col.clone()]);
            }
            if cols.iter().any(|c| same(d, &c.name, &col)) {
                return Err(format!("column {col} twice"));
            }
            cols.push(PlainCol {
                name: col,
                not_null: not_null || pk,
                key: false,
            });
        }
        if !p.eat_kind(K::Comma) {
            break;
        }
    }
    p.expect_kind(K::RParen)?;
    // A T-SQL filegroup does not change what a write does.
    if d == Dialect::Tsql && p.eat_kw("ON") {
        p.ident()?;
    }
    if !p.at_end() {
        return Err(p.err("a table option"));
    }
    for k in key.unwrap_or_default() {
        let c = cols
            .iter_mut()
            .find(|c| same(d, &c.name, &k))
            .ok_or_else(|| format!("key column {k} is not a column"))?;
        if c.key {
            return Err(format!("key column {k} twice"));
        }
        c.key = true;
        c.not_null = true;
    }
    Ok(PlainTable { name, cols })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(sql: &str, d: Dialect) -> Result<PlainTable, String> {
        let root = crate::parse(sql, d).syntax();
        let n = root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::STATEMENT)
            .unwrap();
        read(&n, d)
    }

    #[test]
    fn plain_tables_read_and_everything_else_is_refused() {
        let t = table(
            "CREATE TABLE dbo.Acct (Id int NOT NULL, Qty int NULL, Total int NOT NULL, \
             CONSTRAINT PK_Acct PRIMARY KEY CLUSTERED (Id)) ON [PRIMARY];",
            Dialect::Tsql,
        )
        .unwrap();
        assert_eq!(t.name, ["dbo", "Acct"]);
        let flags: Vec<(&str, bool, bool)> = t
            .cols
            .iter()
            .map(|c| (c.name.as_str(), c.not_null, c.key))
            .collect();
        assert_eq!(
            flags,
            [
                ("Id", true, true),
                ("Qty", false, false),
                ("Total", true, false)
            ]
        );
        let pg = table(
            "CREATE TABLE acct (id integer PRIMARY KEY, qty integer, total integer NOT NULL);",
            Dialect::Postgres,
        )
        .unwrap();
        assert_eq!(
            pg.cols[0],
            PlainCol {
                name: "id".into(),
                not_null: true,
                key: true
            }
        );
        assert!(!pg.cols[1].not_null);
        for (sql, d) in [
            ("CREATE TABLE t (a int);", Dialect::Tsql),
            ("CREATE TABLE t (a int NULL DEFAULT 0);", Dialect::Tsql),
            ("CREATE TABLE t (a int IDENTITY(1,1) NOT NULL);", Dialect::Tsql),
            ("CREATE TABLE t (a int NULL CHECK (a > 0));", Dialect::Tsql),
            ("CREATE TABLE t (a int NOT NULL, b AS (a + 1));", Dialect::Tsql),
            ("CREATE TABLE #t (a int NULL);", Dialect::Tsql),
            ("CREATE TABLE t (a bigint NULL);", Dialect::Tsql),
            ("CREATE TABLE t (a int NOT NULL, UNIQUE (a));", Dialect::Tsql),
            (
                "CREATE TABLE t (a int NOT NULL, CONSTRAINT pk PRIMARY KEY (a) WITH (IGNORE_DUP_KEY = ON));",
                Dialect::Tsql,
            ),
            ("CREATE TABLE t (a int NULL, PRIMARY KEY (b));", Dialect::Tsql),
            ("CREATE TABLE t (a int, a int);", Dialect::Postgres),
            ("CREATE TABLE IF NOT EXISTS t (a int);", Dialect::Postgres),
            ("CREATE TEMP TABLE t (a int);", Dialect::Postgres),
            ("CREATE UNLOGGED TABLE t (a int);", Dialect::Postgres),
            ("CREATE TABLE t (a int REFERENCES u (a));", Dialect::Postgres),
            ("CREATE TABLE t (a int) INHERITS (u);", Dialect::Postgres),
            ("CREATE TABLE t (a int PRIMARY KEY DEFERRABLE INITIALLY DEFERRED);", Dialect::Postgres),
            ("CREATE TABLE t (a int GENERATED ALWAYS AS IDENTITY);", Dialect::Postgres),
            ("CREATE TABLE t (a int NULL NOT NULL);", Dialect::Postgres),
            ("CREATE TABLE t (a int PRIMARY KEY, b int PRIMARY KEY);", Dialect::Postgres),
            ("CREATE TABLE t (LIKE u);", Dialect::Postgres),
        ] {
            assert!(table(sql, d).is_err(), "{sql}");
        }
    }
}
