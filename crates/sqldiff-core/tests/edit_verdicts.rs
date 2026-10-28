//! Expression-level verdicts for a statement edited in place (rule
//! `equiv.edit`): a changed expression is called different only with a
//! concrete input that tells the two versions apart, and never equal when a
//! changed declaration can still be observed. The engine behaviour each
//! witness relies on was checked on SQL Server 2022 and PostgreSQL 18.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;

const TSQL_DDL: &str = "
CREATE TABLE dbo.item (item_id int NOT NULL PRIMARY KEY, name nvarchar(50) NOT NULL, price decimal(12,2) NOT NULL);
";
const PG_DDL: &str = "
CREATE TABLE item (item_id integer PRIMARY KEY, name varchar(50) NOT NULL, price numeric(12,2) NOT NULL);
";

fn run(a: &str, b: &str, d: Dialect) -> Report {
    let ddl = if d == T { TSQL_DDL } else { PG_DDL };
    let opts = Options {
        catalog_a: vec![Artifact::new("catalog.sql", d, ddl)],
        catalog_b: vec![Artifact::new("catalog.sql", d, ddl)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", d, a),
        &Artifact::new("b.sql", d, b),
        &opts,
    )
}

fn edit_witness(r: &Report) -> String {
    r.findings
        .iter()
        .find(|f| f.rule == "equiv.edit" && f.verdict == Verdict::Different)
        .unwrap_or_else(|| panic!("no different equiv.edit finding: {:#?}", r.findings))
        .witness
        .clone()
}

fn tsql_proc(cond: &str) -> String {
    format!(
        "CREATE PROCEDURE dbo.cheap @max decimal(12,2) AS
         BEGIN
           SELECT item_id FROM dbo.item WHERE {cond} ORDER BY item_id;
         END"
    )
}

#[test]
fn changed_comparison_has_a_witness_row() {
    let r = run(&tsql_proc("price < @max"), &tsql_proc("price <= @max"), T);
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
    edit_witness(&r);
}

#[test]
fn reformatting_and_keyword_case_stay_equal() {
    let b = "create procedure dbo.cheap   @max decimal(12,2)   as
             begin
               -- only layout changed
               select item_id
                 from dbo.item
                where price < @max
                order by item_id;
             end";
    let r = run(&tsql_proc("price < @max"), b, T);
    assert_eq!(r.verdict(), Verdict::Equal, "{:#?}", r.findings);
}

#[test]
fn exists_select_list_is_not_a_difference() {
    let p = |n: u32| {
        format!(
            "CREATE PROCEDURE dbo.has_items AS
             BEGIN
               IF EXISTS (SELECT {n} FROM dbo.item WHERE price > 0) RETURN 1;
               RETURN 0;
             END"
        )
    };
    let r = run(&p(1), &p(2), T);
    assert_ne!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
}

fn tsql_param(width: u32, body: &str) -> String {
    format!(
        "CREATE PROCEDURE dbo.find @name nvarchar({width}) AS
         BEGIN
           {body}
         END"
    )
}

#[test]
fn wider_string_parameter_keeps_characters_a_truncates() {
    let body = "SELECT item_id FROM dbo.item WHERE name = @name;";
    let r = run(&tsql_param(40, body), &tsql_param(41, body), T);
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
    let w = r
        .findings
        .iter()
        .map(|f| f.witness.as_str())
        .find(|w| w.contains("41 characters"))
        .unwrap_or_else(|| panic!("no truncation witness: {:#?}", r.findings));
    assert!(w.contains("@name"), "{w}");
}

#[test]
fn wider_parameter_only_tested_for_null_is_not_different() {
    // The parameter's value is never read, only whether it is NULL, so the
    // width cannot be observed.
    let body = "IF @name IS NULL RETURN 1; RETURN 0;";
    let r = run(&tsql_param(40, body), &tsql_param(41, body), T);
    assert_ne!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
}

#[test]
fn changed_declared_type_is_never_equal() {
    let p = |prec: u32| {
        format!(
            "CREATE PROCEDURE dbo.stamp AS
             BEGIN
               DECLARE @at datetime2({prec}) = SYSDATETIME();
               SELECT @at;
             END"
        )
    };
    let r = run(&p(7), &p(6), T);
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.verdict()
    );
}

