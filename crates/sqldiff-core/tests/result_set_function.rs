//! A T-SQL procedure's one result set against the PL/pgSQL function RETURNS
//! TABLE it became: its SELECT, or its data change with OUTPUT but no INTO,
//! against the function's one RETURN QUERY, under ASM-RESULT-SET-FUNCTION.
//! RETURN QUERY needs each column to have exactly the declared type, and the
//! function's output columns are variables a same-named column clashes with
//! (both checked on PostgreSQL 18.6).

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;

const DDL: &str =
    "CREATE TABLE item (item_id integer PRIMARY KEY, name varchar(100) NOT NULL, qty integer);";

fn run(a: &str, b: &str) -> Report {
    let opts = Options {
        catalog_a: vec![Artifact::new("c.sql", T, DDL)],
        catalog_b: vec![Artifact::new("c.sql", P, DDL)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", T, a),
        &Artifact::new("b.sql", P, b),
        &opts,
    )
}

fn ts(body: &str) -> String {
    format!("CREATE PROCEDURE dbo.p @id int AS\nBEGIN\n  SET NOCOUNT ON;\n  SET XACT_ABORT ON;\n  {body}\nEND\n")
}

fn pg(cols: &str, body: &str) -> String {
    format!("CREATE OR REPLACE FUNCTION p(p_id integer) RETURNS TABLE ({cols})\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  {body}\nEND;\n$$;\n")
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

fn equal(a: &str, b: &str) -> Report {
    let r = run(a, b);
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{a}\n{b}\n{}",
        why(&r)
    );
    assert!(
        r.assumptions
            .iter()
            .any(|x| x.id == "ASM-RESULT-SET-FUNCTION"),
        "{:?}",
        r.assumptions
    );
    r
}

fn not_equal(a: &str, b: &str, what: &str) -> Report {
    let r = run(a, b);
    assert!(
        matches!(r.verdict(), Verdict::Different | Verdict::Unknown),
        "{what}: {a}\n{b}\n{}",
        why(&r)
    );
    r
}

const SEL: &str = "SELECT i.name FROM dbo.item i WHERE i.item_id = @id;";
const RQ: &str =
    "RETURN QUERY SELECT CAST(i.name AS varchar(100)) FROM item i WHERE i.item_id = p_id;";

#[test]
fn a_select_against_return_query() {
    equal(&ts(SEL), &pg("name varchar(100)", RQ));
    // `RETURN 0` sets the status a procedure returns anyway.
    equal(
        &ts(&format!("{SEL}\n  RETURN 0;")),
        &pg("name varchar(100)", &format!("{RQ}\n  RETURN;")),
    );
    // With `#variable_conflict use_column` the bare name is the column.
    let b = pg("name varchar(100)", &RQ.replace("i.name", "name"))
        .replace("AS $$\n", "AS $$\n#variable_conflict use_column\n");
    equal(&ts(SEL), &b);
}

#[test]
fn changed_queries_names_and_types_are_not_equal() {
    let cases = [
        (pg("nm varchar(100)", RQ), "another column name"),
        (
            pg("name varchar(100)", &RQ.replace("= p_id", "<> p_id")),
            "another condition",
        ),
        (
            pg(
                "name varchar(100)",
                &RQ.replace("i.name AS", "CAST(i.qty AS varchar(100)) AS"),
            ),
            "another column",
        ),
        (
            pg("name varchar(5)", &RQ.replace("varchar(100)", "varchar(5)")),
            "a cast that cuts the value",
        ),
        (
            pg(
                "name varchar(100)",
                &RQ.replace("CAST(i.name AS varchar(100))", "i.name"),
            ),
            "no cast to the declared type",
        ),
        (pg("name text", RQ), "a cast to another type than declared"),
        (
            pg("name varchar(100)", &RQ.replace("i.name", "name")),
            "a name both a column and an output column",
        ),
    ];
    for (b, what) in cases {
        not_equal(&ts(SEL), &b, what);
    }
}

#[test]
fn output_without_into_against_return_query_returning() {
    equal(
        &ts("INSERT INTO dbo.item (item_id, name) OUTPUT inserted.item_id VALUES (@id, N'x');"),
        &pg(
            "item_id integer",
            "RETURN QUERY INSERT INTO item (item_id, name) VALUES (p_id, 'x') RETURNING CAST(new.item_id AS integer);",
        ),
    );
    let upd = ts("UPDATE dbo.item SET qty = 1 OUTPUT deleted.qty AS was WHERE item_id = @id;");
    let r = equal(
        &upd,
        &pg(
            "was integer",
            "RETURN QUERY UPDATE item SET qty = 1 WHERE item.item_id = p_id RETURNING CAST(old.qty AS integer);",
        ),
    );
    assert!(r
        .assumptions
        .iter()
        .any(|x| x.id == "ASM-OUTPUT-TARGET-TRIGGERS"));
    not_equal(
        &upd,
        &pg(
            "was integer",
            "RETURN QUERY UPDATE item SET qty = 1 WHERE item.item_id = p_id RETURNING CAST(new.qty AS integer);",
        ),
        "the row after the change",
    );
    // `item_id` is the output column and a column of item.
    not_equal(
        &ts("DELETE FROM dbo.item OUTPUT deleted.item_id WHERE item_id = @id;"),
        &pg(
            "item_id integer",
            "RETURN QUERY DELETE FROM item WHERE item_id = p_id RETURNING CAST(old.item_id AS integer);",
        ),
        "an ambiguous name",
    );
}

#[test]
fn quoted_columns_of_the_changed_row() {
    let ddl_t = "CREATE TABLE dbo.Lineage ([Lineage Key] int IDENTITY(1,1) NOT NULL PRIMARY KEY, [Table Name] nvarchar(128) NOT NULL);";
    let ddl_p = "CREATE TABLE Lineage (\"Lineage Key\" integer GENERATED BY DEFAULT AS IDENTITY NOT NULL PRIMARY KEY, \"Table Name\" varchar(128) NOT NULL);";
    let opts = Options {
        catalog_a: vec![Artifact::new("c.sql", T, ddl_t)],
        catalog_b: vec![Artifact::new("c.sql", P, ddl_p)],
        ..Options::default()
    };
    let a = "CREATE PROCEDURE dbo.p @t nvarchar(128) AS\nBEGIN\n  SET XACT_ABORT ON;\n  INSERT dbo.Lineage ([Table Name]) OUTPUT inserted.[Lineage Key] AS LineageKey VALUES (@t);\nEND\n";
    let b = |col: &str| {
        format!("CREATE OR REPLACE FUNCTION p(p_t varchar(128)) RETURNS TABLE (LineageKey integer)\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  RETURN QUERY INSERT INTO Lineage (\"Table Name\") VALUES (p_t) RETURNING CAST(new.{col} AS integer);\nEND;\n$$;\n")
    };
    let r = diff(
        &Artifact::new("a.sql", T, a),
        &Artifact::new("b.sql", P, &b("\"Lineage Key\"")),
        &opts,
    );
    assert_eq!(r.verdict(), Verdict::EqualUnderAssumptions, "{}", why(&r));
    let r = diff(
        &Artifact::new("a.sql", T, a),
        &Artifact::new("b.sql", P, &b("\"Table Name\"")),
        &opts,
    );
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions, "{}", why(&r));
}

