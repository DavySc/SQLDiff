//! A T-SQL script of two or more procedural batches (two PostgreSQL DO
//! blocks) keeps the behaviour analysis it had when it was one routine pair:
//! the batch autocommits per statement, the DO block is atomic.

use sqldiff_convert::{convert, Status};
use sqldiff_core::Dialect;

const SCRIPT: &str = "CREATE TABLE dbo.t1 (id int NOT NULL PRIMARY KEY, v int NOT NULL);
GO
DECLARE @x int = 1;
IF @x = 1
BEGIN
    INSERT INTO dbo.t1 (id, v) VALUES (1, 1);
    UPDATE dbo.t1 SET v = 2 WHERE id = 1;
END
GO
DECLARE @y int = 2;
IF @y = 2
BEGIN
    BEGIN TRAN;
    INSERT INTO dbo.t1 (id, v) VALUES (2, 1);
    UPDATE dbo.t1 SET v = 3 WHERE id = 2;
    COMMIT;
END
GO
";

#[test]
fn two_procedural_batches_still_differ_on_durability() {
    let c = convert(SCRIPT, Dialect::Tsql, Dialect::Postgres).unwrap();
    let u = c
        .units
        .iter()
        .find(|u| u.src.line == 7)
        .unwrap_or_else(|| panic!("no unit at line 7: {:#?}", c.units));
    assert_eq!(u.status, Status::Differs, "{:#?}", u.notes);
    assert!(
        c.report.findings.iter().any(|f| f.rule == "txn.durability"),
        "{:#?}",
        c.report.findings
    );
}
