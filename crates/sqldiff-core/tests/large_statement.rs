//! One very long statement is compared in bounded time and memory. The
//! token alignment used a table of (tokens of A) x (tokens of B) cells, which
//! needed gigabytes for 20 000 tokens, and the witness search listed its
//! candidate values with a quadratic duplicate check, which made an IN list
//! of a few thousand numbers run for minutes.

use sqldiff_core::{diff, Artifact, Dialect, Options, Verdict};

fn run(a: String, b: String) -> Verdict {
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Tsql, b),
        &Options::default(),
    )
    .verdict()
}

/// `n` items, the last one changed in B.
fn pair(n: usize, stmt: impl Fn(&dyn Fn(usize) -> usize) -> String) -> (String, String) {
    (stmt(&|i| i), stmt(&|i| if i == n - 1 { i + 1 } else { i }))
}

#[test]
fn a_flat_list_of_ten_thousand_items_is_compared() {
    let n = 10_000;
    let (a, b) = pair(n, |g| {
        format!(
            "SELECT {};\n",
            (0..n)
                .map(|i| g(i).to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    });
    assert_eq!(run(a, b), Verdict::Different);
}

#[test]
fn an_in_list_of_thousands_of_numbers_is_compared() {
    let n = 2_500;
    let (a, b) = pair(n, |g| {
        format!(
            "SELECT 1 FROM dbo.t WHERE a IN ({});\n",
            (0..n)
                .map(|i| g(i).to_string())
                .collect::<Vec<_>>()
                .join(",")
        )
    });
    assert_eq!(run(a, b), Verdict::Different);
}

fn long_script() -> (String, String) {
    let n = 4_300;
    let base = |i: usize| format!("UPDATE dbo.t{} SET c = {} WHERE id = {};\n", i % 50, i, i);
    let stmt = |i: usize, b: bool| -> String {
        if i % 100 == 0 {
            "BEGIN TRAN;\n".into()
        } else if i % 100 == 99 {
            "COMMIT;\n".into()
        } else if b && (i == 3 || i == n - 3) {
            format!("DELETE FROM dbo.x WHERE id = {i};\n")
        } else if b && i == n / 2 {
            format!("UPDATE dbo.t9 SET c = 0 WHERE id = {};\n", i + 1_000_000)
        } else {
            base(i)
        }
    };
    (
        (0..n).map(|i| stmt(i, false)).collect(),
        (0..n).map(|i| stmt(i, true)).collect(),
    )
}

fn script_report() -> sqldiff_core::Report {
    let (a, b) = long_script();
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Tsql, b),
        &Options::default(),
    )
}

/// A script of thousands of statements with edits at both ends and
/// transactions in between is aligned exactly: only the real edits are
/// reported, and no COMMIT is said to have moved. (The alignment table of
/// such a script is past the size it is built in whole.)
#[test]
fn a_long_script_with_few_edits_reports_only_those() {
    let r = script_report();
    let count = |rule: &str| r.findings.iter().filter(|f| f.rule == rule).count();
    assert_eq!(count("stmt.moved"), 0, "nothing moved");
    assert_eq!(count("stmt.added"), 2);
    assert_eq!(count("stmt.removed"), 2);
}

/// Past the alignment bound the middle is not aligned, and nothing is
/// claimed about it: one Unknown finding, no moved, added or removed
/// statement made up from a partial alignment.
#[test]
fn past_the_alignment_bound_the_script_is_unknown_not_guessed() {
    let r = sqldiff_core::normalize::with_alignment_bound(1_000_000, script_report);
    let count = |rule: &str| r.findings.iter().filter(|f| f.rule == rule).count();
    assert_eq!(count("stmt.moved"), 0, "{:?}", r.findings);
    assert_eq!(count("stmt.added"), 0);
    assert_eq!(count("stmt.removed"), 0);
    assert_eq!(count("align.bound"), 1);
    assert_eq!(r.verdict(), Verdict::Unknown);
}
