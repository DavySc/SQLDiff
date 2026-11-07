//! Perf plan W6c, E2 downgrade: across dialects, the behavioural explorer
//! pairs the paths of the two sides by condition text only where the
//! identity analysis proved the text the same fact about the input.
use sqldiff_core::{Artifact, Dialect, Options};

/// Rules of the findings and the whole report as text, for a pair of
/// artifacts of the given dialects.
fn report(a: &str, b: &str, da: Dialect, db: Dialect) -> (Vec<String>, String) {
    let (a, b) = (a.to_string(), b.to_string());
    sqldiff_core::with_engine_stack(move || {
        let aa = Artifact::new("a.sql", da, a);
        let bb = Artifact::new("b.sql", db, b);
        let r = sqldiff_core::diff(&aa, &bb, &Options::default());
        (
            r.findings.iter().map(|f| f.rule.clone()).collect(),
            format!("{r:?}"),
        )
    })
}

fn tsql(body: &str) -> String {
    format!("CREATE PROCEDURE dbo.p @x int\nAS\nBEGIN\n{body}\nEND\n")
}

fn pg(decl: &str, body: &str) -> String {
    format!("CREATE PROCEDURE public.p(p_x integer)\nLANGUAGE plpgsql AS $$\n{decl}\nBEGIN\n{body}\nEND $$;\n")
}

fn rollback_case(cond_t: &str, cond_p: &str) -> (String, String) {
    (
        tsql(&format!(
            "SET XACT_ABORT ON;\nIF {cond_t}\nBEGIN\nBEGIN TRAN;\nUPDATE t SET c = 1;\nROLLBACK;\nEND\nELSE\nUPDATE t SET c = 2;"
        )),
        pg("", &format!(
            "IF {cond_p} THEN\nUPDATE t SET c = 1;\nROLLBACK;\nELSE\nUPDATE t SET c = 2;\nEND IF;"
        )),
    )
}

const PARAM: (&str, &str) = ("@x > 5", "p_x > 5");
const EXISTS: (&str, &str) = (
    "EXISTS (SELECT 1 FROM s WHERE k = @x)",
    "EXISTS (SELECT 1 FROM s WHERE k = p_x)",
);

fn crossing(cond: (&str, &str)) -> (Vec<String>, String) {
    let (a, b) = rollback_case(cond.0, cond.1);
    report(&a, &b, Dialect::Tsql, Dialect::Postgres)
}

/// An identified condition (parameters only) keeps pairing the paths by its
/// text: the routine's paths agree two by two and nothing crosses.
#[test]
fn an_identified_condition_still_pairs_paths_by_text() {
    let (rules, _) = crossing(PARAM);
    assert!(
        !rules.iter().any(|r| r.starts_with("txn.")),
        "an identified condition must pair true with true: {rules:?}"
    );
}

/// A condition on table state is not identified (the sides' tables may
/// differ at that point), so its text no longer pairs the paths: the
/// combinations that cross (A takes the branch, B does not) are compared too
/// and their difference is reported, not hidden by the pairing.
#[test]
fn an_unidentified_condition_compares_all_path_combinations() {
    let (rules, _) = crossing(EXISTS);
    assert!(
        rules.iter().any(|r| r == "txn.caller-transaction"),
        "the crossing paths must be compared: {rules:?}"
    );
}

/// The marker that keeps the sides' texts apart is an implementation detail:
/// no finding, witness or reason may show it.
#[test]
fn the_unidentified_marker_never_reaches_the_report() {
    let (_, text) = crossing(EXISTS);
    assert!(
        !text.contains('\u{1}') && !text.contains("\\u{1}"),
        "marker leaked"
    );
}

/// Without the identity analysis (same dialect) the text still pairs the
/// paths, as before: this PR changes the cross-dialect comparison only.
#[test]
fn the_same_dialect_comparison_is_unchanged() {
    let (a, _) = rollback_case(EXISTS.0, EXISTS.1);
    let (rules, _) = report(&a, &a, Dialect::Tsql, Dialect::Tsql);
    assert!(!rules.iter().any(|r| r.starts_with("txn.")), "{rules:?}");
}
