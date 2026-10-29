//! `statement_timestamp()::timestamp` (the PostgreSQL conversion of
//! SYSDATETIME()) equals SYSDATETIME() only in a conversion check, under
//! ASM-TIME-SOURCE and ASM-FRACTIONAL-SECONDS; a plain diff keeps them
//! apart.

use sqldiff_core::{diff, Artifact, Dialect, Options, Verdict};

#[test]
fn a_plain_diff_does_not_equate_the_clocks() {
    let a = "CREATE TABLE dbo.T (Id int NOT NULL PRIMARY KEY, W datetime2(7) NOT NULL DEFAULT (SYSDATETIME()));";
    let b = "CREATE TABLE T (Id integer NOT NULL PRIMARY KEY, W timestamp NOT NULL DEFAULT statement_timestamp()::timestamp);";
    let r = diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Postgres, b),
        &Options::default(),
    );
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.verdict()
    );
}
