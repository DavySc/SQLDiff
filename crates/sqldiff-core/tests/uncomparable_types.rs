//! Types SQL Server cannot compare or sort: text, ntext, image and xml.
//! Checked on SQL Server 2022: GROUP BY, DISTINCT, COUNT(DISTINCT), UNION,
//! IN, IN (subquery), =, ORDER BY and MIN fail on them (Msg 305, 306, 402,
//! 421, 5335, 8117); only IS NULL and LIKE work. PostgreSQL compares text
//! and bytea, and has no xml equality. So none of these is shown equal,
//! also under ASM-CI-COLLATION; a value copied as it is still is.

use sqldiff_core::{diff, Artifact, Dialect, Options, Verdict};

const MS_DDL: &str = "CREATE TABLE dbo.D (Id int NOT NULL PRIMARY KEY, T text NULL, N ntext NULL, X xml NULL, I image NULL, T2 text NULL, N2 ntext NULL, X2 xml NULL, I2 image NULL);";
const PG_DDL: &str = "CREATE TABLE D (Id integer NOT NULL PRIMARY KEY, T text NULL, N text NULL, X xml NULL, I bytea NULL, T2 text NULL, N2 text NULL, X2 xml NULL, I2 bytea NULL);";

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
fn values_copied_as_they_are() {
    for c in ["T", "N", "X"] {
        let a = format!("UPDATE dbo.D SET {c}2 = {c} WHERE Id = 1;");
        assert!(equal(&a, &a.replace("dbo.", "")), "{a}");
    }
}

#[test]
fn comparisons_are_not_shown_equal() {
    let mut bad = Vec::new();
    for c in ["T", "N", "X", "I"] {
        for q in [
            format!("SELECT {c}, COUNT(*) AS n FROM dbo.D GROUP BY {c};"),
            format!("SELECT DISTINCT {c} FROM dbo.D;"),
            format!("SELECT COUNT(DISTINCT {c}) AS n FROM dbo.D;"),
            format!("SELECT {c} FROM dbo.D UNION SELECT {c}2 FROM dbo.D;"),
            format!("SELECT Id FROM dbo.D WHERE {c} IN ({c}2);"),
            format!("SELECT Id FROM dbo.D WHERE {c} IN (SELECT {c}2 FROM dbo.D);"),
            format!("SELECT Id FROM dbo.D WHERE {c} = {c}2;"),
            format!("SELECT Id FROM dbo.D ORDER BY {c};"),
            format!("SELECT MIN({c}) AS m FROM dbo.D;"),
        ] {
            if equal(&q, &q.replace("dbo.", "")) {
                bad.push(q);
            }
        }
    }
    assert!(bad.is_empty(), "shown equal:\n{}", bad.join("\n"));
}