#[test]
fn subtracting_two_strings_does_not_compile() {
    let p = |op: &str| {
        format!(
            "CREATE PROCEDURE dbo.label AS
             BEGIN
               DECLARE @s nvarchar(100);
               SET @s = N'item ' {op} N'list';
               SELECT @s;
             END"
        )
    };
    let r = run(&p("+"), &p("-"), T);
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
    assert!(
        r.findings
            .iter()
            .any(|f| f.witness.contains("two character strings")),
        "{:#?}",
        r.findings
    );
}

#[test]
fn string_minus_number_is_not_flagged_as_invalid() {
    // `nvarchar - int` converts the string and compiles; only a run with a
    // non-numeric string fails, so validity must not claim it.
    let p = |op: &str| {
        format!(
            "CREATE PROCEDURE dbo.calc @n nvarchar(10) AS
             BEGIN
               SELECT @n {op} 1;
             END"
        )
    };
    let r = run(&p("+"), &p("-"), T);
    assert!(
        !r.findings
            .iter()
            .any(|f| f.witness.contains("two character strings")),
        "{:#?}",
        r.findings
    );
}

fn pg_fn(cond: &str) -> String {
    format!(
        "CREATE FUNCTION cheap(p_max numeric) RETURNS SETOF integer LANGUAGE plpgsql AS $$
         BEGIN
           RETURN QUERY SELECT item_id FROM item WHERE {cond} ORDER BY item_id;
         END; $$;"
    )
}

#[test]
fn postgres_changed_comparison_has_a_witness_row() {
    let r = run(&pg_fn("price < p_max"), &pg_fn("price <= p_max"), P);
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
    edit_witness(&r);
}

#[test]
fn postgres_and_or_swap_is_different() {
    let r = run(
        &pg_fn("price < p_max AND item_id > 0"),
        &pg_fn("price < p_max OR item_id > 0"),
        P,
    );
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
}

fn pg_sql_fn(returns: &str, cond: &str) -> String {
    format!(
        "CREATE FUNCTION cheap(p_max numeric) RETURNS {returns} LANGUAGE sql STABLE AS $$
           SELECT item_id FROM item WHERE {cond} ORDER BY item_id;
         $$;"
    )
}

#[test]
fn postgres_sql_function_returns_its_last_query() {
    let r = run(
        &pg_sql_fn("SETOF integer", "price < p_max"),
        &pg_sql_fn("SETOF integer", "price <= p_max"),
        P,
    );
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
    edit_witness(&r);
}

#[test]
fn postgres_scalar_sql_function_is_not_judged_by_its_whole_result() {
    // A scalar SQL function returns only the first row, so a difference in
    // later rows must not be reported from the full result set.
    let r = run(
        &pg_sql_fn("integer", "price < p_max"),
        &pg_sql_fn("integer", "price <= p_max"),
        P,
    );
    assert!(
        !r.findings
            .iter()
            .any(|f| f.rule == "equiv.edit" && f.witness.contains("as its result set")),
        "{:#?}",
        r.findings
    );
}

fn tsql_write(stmt: &str) -> String {
    format!(
        "CREATE PROCEDURE dbo.touch @id int AS
         BEGIN
           {stmt}
         END"
    )
}

#[test]
fn set_column_to_its_own_value_is_not_different() {
    // `SET price = price` leaves the row as it was.
    let r = run(
        &tsql_write("UPDATE dbo.item SET name = N'x' WHERE item_id = @id;"),
        &tsql_write("UPDATE dbo.item SET name = N'x', price = price WHERE item_id = @id;"),
        T,
    );
    assert_ne!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
}

