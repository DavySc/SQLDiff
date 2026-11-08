//! The statement-level allowance (`Options::statement_work_cap`) through the
//! conversion check: a cut never moves a unit, or the file, toward converted.
//! A whole unit the allowance did not let the engine compare must stay
//! unproven, however the engine names what it skipped.

use sqldiff_convert::{convert_with, Conversion, Status};
use sqldiff_core::{Dialect, Options};

/// A script with a table, a routine that writes twice, and a second segment
/// with a table and a statement outside the routine.
const SCRIPT: &str = "CREATE TABLE dbo.t (id int NOT NULL, v int NULL);
GO
CREATE PROCEDURE dbo.p @x int AS
BEGIN
INSERT INTO dbo.t (id, v) VALUES (@x, 2);
UPDATE dbo.t SET v = 3 WHERE id = @x;
END;
GO
CREATE TABLE dbo.u (id int NOT NULL);
INSERT INTO dbo.u (id) VALUES (1);
CREATE INDEX ix_u ON dbo.u (id);
GO
";

/// Control flow, a DECLARE, a function, and a transaction inside a TRY.
const NESTED: &str = "CREATE TABLE dbo.t (id int NOT NULL, v int NULL);\nGO\nCREATE PROCEDURE dbo.p @x int, @y int OUTPUT AS\nBEGIN\nSET NOCOUNT ON;\nDECLARE @n int = 0;\nIF @x > 0\nBEGIN\n  INSERT INTO dbo.t (id, v) VALUES (@x, 2);\n  UPDATE dbo.t SET v = 3 WHERE id = @x;\nEND\nELSE\nBEGIN\n  DELETE FROM dbo.t WHERE id = @x;\nEND\nWHILE @n < 3\nBEGIN\n  SET @n = @n + 1;\n  UPDATE dbo.t SET v = v + 1 WHERE id = @n;\nEND\nSELECT id, v FROM dbo.t WHERE id = @x;\nSET @y = @n;\nEND;\nGO\n";
const TRYCATCH: &str = "CREATE TABLE dbo.t (id int NOT NULL, v int NULL);\nGO\nCREATE PROCEDURE dbo.q @x int AS\nBEGIN\nSET XACT_ABORT ON;\nBEGIN TRY\n  BEGIN TRANSACTION;\n  INSERT INTO dbo.t (id, v) VALUES (@x, 2);\n  EXEC dbo.p @x;\n  COMMIT;\nEND TRY\nBEGIN CATCH\n  IF @@TRANCOUNT > 0 ROLLBACK;\n  THROW;\nEND CATCH\nEND;\nGO\n";
const FUNC: &str = "CREATE FUNCTION dbo.f (@a int) RETURNS int AS\nBEGIN\n  DECLARE @r int;\n  IF @a > 10 SET @r = @a * 2; ELSE SET @r = @a;\n  RETURN @r;\nEND;\nGO\n";

/// The 1-based lines inside a `CREATE PROCEDURE` or `CREATE FUNCTION`, up to
/// the `GO` that ends its batch.
fn routine_lines(text: &str) -> std::collections::BTreeSet<usize> {
    let mut out = std::collections::BTreeSet::new();
    let mut inside = false;
    for (i, l) in text.lines().enumerate() {
        let up = l.trim().to_ascii_uppercase();
        if up.starts_with("CREATE PROCEDURE") || up.starts_with("CREATE FUNCTION") {
            inside = true;
        }
        if up == "GO" {
            inside = false;
        }
        if inside {
            out.insert(i + 1);
        }
    }
    out
}

fn run(cap: Option<u64>) -> Conversion {
    run_script(SCRIPT, Dialect::Postgres, cap)
}

fn run_script(text: &str, to: Dialect, cap: Option<u64>) -> Conversion {
    convert_with(
        text,
        Dialect::Tsql,
        to,
        &Options {
            statement_work_cap: cap,
            ..Options::default()
        },
    )
    .unwrap()
}

fn proven(s: Status) -> bool {
    matches!(s, Status::Converted | Status::Assumptions)
}

#[test]
fn no_cap_moves_a_unit_or_the_file_toward_converted() {
    let full = run(None);
    assert!(
        full.units.iter().any(|u| proven(u.status)),
        "the fixture needs a unit that is proven without a cap"
    );
    let mut cut_seen = false;
    for cap in (0..40u64).chain([60, 100, 200, 400, 1_000, 5_000]) {
        let c = run(Some(cap));
        assert_eq!(c.units.len(), full.units.len(), "cap {cap}");
        for (i, (u, f)) in c.units.iter().zip(&full.units).enumerate() {
            if !proven(f.status) {
                assert!(
                    !proven(u.status),
                    "cap {cap}: unit {i} (src line {}) was {:?}, is {:?}",
                    u.src.line,
                    f.status,
                    u.status
                );
            }
        }
        // A file the engine could not fully compare is not converted.
        let blocked = |c: &Conversion| !c.unproven.is_empty() || !c.unplaced.is_empty();
        if blocked(&full) {
            assert!(blocked(&c), "cap {cap}: the file lost its block");
        }
        let cut = c
            .report
            .unknown_reasons
            .iter()
            .any(|r| r.contains("statement-level work budget"));
        if cut {
            cut_seen = true;
            // A cut that left a whole unit uncompared blocks the file; a cut
            // inside a unit names the statements after it, which are then
            // not proven.
            let whole = c
                .report
                .unknown_reasons
                .iter()
                .any(|r| r.contains(" lines ") && r.contains("statement-level work budget"));
            if whole {
                assert!(blocked(&c), "cap {cap}: a skipped unit must block the file");
            }
        }
    }
    assert!(cut_seen, "no cap cut anything");
}

