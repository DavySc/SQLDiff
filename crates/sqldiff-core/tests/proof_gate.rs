//! The proof gate (decision 3): which reports reach the checker, and what
//! `report` and `require` do with its answer. The checker here mirrors
//! `check` in formal/lean/Sqldiff/Gate.lean.

use std::sync::Arc;

use sqldiff_core::proof::{Answer, FnChecker, Mode, Request, Settings, Status};
use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

fn lean_check(r: &Request) -> Answer {
    if (&r.dialect_a, &r.target_a, &r.tree_a) == (&r.dialect_b, &r.target_b, &r.tree_b) {
        Answer::Confirmed {
            theorem: "Sqldiff.Gate.check_sound".into(),
        }
    } else {
        Answer::Unconfirmed {
            reason: "outside the verified fragment".into(),
        }
    }
}

fn never_called(_: &Request) -> Answer {
    panic!("the checker must not be called")
}

fn run(
    a: (&str, Dialect),
    b: (&str, Dialect),
    mode: Mode,
    checker: Option<fn(&Request) -> Answer>,
) -> Report {
    let opts = Options {
        proof: Settings {
            mode,
            checker: checker.map(|f| Arc::new(FnChecker(f)) as _),
        },
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", a.1, a.0),
        &Artifact::new("b.sql", b.1, b.0),
        &opts,
    )
}

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;
const PROC: &str =
    "CREATE PROCEDURE dbo.p @id int AS\nBEGIN\n  UPDATE dbo.t SET x = 1 WHERE id = @id;\nEND\n";
const PROC_REFORMATTED: &str =
    "CREATE PROCEDURE dbo.p @id int AS\nBEGIN\n    -- set x\n    UPDATE dbo.t\n       SET x = 1\n     WHERE id = @id;\nEND\n";

#[test]
fn off_leaves_no_trace() {
    let r = run(
        (PROC, T),
        (PROC_REFORMATTED, T),
        Mode::Off,
        Some(never_called),
    );
    assert_eq!(r.verdict(), Verdict::Equal);
    assert!(r.proof.is_none());
}

#[test]
fn a_reformatted_routine_is_confirmed() {
    for mode in [Mode::Report, Mode::Require] {
        let r = run((PROC, T), (PROC_REFORMATTED, T), mode, Some(lean_check));
        assert_eq!(r.verdict(), Verdict::Equal, "{mode:?}");
        let p = r.proof.as_ref().unwrap();
        assert_eq!(p.status, Status::Confirmed);
        assert_eq!(p.theorem.as_deref(), Some("Sqldiff.Gate.check_sound"));
        assert!(r.verdict_before_proof.is_none());
    }
}

#[test]
fn keyword_case_is_not_in_the_phase_0_fragment() {
    // Rust calls this equal; the trivial fragment compares token text exactly.
    let lower = PROC.replace("UPDATE", "update");
    let r = run((PROC, T), (&lower, T), Mode::Report, Some(lean_check));
    assert_eq!(r.verdict(), Verdict::Equal);
    assert_eq!(r.proof.unwrap().status, Status::Unconfirmed);
    let r = run((PROC, T), (&lower, T), Mode::Require, Some(lean_check));
    assert_eq!(r.verdict(), Verdict::Unknown);
    assert_eq!(r.verdict_before_proof, Some(Verdict::Equal));
    assert!(r
        .unknown_reasons
        .iter()
        .any(|x| x.starts_with("not confirmed by the proof checker")));
}

#[test]
fn require_without_a_checker_is_unknown() {
    let r = run((PROC, T), (PROC_REFORMATTED, T), Mode::Require, None);
    assert_eq!(r.verdict(), Verdict::Unknown);
    let p = r.proof.unwrap();
    assert_eq!(p.status, Status::Unconfirmed);
    assert!(p.reason.unwrap().contains("sqldiff-check"));
    // In report mode the verdict stands.
    let r = run((PROC, T), (PROC_REFORMATTED, T), Mode::Report, None);
    assert_eq!(r.verdict(), Verdict::Equal);
}

#[test]
fn cross_dialect_equals_stay_unconfirmed_without_asking() {
    let pg = "CREATE PROCEDURE p(p_id int) LANGUAGE plpgsql AS $$\nBEGIN\n  UPDATE t SET x = 1 WHERE id = p_id;\nEND $$;\n";
    let r = run((PROC, T), (pg, P), Mode::Require, Some(never_called));
    assert_ne!(r.verdict(), Verdict::Equal);
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
    if let Some(p) = r.proof {
        assert_ne!(p.status, Status::Confirmed);
    }
}

#[test]
fn other_verdicts_are_not_sent() {
    let changed = PROC.replace("x = 1", "x = 2");
    let r = run((PROC, T), (&changed, T), Mode::Require, Some(never_called));
    assert_ne!(r.verdict(), Verdict::Equal);
    assert_eq!(r.proof.unwrap().status, Status::NotApplicable);
}

#[test]
fn require_survives_a_later_settle() {
    // Later passes derive the verdict again; the unknown reason keeps it.
    let lower = PROC.replace("UPDATE", "update");
    let mut r = run((PROC, T), (&lower, T), Mode::Require, Some(lean_check));
    r.settle();
    assert_eq!(r.verdict(), Verdict::Unknown);
}
