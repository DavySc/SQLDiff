//! Dynamic SQL whose text is known before it runs: the statement inside the
//! string is converted by the same rules and run with `EXECUTE '…'`; the
//! engine compares the two statements. Text that is not known stays not
//! converted.

use sqldiff_convert::{convert, Conversion, Status, Unit};
use sqldiff_core::Dialect;

fn pg(text: &str) -> Conversion {
    convert(text, Dialect::Tsql, Dialect::Postgres).unwrap()
}

/// The innermost unit whose input contains `needle`.
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

fn reason(u: &Unit) -> String {
    u.notes.first().map(|n| n.text.clone()).unwrap_or_default()
}

fn proc(body: &str) -> String {
    format!("CREATE PROCEDURE dbo.p @n sysname\nAS\nBEGIN\n    SET XACT_ABORT ON;\n    DECLARE @SQL nvarchar(max);\n{body}\nEND;\n")
}

#[test]
fn known_text_is_converted_and_compared() {
    let src =
        proc("    SET @SQL = N'UPDATE dbo.t SET a = N''x'' WHERE id = 2;';\n    EXECUTE (@SQL);");
    let c = pg(&src);
    assert!(
        c.output
            .contains("EXECUTE 'UPDATE t SET a = ''x'' WHERE id = 2';"),
        "{}",
        c.output
    );
    // The variable keeps the T-SQL text, which nothing runs any more.
    assert!(c
        .output
        .contains("v_sql := 'UPDATE dbo.t SET a = N''x'' WHERE id = 2;';"));
    let set = unit(&c, &src, "SET @SQL");
    assert!(!has_rule(set, "dynamic-sql.text"), "{:?}", set.notes);
    let exec = unit(&c, &src, "EXECUTE (@SQL)");
    assert_eq!(
        exec.status,
        Status::Assumptions,
        "{:?}\n{}",
        exec.notes,
        c.output
    );
    // The rules' notes on the statement inside say where they apply.
    assert!(exec
        .notes
        .iter()
        .any(|n| n.text.starts_with("In the dynamic SQL: ")));
}

#[test]
fn sp_executesql_with_a_literal() {
    let src = proc("    EXEC sp_executesql N'DELETE FROM dbo.t WHERE id = 3';");
    let c = pg(&src);
    assert!(
        c.output.contains("EXECUTE 'DELETE FROM t WHERE id = 3';"),
        "{}",
        c.output
    );
    let u = unit(&c, &src, "sp_executesql");
    assert_eq!(u.status, Status::Assumptions, "{:?}", u.notes);
}

#[test]
fn text_built_from_a_parameter_is_not_converted() {
    let src = proc("    SET @SQL = N'CREATE ROLE ' + QUOTENAME(@n) + N';';\n    EXECUTE (@SQL);");
    let c = pg(&src);
    let exec = unit(&c, &src, "EXECUTE (@SQL)");
    assert_eq!(exec.status, Status::NotConverted);
    assert!(
        reason(exec).contains("not known before it runs"),
        "{}",
        reason(exec)
    );
    assert!(reason(exec).contains("@n"), "{}", reason(exec));
    // Its text would reach PostgreSQL as T-SQL if someone ran it by hand.
    assert!(has_rule(unit(&c, &src, "SET @SQL"), "dynamic-sql.text"));
}

#[test]
fn texts_that_run_differently_are_not_converted() {
    for (text, why) in [
        ("SELECT a FROM dbo.t", "query"),
        ("INSERT INTO #x VALUES (1)", "#temp"),
        ("DELETE FROM dbo.t; DELETE FROM dbo.u", "2 statements"),
        ("UPDATE dbo.t SET a = @x", "@x"),
        ("SET NOCOUNT ON", "SET"),
    ] {
        let src = proc(&format!("    EXECUTE (N'{text}');"));
        let c = pg(&src);
        let u = unit(&c, &src, "EXECUTE (N'");
        assert_eq!(u.status, Status::NotConverted, "{text}");
        assert!(reason(u).contains(why), "{why}: {}", reason(u));
    }
}

/// `EXEC (N'EXEC dbo.q')` is not turned into `EXECUTE 'CALL q()'`: a
/// procedure called through `EXECUTE` cannot commit, and the callee's
/// conversion may hold `COMMIT` (`tsql.txn.autocommit`), which PostgreSQL
/// then refuses with 2D000 and writes nothing.
#[test]
fn a_procedure_call_inside_the_text_is_not_converted() {
    for text in ["EXEC dbo.q", "EXECUTE dbo.q 1"] {
        let src = proc(&format!("    EXECUTE (N'{text}');"));
        let c = pg(&src);
        assert!(!c.output.contains("CALL q("), "{text}: {}", c.output);
        let u = unit(&c, &src, "EXECUTE (N'");
        assert_eq!(u.status, Status::NotConverted, "{text}: {:?}", u.notes);
        let why = reason(u);
        assert!(
            why.starts_with("dynamic SQL with known text, not converted")
                && why.contains("calls a procedure")
                && why.contains("cannot commit")
                && why.contains("EXEC dbo.q"),
            "{text}: {why}"
        );
    }
}

#[test]
fn a_statement_with_no_rule_inside_the_text_is_not_converted() {
    let src = proc("    EXECUTE (N'CREATE SERVER AUDIT a TO APPLICATION_LOG');");
    let c = pg(&src);
    let u = unit(&c, &src, "EXECUTE (N'");
    assert_eq!(u.status, Status::NotConverted);
    assert!(
        reason(u).starts_with("dynamic SQL with known text, not converted"),
        "{}",
        reason(u)
    );
}

