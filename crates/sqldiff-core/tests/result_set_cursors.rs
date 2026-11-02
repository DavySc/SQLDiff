//! A T-SQL procedure's result sets against the PL/pgSQL procedure they became
//! (decision D8, 2026-11-02): one INOUT refcursor parameter `result_set_N`
//! per result set after the procedure's own parameters, each opened by
//! `OPEN result_set_N FOR query`, under ASM-RESULTSET-REFCURSOR. A PostgreSQL
//! cursor reads its rows when they are fetched, under the snapshot of its
//! OPEN (probed on PostgreSQL 18, eval/refcursor/probe-pg.sql), so the engine
//! compares the pair only where that is how T-SQL sends the rows.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;

const TS_DDL: &str = "CREATE TABLE dbo.item (item_id int NOT NULL PRIMARY KEY, name varchar(100) NOT NULL, qty int NOT NULL);";
const PG_DDL: &str = "CREATE TABLE item (item_id integer NOT NULL PRIMARY KEY, name varchar(100) NOT NULL, qty integer NOT NULL);";

fn run(a: &str, b: &str) -> Report {
    let opts = Options {
        catalog_a: vec![Artifact::new("c.sql", T, TS_DDL)],
        catalog_b: vec![Artifact::new("c.sql", P, PG_DDL)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", T, a),
        &Artifact::new("b.sql", P, b),
        &opts,
    )
}

/// A T-SQL procedure with the given statements.
fn ts(body: &str) -> String {
    format!("CREATE PROCEDURE dbo.p @id int AS\nBEGIN\n  SET NOCOUNT ON;\n  SET XACT_ABORT ON;\n  {body}\nEND\n")
}

/// The PostgreSQL procedure with `n` cursor parameters and the statements.
fn pg(n: usize, body: &str) -> String {
    let cursors: String = (1..=n)
        .map(|i| format!(", INOUT result_set_{i} refcursor DEFAULT NULL"))
        .collect();
    format!("CREATE OR REPLACE PROCEDURE p(p_id integer{cursors})\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  {body}\nEND;\n$$;\n")
}

fn why(r: &Report) -> String {
    format!(
        "{:?}\n{:?}\n{:?}",
        r.verdict(),
        r.findings
            .iter()
            .map(|f| (&f.rule, &f.witness))
            .collect::<Vec<_>>(),
        r.unknown_reasons
    )
}

fn equal(a: &str, b: &str) {
    let r = run(a, b);
    assert!(
        matches!(r.verdict(), Verdict::EqualUnderAssumptions),
        "{a}\n{b}\n{}",
        why(&r)
    );
    assert!(
        r.assumptions
            .iter()
            .any(|x| x.id == "ASM-RESULTSET-REFCURSOR"),
        "{:?}",
        r.assumptions
    );
}

fn not_equal(a: &str, b: &str) {
    let r = run(a, b);
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "must not be equal:\n{a}\n{b}\n{}",
        why(&r)
    );
}

const Q1: &str = "SELECT item_id, name FROM dbo.item WHERE item_id = @id ORDER BY item_id;";
const Q2: &str = "SELECT qty, name FROM dbo.item ORDER BY qty, item_id;";
const O1: &str =
    "OPEN result_set_1 FOR SELECT item_id, name FROM item WHERE item_id = p_id ORDER BY item_id;";
const O2: &str = "OPEN result_set_2 FOR SELECT qty, name FROM item ORDER BY qty, item_id;";

#[test]
fn one_and_several_result_sets_are_compared_cursor_by_cursor() {
    equal(&ts(Q1), &pg(1, O1));
    equal(
        &ts(&format!("{Q1}\n  {Q2}")),
        &pg(2, &format!("{O1}\n  {O2}")),
    );
}

#[test]
fn writes_around_the_result_sets_are_ordinary_statements() {
    // Later changes are invisible to a cursor opened before them (probe P2);
    // one write only, so that T-SQL's autocommit and PostgreSQL's one
    // transaction commit the same work.
    equal(
        &ts(&format!(
            "UPDATE dbo.item SET qty = qty + 1 WHERE item_id = @id;\n  {Q1}\n  {Q2}"
        )),
        &pg(
            2,
            &format!("UPDATE item SET qty = qty + 1 WHERE item_id = p_id;\n  {O1}\n  {O2}"),
        ),
    );
    equal(
        &ts(&format!(
            "{Q1}\n  {Q2}\n  DELETE FROM dbo.item WHERE qty < 0;"
        )),
        &pg(
            2,
            &format!("{O1}\n  {O2}\n  DELETE FROM item WHERE qty < 0;"),
        ),
    );
    // A change to what a cursor reads is not the same change.
    not_equal(
        &ts(&format!("{Q1}\n  DELETE FROM dbo.item WHERE qty < 0;")),
        &pg(1, &format!("{O1}\n  DELETE FROM item WHERE qty <= 0;")),
    );
}

#[test]
fn a_cursor_that_is_not_the_same_result_set_is_not_equal() {
    let a = ts(&format!("{Q1}\n  {Q2}"));
    // Swapped: result_set_1 would carry the second result set.
    not_equal(
        &a,
        &pg(
            2,
            &format!(
                "{}\n  {}",
                O2.replace("result_set_2", "result_set_1"),
                O1.replace("result_set_1", "result_set_2")
            ),
        ),
    );
    // A different row filter, order, or column.
    not_equal(
        &a,
        &pg(
            2,
            &format!(
                "{}\n  {O2}",
                O1.replace("item_id = p_id", "item_id <> p_id")
            ),
        ),
    );
    not_equal(
        &a,
        &pg(
            2,
            &format!(
                "{O1}\n  {}",
                O2.replace("ORDER BY qty, item_id", "ORDER BY qty DESC, item_id")
            ),
        ),
    );
    not_equal(
        &a,
        &pg(
            2,
            &format!("{O1}\n  {}", O2.replace("qty, name FROM", "name, qty FROM")),
        ),
    );
    // One cursor fewer than result sets, or one more.
    not_equal(&a, &pg(1, O1));
    not_equal(&ts(Q1), &pg(2, &format!("{O1}\n  {O2}")));
}

