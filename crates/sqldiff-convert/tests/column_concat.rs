//! T-SQL `+` between string columns concatenates. The columns' types come
//! from the catalog, so the rewrite needs no variable or literal beside
//! them. A column whose type is unknown, or for which `||` gives another
//! value than `+`, keeps the `+`.

use sqldiff_convert::{convert, Status};
use sqldiff_core::Dialect;

const TABLE: &str = "create table \"item\" (\"id\" int not null, \"a\" varchar(50) not null, \"b\" varchar(50) not null, \"n\" int not null, \"m\" int not null, \"c\" char(10) null, \"nc\" nchar(10) null, \"d\" datetime null, \"dt\" date null, \"g\" uniqueidentifier null, \"bt\" bit null, \"x\" text null, \"vb\" varbinary(10) null, \"f\" decimal(10,2) null, \"nv\" nvarchar(20) null)\ngo\ncreate table \"other\" (\"a\" int null, \"z\" varchar(5) null)\ngo\n";

fn conv(select: &str, to: Dialect) -> (String, Status) {
    let src = format!("{TABLE}create view \"v\" as\n{select}\ngo\n");
    let c = convert(&src, Dialect::Tsql, to).unwrap();
    let st = c
        .units
        .iter()
        .filter(|u| u.src.start >= src.find("create view").unwrap())
        .map(|u| u.status)
        .max()
        .unwrap();
    (c.output, st)
}

fn pg(expr: &str) -> (String, Status) {
    conv(
        &format!("select {expr} as r from \"item\" i"),
        Dialect::Postgres,
    )
}

#[test]
fn varchar_columns_concatenate() {
    for (e, want) in [
        ("i.a + i.b", "i.a || i.b"),
        ("i.a + i.nv", "i.a || i.nv"),
        ("i.a + ' ' + i.b", "i.a || ' ' || i.b"),
        ("i.a + 'x'", "i.a || 'x'"),
        ("i.c + i.a", "i.c + i.a"), // CHAR on PostgreSQL: see below
    ] {
        let (o, _) = pg(e);
        assert!(o.contains(want), "{e}: {o}");
    }
}

#[test]
fn unqualified_names_concatenate() {
    let (o, _) = conv("select a + b as r from \"item\"", Dialect::Postgres);
    assert!(o.contains("a || b"), "{o}");
}

#[test]
fn numeric_columns_stay_addition() {
    for e in [
        "i.n + i.m",
        "i.a + i.n",
        "i.a + i.f",
        "i.a + i.bt",
        "i.n + i.a",
    ] {
        let (o, _) = pg(e);
        assert!(!o.contains("||"), "{e}: {o}");
    }
}

#[test]
fn columns_with_another_meaning_keep_plus_and_are_refused() {
    for e in [
        "i.a + i.d",
        "i.a + i.dt",
        "i.a + i.g",
        "i.a + i.vb",
        "i.x + i.a",
        "i.c + i.a",
        "i.nc + i.a",
        "'x' + i.d",
    ] {
        let (o, st) = pg(e);
        let code: String = o.lines().filter(|l| !l.starts_with("--")).collect();
        assert!(!code.contains("||"), "{e}: {o}");
        assert_eq!(st, Status::NotConverted, "{e}: {o}");
        assert!(!o.contains("ASM-PLUS-NUMERIC"), "{e}: {o}");
    }
}

#[test]
fn char_columns_keep_their_padding_on_db2() {
    let (o, _) = conv("select i.c + i.a as r from \"item\" i", Dialect::Db2i);
    assert!(o.contains("i.c || i.a"), "{o}");
}

#[test]
fn an_unresolved_column_next_to_a_string_keeps_plus() {
    // `a` is int in `other` and varchar in `item`: unknown here.
    let (o, _) = conv(
        "select (select a + z from \"other\") as r from \"item\" i",
        Dialect::Postgres,
    );
    assert!(!o.contains("a || z"), "{o}");
    let (o, _) = conv(
        "select i.a + o.z as r from \"item\" i join \"other\" o on 1=1",
        Dialect::Postgres,
    );
    assert!(o.contains("i.a || o.z"), "{o}");
}
