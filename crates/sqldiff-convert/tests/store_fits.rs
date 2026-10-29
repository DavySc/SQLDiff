//! A value stored in a variable, parameter or column of another type counts
//! as unchanged only where the engine knows both types. Each case below was
//! run on SQL Server 2022 and PostgreSQL 18 (outputs in the comments): the
//! store changes the value on one side only, so the unit must not be shown
//! equal.

use sqldiff_convert::{convert, Conversion, Status, Unit};
use sqldiff_core::Dialect;

fn conv(text: &str, to: Dialect) -> Conversion {
    convert(text, Dialect::Tsql, to).unwrap()
}

/// The smallest unit whose input contains `needle`.
fn unit<'a>(c: &'a Conversion, input: &str, needle: &str) -> &'a Unit {
    let at = input
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} not in input"));
    c.units
        .iter()
        .filter(|u| u.src.start <= at && at < u.src.end.max(u.src.start + 1))
        .min_by_key(|u| u.src.end - u.src.start)
        .unwrap_or_else(|| panic!("no unit for {needle}"))
}

fn not_proven(src: &str, needle: &str, to: Dialect) {
    let c = conv(src, to);
    let u = unit(&c, src, needle);
    assert!(
        u.status > Status::Assumptions,
        "{to:?}: {needle} is {:?}: {:?}\n{}",
        u.status,
        u.notes,
        c.output
    );
}

#[test]
fn a_longer_fixed_binary_is_cut_off_in_tsql() {
    // SQL Server: @c = 0x0102. PostgreSQL bytea: \x010203.
    let src =
        "CREATE PROCEDURE dbo.p @c binary(2) OUTPUT, @d binary(3) AS BEGIN SET @c = @d; END\n";
    for to in [Dialect::Postgres, Dialect::Db2i] {
        not_proven(src, "SET @c = @d", to);
    }
}

#[test]
fn a_shorter_fixed_binary_is_zero_padded_in_tsql() {
    // SQL Server: @c = 0x010200. PostgreSQL bytea keeps \x0102.
    let src =
        "CREATE PROCEDURE dbo.p @c binary(3) OUTPUT, @d binary(2) AS BEGIN SET @c = @d; END\n";
    for to in [Dialect::Postgres, Dialect::Db2i] {
        not_proven(src, "SET @c = @d", to);
    }
}

#[test]
fn image_into_fixed_binary_is_cut_off_or_padded() {
    // SQL Server: 0x01020304 → 0x010203, 0x01 → 0x010000.
    let src = "CREATE PROCEDURE dbo.p @c binary(3) OUTPUT, @d image AS BEGIN SET @c = @d; END\n";
    not_proven(src, "SET @c = @d", Dialect::Postgres);
}

#[test]
fn money_into_smallmoney_can_fail() {
    // SQL Server: error 237 for 1000000. PostgreSQL ignores the INOUT
    // parameter's numeric(10,4) and keeps 1000000.
    let src = "CREATE PROCEDURE dbo.p @c smallmoney OUTPUT, @d money AS BEGIN SET @c = @d; END\n";
    for to in [Dialect::Postgres, Dialect::Db2i] {
        not_proven(src, "SET @c = @d", to);
    }
}

#[test]
fn char_into_text_keeps_its_blanks_only_in_tsql() {
    // SQL Server: '[a  ]'. PostgreSQL drops a bpchar's trailing blanks when
    // it becomes text or varchar: '[a]'.
    let src =
        "CREATE PROCEDURE dbo.p @c char(3), @v varchar(max) OUTPUT AS BEGIN SET @v = @c; END\n";
    not_proven(src, "SET @v = @c", Dialect::Postgres);
    let src = "CREATE PROCEDURE dbo.p @c char(3), @v varchar(10) OUTPUT AS BEGIN SET @v = CAST(@c AS varchar(10)); END\n";
    not_proven(src, "SET @v = CAST", Dialect::Postgres);
}
