//! A string literal compared with (or cast to) a value of another type
//! converts to that type, so the comparison is not a string comparison and
//! collation plays no part. Each engine's conversion was checked on SQL
//! Server 2022 and PostgreSQL 18; see `equiv::literal`.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;
const D: Dialect = Dialect::Db2i;

const TSQL_DDL: &str = "CREATE TABLE dbo.pl (k int NOT NULL PRIMARY KEY, e datetime2 NOT NULL,
  d date NOT NULL, dt datetime NOT NULL, tm time NOT NULL, amt decimal(10,2) NOT NULL,
  n int NOT NULL, g uniqueidentifier NOT NULL, b bit NOT NULL, rv timestamp NOT NULL);";
const PG_DDL: &str = "CREATE TABLE pl (k integer NOT NULL PRIMARY KEY, e timestamp NOT NULL,
  d date NOT NULL, dt timestamp(3) NOT NULL, tm time NOT NULL, amt numeric(10,2) NOT NULL,
  n integer NOT NULL, g uuid NOT NULL, b boolean NOT NULL, rv bytea NOT NULL);";
const DB2_DDL: &str = "CREATE TABLE pl (k INTEGER NOT NULL PRIMARY KEY, e TIMESTAMP NOT NULL,
  d DATE NOT NULL, dt TIMESTAMP(3) NOT NULL, tm TIME NOT NULL, amt DECIMAL(10,2) NOT NULL,
  n INTEGER NOT NULL);";

fn ddl(d: Dialect) -> &'static str {
    match d {
        Dialect::Tsql => TSQL_DDL,
        Dialect::Postgres => PG_DDL,
        Dialect::Db2i => DB2_DDL,
    }
}

fn view(d: Dialect, cond: &str) -> String {
    match d {
        Dialect::Tsql => format!("CREATE VIEW dbo.v AS SELECT k FROM dbo.pl WHERE {cond};"),
        _ => format!("CREATE VIEW v AS SELECT k FROM pl WHERE {cond};"),
    }
}

fn run_with(a: &str, da: Dialect, b: &str, db: Dialect, catalog: bool) -> Report {
    let opts = if catalog {
        Options {
            catalog_a: vec![Artifact::new("catalog.sql", da, ddl(da))],
            catalog_b: vec![Artifact::new("catalog.sql", db, ddl(db))],
            ..Options::default()
        }
    } else {
        Options::default()
    };
    diff(
        &Artifact::new("a.sql", da, a),
        &Artifact::new("b.sql", db, b),
        &opts,
    )
}

/// The two views with these WHERE conditions, catalogs on both sides.
fn run(ca: &str, da: Dialect, cb: &str, db: Dialect) -> Report {
    run_with(&view(da, ca), da, &view(db, cb), db, true)
}

fn rules(r: &Report) -> Vec<&str> {
    r.findings.iter().map(|f| f.rule.as_str()).collect()
}

fn assumptions(r: &Report) -> Vec<&str> {
    r.assumptions.iter().map(|a| a.id.as_str()).collect()
}

fn equal(r: &Report) -> bool {
    matches!(r.verdict, Verdict::Equal | Verdict::EqualUnderAssumptions)
}

fn no_collation_finding(r: &Report) {
    for rule in ["equiv.case-sensitivity", "equiv.trailing-spaces"] {
        assert!(!rules(r).contains(&rule), "{rule}: {:?}", r.findings);
    }
}

/// The case from the temporal-table review: the SQL Server period end
/// against a datetime2 column was reported as a case-sensitivity difference.
/// It is not a string comparison; but PostgreSQL rounds the seventh digit
/// (to 10000-01-01 00:00:00), so the pair stays unknown, not equal.
#[test]
fn datetime2_period_end_is_not_a_string_comparison() {
    let lit = "e = '9999-12-31 23:59:59.9999999'";
    let r = run(lit, T, lit, P);
    no_collation_finding(&r);
    assert_eq!(r.verdict, Verdict::Unknown, "{:?}", r.findings);
    let text = format!("{:?}", r.findings);
    assert!(text.contains("not modelled for certain"), "{text}");
    // The same literal on both sides of one dialect is equal.
    assert!(equal(&run(lit, T, lit, T)));
}

