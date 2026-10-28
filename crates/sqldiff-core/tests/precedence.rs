//! Operator precedence as each engine reads it. PostgreSQL ranks `||` below
//! `+` and `-`: `'a' || 1 + 2` is `'a3'`, `('a' || 1) + 2` an error.

use sqldiff_core::{diff, Artifact, Dialect, Options, Verdict};

#[test]
fn postgres_concatenation_and_addition_are_not_regrouped() {
    let a = Artifact::new("a.sql", Dialect::Postgres, "SELECT 'a' || 1 + 2 AS x;\n");
    let b = Artifact::new("b.sql", Dialect::Postgres, "SELECT ('a' || 1) + 2 AS x;\n");
    let r = diff(&a, &b, &Options::default());
    assert!(
        !matches!(r.verdict, Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.verdict
    );
    // The grouping PostgreSQL applies anyway is equal.
    let c = Artifact::new("c.sql", Dialect::Postgres, "SELECT 'a' || (1 + 2) AS x;\n");
    let r = diff(&a, &c, &Options::default());
    assert_eq!(r.verdict, Verdict::Equal, "{:?}", r.findings);
}
