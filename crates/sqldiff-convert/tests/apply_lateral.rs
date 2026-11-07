//! CROSS APPLY and OUTER APPLY become PostgreSQL's lateral joins
//! (`tsql.join.apply`): `CROSS JOIN LATERAL` for the first, `LEFT JOIN LATERAL
//! … ON TRUE` for the second, in the places a join stands.

use sqldiff_convert::{convert, Conversion};
use sqldiff_core::Dialect;

fn pg(text: &str) -> Conversion {
    convert(text, Dialect::Tsql, Dialect::Postgres).unwrap()
}

fn db2(text: &str) -> Conversion {
    convert(text, Dialect::Tsql, Dialect::Db2i).unwrap()
}

const TABLES: &str = "CREATE TABLE dbo.P(Id int NOT NULL PRIMARY KEY);\nGO\nCREATE TABLE dbo.C(Id int NOT NULL PRIMARY KEY, PId int NOT NULL, At datetime2 NOT NULL);\nGO\n";

fn view(body: &str) -> String {
    format!("{TABLES}CREATE VIEW dbo.V AS\n{body}\nGO\n")
}

#[test]
fn cross_apply_is_a_cross_join_lateral() {
    let src = view("SELECT p.Id, l.At FROM dbo.P p\nCROSS APPLY (SELECT TOP 1 c.At AS At FROM dbo.C c WHERE c.PId = p.Id ORDER BY c.At DESC) l");
    let o = pg(&src).output;
    assert!(
        o.contains("CROSS JOIN LATERAL (SELECT c.At AS At FROM C c WHERE c.PId = p.Id ORDER BY c.At DESC NULLS LAST LIMIT 1) l"),
        "{o}"
    );
    assert!(!o.to_ascii_uppercase().contains("APPLY"), "{o}");
}

#[test]
fn outer_apply_is_a_left_join_lateral_on_true() {
    let src = view("SELECT p.Id, l.At FROM dbo.P p\nOUTER APPLY (SELECT TOP 1 c.At AS At FROM dbo.C c WHERE c.PId = p.Id ORDER BY c.At DESC) l\nWHERE p.Id > 0");
    let o = pg(&src).output;
    assert!(
        o.contains("LEFT JOIN LATERAL (SELECT c.At AS At FROM C c WHERE c.PId = p.Id ORDER BY c.At DESC NULLS LAST LIMIT 1) l ON TRUE\nWHERE p.Id > 0"),
        "{o}"
    );
}

#[test]
fn an_as_alias_and_a_column_list_stay_before_on_true() {
    let src = view("SELECT p.Id, l.a FROM dbo.P p\nOUTER APPLY (SELECT c.At FROM dbo.C c WHERE c.PId = p.Id) AS l (a)");
    let o = pg(&src).output;
    assert!(o.contains(") AS l (a) ON TRUE"), "{o}");
}

#[test]
fn a_function_on_the_right_is_the_lateral_item() {
    let src = view("SELECT p.Id FROM dbo.P p\nCROSS APPLY dbo.f(p.Id) x");
    let o = pg(&src).output;
    assert!(o.contains("CROSS JOIN LATERAL f(p.Id) x"), "{o}");
}

#[test]
fn two_applies_and_one_inside_the_other() {
    let src = view("SELECT p.Id FROM dbo.P p\nCROSS APPLY (SELECT c.Id FROM dbo.C c OUTER APPLY (SELECT d.Id FROM dbo.C d WHERE d.PId = c.Id) e WHERE c.PId = p.Id) x\nOUTER APPLY (SELECT f.Id FROM dbo.C f WHERE f.PId = p.Id) y");
    let o = pg(&src).output;
    assert!(!o.to_ascii_uppercase().contains("APPLY"), "{o}");
    assert!(o.contains("CROSS JOIN LATERAL ("), "{o}");
    assert!(
        o.contains("LEFT JOIN LATERAL (SELECT d.Id FROM C d WHERE d.PId = c.Id) e ON TRUE"),
        "{o}"
    );
    assert!(
        o.contains("LEFT JOIN LATERAL (SELECT f.Id FROM C f WHERE f.PId = p.Id) y ON TRUE"),
        "{o}"
    );
}

#[test]
fn a_column_named_apply_and_the_other_join_words_are_not_touched() {
    let src = view("SELECT p.Id FROM dbo.P p LEFT JOIN dbo.C c ON c.PId = p.Id CROSS JOIN dbo.P q");
    let o = pg(&src).output;
    assert!(
        o.contains("LEFT JOIN C c ON c.PId = p.Id CROSS JOIN P q"),
        "{o}"
    );
    assert!(!o.contains("LATERAL"), "{o}");
}

#[test]
fn db2_for_i_has_no_rule_yet() {
    // The control: the APPLY is passed through, as before this rule.
    let src = view(
        "SELECT p.Id FROM dbo.P p\nCROSS APPLY (SELECT c.Id FROM dbo.C c WHERE c.PId = p.Id) x",
    );
    let o = db2(&src).output;
    assert!(o.to_ascii_uppercase().contains("CROSS APPLY"), "{o}");
}
