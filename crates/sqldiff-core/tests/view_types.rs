//! A view that reads one table directly types its columns from the
//! definition, so functions over its varchar columns are judged like those
//! over the table's; a view that could mix sources, or a char(n) column,
//! keeps the refusal.
use sqldiff_core::{diff, Artifact, Dialect, Options, Verdict};

fn verdict(ddl: &str, src: &str, cond: &str) -> Verdict {
    let d = Dialect::Tsql;
    let opts = Options {
        catalog_a: vec![Artifact::new("catalog.sql", d, ddl)],
        catalog_b: vec![Artifact::new("catalog.sql", d, ddl)],
        ..Options::default()
    };
    let q = |c: &str| {
        format!("CREATE PROCEDURE p @id int AS BEGIN SELECT id FROM {src} WHERE id = @id AND s = 'a' AND {c}; END")
    };
    diff(
        &Artifact::new("a.sql", d, q(cond)),
        &Artifact::new("b.sql", d, q("1 = 0")),
        &opts,
    )
    .verdict()
}

const TABLES: &str = "CREATE TABLE t (id int PRIMARY KEY, s varchar(10) NOT NULL, c char(5) NOT NULL); CREATE TABLE u (id int PRIMARY KEY, s char(8) NOT NULL);";

#[test]
fn a_varchar_view_column_is_judged_like_the_table_column() {
    let ddl = format!("{TABLES} CREATE VIEW vw AS SELECT id, s FROM t;");
    for cond in ["LEN(s) = 1", "UPPER(s) = 'A'", "s + 'x' = 'ax'"] {
        assert_eq!(
            verdict(&ddl, "vw", cond),
            verdict(&ddl, "t", cond),
            "{cond}"
        );
        assert_eq!(verdict(&ddl, "vw", cond), Verdict::Different, "{cond}");
    }
}

#[test]
fn a_char_view_column_keeps_the_refusal() {
    let ddl = format!("{TABLES} CREATE VIEW vc AS SELECT id, c AS s FROM t;");
    assert_ne!(verdict(&ddl, "vc", "LEN(s) = 1"), Verdict::Different);
}

#[test]
fn a_view_over_a_join_keeps_the_refusal() {
    // `s` is char(8) in `u`, varchar(10) in `t`: which one the view column
    // is depends on the join, which the catalog does not model.
    let ddl = format!("{TABLES} CREATE VIEW vj AS SELECT t.id, u.s FROM t JOIN u ON u.id = t.id;");
    assert_ne!(verdict(&ddl, "vj", "LEN(s) = 1"), Verdict::Different);
}

const T1: &str = "CREATE TABLE t (id int PRIMARY KEY, s varchar(10) NOT NULL, c char(5) NOT NULL);";

fn not_judged(ddl: &str, src: &str) {
    for cond in ["s + 'x' = 'ax'", "LEN(s) = 1"] {
        assert_ne!(verdict(ddl, src, cond), Verdict::Different, "{cond}: {ddl}");
    }
}

#[test]
fn a_view_over_a_view_is_not_trusted() {
    // The inner view's column is a guess; a CTE named like the table can put
    // a char(5) column under a varchar name.
    let ddl = format!(
        "{T1} CREATE TABLE t2 (id int PRIMARY KEY, c char(5) NOT NULL);\nGO\n\
         CREATE VIEW vc AS WITH t AS (SELECT id, c AS s FROM t2) SELECT id, s FROM t;\nGO\n\
         CREATE VIEW vw AS SELECT id, s FROM vc;\nGO\n"
    );
    not_judged(&ddl, "vw");
}

#[test]
fn a_changed_base_table_makes_the_view_types_stale() {
    for later in [
        "ALTER TABLE t ALTER COLUMN s char(10) NOT NULL;",
        "DROP TABLE t; CREATE TABLE t (id int PRIMARY KEY, s char(10) NOT NULL);",
    ] {
        let ddl = format!("{T1} CREATE VIEW vw AS SELECT id, s FROM t;\nGO\n{later}");
        not_judged(&ddl, "vw");
    }
}

#[test]
fn expressions_casts_and_swapped_names_are_not_trusted() {
    for def in [
        "SELECT id, CAST(s AS char(5)) AS s FROM t",
        "SELECT id, CASE WHEN id > 0 THEN c ELSE s END AS s FROM t",
        "SELECT id, ISNULL(c, s) AS s FROM t",
        "SELECT id, c AS s FROM t",
    ] {
        let ddl = format!("{T1} CREATE VIEW vw AS {def};");
        not_judged(&ddl, "vw");
    }
    not_judged(
        &format!("{T1} CREATE VIEW vw (id, s, c) AS SELECT id, c, s FROM t;"),
        "vw",
    );
}
