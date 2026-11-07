//! The statement-level allowance (`Options::statement_work_cap`, perf plan R12
//! step B): past it the lists of a same-dialect diff that are not yet aligned
//! take the `align.bound` exit with a work-budget witness, whole gaps at a
//! time. A cut never turns a difference into an equal or a made-up
//! added/removed statement, and an allowance that is off, or large enough,
//! changes nothing.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};
use sqldiff_model::work::{scope, Site};

/// Two versions of a script of `n` statements with a few edits and a block
/// of different statements in the middle (a gap to score).
fn scripts(n: usize) -> (String, String) {
    let line = |i: usize, b: bool| {
        if b && i == 3 {
            "DELETE FROM dbo.x WHERE id = 3;\n".to_string()
        } else if b && (n / 2..n / 2 + 6).contains(&i) {
            format!("UPDATE dbo.m SET c = {} WHERE id = {i};\n", i + 7)
        } else if (n / 2..n / 2 + 6).contains(&i) {
            format!("UPDATE dbo.m SET c = {i} WHERE id = {i};\n")
        } else {
            format!("UPDATE dbo.t{} SET c = {i} WHERE id = {i};\n", i % 7)
        }
    };
    (
        (0..n).map(|i| line(i, false)).collect(),
        (0..n).map(|i| line(i, true)).collect(),
    )
}

fn run(cap: Option<u64>) -> (Report, u64) {
    let (a, b) = scripts(120);
    let (r, w) = scope("t", || {
        diff(
            &Artifact::new("a.sql", Dialect::Tsql, a),
            &Artifact::new("b.sql", Dialect::Tsql, b),
            &Options {
                statement_work_cap: cap,
                ..Options::default()
            },
        )
    });
    (r, w.get(Site::AlignCells) + w.get(Site::GapScores))
}

fn json(r: &Report) -> String {
    serde_json::to_string(r).unwrap()
}

fn cut(r: &Report) -> bool {
    r.findings
        .iter()
        .any(|f| f.rule == "align.bound" && f.witness.contains("statement-level work budget"))
}

#[test]
fn a_large_allowance_changes_nothing() {
    let (full, total) = run(None);
    assert!(total > 0);
    for cap in [total, total + 1, total * 3, u64::MAX] {
        let (r, _) = run(Some(cap));
        assert_eq!(json(&r), json(&full), "cap {cap} of {total}");
    }
}

#[test]
fn no_allowance_makes_a_difference_equal_or_invents_one() {
    let (full, total) = run(None);
    let count = |r: &Report, rule: &str| r.findings.iter().filter(|f| f.rule == rule).count();
    let mut cuts = 0;
    for cap in (0..=total).step_by(((total / 60).max(1)) as usize) {
        let (r, _) = run(Some(cap));
        let ctx = format!("cap {cap} of {total}");
        if json(&r) != json(&full) {
            assert!(
                cut(&r),
                "{ctx}: a changed report says why: {:?}",
                r.findings
            );
            cuts += 1;
        }
        // Never more added/removed statements than the full alignment found:
        // a stretch left unaligned is not reported as added or removed.
        assert!(
            count(&r, "stmt.added") <= count(&full, "stmt.added"),
            "{ctx}"
        );
        assert!(
            count(&r, "stmt.removed") <= count(&full, "stmt.removed"),
            "{ctx}"
        );
        // A report that had a proven difference is never Equal.
        if full.verdict() == Verdict::Different {
            assert_ne!(r.verdict(), Verdict::Equal, "{ctx}");
        }
        // Whatever is cut leaves a finding that is not Equal.
        if cut(&r) {
            assert_ne!(r.verdict(), Verdict::Equal, "{ctx}");
        }
    }
    assert!(cuts > 0, "the sweep never cut anything (total {total})");
}

#[test]
fn the_witness_names_the_budget_and_the_stretch() {
    let (r, _) = run(Some(0));
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == "align.bound")
        .expect("a cut");
    assert!(
        f.witness
            .contains("statement-level work budget (0 of 0 units of work)"),
        "{}",
        f.witness
    );
    let loc = f.a.as_ref().expect("located on side A");
    assert!(loc.end_line > loc.start_line + 50, "{loc:?}");
}
