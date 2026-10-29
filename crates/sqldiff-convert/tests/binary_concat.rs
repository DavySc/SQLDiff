//! T-SQL `+` between binary strings concatenates them. PostgreSQL bytea and
//! Db2 for i binary strings have no `+`, so the output writes `||`, and the
//! engine proves the two equal.

use sqldiff_convert::{convert, Conversion, Status, Unit};
use sqldiff_core::Dialect;

fn conv(text: &str, to: Dialect) -> Conversion {
    convert(text, Dialect::Tsql, to).unwrap()
}

/// The unit whose input contains `needle`.
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

fn has_rule(u: &Unit, rule: &str) -> bool {
    u.notes.iter().any(|n| n.rule == rule)
}

const SRC: &str = "DECLARE @b varbinary(max);\nDECLARE @c varbinary(10);\nSET @b = 0x0102;\nSET @b += 0x0304;\nSET @b = @b + 0x05 + @c;\nSET @b = 1 + @b + 0x01;\n";

#[test]
fn compound_and_chained_binary_plus_become_concatenation_on_postgres() {
    let c = conv(SRC, Dialect::Postgres);
    let o = &c.output;
    assert!(o.contains("v_b := v_b || '\\x0304'::bytea;"), "{o}");
    assert!(o.contains("v_b := v_b || '\\x05'::bytea || v_c;"), "{o}");
    for needle in ["SET @b += 0x0304", "SET @b = @b + 0x05"] {
        let u = unit(&c, SRC, needle);
        assert_eq!(u.status, Status::Assumptions, "{needle}: {:?}", u.notes);
        assert!(has_rule(u, "ASM-CONCAT-NULL"), "{needle}");
    }
}

#[test]
fn plus_next_to_a_number_is_not_concatenation() {
    // `1 + @b` is int addition in T-SQL (int ranks above varbinary), so the
    // whole expression is not a binary chain: `+` stays and the unit is not
    // shown equal.
    let c = conv(SRC, Dialect::Postgres);
    assert!(
        c.output.contains("v_b := 1 + v_b + '\\x01'::bytea;"),
        "{}",
        c.output
    );
    let u = unit(&c, SRC, "SET @b = 1 + @b");
    assert!(u.status > Status::Assumptions, "{:?}", u.notes);
}

#[test]
fn appending_past_a_bounded_length_names_the_cut_off() {
    // T-SQL cuts a value longer than varbinary(2) off silently; bytea keeps
    // it. Equal only under the assumption that says so.
    let src = "DECLARE @c varbinary(2);\nSET @c = 0x01;\nSET @c += 0x0203;\n";
    let c = conv(src, Dialect::Postgres);
    assert!(
        c.output.contains("v_c := v_c || '\\x0203'::bytea;"),
        "{}",
        c.output
    );
    let u = unit(&c, src, "SET @c += 0x0203");
    assert!(u.status >= Status::Assumptions, "{:?}", u.notes);
    assert!(
        u.status > Status::Assumptions || has_rule(u, "ASM-VARIABLE-RANGE"),
        "{:?}",
        u.notes
    );
}

#[test]
fn db2_writes_concatenation_too() {
    let c = conv(SRC, Dialect::Db2i);
    let o = &c.output;
    assert!(o.contains("SET v_b = v_b || BX'0304';"), "{o}");
    assert!(o.contains("SET v_b = v_b || BX'05' || v_c;"), "{o}");
    assert!(o.contains("SET v_b = 1 + v_b + BX'01';"), "{o}");
}

#[test]
fn overflowing_a_bounded_output_parameter_is_not_proven() {
    // SQL Server 2022: with @x = 0x0A0B, `EXEC dbo.p1 @x OUTPUT` returns
    // 0x0A0B (the appended byte is cut off); PostgreSQL returns \x0a0b01.
    let src = "CREATE PROCEDURE dbo.p1 @c varbinary(2) OUTPUT AS BEGIN SET @c = @c + 0x01; END\n";
    let c = conv(src, Dialect::Postgres);
    assert!(
        c.output.contains("p_c := p_c || '\\x01'::bytea;"),
        "{}",
        c.output
    );
    let u = unit(&c, src, "SET @c = @c + 0x01");
    assert!(u.status > Status::Assumptions, "{:?}", u.notes);
}

#[test]
fn a_longer_binary_into_a_bounded_parameter_is_not_proven() {
    // SQL Server returns 0x0102 for @d = 0x010203; PostgreSQL all three bytes.
    let src = "CREATE PROCEDURE dbo.p2 @c varbinary(2) OUTPUT, @d varbinary(3) AS BEGIN SET @c = @d; END\n";
    let c = conv(src, Dialect::Postgres);
    let u = unit(&c, src, "SET @c = @d");
    assert!(u.status > Status::Assumptions, "{:?}", u.notes);
}
