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
    // Everything the allowance is charged with: the tree diff and the edit
    // checker's metered work.
    (
        r,
        w.get(Site::AlignCells)
            + w.get(Site::GapScores)
            + w.get(Site::EditReach)
            + w.get(Site::EditClosure)
            + w.get(Site::WitnessTries),
    )
}

fn json(r: &Report) -> String {
    serde_json::to_string(r).unwrap()
}

fn cut(r: &Report) -> bool {
    r.findings.iter().any(|f| {
        (f.rule == "align.bound" && f.witness.contains("statement-level work budget"))
            || f.witness
                .contains("not examined: past the statement-level work budget")
            || f.witness
                .contains("witness search stopped at the statement-level work budget")
    })
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

/// A procedure whose second version changes the comparison of several
/// selects among padding statements: the edit checker proves each of them
/// different (`equiv.edit`) when it gets to run.
fn routine(b: bool) -> String {
    let mut s = String::from("CREATE PROCEDURE dbo.cheap @max decimal(12,2) AS\nBEGIN\n");
    for i in 0..24 {
        if i % 6 == 2 {
            let op = if b { "<=" } else { "<" };
            s += &format!(
                "SELECT item_id FROM dbo.item WHERE price {op} @max + {i} ORDER BY item_id;\n"
            );
        } else {
            s += &format!("PRINT {i};\n");
        }
    }
    s += "END\n";
    s
}

const DDL: &str = "CREATE TABLE dbo.item (item_id int NOT NULL PRIMARY KEY, name nvarchar(50) NOT NULL, price decimal(12,2) NOT NULL);\n";

fn run_routine(cap: Option<u64>) -> (Report, u64) {
    let (r, w) = scope("t", || {
        diff(
            &Artifact::new("a.sql", Dialect::Tsql, routine(false)),
            &Artifact::new("b.sql", Dialect::Tsql, routine(true)),
            &Options {
                statement_work_cap: cap,
                catalog_a: vec![Artifact::new("catalog.sql", Dialect::Tsql, DDL)],
                catalog_b: vec![Artifact::new("catalog.sql", Dialect::Tsql, DDL)],
                ..Options::default()
            },
        )
    });
    (
        r,
        w.get(Site::AlignCells)
            + w.get(Site::GapScores)
            + w.get(Site::EditReach)
            + w.get(Site::EditClosure)
            + w.get(Site::WitnessTries),
    )
}

fn edit_cut(r: &Report) -> bool {
    r.findings.iter().any(|f| {
        f.witness
            .contains("not examined: past the statement-level work budget")
    })
}

#[test]
fn the_edit_checker_stops_at_the_allowance_and_says_so() {
    let (full, total) = run_routine(None);
    let edits = |r: &Report| r.findings.iter().filter(|f| f.rule == "equiv.edit").count();
    assert!(
        edits(&full) > 0,
        "the fixture needs a proven edit: {:?}",
        full.findings
    );
    assert_eq!(json(&run_routine(Some(total + 1)).0), json(&full));
    let mut cut_seen = false;
    let step = ((total / 80).max(1)) as usize;
    for cap in (0..=total + 1).step_by(step) {
        let (r, _) = run_routine(Some(cap));
        if json(&r) == json(&full) {
            continue;
        }
        // A changed report loses proofs only through a visible cut, and no
        // cut makes a difference equal.
        assert!(
            edit_cut(&r) || cut(&r),
            "cap {cap} of {total}: a changed report says why: {:?}",
            r.findings
        );
        assert!(edits(&r) <= edits(&full), "cap {cap}");
        assert_ne!(r.verdict(), Verdict::Equal, "cap {cap}");
        cut_seen |= edit_cut(&r);
    }
    assert!(cut_seen, "no allowance cut the edit checker");
}

/// Two versions of one select that no input tells apart, but whose
/// equivalence the engine does not prove: the witness search runs through
/// its candidates and finds none.
fn lookalike(b: bool) -> String {
    let cond = if b {
        "price * 1 + 0 < @max"
    } else {
        "price < @max"
    };
    format!(
        "CREATE PROCEDURE dbo.cheap @max decimal(12,2) AS\nBEGIN\nSELECT item_id FROM dbo.item WHERE {cond} ORDER BY item_id;\nEND\n"
    )
}

fn run_lookalike(cap: Option<u64>) -> (Report, u64) {
    let (r, w) = scope("t", || {
        diff(
            &Artifact::new("a.sql", Dialect::Tsql, lookalike(false)),
            &Artifact::new("b.sql", Dialect::Tsql, lookalike(true)),
            &Options {
                statement_work_cap: cap,
                catalog_a: vec![Artifact::new("catalog.sql", Dialect::Tsql, DDL)],
                catalog_b: vec![Artifact::new("catalog.sql", Dialect::Tsql, DDL)],
                ..Options::default()
            },
        )
    });
    (r, w.get(Site::WitnessTries))
}

fn names_budget(r: &Report) -> bool {
    r.findings
        .iter()
        .any(|f| f.witness.contains("statement-level work budget"))
}

#[test]
fn a_witness_search_cut_by_the_allowance_does_not_read_as_no_witness() {
    let (full, tries) = run_lookalike(None);
    assert!(tries > 1_000, "the fixture needs a long search: {tries}");
    assert!(!names_budget(&full));
    let mut stopped = 0;
    for cap in (0..tries + 200).step_by(41) {
        let (r, _) = run_lookalike(Some(cap));
        let witnesses: Vec<&str> = r.findings.iter().map(|f| f.witness.as_str()).collect();
        // Cut inside the search: the witness says so, and says "stopped".
        if witnesses
            .iter()
            .any(|w| w.contains("witness search stopped at the statement-level work budget"))
        {
            stopped += 1;
        }
        // Cut anywhere: a changed report names the budget, and a cut
        // candidate is still Unknown, never Equal.
        if json(&r) != json(&full) {
            assert!(names_budget(&r), "cap {cap}: {witnesses:?}");
        }
        assert_ne!(r.verdict(), Verdict::Equal, "cap {cap}");
    }
    assert!(stopped > 5, "only {stopped} caps stopped the search");
    let (r, _) = run_lookalike(Some(tries * 4));
    assert_eq!(json(&r), json(&full), "a large allowance");
}
