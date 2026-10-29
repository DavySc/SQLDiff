//! The observer profile's rows-affected counts (decision 1 in
//! docs/verification-roadmap.md).

use sqldiff_core::observer::{Observer, RowCounts};
use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;

const COUNTS: &str =
    "CREATE PROCEDURE dbo.touch @id int AS\nBEGIN\n  UPDATE dbo.t SET x = 1 WHERE id = @id;\nEND\n";
const NOCOUNT: &str = "CREATE PROCEDURE dbo.touch @id int AS\nBEGIN\n  SET NOCOUNT ON;\n  UPDATE dbo.t SET x = 1 WHERE id = @id;\nEND\n";
const PG: &str = "CREATE PROCEDURE touch(p_id int) LANGUAGE plpgsql AS $$\nBEGIN\n  UPDATE t SET x = 1 WHERE id = p_id;\nEND $$;\n";

fn run(a: (&str, Dialect), b: (&str, Dialect), row_counts: RowCounts) -> Report {
    let opts = Options {
        observer: Observer { row_counts },
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", a.1, a.0),
        &Artifact::new("b.sql", b.1, b.0),
        &opts,
    )
}

fn assumes(r: &Report) -> bool {
    r.assumptions
        .iter()
        .any(|x| x.id == "ASM-OBSERVER-ROWCOUNT")
}

#[test]
fn across_dialects_counts_are_an_assumption_by_default() {
    let r = run((COUNTS, T), (PG, P), RowCounts::SameDialect);
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:?}",
        r.findings
    );
    assert!(assumes(&r));
    // No counts to miss with SET NOCOUNT ON first.
    let r = run((NOCOUNT, T), (PG, P), RowCounts::SameDialect);
    assert!(!assumes(&r), "{:?}", r.assumptions);
    // Reversed sides too.
    let r = run((PG, P), (COUNTS, T), RowCounts::SameDialect);
    assert!(assumes(&r));
}

#[test]
fn an_observer_that_reads_counts_sees_them_differ() {
    let r = run((COUNTS, T), (PG, P), RowCounts::Always);
    assert_eq!(r.verdict(), Verdict::Unknown);
    let r = run((COUNTS, T), (NOCOUNT, T), RowCounts::Always);
    assert_eq!(r.verdict(), Verdict::Different);
}

#[test]
fn within_one_dialect_set_nocount_is_observed_unless_counts_are_not() {
    let r = run((COUNTS, T), (NOCOUNT, T), RowCounts::SameDialect);
    assert_eq!(r.verdict(), Verdict::Different);
    let r = run((COUNTS, T), (NOCOUNT, T), RowCounts::Never);
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:?}",
        r.findings
    );
    assert!(assumes(&r));
    // Only the setting is set aside; another change still counts.
    let changed = NOCOUNT.replace("x = 1", "x = 2");
    let r = run((COUNTS, T), (&changed, T), RowCounts::Never);
    assert_ne!(r.verdict(), Verdict::Equal);
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
}

#[test]
fn set_nocount_is_kept_where_the_code_reads_rowcount() {
    // SET resets @@ROWCOUNT to 0: an added SET NOCOUNT ON before
    // `IF @@ROWCOUNT = 0` changes the branch, whatever the observer reads.
    let a = "CREATE PROCEDURE dbo.up @id int AS\nBEGIN\n  UPDATE dbo.t SET x = 1 WHERE id = @id;\n  IF @@ROWCOUNT = 0 INSERT INTO dbo.t (id, x) VALUES (@id, 1);\nEND\n";
    let b = "CREATE PROCEDURE dbo.up @id int AS\nBEGIN\n  UPDATE dbo.t SET x = 1 WHERE id = @id;\n  SET NOCOUNT ON;\n  IF @@ROWCOUNT = 0 INSERT INTO dbo.t (id, x) VALUES (@id, 1);\nEND\n";
    let r = run((a, T), (b, T), RowCounts::Never);
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.verdict()
    );
}
