//! T-SQL LIKE patterns become PostgreSQL ones (checked on SQL Server 2022
//! and PostgreSQL 18): `[c]` classes become the character, escaped where
//! PostgreSQL would read a wildcard, and a literal `\` is doubled, since
//! `\` is PostgreSQL's default escape. Classes with no exact form and
//! ESCAPE clauses are refused. Case and trailing spaces are flagged with
//! ASM-CI-COLLATION.

use sqldiff_convert::{check, rewrite_in, Conversion, Status};
use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{Artifact, Dialect, Options};

const TABLES: &str =
    "CREATE TABLE dbo.files (file_id int NOT NULL PRIMARY KEY, path varchar(200) NOT NULL);\n";

fn convert(stmt: &str) -> Conversion {
    let src = format!("{stmt}\n");
    let mut cat = Catalog::default();
    let root = sqldiff_syntax::parse(TABLES, Dialect::Tsql).syntax();
    ddl::load_tree(&mut cat, &root, Dialect::Tsql);
    let target = rewrite_in(TABLES, Dialect::Tsql, Dialect::Postgres, None)
        .output()
        .to_string();
    let opts = Options {
        catalog_a: vec![Artifact::new("t.sql", Dialect::Tsql, TABLES)],
        catalog_b: vec![Artifact::new("t.sql", Dialect::Postgres, &target)],
        ..Options::default()
    };
    check(
        &src,
        rewrite_in(&src, Dialect::Tsql, Dialect::Postgres, Some(&cat)),
        &opts,
    )
}

#[test]
fn literal_patterns_are_rewritten_and_proven_under_the_collation_assumption() {
    for (ts, pg) in [
        ("'%[%]%'", "'%\\%%'"),
        ("'_:\\%'", "'_:\\\\%'"),
        ("'a[_]b%'", "'a\\_b%'"),
        ("N'a[[]b'", "'a[b'"),
        ("'s3://%/%'", "'s3://%/%'"),
        ("'it''s%'", "'it''s%'"),
    ] {
        let c = convert(&format!(
            "SELECT file_id FROM dbo.files WHERE path LIKE {ts};"
        ));
        let want = format!("WHERE path LIKE {pg};");
        assert!(c.output.contains(&want), "{ts}: {}", c.output);
        let u = c.units.last().unwrap();
        assert_eq!(u.status, Status::Assumptions, "{ts}: {:?}", u.notes);
        assert!(
            u.notes.iter().any(|n| n.rule == "ASM-CI-COLLATION"),
            "{ts}: {:?}",
            u.notes
        );
    }
}

#[test]
fn patterns_without_an_exact_form_are_refused() {
    for ts in [
        "'[ab]%'",
        "'[a-c]%'",
        "'[^a]%'",
        "'[]'",
        "'[a'",
        "'a!%' ESCAPE '!'",
    ] {
        let c = convert(&format!(
            "SELECT file_id FROM dbo.files WHERE path LIKE {ts};"
        ));
        let u = c.units.last().unwrap();
        assert_eq!(u.status, Status::NotConverted, "{ts}: {}", c.output);
    }
}

/// Ola Hallengren's `LIKE REPLACE(REPLACE(x,'[','[[]'),'_','[_]')` matches x
/// with `%` its only wildcard; PostgreSQL escapes its own escape first.
#[test]
fn an_escaped_value_pattern_is_rewritten_and_proven() {
    let c = convert(
        "SELECT a.file_id FROM dbo.files a JOIN dbo.files b ON a.path LIKE REPLACE(REPLACE(b.path,'[','[[]'),'_','[_]');",
    );
    assert!(
        c.output
            .contains("a.path LIKE replace(replace(b.path, '\\', '\\\\'), '_', '\\_')"),
        "{}",
        c.output
    );
    let u = c.units.last().unwrap();
    assert_eq!(u.status, Status::Assumptions, "{:?}", u.notes);
    // Another escaping is copied and stays unproven.
    let c = convert(
        "SELECT a.file_id FROM dbo.files a JOIN dbo.files b ON a.path LIKE REPLACE(b.path,'_','[_]');",
    );
    let u = c.units.last().unwrap();
    assert!(u.status >= Status::Unproven, "{:?} {}", u.status, c.output);
}