#[test]
fn insert_values_are_matched_by_column() {
    // Naming the columns in another order inserts the same row.
    let r = run(
        &tsql_write("INSERT INTO dbo.item (item_id, name, price) VALUES (@id, N'x', 1);"),
        &tsql_write("INSERT INTO dbo.item (price, item_id, name) VALUES (1, @id, N'x');"),
        T,
    );
    assert_ne!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
    // A changed value is still found, and named.
    let r = run(
        &tsql_write("INSERT INTO dbo.item (item_id, name, price) VALUES (@id, N'x', 1);"),
        &tsql_write("INSERT INTO dbo.item (price, item_id, name) VALUES (2, @id, N'x');"),
        T,
    );
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
    assert!(
        edit_witness(&r).contains("price = 2"),
        "{}",
        edit_witness(&r)
    );
}

#[test]
fn update_witness_names_the_columns() {
    let r = run(
        &tsql_write("UPDATE dbo.item SET price = 1 WHERE item_id = @id;"),
        &tsql_write("UPDATE dbo.item SET price = 2 WHERE item_id = @id;"),
        T,
    );
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
    let w = edit_witness(&r);
    assert!(w.contains("price = 1") && w.contains("price = 2"), "{w}");
}

#[test]
fn unmodelled_function_may_return_null() {
    // IIF is not modelled: its result may be NULL, as CASE's is.
    let p = |e: &str| {
        format!(
            "CREATE PROCEDURE dbo.flag @x int AS
             BEGIN
               SELECT {e} AS flag;
             END"
        )
    };
    let r = run(
        &p("IIF(@x > 0, 1, NULL)"),
        &p("CASE WHEN @x > 0 THEN 1 END"),
        T,
    );
    assert_ne!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
    // A function the evaluator does not know against a NULL: either may hold.
    let r = run(&p("SOUNDEX(N'x')"), &p("NULL"), T);
    assert_ne!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
}

fn decimal_param(d: Dialect, precision: u32) -> String {
    if d == T {
        format!(
            "CREATE PROCEDURE dbo.set_price @id int, @p decimal({precision},2) AS
             BEGIN
               UPDATE dbo.item SET price = @p WHERE item_id = @id;
             END"
        )
    } else {
        format!(
            "CREATE FUNCTION set_price(p_id integer, p_p numeric({precision},2)) RETURNS void
             LANGUAGE plpgsql AS $$
             BEGIN
               UPDATE item SET price = p_p WHERE item_id = p_id;
             END; $$;"
        )
    }
}

#[test]
fn decimal_parameter_precision_only_matters_in_tsql() {
    // SQL Server rejects an argument that overflows decimal(p,s) (checked on
    // SQL Server 2022); PostgreSQL ignores a parameter's typmod.
    let r = run(&decimal_param(T, 8), &decimal_param(T, 10), T);
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
    let r = run(&decimal_param(P, 8), &decimal_param(P, 10), P);
    assert!(
        !r.findings
            .iter()
            .any(|f| f.witness.contains("arithmetic overflow")),
        "{:#?}",
        r.findings
    );
}

#[test]
fn functions_that_never_return_null_still_differ_from_null() {
    // format() with a format string returns text even for NULL arguments
    // (checked on PostgreSQL 18), so it is told apart from NULL.
    let f = |e: &str| {
        format!(
            "CREATE FUNCTION labels() RETURNS TABLE (item_id integer, label text)
             LANGUAGE sql STABLE AS $$
               SELECT item_id, {e} FROM item WHERE price > 0 ORDER BY item_id;
             $$;"
        )
    };
    let r = run(&f("format('item %s', name)"), &f("NULL::text"), P);
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
    assert!(edit_witness(&r).contains("format("), "{}", edit_witness(&r));
}

