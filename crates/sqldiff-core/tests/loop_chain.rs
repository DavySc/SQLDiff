//! A reverse dependency chain (`SET @w{k} = @w{k+1} + c;`) with the last tenth
//! of the statements edited: the closure behind the edit checker's
//! interaction test needs one link per statement against unit order. Inside a
//! loop every edit takes the per-edit scan; with full passes over every unit
//! per link that was cubic (4,000 statements: 210 s inside one WHILE against
//! 6.8 s outside it). This pins that the loop and the plain body take about
//! the same work, and that a loop body of a few thousand statements finishes.
//! The gate is a work count (closure wake-ups), not time.

use sqldiff_core::equiv::edit::loop_answer_counts;
use sqldiff_core::{diff, Artifact, Dialect, Options};

fn body(n: usize, edit_every: usize, b: bool, in_loop: bool) -> String {
    let mut decl = String::from("DECLARE @p2 int = 0");
    for i in 0..=n {
        decl.push_str(&format!(";\nDECLARE @w{i} int = 0"));
    }
    decl.push(';');
    let mut s = String::new();
    for k in 0..n {
        let c = if b && k + edit_every >= n { 7 } else { 3 };
        s.push_str(&format!("SET @w{k} = @w{} + {c};\n", k + 1));
    }
    let inner = if in_loop {
        format!("WHILE @p2 < 1000 BEGIN\n{s}SET @p2 = @p2 + 1;\nEND\n")
    } else {
        s
    };
    format!("CREATE PROCEDURE dbo.r @p int AS\nBEGIN\n{decl}\n{inner}SELECT @w0;\nEND\n")
}

fn run(n: usize, in_loop: bool) -> (usize, usize, usize) {
    let (a, b) = (
        body(n, n / 10, false, in_loop),
        body(n, n / 10, true, in_loop),
    );
    let before = loop_answer_counts();
    let r = diff(
        &Artifact::new("a.sql", Dialect::Tsql, a.as_str()),
        &Artifact::new("b.sql", Dialect::Tsql, b.as_str()),
        &Options::default(),
    );
    let after = loop_answer_counts();
    (r.findings.len(), after.0 - before.0, after.1 - before.1)
}

#[test]
fn a_chain_inside_a_loop_costs_what_it_costs_outside() {
    let n = 1500;
    let t = std::time::Instant::now();
    let (_, plain_answers, _) = run(n, false);
    assert_eq!(
        plain_answers, 0,
        "no edit of the plain body is inside a loop"
    );
    let (findings, answers, visits) = run(n, true);
    assert!(findings > 0);
    assert!(
        answers >= n / 10,
        "the loop edits take the per-edit scan ({answers})"
    );
    // Each edit's closure walks the chain once: its reader lists, a few units
    // per link. Full passes would be on the order of answers x n x n / 2.
    let bound = answers * n * 8;
    assert!(
        visits <= bound,
        "closure wake-ups {visits} exceed {bound} for {answers} edits of {n} statements"
    );
    // Guard against a hang only (a wall-clock limit is never part of an
    // answer): the passes took minutes here.
    assert!(t.elapsed().as_secs() < 300, "took {:?}", t.elapsed());
}
