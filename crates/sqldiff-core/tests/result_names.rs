//! Result column names are observed (decision 1 in
//! docs/verification-roadmap.md): exactly within one dialect, up to letter
//! case across dialects under ASM-RESULT-NAMES-CI. Names nothing outside the
//! statement sees (derived tables) are not.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;

fn run(a: &str, da: Dialect, b: &str, db: Dialect) -> Report {
    diff(
        &Artifact::new("a.sql", da, a),
        &Artifact::new("b.sql", db, b),
        &Options::default(),
    )
}

fn tsql(body: &str) -> String {
    format!("CREATE PROCEDURE dbo.p @id int AS\nBEGIN\n  {body}\nEND\n")
}

fn pg(body: &str) -> String {
    format!("CREATE FUNCTION p(p_id int) RETURNS TABLE (orderid int) LANGUAGE plpgsql AS $$\nBEGIN\n  {body}\nEND $$;\n")
}

fn has(r: &Report, id: &str) -> bool {
    r.assumptions.iter().any(|a| a.id == id)
}

fn rule(r: &Report) -> Vec<&str> {
    r.findings.iter().map(|f| f.rule.as_str()).collect()
}

const SELECT: &str = "SELECT o.OrderId AS OrderId FROM dbo.Orders o WHERE o.OrderId = @id;";

#[test]
fn a_renamed_alias_is_different() {
    let r = run(
        &tsql(SELECT),
        T,
        &tsql(&SELECT.replace("AS OrderId", "AS Id")),
        T,
    );
    assert_eq!(r.verdict(), Verdict::Different, "{:?}", rule(&r));
    assert!(rule(&r).contains(&"equiv.result-column-name"));
}

#[test]
fn a_t_sql_alias_keeps_its_case_whatever_the_collation() {
    for b in [
        SELECT.replace("AS OrderId", "AS orderid"),
        SELECT.replace("AS OrderId", "AS [ORDERID]"),
    ] {
        let r = run(&tsql(SELECT), T, &tsql(&b), T);
        assert_eq!(r.verdict(), Verdict::Different, "{b}: {:?}", rule(&r));
    }
    // Inside a compound statement it is never equal; statement equivalence
    // leaves that SELECT open, so it is unknown rather than different.
    let wrap = |s: &str| tsql(&format!("IF @id > 0\n  BEGIN\n    {s}\n  END"));
    let r = run(
        &wrap(SELECT),
        T,
        &wrap(&SELECT.replace("AS OrderId", "AS orderid")),
        T,
    );
    assert!(
        matches!(r.verdict(), Verdict::Different | Verdict::Unknown),
        "{:?}",
        rule(&r)
    );
}

#[test]
fn quoting_that_returns_the_same_name_is_equal() {
    let r = run(
        &tsql(SELECT),
        T,
        &tsql(&SELECT.replace("AS OrderId", "AS [OrderId]")),
        T,
    );
    assert_ne!(r.verdict(), Verdict::Different, "{:?}", rule(&r));
    assert_ne!(r.verdict(), Verdict::Unknown, "{:?}", rule(&r));
}

#[test]
fn other_names_still_fold_under_the_collation_assumption() {
    let b = SELECT.replace("FROM dbo.Orders o", "FROM dbo.orders o");
    let r = run(&tsql(SELECT), T, &tsql(&b), T);
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:?}",
        rule(&r)
    );
    assert!(has(&r, "ASM-TSQL-CI-COLLATION"));
}

#[test]
fn postgres_folds_unquoted_aliases_itself() {
    let q = "RETURN QUERY SELECT o.id AS OrderId FROM orders o WHERE o.id = p_id;";
    let r = run(&pg(q), P, &pg(&q.replace("AS OrderId", "AS orderid")), P);
    assert_ne!(r.verdict(), Verdict::Different, "{:?}", rule(&r));
}

#[test]
fn a_derived_tables_names_are_not_observed() {
    let a =
        "SELECT d.v AS OrderId FROM (SELECT o.OrderId AS v FROM dbo.Orders o) d WHERE d.v = @id;";
    let b =
        "SELECT d.w AS OrderId FROM (SELECT o.OrderId AS w FROM dbo.Orders o) d WHERE d.w = @id;";
    let r = run(&tsql(a), T, &tsql(b), T);
    assert_ne!(r.verdict(), Verdict::Different, "{:?}", rule(&r));
    assert!(
        !rule(&r).contains(&"equiv.result-column-name"),
        "{:?}",
        rule(&r)
    );
}

#[test]
fn across_dialects_letter_case_needs_the_assumption() {
    let t = tsql("SELECT o.OrderId AS OrderId FROM dbo.Orders o WHERE o.OrderId = @id;");
    let unquoted =
        pg("RETURN QUERY SELECT o.orderid AS OrderId FROM orders o WHERE o.orderid = p_id;");
    let r = run(&t, T, &unquoted, P);
    if matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions) {
        assert!(has(&r, "ASM-RESULT-NAMES-CI"), "{:?}", r.assumptions);
    }
}
