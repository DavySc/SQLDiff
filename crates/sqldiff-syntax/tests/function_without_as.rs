//! `AS` is optional before a T-SQL function's body (`RETURNS … BEGIN`,
//! `RETURNS TABLE RETURN …`); the parser must build the same routine with or
//! without it, and keep requiring it for a procedure.

use sqldiff_syntax::{parse, Dialect, SyntaxKind};

fn body_and_problems(text: &str) -> (bool, Vec<String>) {
    let p = parse(text, Dialect::Tsql);
    let has_body = p
        .syntax()
        .descendants()
        .any(|n| n.kind() == SyntaxKind::ROUTINE_BODY);
    (
        has_body,
        p.diagnostics.iter().map(|d| d.message.clone()).collect(),
    )
}

#[test]
fn a_scalar_function_may_leave_out_as() {
    for text in [
        "CREATE FUNCTION f ()\nRETURNS bit\nBEGIN\n  RETURN 1\nEND\nGO\n",
        "CREATE FUNCTION dbo.f (@x int)\nRETURNS int WITH SCHEMABINDING\nAS\nBEGIN\n  RETURN @x\nEND\nGO\n",
        "ALTER FUNCTION f () RETURNS int BEGIN RETURN 1 END",
    ] {
        let (body, problems) = body_and_problems(text);
        assert!(body && problems.is_empty(), "{text:?}: body={body} {problems:?}");
    }
}

#[test]
fn a_table_valued_function_may_leave_out_as() {
    for text in [
        "CREATE FUNCTION f ()\nRETURNS TABLE\nRETURN (SELECT 1 AS a)\nGO\n",
        "CREATE FUNCTION f ()\nRETURNS @t TABLE (a int, begin_at int)\nBEGIN\n  INSERT @t VALUES (1, 2)\n  RETURN\nEND\nGO\n",
    ] {
        let (body, problems) = body_and_problems(text);
        assert!(body && problems.is_empty(), "{text:?}: body={body} {problems:?}");
    }
}

#[test]
fn the_body_holds_the_statements_not_the_header() {
    let text = "CREATE FUNCTION f ()\nRETURNS bit\nBEGIN\n  RETURN 1\nEND\nGO\n";
    let p = parse(text, Dialect::Tsql);
    let body = p
        .syntax()
        .descendants()
        .find(|n| n.kind() == SyntaxKind::ROUTINE_BODY)
        .unwrap();
    let t = body.text().to_string();
    assert!(t.trim_start().starts_with("BEGIN"), "{t:?}");
    assert!(!t.contains("RETURNS"), "{t:?}");
}

#[test]
fn a_procedure_still_needs_as() {
    let (body, problems) = body_and_problems("CREATE PROCEDURE p\nBEGIN\n  SELECT 1\nEND\nGO\n");
    assert!(!body);
    assert!(
        problems.iter().any(|m| m.contains("expected AS")),
        "{problems:?}"
    );
}

#[test]
fn a_function_with_nothing_after_the_header_is_still_a_problem() {
    let (body, problems) = body_and_problems("CREATE FUNCTION f () RETURNS int\nGO\n");
    assert!(!body);
    assert!(
        problems.iter().any(|m| m.contains("expected AS")),
        "{problems:?}"
    );
}
