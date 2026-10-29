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