#[test]
fn date_and_time_literals_compare_as_values() {
    for (a, b) in [
        ("e = '2020-01-01 10:00:00'", "e = '2020-01-01 10:00:00'"),
        ("e = '20200101 10:00'", "e = '2020-01-01T10:00:00.000'"),
        ("e >= '2020-01-01'", "e >= '2020-01-01 00:00:00'"),
        ("'2020-01-01' < e", "e > '2020-01-01'"),
        ("d = '2020-01-01'", "d = '20200101'"),
        ("tm = '10:00:00.5'", "tm = '10:00:00.500'"),
        (
            "e BETWEEN '2020-01-01' AND '2020-12-31 23:59:59.999999'",
            "e BETWEEN '2020-01-01' AND '2020-12-31 23:59:59.999999'",
        ),
        (
            "e IN ('2020-01-01', '2021-01-01')",
            "e IN ('2020-01-01', '2021-01-01')",
        ),
    ] {
        let r = run(a, T, b, P);
        no_collation_finding(&r);
        assert!(equal(&r), "{a} / {b}: {:?} {:?}", r.verdict, r.findings);
    }
    // Different values are not equal.
    let r = run("d = '2020-01-01'", T, "d = '2020-01-02'", P);
    assert!(!equal(&r), "{:?}", r.verdict);
}

/// Before: string literals inside BETWEEN, IN and CAST compared as equal
/// text on both sides, although PostgreSQL rounds a seventh digit (here to
/// the next day) and SQL Server does not.
#[test]
fn literals_that_round_differently_are_not_equal() {
    for (a, b) in [
        (
            "e BETWEEN '2020-01-01' AND '2020-01-01 23:59:59.9999999'",
            "e BETWEEN '2020-01-01' AND '2020-01-01 23:59:59.9999999'",
        ),
        (
            "e IN ('2020-01-01 23:59:59.9999999')",
            "e IN ('2020-01-01 23:59:59.9999999')",
        ),
        (
            "e = CAST('2020-01-01 10:00:00.1234567' AS datetime2)",
            "e = CAST('2020-01-01 10:00:00.1234567' AS timestamp)",
        ),
        ("tm = '10:00:00.1234567'", "tm = '10:00:00.1234567'"),
    ] {
        let r = run(a, T, b, P);
        assert!(!equal(&r), "{a}: {:?}", r.verdict);
        assert_ne!(r.verdict, Verdict::Different, "{a}: {:?}", r.findings);
    }
}

/// SQL Server converts '1.005' to the column's decimal(10,2), rounding it to
/// 1.01; PostgreSQL compares the exact value, which matches no row.
#[test]
fn decimal_literals_round_to_the_column_scale_on_sql_server_only() {
    let r = run("amt = '1.005'", T, "amt = '1.005'", P);
    assert!(!equal(&r), "{:?}", r.verdict);
    no_collation_finding(&r);
    for (a, b) in [
        ("amt = '1.01'", "amt = '1.010'"),
        ("amt = '1.01'", "amt = 1.01"),
        ("n = '05'", "n = 5"),
        ("n < '+7'", "n < '7'"),
    ] {
        let r = run(a, T, b, P);
        assert!(equal(&r), "{a} / {b}: {:?} {:?}", r.verdict, r.findings);
    }
    // Out of the integer range: both fail, but that is not modelled.
    let r = run("n = '9999999999'", T, "n = '9999999999'", P);
    assert!(!equal(&r), "{:?}", r.verdict);
}

