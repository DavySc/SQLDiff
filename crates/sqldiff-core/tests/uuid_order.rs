//! T-SQL uniqueidentifier against PostgreSQL uuid. Equality agrees; order
//! does not: SQL Server compares the byte groups last group first
//! (00000000-0000-0000-0000-000000000001 sorts after
//! 01000000-0000-0000-0000-000000000000), PostgreSQL byte by byte.

use sqldiff_core::{diff, Artifact, Dialect, Options, Verdict};

const MS_DDL: &str =
    "CREATE TABLE dbo.G (Id int NOT NULL PRIMARY KEY, g uniqueidentifier NOT NULL, h uniqueidentifier NULL);";
const PG_DDL: &str =
    "CREATE TABLE G (Id integer NOT NULL PRIMARY KEY, g uuid NOT NULL, h uuid NULL);";

fn equal(a: &str, b: &str) -> bool {
    let _ci = sqldiff_core::equiv::compare::ci_collation_scope(true);
    let opts = Options {
        catalog_a: vec![Artifact::new("catalog.sql", Dialect::Tsql, MS_DDL)],
        catalog_b: vec![Artifact::new("catalog.sql", Dialect::Postgres, PG_DDL)],
        ..Options::default()
    };
    let r = diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Postgres, b),
        &opts,
    );
    matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions)
}

#[test]
fn uuid_equality_agrees() {
    assert!(equal(
        "SELECT Id FROM dbo.G WHERE g = h;",
        "SELECT Id FROM G WHERE g = h;"
    ));
    assert!(equal(
        "SELECT Id FROM dbo.G WHERE g <> h;",
        "SELECT Id FROM G WHERE g <> h;"
    ));
}

#[test]
fn uuid_order_differs() {
    let mut bad = Vec::new();
    for (a, b) in [
        (
            "SELECT Id FROM dbo.G WHERE g < h;",
            "SELECT Id FROM G WHERE g < h;",
        ),
        (
            "SELECT Id FROM dbo.G WHERE h >= g;",
            "SELECT Id FROM G WHERE h >= g;",
        ),
        (
            "SELECT Id FROM dbo.G WHERE g BETWEEN h AND h;",
            "SELECT Id FROM G WHERE g BETWEEN h AND h;",
        ),
        (
            "SELECT Id FROM dbo.G ORDER BY g;",
            "SELECT Id FROM G ORDER BY g;",
        ),
        (
            "SELECT Id, ROW_NUMBER() OVER (ORDER BY g) AS r FROM dbo.G;",
            "SELECT Id, ROW_NUMBER() OVER (ORDER BY g) AS r FROM G;",
        ),
        (
            "SELECT g FROM dbo.G UNION ALL SELECT g FROM dbo.G ORDER BY g;",
            "SELECT g FROM G UNION ALL SELECT g FROM G ORDER BY g;",
        ),
        (
            "SELECT MIN(g) AS m FROM dbo.G;",
            "SELECT MIN(g) AS m FROM G;",
        ),
        (
            "SELECT MAX(h) AS m FROM dbo.G;",
            "SELECT MAX(h) AS m FROM G;",
        ),
    ] {
        if equal(a, b) {
            bad.push(format!("shown equal:\n  {a}\n  {b}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