fn edit_differents(r: &Report) -> Vec<String> {
    r.findings
        .iter()
        .filter(|f| f.rule == "equiv.edit" && f.verdict == Verdict::Different)
        .map(|f| f.witness.clone())
        .collect()
}

fn tsql_out(body: &str) -> String {
    format!("CREATE PROCEDURE dbo.O @p INT, @x INT OUTPUT AS BEGIN {body} END")
}

// Two edits that make up for each other: each one alone has a witness, but
// the witness gives the second statement the same @x on both sides although
// the first changed statement set it differently. Both return (p+1)*2.
#[test]
fn compensating_edits_are_not_different() {
    let a = tsql_out("SET @x = @p + 1; SET @x = @x * 2;");
    let b = tsql_out("SET @x = @p + 2; SET @x = (@x - 1) * 2;");
    let r = run(&a, &b, T);
    assert!(edit_differents(&r).is_empty(), "{:#?}", r.findings);
    assert_ne!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
    assert_ne!(r.verdict(), Verdict::Equal, "{:#?}", r.findings);
}

// The same two edits, but they do not make up for each other: (p+1)*2 vs
// (p+1)*3. The edits are only judged together, so the pair stays unknown
// rather than being called different from a per-statement witness; it is
// never equal.
#[test]
fn dependent_edits_that_really_differ_are_never_equal() {
    let a = tsql_out("SET @x = @p + 1; SET @x = @x * 2;");
    let b = tsql_out("SET @x = @p + 2; SET @x = (@x - 1) * 3;");
    let r = run(&a, &b, T);
    assert_ne!(r.verdict(), Verdict::Equal, "{:#?}", r.findings);
    assert!(edit_differents(&r).is_empty(), "{:#?}", r.findings);
}

// Edits that do not feed each other keep their witnesses.
#[test]
fn independent_edits_keep_their_witnesses() {
    let p = |k: u32, m: u32| {
        format!(
            "CREATE PROCEDURE dbo.O @p INT, @q INT, @x INT OUTPUT, @y INT OUTPUT AS
             BEGIN
               SET @x = @p + {k};
               SET @y = @q * {m};
             END"
        )
    };
    let r = run(&p(1, 2), &p(2, 3), T);
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
    assert_eq!(edit_differents(&r).len(), 2, "{:#?}", r.findings);
}

// One edit after an unchanged assignment still has a witness.
#[test]
fn edit_after_unchanged_code_keeps_its_witness() {
    let a = tsql_out("SET @x = @p + 1; SET @x = @x * 2;");
    let b = tsql_out("SET @x = @p + 1; SET @x = @x * 3;");
    let r = run(&a, &b, T);
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
    assert_eq!(edit_differents(&r).len(), 1, "{:#?}", r.findings);
}

// A changed condition on the way to the edit reads a value an earlier
// change set.
#[test]
fn guard_fed_by_an_earlier_change_is_not_a_witness() {
    let a = tsql_out("SET @x = @p + 1; IF @x > 3 SET @x = @x * 2;");
    let b = tsql_out("SET @x = @p + 2; IF @x > 3 SET @x = (@x - 1) * 2;");
    let r = run(&a, &b, T);
    assert!(edit_differents(&r).is_empty(), "{:#?}", r.findings);
    assert_ne!(r.verdict(), Verdict::Equal, "{:#?}", r.findings);
}

// The earlier change reaches the edit through an unchanged statement.
#[test]
fn change_reaching_the_edit_through_unchanged_code() {
    let a = tsql_out("DECLARE @t INT; SET @t = @p + 1; SET @x = @t; SET @x = @x * 2;");
    let b = tsql_out("DECLARE @t INT; SET @t = @p + 2; SET @x = @t; SET @x = (@x - 1) * 2;");
    let r = run(&a, &b, T);
    assert!(edit_differents(&r).is_empty(), "{:#?}", r.findings);
    assert_ne!(r.verdict(), Verdict::Equal, "{:#?}", r.findings);
}

