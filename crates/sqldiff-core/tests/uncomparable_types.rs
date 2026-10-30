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

#[test]
fn built_ins_sql_server_refuses_are_not_shown_equal() {
    // UPPER, LOWER, LEFT, LEN, REPLACE and STRING_AGG: Msg 8116; `+` and
    // NULLIF: Msg 402.
    let mut bad = Vec::new();
    for c in ["T", "N"] {
        for (a, b) in [
            (format!("UPPER({c})"), format!("upper({c})")),
            (format!("LOWER({c})"), format!("lower({c})")),
            (format!("LEFT({c}, 3)"), format!("left({c}, 3)")),
            (format!("LEN({c})"), format!("length(rtrim({c}))")),
            (
                format!("REPLACE({c}, 'a', 'b')"),
                format!("replace({c}, 'a', 'b')"),
            ),
            (format!("NULLIF({c}, 'a')"), format!("nullif({c}, 'a')")),
            (format!("{c} + 'x'"), format!("{c} || 'x'")),
            (format!("'x' + {c}"), format!("'x' || {c}")),
        ] {
            let a = format!("SELECT {a} AS v FROM dbo.D;");
            if equal(&a, &format!("SELECT {b} AS v FROM D;")) {
                bad.push(a);
            }
        }
        let a = format!("SELECT STRING_AGG({c}, ',') AS v FROM dbo.D;");
        if equal(&a, &format!("SELECT string_agg({c}, ',') AS v FROM D;")) {
            bad.push(a);
        }
    }
    assert!(bad.is_empty(), "shown equal:\n{}", bad.join("\n"));
}

#[test]
fn built_ins_that_take_text_still_compare() {
    for c in ["T", "N"] {
        let a = format!("SELECT COALESCE({c}, 'x') AS v FROM dbo.D;");
        let b = format!("SELECT coalesce({c}, 'x') AS v FROM D;");
        assert!(equal(&a, &b), "{a}");
    }
}