#[test]
fn null_and_empty_text_are_not_converted() {
    let src = proc("    EXECUTE (@SQL);");
    let c = pg(&src);
    assert!(reason(unit(&c, &src, "EXECUTE (@SQL)")).contains("NULL"));
    let src = proc("    SET @SQL = N'';\n    EXECUTE (@SQL);");
    let c = pg(&src);
    assert!(reason(unit(&c, &src, "EXECUTE (@SQL)")).contains("empty"));
}

#[test]
fn inside_an_if_it_is_converted_but_not_proven() {
    let src = proc("    IF @n IS NULL\n    BEGIN\n        EXECUTE (N'DELETE FROM dbo.t WHERE id = 3');\n    END;");
    let c = pg(&src);
    assert!(
        c.output.contains("EXECUTE 'DELETE FROM t WHERE id = 3';"),
        "{}",
        c.output
    );
    let u = unit(&c, &src, "EXECUTE (N'");
    assert_eq!(u.status, Status::Unproven, "{:?}", u.notes);
}

#[test]
fn db2_is_not_converted_yet() {
    let src = proc("    EXECUTE (N'DELETE FROM dbo.t WHERE id = 3');");
    let c = convert(&src, Dialect::Tsql, Dialect::Db2i).unwrap();
    let u = unit(&c, &src, "EXECUTE (N'");
    assert_eq!(u.status, Status::NotConverted);
    assert!(reason(u).contains("Db2 for i"), "{}", reason(u));
}

#[test]
fn sp_executesql_parameters_become_using() {
    let src = proc("    DECLARE @a int = 1;\n    DECLARE @id int = 2;\n    EXEC sp_executesql N'UPDATE dbo.t SET a = @v WHERE id = @k', N'@v int, @k int', @v = @a, @k = @id;");
    let c = pg(&src);
    assert!(
        c.output
            .contains("EXECUTE 'UPDATE t SET a = $1 WHERE id = $2' USING v_a, v_id;"),
        "{}",
        c.output
    );
    let u = unit(&c, &src, "sp_executesql");
    assert_eq!(u.status, Status::Assumptions, "{:?}\n{}", u.notes, c.output);
}

#[test]
fn parameters_passed_with_a_conversion_are_not_converted() {
    // A bigint passed to an int parameter: T-SQL converts it as it passes it.
    let src = proc("    DECLARE @b bigint = 1;\n    EXEC sp_executesql N'UPDATE dbo.t SET a = @v WHERE id = 1', N'@v int', @v = @b;");
    let c = pg(&src);
    let u = unit(&c, &src, "sp_executesql");
    assert_eq!(u.status, Status::NotConverted);
    assert!(reason(u).contains("parameter's own type"), "{}", reason(u));
    // A literal value.
    let src =
        proc("    EXEC sp_executesql N'UPDATE dbo.t SET a = @v WHERE id = 1', N'@v int', @v = 3;");
    let c = pg(&src);
    let u = unit(&c, &src, "sp_executesql");
    assert_eq!(u.status, Status::NotConverted);
    assert!(reason(u).contains("not a variable"), "{}", reason(u));
}

#[test]
fn only_text_that_reaches_a_refused_exec_stays_t_sql() {
    // The first text runs converted; the second has no rule. The variable
    // runs both, but only the second assignment's value reaches the refused
    // EXEC.
    let src = proc(
        "    SET @SQL = N'DELETE FROM dbo.t WHERE id = 3;';\n    EXECUTE (@SQL);\n    SET @SQL = N'CREATE TYPE dbo.l AS TABLE (id int);';\n    EXECUTE (@SQL);",
    );
    let c = pg(&src);
    let first = unit(&c, &src, "SET @SQL = N'DELETE");
    assert!(!has_rule(first, "dynamic-sql.text"), "{:?}", first.notes);
    let second = unit(&c, &src, "SET @SQL = N'CREATE TYPE");
    assert!(has_rule(second, "dynamic-sql.text"), "{:?}", second.notes);
    // The converted EXEC pairs with its T-SQL statement past the refused one.
    let exec = unit(&c, &src, "EXECUTE (@SQL);\n    SET");
    assert_eq!(
        exec.status,
        Status::Assumptions,
        "{:?}\n{}",
        exec.notes,
        c.output
    );
}

#[test]
fn text_read_through_unknown_assignments_keeps_every_one_t_sql() {
    // The refused EXEC's text is not known (a parameter), so any assignment
    // may reach it.
    let src = proc(
        "    SET @SQL = N'DELETE FROM dbo.t WHERE id = 3;';\n    EXECUTE (@SQL);\n    SET @SQL = N'DROP TABLE ' + QUOTENAME(@n);\n    EXECUTE (@SQL);",
    );
    let c = pg(&src);
    let first = unit(&c, &src, "SET @SQL = N'DELETE");
    assert!(has_rule(first, "dynamic-sql.text"), "{:?}", first.notes);
}

#[test]
fn a_multi_line_string_keeps_its_value() {
    // The lines inside the literal are neither indented nor dedented.
    let src = proc(
        "    SET @SQL = N'\nDELETE FROM dbo.t\n  WHERE id = 3\n          ';\n    EXECUTE (@SQL);",
    );
    let c = pg(&src);
    assert!(
        c.output
            .contains("v_sql := '\nDELETE FROM dbo.t\n  WHERE id = 3\n          ';\n"),
        "{}",
        c.output
    );
    let set = unit(&c, &src, "SET @SQL");
    assert_eq!(set.status, Status::Assumptions, "{:?}", set.notes);
}
