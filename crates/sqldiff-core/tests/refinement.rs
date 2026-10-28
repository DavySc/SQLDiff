//! "Equal" means B refines A, the reference (decision 1 in
//! docs/verification-roadmap.md); `equivalent` says whether A also refines B.

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
    format!("CREATE PROCEDURE dbo.l AS\nBEGIN\n  {body}\nEND\n")
}

const UNORDERED: &str = "SELECT o.OrderId FROM dbo.Orders o;";
const ORDERED: &str = "SELECT o.OrderId FROM dbo.Orders o ORDER BY o.OrderId;";

fn equal(r: &Report) -> bool {
    matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions)
}

#[test]
fn identical_sides_are_equivalent() {
    let r = run(&tsql(UNORDERED), T, &tsql(UNORDERED), T);
    assert_eq!(r.verdict(), Verdict::Equal);
    assert_eq!(r.equivalent, Some(true));
    assert!(r.refinements.is_empty());
}

#[test]
fn ordering_an_unordered_result_refines_it() {
    let r = run(&tsql(UNORDERED), T, &tsql(ORDERED), T);
    assert!(equal(&r), "{:?}", r.findings);
    assert_eq!(r.equivalent, Some(false));
    assert_eq!(r.refinements.len(), 1);
}

#[test]
fn the_reference_is_side_a() {
    // Dropping an ORDER BY lets B return orders A never does; until the
    // engine knows the result can hold two rows it stays unknown.
    let r = run(&tsql(ORDERED), T, &tsql(UNORDERED), T);
    assert!(!equal(&r), "{:?}", r.verdict());
    assert_eq!(r.equivalent, None);
}

#[test]
fn an_order_that_picks_rows_is_not_a_refinement() {
    let top = |s: &str| s.replace("SELECT o.OrderId", "SELECT TOP 5 o.OrderId");
    let r = run(&tsql(&top(UNORDERED)), T, &tsql(&top(ORDERED)), T);
    assert!(!equal(&r), "{:?}", r.verdict());
}

#[test]
fn across_dialects_too() {
    let pg = "CREATE FUNCTION l() RETURNS TABLE (orderid int) LANGUAGE plpgsql AS $$\nBEGIN\n  RETURN QUERY SELECT o.orderid FROM orders o ORDER BY o.orderid;\nEND $$;\n";
    let r = run(&tsql(UNORDERED), T, pg, P);
    if equal(&r) {
        assert_eq!(r.equivalent, Some(false), "{:?}", r.refinements);
    }
}