#[test]
fn the_cursor_parameters_must_follow_the_procedures_own() {
    // Named otherwise, not INOUT, not refcursor, or in front of the others.
    let a = ts(Q1);
    let body = |name: &str| {
        format!(
            "OPEN {name} FOR SELECT item_id, name FROM item WHERE item_id = p_id ORDER BY item_id;"
        )
    };
    for b in [
        pg(1, &body("result_set_1"))
            .replace("result_set_1 refcursor", "rs refcursor")
            .replace("OPEN result_set_1", "OPEN rs"),
        pg(1, &body("result_set_1")).replace(
            "INOUT result_set_1 refcursor DEFAULT NULL",
            "result_set_1 refcursor DEFAULT NULL",
        ),
        pg(1, &body("result_set_1")).replace("result_set_1 refcursor", "result_set_1 integer"),
        pg(1, &body("result_set_1")).replace(
            "p_id integer, INOUT result_set_1 refcursor DEFAULT NULL",
            "INOUT result_set_1 refcursor DEFAULT NULL, p_id integer",
        ),
    ] {
        not_equal(&a, &b);
    }
}

#[test]
fn a_result_set_under_control_flow_is_not_compared() {
    // T-SQL numbers the result sets it sends, the cursors keep their names.
    not_equal(
        &ts(&format!("IF @id > 0\n  BEGIN\n  {Q1}\n  END")),
        &pg(1, &format!("IF p_id > 0 THEN\n  {O1}\n  END IF;")),
    );
    not_equal(
        &ts(&format!(
            "WHILE @id > 0\n  BEGIN\n  {Q1}\n  SET @id = @id - 1;\n  END"
        )),
        &pg(
            1,
            &format!("WHILE p_id > 0 LOOP\n  {O1}\n  p_id := p_id - 1;\n  END LOOP;"),
        ),
    );
}

#[test]
fn what_would_close_or_break_a_cursor_after_its_open_is_not_compared() {
    let a = |after: &str| ts(&format!("{Q1}\n  {after}"));
    let b = |after: &str| pg(1, &format!("{O1}\n  {after}"));
    // DROP and TRUNCATE of a table an open cursor reads fail in PostgreSQL
    // (probe P3), a COMMIT closes the cursor, a call may do either.
    for (t, p) in [
        ("DROP TABLE dbo.item;", "DROP TABLE item;"),
        ("TRUNCATE TABLE dbo.item;", "TRUNCATE item;"),
        (
            "ALTER TABLE dbo.item ADD note int NULL;",
            "ALTER TABLE item ADD COLUMN note integer NULL;",
        ),
        ("EXEC dbo.other;", "CALL other();"),
    ] {
        not_equal(&a(t), &b(p));
    }
    not_equal(
        &ts(&format!("BEGIN TRAN;\n  {Q1}\n  COMMIT;")),
        &pg(1, &format!("{O1}\n  COMMIT;")),
    );
}

#[test]
fn a_query_that_runs_when_the_rows_are_fetched_is_not_compared() {
    // PostgreSQL evaluates these at FETCH, T-SQL at the SELECT (probe P6).
    for (t, p) in [
        (
            "SELECT item_id, NEWID() AS g FROM dbo.item;",
            "SELECT item_id, gen_random_uuid() AS g FROM item",
        ),
        (
            "SELECT item_id, RAND() AS r FROM dbo.item;",
            "SELECT item_id, random() AS r FROM item",
        ),
        (
            "SELECT item_id, GETDATE() AS now FROM dbo.item;",
            "SELECT item_id, clock_timestamp() AS now FROM item",
        ),
        (
            "SELECT dbo.f(item_id) AS x FROM dbo.item;",
            "SELECT f(item_id) AS x FROM item",
        ),
        (
            "SELECT item_id, @@ROWCOUNT AS n FROM dbo.item;",
            "SELECT item_id, 1 AS n FROM item",
        ),
    ] {
        not_equal(&ts(t), &pg(1, &format!("OPEN result_set_1 FOR {p};")));
    }
}

#[test]
fn a_name_equals_item_is_a_column_alias_in_t_sql() {
    // `Total = qty + 1` names the column, as `qty + 1 AS total` does.
    equal(
        &ts("SELECT Total = qty + 1, name FROM dbo.item ORDER BY item_id;"),
        &pg(
            1,
            "OPEN result_set_1 FOR SELECT qty + 1 AS total, name FROM item ORDER BY item_id;",
        ),
    );
    not_equal(
        &ts("SELECT Total = qty + 1, name FROM dbo.item ORDER BY item_id;"),
        &pg(
            1,
            "OPEN result_set_1 FOR SELECT qty + 2 AS total, name FROM item ORDER BY item_id;",
        ),
    );
    not_equal(
        &ts("SELECT Total = qty + 1, name FROM dbo.item ORDER BY item_id;"),
        &pg(
            1,
            "OPEN result_set_1 FOR SELECT qty + 1 AS amount, name FROM item ORDER BY item_id;",
        ),
    );
}
