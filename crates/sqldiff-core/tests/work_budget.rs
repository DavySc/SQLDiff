//! The behaviour model spends a counted amount of work per routine pair and
//! per file. When it runs out, the pairs not compared are reported as
//! "not proven" (`equiv.budget`) on the routine's statements, never as
//! equal; a normal routine never meets the budget.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const ROUTINE: &str = "CREATE PROCEDURE dbo.adjust @Item int, @Delta int AS
BEGIN
  SET XACT_ABORT ON;
  BEGIN TRAN;
  UPDATE dbo.stock SET qty = qty + @Delta WHERE item_id = @Item;
  IF @@ROWCOUNT = 0
    INSERT INTO dbo.stock (item_id, qty) VALUES (@Item, @Delta);
  INSERT INTO dbo.stock_log (item_id, delta) VALUES (@Item, @Delta);
  COMMIT;
END
GO
";

fn run(budgets: Option<(u64, u64)>) -> Report {
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, ROUTINE.to_string()),
        &Artifact::new("b.sql", Dialect::Tsql, ROUTINE.to_string()),
        &Options {
            work_budgets: budgets,
            ..Options::default()
        },
    )
}

fn budget_findings(r: &Report) -> usize {
    r.findings
        .iter()
        .filter(|f| f.rule == "equiv.budget" && f.verdict == Verdict::Unknown)
        .count()
}

#[test]
fn a_routine_within_budget_has_no_budget_finding() {
    assert_eq!(budget_findings(&run(None)), 0);
}

#[test]
fn a_routine_over_its_routine_budget_is_not_proven() {
    let r = run(Some((2, u64::MAX)));
    assert!(budget_findings(&r) > 0, "{:#?}", r.findings);
    assert!(witnesses(&r)
        .iter()
        .all(|w| w.contains("routine work budget")));
}

#[test]
fn a_routine_over_the_file_budget_is_not_proven() {
    let r = run(Some((u64::MAX, 2)));
    assert!(budget_findings(&r) > 0, "{:#?}", r.findings);
    assert!(witnesses(&r).iter().all(|w| w.contains("file work budget")));
}

/// The witnesses of the budget findings: they name the cap that was hit.
fn witnesses(r: &Report) -> Vec<&str> {
    r.findings
        .iter()
        .filter(|f| f.rule == "equiv.budget")
        .map(|f| f.witness.as_str())
        .collect()
}

#[test]
fn the_same_input_stops_at_the_same_place() {
    let a = run(Some((3, u64::MAX)));
    let b = run(Some((3, u64::MAX)));
    let key = |r: &Report| {
        r.findings
            .iter()
            .map(|f| format!("{}|{:?}|{}", f.rule, f.verdict, f.fingerprint))
            .collect::<Vec<_>>()
    };
    assert_eq!(key(&a), key(&b));
}