#[test]
fn a_skipped_routine_leaves_no_unit_in_it_proven_in_either_dialect() {
    for to in [Dialect::Postgres, Dialect::Db2i] {
        for (name, text) in [
            ("script", SCRIPT),
            ("nested", NESTED),
            ("try/catch", TRYCATCH),
            ("function", FUNC),
            ("mixed", MIXED),
        ] {
            let full = run_script(text, to, None);
            let inside = routine_lines(text);
            // The engine compares nothing at cap 0.
            let c = run_script(text, to, Some(0));
            assert!(
                !c.unproven.is_empty(),
                "{name}/{to:?}: a file nothing was compared in is blocked"
            );
            for (i, (u, f)) in c.units.iter().zip(&full.units).enumerate() {
                if !proven(f.status) {
                    assert!(
                        !proven(u.status),
                        "{name}/{to:?} unit {i} line {}",
                        u.src.line
                    );
                }
                if (u.src.line..=u.src.end_line).any(|l| inside.contains(&l)) {
                    assert!(
                        !proven(u.status),
                        "{name}/{to:?}: unit {i} (src line {}) inside a skipped routine is {:?}",
                        u.src.line,
                        u.status
                    );
                }
            }
            // A range reason (the whole unit) always blocks the file: this
            // does not depend on the text of any line-less reason.
            for cap in [0, 1, 5, 20, 100] {
                let c = run_script(text, to, Some(cap));
                if c.report
                    .unknown_reasons
                    .iter()
                    .any(|r| r.contains(" lines ") && r.contains("statement-level work budget"))
                {
                    assert!(!c.unproven.is_empty(), "{name}/{to:?} cap {cap}");
                }
            }
        }
    }
}

/// Top-level statements around routines, in the same batch and in others.
const MIXED: &str = "CREATE TABLE dbo.t (id int NOT NULL, v int NULL);\nINSERT INTO dbo.t (id, v) VALUES (1, 1);\nGO\nCREATE PROCEDURE dbo.a @x int AS\nBEGIN\n  IF @x > 0\n    UPDATE dbo.t SET v = 3 WHERE id = @x;\n  ELSE\n    DELETE FROM dbo.t WHERE id = @x;\nEND;\nGO\nUPDATE dbo.t SET v = 5 WHERE id = 1;\nDECLARE @k int = 2;\nWHILE @k > 0\nBEGIN\n  UPDATE dbo.t SET v = v + 1 WHERE id = @k;\n  SET @k = @k - 1;\nEND\nGO\nCREATE PROCEDURE dbo.b @x int AS\nBEGIN\n  BEGIN TRY\n    INSERT INTO dbo.t (id, v) VALUES (@x, 2);\n  END TRY\n  BEGIN CATCH\n    THROW;\n  END CATCH\nEND;\nGO\nCREATE TABLE dbo.w (id int NOT NULL);\nCREATE INDEX ix_w ON dbo.w (id);\nGO\n";

/// The `(first, last)` lines a budget reason names on one side of the
/// conversion (`out` = the converted text).
fn budget_lines(c: &Conversion, out: bool) -> Vec<(usize, usize)> {
    let tag = if out { "side B" } else { "side A" };
    c.report
        .unknown_reasons
        .iter()
        .filter(|r| r.starts_with(tag) && r.contains("statement-level work budget"))
        .filter_map(|r| {
            let rest = r.split_once(") ")?.1;
            if let Some(x) = rest.strip_prefix("lines ") {
                let (a, b) = x.split_once(':')?.0.split_once('-')?;
                Some((a.parse().ok()?, b.parse().ok()?))
            } else {
                let n: usize = rest
                    .strip_prefix("line ")?
                    .split_once(':')?
                    .0
                    .parse()
                    .ok()?;
                Some((n, n))
            }
        })
        .collect()
}

/// A cut only touches what it names: a unit that no budget reason reaches,
/// on either side, keeps the status it has without a cap. A range that
/// started at the file's first line or covered the routines between the
/// batches of a script would fail this.
#[test]
fn a_cut_leaves_the_units_it_does_not_name_as_they_were() {
    for to in [Dialect::Postgres, Dialect::Db2i] {
        let full = run_script(MIXED, to, None);
        assert!(
            full.units.iter().filter(|u| proven(u.status)).count() > 2,
            "{to:?}"
        );
        let mut partial = false;
        for cap in 0..400u64 {
            let c = run_script(MIXED, to, Some(cap));
            let (ra, rb) = (budget_lines(&c, false), budget_lines(&c, true));
            if ra.is_empty() && rb.is_empty() {
                break;
            }
            let reached = |u: &sqldiff_convert::Unit| {
                ra.iter()
                    .any(|&(f, l)| u.src.line <= l && f <= u.src.end_line)
                    || rb
                        .iter()
                        .any(|&(f, l)| u.out.line <= l && f <= u.out.end_line)
            };
            for (i, (u, f)) in c.units.iter().zip(&full.units).enumerate() {
                if !reached(u) {
                    assert_eq!(
                        u.status, f.status,
                        "{to:?} cap {cap}: unit {i} (src line {}) is not named by a cut but changed",
                        u.src.line
                    );
                } else if proven(f.status) && proven(u.status) {
                    panic!("{to:?} cap {cap}: unit {i} (src line {}) is named by a cut but still proven", u.src.line);
                }
            }
            partial |= c.units.iter().any(|u| !reached(u) && proven(u.status))
                && c.units.iter().any(|u| reached(u));
        }
        assert!(partial, "{to:?}: no cap cut only part of the file");
    }
}
