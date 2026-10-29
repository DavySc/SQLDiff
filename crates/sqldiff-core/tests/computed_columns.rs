//! Computed columns (T-SQL `c AS (expr)`) against PostgreSQL generated
//! columns (`c type GENERATED ALWAYS AS (expr) STORED`): the column types
//! SQL Server derives, and the expressions compared row by row.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;

fn run(a: &str, b: &str) -> Report {
    diff(
        &Artifact::new("a.sql", T, a),
        &Artifact::new("b.sql", P, b),
        &Options::default(),
    )
}

fn equal(a: &str, b: &str) -> Report {
    let r = run(a, b);
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:#?}\n{:#?}",
        r.findings,
        r.unknown_reasons
    );
    assert!(r.unknown_reasons.is_empty(), "{:?}", r.unknown_reasons);
    r
}

fn not_equal(a: &str, b: &str, needle: &str) {
    let r = run(a, b);
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:#?}",
        r.findings
    );
    let all: Vec<String> = r
        .findings
        .iter()
        .map(|f| format!("{} {}", f.rule, f.witness))
        .chain(r.unknown_reasons.iter().cloned())
        .collect();
    assert!(all.iter().any(|x| x.contains(needle)), "{needle}: {all:#?}");
}

fn tsql(col: &str) -> String {
    format!(
        "CREATE TABLE dbo.People (
    PersonID int NOT NULL PRIMARY KEY,
    FullName nvarchar(50) NOT NULL,
    PreferredName nvarchar(50) NULL,
    Notes nvarchar(max) NULL,
    {col}
);"
    )
}

fn pg(col: &str) -> String {
    format!(
        "CREATE TABLE People (
    PersonID integer NOT NULL PRIMARY KEY,
    FullName varchar(50) NOT NULL,
    PreferredName varchar(50) NULL,
    Notes text NULL,
    {col}
);"
    )
}

#[test]
fn concat_reads_null_as_empty() {
    equal(
        &tsql("SearchName AS (concat(PreferredName, N' ', FullName)) PERSISTED NOT NULL"),
        &pg("SearchName varchar(101) GENERATED ALWAYS AS (COALESCE(PreferredName, '') || ' ' || COALESCE(FullName, '')) STORED NOT NULL"),
    );
    // With an nvarchar(max) part the result is unbounded.
    equal(
        &tsql("SearchDetails AS (concat(FullName, N' ', Notes))"),
        &pg("SearchDetails text GENERATED ALWAYS AS (COALESCE(FullName, '') || ' ' || COALESCE(Notes, '')) STORED"),
    );
}

#[test]
fn concat_near_misses() {
    // `||` without COALESCE is NULL when a part is.
    not_equal(
        &tsql("SearchName AS (concat(PreferredName, N' ', FullName)) PERSISTED NOT NULL"),
        &pg("SearchName varchar(101) GENERATED ALWAYS AS (PreferredName || ' ' || FullName) STORED NOT NULL"),
        "SearchName",
    );
    // Parts in another order.
    not_equal(
        &tsql("SearchName AS (concat(PreferredName, N' ', FullName)) PERSISTED NOT NULL"),
        &pg("SearchName varchar(101) GENERATED ALWAYS AS (COALESCE(FullName, '') || ' ' || COALESCE(PreferredName, '')) STORED NOT NULL"),
        "SearchName",
    );
    // A shorter column type.
    not_equal(
        &tsql("SearchName AS (concat(PreferredName, N' ', FullName)) PERSISTED NOT NULL"),
        &pg("SearchName varchar(100) GENERATED ALWAYS AS (COALESCE(PreferredName, '') || ' ' || COALESCE(FullName, '')) STORED NOT NULL"),
        "equiv.column-type",
    );
    // A plain column against a computed one.
    not_equal(
        &tsql("SearchName AS (concat(PreferredName, N' ', FullName)) PERSISTED NOT NULL"),
        &pg("SearchName varchar(101) NOT NULL"),
        "equiv.column-computed",
    );
    // NOT NULL on one side only.
    not_equal(
        &tsql("SearchName AS (concat(PreferredName, N' ', FullName)) PERSISTED NOT NULL"),
        &pg("SearchName varchar(101) GENERATED ALWAYS AS (COALESCE(PreferredName, '') || ' ' || COALESCE(FullName, '')) STORED"),
        "equiv.column-null",
    );
}
