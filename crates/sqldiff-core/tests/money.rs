//! T-SQL `money` against PostgreSQL `numeric(19, 4)`. Equal only for the
//! shapes SQL Server 2022 and PostgreSQL 18 compute alike for every input
//! (runs in eval/builtins/2026-11-02-money); every other use of money stays
//! "not shown equal". The cases use their own column names and no corpus
//! table.

use sqldiff_core::{diff, Artifact, Dialect, Options, Verdict};

const MS_DDL: &str =
    "CREATE TABLE dbo.Acct (Id int NOT NULL PRIMARY KEY, Price money NOT NULL, Fee money NOT NULL,
  Qty int NOT NULL, Rate decimal(9,2) NOT NULL, F float NOT NULL);";
const PG_DDL: &str = "CREATE TABLE Acct (Id integer NOT NULL PRIMARY KEY, Price numeric(19,4) NOT NULL, Fee numeric(19,4) NOT NULL,
  Qty integer NOT NULL, Rate numeric(9,2) NOT NULL, F double precision NOT NULL);";

fn equal(a: &str, b: &str) -> bool {
    let opts = Options {
        catalog_a: vec![Artifact::new("catalog.sql", Dialect::Tsql, MS_DDL)],
        catalog_b: vec![Artifact::new("catalog.sql", Dialect::Postgres, PG_DDL)],
        ..Options::default()
    };
    let r = diff(
        &Artifact::new(
            "a.sql",
            Dialect::Tsql,
            format!("CREATE VIEW dbo.V AS SELECT Id, {a} AS x FROM dbo.Acct;"),
        ),
        &Artifact::new(
            "b.sql",
            Dialect::Postgres,
            format!("CREATE OR REPLACE VIEW V AS SELECT Id, {b} AS x FROM Acct;"),
        ),
        &opts,
    );
    matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions)
}

fn table_equal(col_a: &str, col_b: &str) -> bool {
    let r = diff(
        &Artifact::new(
            "a.sql",
            Dialect::Tsql,
            format!("CREATE TABLE dbo.T (A money NOT NULL, B money NOT NULL, Big bigint NOT NULL, Q int NOT NULL, R decimal(9,2) NOT NULL, {col_a});"),
        ),
        &Artifact::new(
            "b.sql",
            Dialect::Postgres,
            format!("CREATE TABLE T (A numeric(19,4) NOT NULL, B numeric(19,4) NOT NULL, Big bigint NOT NULL, Q integer NOT NULL, R numeric(9,2) NOT NULL, {col_b});"),
        ),
        &Options::default(),
    );
    matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions)
}

#[test]
fn money_converts_to_whole_numbers_and_decimals_like_numeric() {
    for (a, b) in [
        ("CAST(Price AS int)", "CAST(Price AS integer)"),
        ("CAST(Price AS bigint)", "CAST(Price AS bigint)"),
        (
            "CAST(Price AS decimal(10,2))",
            "CAST(Price AS numeric(10,2))",
        ),
        (
            "CAST(Price AS decimal(18,0))",
            "CAST(Price AS numeric(18,0))",
        ),
        (
            "CAST(Price AS decimal(20,4))",
            "CAST(Price AS numeric(20,4))",
        ),
    ] {
        assert!(equal(a, b), "{a} vs {b}");
    }
}

#[test]
fn money_uses_that_differ_stay_unproven() {
    for (a, b) in [
        // T-SQL prints two decimals, numeric prints four.
        ("CAST(Price AS varchar(30))", "CAST(Price AS varchar(30))"),
        // A product and a quotient round to four decimals in T-SQL only.
        ("Price * Fee", "Price * Fee"),
        ("Price / Fee", "Price / Fee"),
        ("Price / Qty", "Price / Qty"),
        // Money with a float converts the money to float.
        ("Price + F", "Price + F"),
        ("Price < F", "Price < F"),
        ("ROUND(Price, 2)", "ROUND(Price, 2)"),
        // The smallest money value has no negative or absolute value in T-SQL.
        ("-Price", "-Price"),
        ("ABS(Price)", "ABS(Price)"),
        // A different cast target or a wrong operation is never equal.
        ("CAST(Price AS int)", "CAST(Price AS bigint)"),
        ("Price + Fee", "Price - Fee"),
    ] {
        assert!(!equal(a, b), "{a} vs {b}");
    }
}

#[test]
fn money_with_a_decimal_is_decimal_arithmetic() {
    assert!(equal("Price + Rate", "Price + Rate"));
    assert!(equal("Price * Rate", "Price * Rate"));
    assert!(equal("Price < Fee", "Price < Fee"));
}

#[test]
fn money_computed_columns_with_exact_arithmetic() {
    let g = |e: &str| format!("C numeric(19,4) GENERATED ALWAYS AS ({e}) STORED");
    for (a, b) in [
        ("C AS A + B PERSISTED", "A + B"),
        ("C AS A - B PERSISTED", "A - B"),
        ("C AS A * Q PERSISTED", "A * Q"),
        ("C AS Q * A PERSISTED", "Q * A"),
        ("C AS (A + B) PERSISTED", "A + B"),
    ] {
        assert!(table_equal(a, &g(b)), "{a} vs {b}");
    }
    // The wrong operation, a different operand, and the rounded product or
    // quotient of money are never proven.
    for (a, b) in [
        ("C AS A + B PERSISTED", "A - B"),
        ("C AS A - B PERSISTED", "B - A"),
        ("C AS A * Q PERSISTED", "A * Q + 1"),
        ("C AS A * B PERSISTED", "A * B"),
        // A bigint is converted to money first and may not fit.
        ("C AS A + Big PERSISTED", "A + Big"),
        // A chain: an intermediate may leave the money range.
        ("C AS A + B - Q PERSISTED", "A + B - Q"),
        ("C AS A + B + A PERSISTED", "A + B + A"),
        ("C AS A / Q PERSISTED", "A / Q"),
    ] {
        assert!(!table_equal(a, &g(b)), "{a} vs {b}");
    }
}