#[test]
fn datetime_dash_literals_depend_on_dateformat() {
    let r = run("dt = '2020-01-02'", T, "dt = '2020-01-02'", P);
    assert!(equal(&r), "{:?} {:?}", r.verdict, r.findings);
    assert!(
        assumptions(&r).contains(&"ASM-TSQL-DATEFORMAT-YMD"),
        "{:?}",
        assumptions(&r)
    );
    let r = run("dt = '20200102'", T, "dt = '2020-01-02'", P);
    assert!(equal(&r), "{:?}", r.verdict);
    assert!(!assumptions(&r).contains(&"ASM-TSQL-DATEFORMAT-YMD"));
    // datetime rounds milliseconds to 1/300 s.
    let r = run(
        "dt = '2020-01-02 10:00:00.001'",
        T,
        "dt = '2020-01-02 10:00:00.001'",
        P,
    );
    assert!(!equal(&r), "{:?}", r.verdict);
}

#[test]
fn uuid_literals_ignore_case_and_braces_but_not_order() {
    let r = run(
        "g = '6F9619FF-8B86-D011-B42D-00C04FC964FF'",
        T,
        "g = '{6f9619ff-8b86-d011-b42d-00c04fc964ff}'",
        P,
    );
    no_collation_finding(&r);
    assert!(equal(&r), "{:?} {:?}", r.verdict, r.findings);
    // uniqueidentifier and uuid sort differently.
    let lit = "g < '6F9619FF-8B86-D011-B42D-00C04FC964FF'";
    let r = run(lit, T, lit, P);
    assert!(!equal(&r), "{:?}", r.verdict);
    no_collation_finding(&r);
}

/// T-SQL `timestamp` is rowversion, not a date and time.
#[test]
fn rowversion_is_not_a_timestamp() {
    let r = run("rv = '2020-01-01'", T, "rv = '2020-01-01'", P);
    assert!(!equal(&r), "{:?}", r.verdict);
    assert_ne!(r.verdict, Verdict::Different);
}

/// Without catalog facts the column may be a string or not: neither a
/// collation difference nor equality is known.
#[test]
fn untyped_column_against_a_literal_is_unknown() {
    let lit = "e = '2020-01-01'";
    let r = run_with(&view(T, lit), T, &view(P, lit), P, false);
    no_collation_finding(&r);
    assert_eq!(r.verdict, Verdict::Unknown, "{:?}", r.findings);
    let text = format!("{:?}", r.findings);
    assert!(text.contains("no catalog facts"), "{text}");
}

/// Db2 for i conversions are not checked on a real system, so nothing is
/// claimed for them either way.
#[test]
fn db2_for_i_literal_conversions_stay_unknown() {
    let lit = "d = '2020-01-01'";
    let r = run(lit, T, lit, D);
    no_collation_finding(&r);
    assert!(!equal(&r), "{:?}", r.verdict);
    assert_ne!(r.verdict, Verdict::Different, "{:?}", r.findings);
}

/// A character column compared with a literal still follows the collation.
#[test]
fn string_columns_keep_the_collation_finding() {
    let ts = "CREATE TABLE dbo.c (k int NOT NULL PRIMARY KEY, email varchar(254) NOT NULL);";
    let pg = "CREATE TABLE c (k integer NOT NULL PRIMARY KEY, email varchar(254) NOT NULL);";
    let opts = Options {
        catalog_a: vec![Artifact::new("catalog.sql", T, ts)],
        catalog_b: vec![Artifact::new("catalog.sql", P, pg)],
        ..Options::default()
    };
    let r = diff(
        &Artifact::new(
            "a.sql",
            T,
            "CREATE VIEW dbo.v AS SELECT k FROM dbo.c WHERE email = 'A@x.com';",
        ),
        &Artifact::new(
            "b.sql",
            P,
            "CREATE VIEW v AS SELECT k FROM c WHERE email = 'A@x.com';",
        ),
        &opts,
    );
    assert_eq!(r.verdict, Verdict::Different);
    assert!(rules(&r).contains(&"equiv.case-sensitivity"));
}