// Compensation through a table: the first change writes the rows the
// second one reads.
#[test]
fn compensating_edits_through_a_table() {
    let p = |k: u32, sel: &str| {
        format!(
            "CREATE PROCEDURE dbo.bump @id INT AS
             BEGIN
               UPDATE dbo.item SET price = price + {k} WHERE item_id = @id;
               SELECT {sel} FROM dbo.item WHERE item_id = @id;
             END"
        )
    };
    let r = run(&p(1, "price"), &p(2, "price - 1"), T);
    assert!(edit_differents(&r).is_empty(), "{:#?}", r.findings);
    assert_ne!(r.verdict(), Verdict::Equal, "{:#?}", r.findings);
}

// The same through a T-SQL table variable.
#[test]
fn compensating_edits_through_a_table_variable() {
    let a = tsql_out(
        "DECLARE @t TABLE (v INT); INSERT INTO @t VALUES (@p + 1); SELECT @x = v * 2 FROM @t;",
    );
    let b = tsql_out(
        "DECLARE @t TABLE (v INT); INSERT INTO @t VALUES (@p + 2); SELECT @x = (v - 1) * 2 FROM @t;",
    );
    let r = run(&a, &b, T);
    assert!(edit_differents(&r).is_empty(), "{:#?}", r.findings);
    assert_ne!(r.verdict(), Verdict::Equal, "{:#?}", r.findings);
}

// Inside a loop, a change later in the body feeds an edit earlier in the
// body on the next iteration.
#[test]
fn compensating_edits_in_a_loop() {
    let a = tsql_out(
        "DECLARE @i INT = 0; SET @x = 0;
         WHILE @i < @p BEGIN SET @x = @x + 2; SET @i = @i + 1; END",
    );
    let b = tsql_out(
        "DECLARE @i INT = 0; SET @x = 0;
         WHILE @i < @p BEGIN SET @x = @x + 1; SET @i = @i + 1; SET @x = @x + 1; END",
    );
    let r = run(&a, &b, T);
    assert!(edit_differents(&r).is_empty(), "{:#?}", r.findings);
    assert_ne!(r.verdict(), Verdict::Equal, "{:#?}", r.findings);
}

// PL/pgSQL: the same compensating pair.
#[test]
fn compensating_edits_plpgsql() {
    let f = |first: &str, second: &str| {
        format!(
            "CREATE FUNCTION o(p integer) RETURNS integer LANGUAGE plpgsql AS $$
             DECLARE x integer;
             BEGIN
               x := {first};
               x := {second};
               RETURN x;
             END $$;"
        )
    };
    let r = run(&f("p + 1", "x * 2"), &f("p + 2", "(x - 1) * 2"), P);
    assert!(edit_differents(&r).is_empty(), "{:#?}", r.findings);
    assert_ne!(r.verdict(), Verdict::Equal, "{:#?}", r.findings);
    let r = run(&f("p + 1", "x * 2"), &f("p + 1", "x * 3"), P);
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
}

// A parameter default that changes together with the body code that reads
// the parameter: the "caller leaves it out" witness does not hold.
#[test]
fn default_change_with_compensating_body_change() {
    let a = "CREATE PROCEDURE dbo.D @p INT = 1, @x INT OUTPUT AS BEGIN SET @x = @p + 1; END";
    let b = "CREATE PROCEDURE dbo.D @p INT = 2, @x INT OUTPUT AS BEGIN SET @x = @p; END";
    let r = run(a, b, T);
    assert!(
        !r.findings
            .iter()
            .any(|f| f.verdict == Verdict::Different && f.witness.contains("leaves out")),
        "{:#?}",
        r.findings
    );
    assert_ne!(r.verdict(), Verdict::Equal, "{:#?}", r.findings);
}
