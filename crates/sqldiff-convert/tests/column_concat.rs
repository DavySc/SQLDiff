//! T-SQL `+` between string columns concatenates. The columns' types come
//! from the catalog, so the rewrite needs no variable or literal beside them.

use sqldiff_convert::convert;
use sqldiff_core::Dialect;

const TABLE: &str = "create table \"item\" (\"id\" int not null, \"a\" varchar(50) not null, \"b\" varchar(50) not null, \"n\" int not null, \"m\" int not null)\ngo\n";

fn out(view: &str) -> String {
    let src = format!("{TABLE}{view}");
    convert(&src, Dialect::Tsql, Dialect::Postgres)
        .unwrap()
        .output
}

#[test]
fn plus_between_varchar_columns_becomes_concatenation() {
    let o = out("create view \"v\" as\nselect i.a + i.b as ab from \"item\" i\ngo\n");
    assert!(o.contains("i.a || i.b"), "{o}");
}

#[test]
fn unqualified_and_chained_columns_concatenate() {
    let o = out("create view \"v\" as\nselect a + ' ' + b as ab from \"item\"\ngo\n");
    assert!(o.contains("a || ' ' || b"), "{o}");
}

#[test]
fn plus_between_integer_columns_stays_addition() {
    let o = out("create view \"v\" as\nselect i.n + i.m as s from \"item\" i\ngo\n");
    assert!(o.contains("i.n + i.m"), "{o}");
}

#[test]
fn a_string_column_next_to_a_number_column_is_not_concatenation() {
    // int ranks above varchar in T-SQL, so this is addition.
    let o = out("create view \"v\" as\nselect i.a + i.n as s from \"item\" i\ngo\n");
    assert!(o.contains("i.a + i.n"), "{o}");
}