#[test]
fn only_one_result_set_that_always_runs_is_compared() {
    let two = (
        ts(&format!("{SEL}\n  {SEL}")),
        pg("name varchar(100)", &format!("{RQ}\n  {RQ}")),
    );
    let inside_if = (
        ts(&format!("IF @id > 0\n    {SEL}")),
        pg(
            "name varchar(100)",
            &format!("IF p_id > 0 THEN\n    {RQ}\n  END IF;"),
        ),
    );
    let after_return = (
        ts(&format!("IF @id < 0 RETURN;\n  {SEL}")),
        pg(
            "name varchar(100)",
            &format!("IF p_id < 0 THEN\n    RETURN;\n  END IF;\n  {RQ}"),
        ),
    );
    let in_block = (
        ts(SEL),
        pg(
            "name varchar(100)",
            &format!("BEGIN\n    {RQ}\n  EXCEPTION WHEN others THEN NULL;\n  END;"),
        ),
    );
    for ((a, b), what) in [
        (two, "two result sets"),
        (inside_if, "inside an IF"),
        (after_return, "after a RETURN"),
        (in_block, "in a block with an EXCEPTION section"),
    ] {
        let r = not_equal(&a, &b, what);
        assert!(
            !r.assumptions
                .iter()
                .any(|x| x.id == "ASM-RESULT-SET-FUNCTION"),
            "{what}: {}",
            why(&r)
        );
    }
    // OUTPUT parameters.
    let r = not_equal(
        "CREATE PROCEDURE dbo.p @id int, @n int OUTPUT AS\nBEGIN\n  SELECT i.name FROM dbo.item i WHERE i.item_id = @id;\nEND\n",
        &pg("name varchar(100)", RQ),
        "OUTPUT parameters",
    );
    assert!(
        r.unknown_reasons
            .iter()
            .any(|x| x.contains("OUTPUT parameters")),
        "{}",
        why(&r)
    );
    // A status other than 0 is not the function's.
    not_equal(
        &ts(&format!("{SEL}\n  RETURN 1;")),
        &pg("name varchar(100)", &format!("{RQ}\n  RETURN;")),
        "RETURN 1",
    );
}

#[test]
fn return_query_is_compared_only_in_a_plpgsql_table_function() {
    // A bare SELECT in PL/pgSQL is an error, not a result set.
    not_equal(
        &ts(SEL),
        &pg(
            "name varchar(100)",
            "SELECT CAST(i.name AS varchar(100)) FROM item i WHERE i.item_id = p_id;",
        ),
        "SELECT without INTO",
    );
}
