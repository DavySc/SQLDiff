//! Casts to an integer, T-SQL to PostgreSQL. T-SQL drops a decimal's or
//! float's fraction (CAST(1.5 AS int) is 1) where PostgreSQL rounds it (2),
//! so the converter writes trunc() first; money rounds on both sides and
//! stays as written. Checked on SQL Server 2022 and PostgreSQL 18, see
//! eval/casts/.

use sqldiff_convert::{check, rewrite_in, Conversion, Status, Unit};
use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{Artifact, Dialect, Options};

const TABLE: &str = "CREATE TABLE dbo.M (
    Id int NOT NULL PRIMARY KEY,
    D decimal(10,2) NULL,
    F float NULL,
    Amt money NULL,
    Q int NULL,
    S varchar(20) NULL
);
";

fn pg(src: &str) -> Conversion {
    let mut cat = Catalog::default();
    for f in [TABLE, src] {
        let root = sqldiff_syntax::parse(f, Dialect::Tsql).syntax();
        ddl::load_tree(&mut cat, &root, Dialect::Tsql);
    }
    let to = Dialect::Postgres;
    let target = rewrite_in(TABLE, Dialect::Tsql, to, None)
        .output()
        .to_string();
    let opts = Options {
        catalog_a: vec![Artifact::new("t.sql", Dialect::Tsql, TABLE)],
        catalog_b: vec![Artifact::new("t.sql", to, &target)],
        ..Options::default()
    };
    check(src, rewrite_in(src, Dialect::Tsql, to, Some(&cat)), &opts)
}

fn view(c: &Conversion) -> &Unit {
    c.units
        .iter()
        .filter(|u| u.status != Status::Converted || u.notes.is_empty())
        .max_by_key(|u| u.status)
        .expect("a unit")
}

fn has(c: &Conversion, text: &str) {
    assert!(c.output.contains(text), "{text}\n{}", c.output);
}

#[test]
fn decimal_and_float_to_integer_drop_the_fraction() {
    let src = "CREATE VIEW dbo.V AS SELECT Id, CAST(D AS int) AS a, CONVERT(bigint, F) AS b, CAST(1.5 AS int) AS c FROM dbo.M;\n";
    let c = pg(src);
    has(&c, "CAST(trunc(D) AS integer)");
    has(&c, "CAST(trunc(F) AS bigint)");
    has(&c, "CAST(trunc(1.5) AS integer)");
    let u = view(&c);
    assert!(u.status < Status::Unproven, "{:?}\n{}", u, c.output);
}

#[test]
fn integers_and_money_stay_as_written() {
    let src = "CREATE VIEW dbo.V AS SELECT Id, CAST(Q AS bigint) AS a, CAST(Amt AS int) AS b FROM dbo.M;\n";
    let c = pg(src);
    has(&c, "CAST(Q AS bigint)");
    assert!(!c.output.contains("trunc(Amt)"), "{}", c.output);
}

#[test]
fn a_string_cast_to_a_number_is_noted() {
    let src = "CREATE VIEW dbo.V AS SELECT Id, CAST(S AS int) AS a FROM dbo.M;\n";
    let c = pg(src);
    let notes: Vec<_> = c
        .units
        .iter()
        .flat_map(|u| &u.notes)
        .map(|n| n.rule.as_str())
        .collect();
    assert!(notes.contains(&"ASM-CAST-TEXT-NUMBER"), "{notes:?}");
    // The engine sees the difference ('' is 0 in T-SQL and an error in
    // PostgreSQL): never converted without a finding.
    assert!(view(&c).status >= Status::Assumptions, "{c:?}");
}
