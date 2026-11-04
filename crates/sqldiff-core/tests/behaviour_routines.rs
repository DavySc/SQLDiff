//! The behaviour model analyses every routine of a file, not the first only
//! (design review of incremental large conversions, finding 0).

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const FIRST: &str = "CREATE PROCEDURE dbo.first_one @Item int AS
BEGIN
  SET XACT_ABORT ON;
  BEGIN TRAN;
  UPDATE dbo.stock SET qty = qty + 1 WHERE item_id = @Item;
  INSERT INTO dbo.stock_log (item_id, delta) VALUES (@Item, 1);
  COMMIT;
END
GO
";

const SECOND: &str = "CREATE PROCEDURE dbo.adjust_stock @Item int, @Delta int AS
BEGIN
  SET XACT_ABORT ON;
  BEGIN TRAN;
  UPDATE dbo.stock SET qty = qty + @Delta WHERE item_id = @Item;
  INSERT INTO dbo.stock_log (item_id, delta) VALUES (@Item, @Delta);
  COMMIT;
END
GO
";

/// `SECOND` with the commit moved before the last write.
const SECOND_MUTATED: &str = "CREATE PROCEDURE dbo.adjust_stock @Item int, @Delta int AS
BEGIN
  SET XACT_ABORT ON;
  BEGIN TRAN;
  UPDATE dbo.stock SET qty = qty + @Delta WHERE item_id = @Item;
  COMMIT;
  INSERT INTO dbo.stock_log (item_id, delta) VALUES (@Item, @Delta);
END
GO
";

fn run(a: &str, b: &str) -> Report {
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, a.to_string()),
        &Artifact::new("b.sql", Dialect::Tsql, b.to_string()),
        &Options::default(),
    )
}

fn behavioural(r: &Report) -> Vec<&sqldiff_core::Finding> {
    r.findings
        .iter()
        .filter(|f| f.rule.starts_with("txn.") || f.rule.starts_with("error."))
        .collect()
}

#[test]
fn a_difference_in_the_second_routine_is_found() {
    let alone = run(SECOND, SECOND_MUTATED);
    assert!(
        behavioural(&alone)
            .iter()
            .any(|f| f.rule == "txn.atomicity" && f.verdict == Verdict::Different),
        "{:#?}",
        alone.findings
    );
    let a = format!("{FIRST}{SECOND}");
    let b = format!("{FIRST}{SECOND_MUTATED}");
    let r = run(&a, &b);
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == "txn.atomicity" && f.verdict == Verdict::Different)
        .unwrap_or_else(|| panic!("no txn.atomicity in the second routine: {:#?}", r.findings));
    // It points into the second routine, not the first.
    let line = f.a.as_ref().expect("located").start_line;
    assert!(line > FIRST.lines().count(), "line {line}");
}

#[test]
fn the_first_routine_keeps_its_findings() {
    let alone = run(FIRST, FIRST);
    let both = run(&format!("{FIRST}{SECOND}"), &format!("{FIRST}{SECOND}"));
    let key = |r: &Report| {
        let mut v: Vec<(String, Verdict)> = behavioural(r)
            .iter()
            .map(|f| (f.rule.clone(), f.verdict))
            .collect();
        v.sort_by(|x, y| x.0.cmp(&y.0));
        v
    };
    // Equal files: nothing different or unknown in either run.
    assert!(
        !key(&alone)
            .iter()
            .any(|(_, v)| matches!(v, Verdict::Different | Verdict::Unknown)),
        "{:#?}",
        alone.findings
    );
    assert!(
        !key(&both)
            .iter()
            .any(|(_, v)| matches!(v, Verdict::Different | Verdict::Unknown)),
        "{:#?}",
        both.findings
    );
}

#[test]
fn routines_meet_by_name_when_their_order_differs() {
    // The second routine of A is the first of B, and it differs.
    let a = format!("{FIRST}{SECOND}");
    let b = format!("{SECOND_MUTATED}{FIRST}");
    let r = run(&a, &b);
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == "txn.atomicity" && f.verdict == Verdict::Different)
        .unwrap_or_else(|| panic!("difference not found: {:#?}", r.findings));
    // It is the mutated routine, not the first one of A.
    assert!(f.a.as_ref().unwrap().start_line > FIRST.lines().count());
    // Reordered and equal: no behavioural difference (the statement layer
    // reports the move).
    let same = run(&a, &format!("{SECOND}{FIRST}"));
    assert!(
        !behavioural(&same)
            .iter()
            .any(|f| matches!(f.verdict, Verdict::Different | Verdict::Unknown)),
        "{:#?}",
        same.findings
    );
}
