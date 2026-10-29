//! Same-dialect statement pairing: a statement rewritten so much that it
//! shares few tokens with its old version is still that statement's edit
//! when the other side runs one of the same kind in the same place. It is
//! "modified" (unknown unless statement equivalence decides it), never a
//! removed statement plus an added one, which would claim a result set or
//! return without counterpart.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;

fn run(a: &str, b: &str, d: Dialect) -> Report {
    diff(
        &Artifact::new("a.sql", d, a),
        &Artifact::new("b.sql", d, b),
        &Options::default(),
    )
}

fn tsql_proc(body: &str) -> String {
    format!("CREATE PROCEDURE dbo.p @p int AS BEGIN {body} END")
}

fn rules(r: &Report) -> Vec<&str> {
    r.findings.iter().map(|f| f.rule.as_str()).collect()
}

fn unpaired(r: &Report) -> bool {
    r.findings
        .iter()
        .any(|f| f.rule == "stmt.removed" || f.rule == "stmt.added")
}

/// Both sides always return 0 (RAND() is in [0, 1)); the engine cannot
/// evaluate RAND(), so the honest verdict is unknown.
#[test]
fn rewritten_select_of_rand_is_unknown_not_different() {
    for (a, b) in [
        ("SELECT FLOOR(RAND()) AS y;", "SELECT 0e0 AS y;"),
        ("SELECT CAST(RAND() AS INT) AS y;", "SELECT 0 AS y;"),
        ("RETURN FLOOR(RAND());", "RETURN 0;"),
        (
            "SELECT 1 AS x; SELECT FLOOR(RAND()) AS y;",
            "SELECT 1 AS x; SELECT 0 AS y;",
        ),
    ] {
        let r = run(&tsql_proc(a), &tsql_proc(b), T);
        assert_eq!(
            r.verdict(),
            Verdict::Unknown,
            "{a} | {b}: {:#?}",
            r.findings
        );
        assert!(!unpaired(&r), "{a} | {b}: {:?}", rules(&r));
        assert!(rules(&r).contains(&"stmt.modified"), "{:?}", rules(&r));
    }
}

#[test]
fn rewritten_plpgsql_return_is_unknown_not_different() {
    let f = |e: &str| {
        format!(
            "CREATE FUNCTION f() RETURNS int LANGUAGE plpgsql AS $$\nBEGIN\n  RETURN {e};\nEND;\n$$;"
        )
    };
    let r = run(&f("floor(random())::int"), &f("0"), P);
    assert_eq!(r.verdict(), Verdict::Unknown, "{:#?}", r.findings);
    assert!(!unpaired(&r), "{:?}", rules(&r));
}

/// Pairing hands the statements to the witness search, which still finds
/// a real difference.
#[test]
fn rewritten_select_that_differs_is_still_different() {
    for (a, b) in [
        ("SELECT ABS(@p) + 1 AS y;", "SELECT 0 AS y;"),
        ("SELECT FLOOR(0e0) AS y;", "SELECT 1e0 AS y;"),
    ] {
        let r = run(&tsql_proc(a), &tsql_proc(b), T);
        assert_eq!(
            r.verdict(),
            Verdict::Different,
            "{a} | {b}: {:#?}",
            r.findings
        );
        assert!(rules(&r).contains(&"equiv.edit"), "{:?}", rules(&r));
    }
}

#[test]
fn extra_result_set_is_still_different() {
    let r = run(
        &tsql_proc("SELECT FLOOR(RAND()) AS y;"),
        &tsql_proc("SELECT 0e0 AS y; SELECT 0e0 AS z;"),
        T,
    );
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
    assert!(rules(&r).contains(&"stmt.added"), "{:?}", rules(&r));
}

#[test]
fn statements_of_different_kinds_are_not_paired() {
    let r = run(
        &tsql_proc("SELECT FLOOR(RAND()) AS y;"),
        &tsql_proc("UPDATE t SET a = 0;"),
        T,
    );
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
    assert!(rules(&r).contains(&"stmt.removed"), "{:?}", rules(&r));
    assert!(rules(&r).contains(&"stmt.added"), "{:?}", rules(&r));
}

/// A statement whose text appears elsewhere on the other side moved; it is
/// not paired with a neighbour of the same kind.
#[test]
fn moved_statement_is_still_reported_moved() {
    let r = run(
        &tsql_proc("UPDATE t SET a = 1; SELECT 1 AS k;"),
        &tsql_proc("SELECT 1 AS k; UPDATE t SET a = 1;"),
        T,
    );
    assert!(rules(&r).contains(&"stmt.moved"), "{:?}", rules(&r));
    let r = run(
        &tsql_proc("UPDATE t SET a = 1; SELECT FLOOR(RAND()) AS k; UPDATE u SET b = 2;"),
        &tsql_proc("UPDATE u SET b = 2; SELECT 0 AS k; UPDATE t SET a = 1;"),
        T,
    );
    assert!(rules(&r).contains(&"stmt.moved"), "{:?}", rules(&r));
}
