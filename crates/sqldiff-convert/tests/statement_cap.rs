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

fn run(cap: Option<u64>) -> Conversion {
    convert_with(
        SCRIPT,
        Dialect::Tsql,
        Dialect::Postgres,
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
                .any(|r| r.contains("a routine or script segment is not compared"));
            if whole {
                assert!(blocked(&c), "cap {cap}: a skipped unit must block the file");
            }
        }
    }
    assert!(cut_seen, "no cap cut anything");
}
