//! SYSDATETIME() becomes `statement_timestamp()::timestamp` for PostgreSQL.
//! In a conversion check the engine reads that as the statement's start
//! time (ASM-TIME-SOURCE), its 6 fraction digits standing for T-SQL's 7
//! (ASM-FRACTIONAL-SECONDS); GETDATE() (1/300 s ticks) stays unproven.

use sqldiff_convert::{convert, Conversion, Status};
use sqldiff_core::Dialect;

fn run(ty: &str, f: &str) -> Conversion {
    let src = format!(
        "CREATE PROCEDURE dbo.P\nAS\nBEGIN\n    SET XACT_ABORT ON;\n    DECLARE @d {ty};\n    SET @d = {f}();\n    INSERT INTO dbo.T (c) VALUES (@d);\nEND\n"
    );
    convert(&src, Dialect::Tsql, Dialect::Postgres).unwrap()
}

fn rules(c: &Conversion) -> Vec<&str> {
    c.units
        .iter()
        .flat_map(|u| u.notes.iter().map(|n| n.rule.as_str()))
        .collect()
}

#[test]
fn sysdatetime_into_datetime2_7_is_proven() {
    let c = run("datetime2(7)", "SYSDATETIME");
    assert!(
        c.output
            .contains("v_d := statement_timestamp()::timestamp;"),
        "{}",
        c.output
    );
    assert!(
        c.units
            .iter()
            .all(|u| matches!(u.status, Status::Converted | Status::Assumptions)),
        "{:#?}\n{}",
        c.units,
        c.output
    );
    let r = rules(&c);
    assert!(
        r.contains(&"ASM-FRACTIONAL-SECONDS") && r.contains(&"ASM-TIME-SOURCE"),
        "{r:?}"
    );
}

#[test]
fn getdate_stays_unproven() {
    let c = run("datetime", "GETDATE");
    assert!(
        c.units.iter().any(|u| u.status == Status::Unproven),
        "{:#?}",
        c.units
    );
}

#[test]
fn a_sysdatetime_column_default_is_proven() {
    let src = "CREATE TABLE dbo.T (Id int NOT NULL PRIMARY KEY, LastEditedWhen datetime2(7) NOT NULL DEFAULT (SYSDATETIME()));\n";
    let c = convert(src, Dialect::Tsql, Dialect::Postgres).unwrap();
    assert!(
        c.output
            .contains("DEFAULT (statement_timestamp()::timestamp)")
            || c.output
                .contains("DEFAULT statement_timestamp()::timestamp"),
        "{}",
        c.output
    );
    assert!(
        c.units
            .iter()
            .all(|u| matches!(u.status, Status::Converted | Status::Assumptions)),
        "{:#?}\n{}",
        c.units,
        c.output
    );
}
