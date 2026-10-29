//! T-SQL RAISERROR compared with the PL/pgSQL RAISE it became: the message
//! text (literal pieces and shown values, with T-SQL's `(null)` for a NULL
//! value), a message against an error, and the assumptions the T-SQL text
//! rules need (2,047-character cut, empty message, a format variable). Each
//! rule has an equal pair and a near miss.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const CATALOG_T: &str = "CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, v int NULL);";
const CATALOG_P: &str = "CREATE TABLE t (id integer NOT NULL PRIMARY KEY, v integer NULL);";

/// A T-SQL procedure with `body`, against a PostgreSQL one with `body_p`.
/// Both declare `@m nvarchar(max)`, `@e nvarchar(100)`, `@n int`, `@b bigint`.
fn run(body: &str, body_p: &str) -> Report {
    let a = format!(
        "CREATE PROCEDURE dbo.p @id int\nAS\nBEGIN\n  SET NOCOUNT ON;\n  DECLARE @m nvarchar(max) = N'start';\n  DECLARE @e nvarchar(100) = N'e';\n  DECLARE @n int = 3;\n  DECLARE @b bigint = 4;\n{body}\nEND\n"
    );
    let b = format!(
        "CREATE OR REPLACE PROCEDURE p(p_id integer)\nLANGUAGE plpgsql\nAS $$\nDECLARE\n  v_m text;\n  v_e varchar(100);\n  v_n integer;\n  v_b bigint;\nBEGIN\n  v_m := 'start';\n  v_e := 'e';\n  v_n := 3;\n  v_b := 4;\n{body_p}\nEND;\n$$;\n"
    );
    let opts = Options {
        catalog_a: vec![Artifact::new("ca.sql", Dialect::Tsql, CATALOG_T)],
        catalog_b: vec![Artifact::new("cb.sql", Dialect::Postgres, CATALOG_P)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, &a),
        &Artifact::new("b.sql", Dialect::Postgres, &b),
        &opts,
    )
}

fn ids(r: &Report) -> Vec<&str> {
    r.assumptions.iter().map(|x| x.id.as_str()).collect()
}

/// Equal under assumptions, nothing undecided.
fn equal(body: &str, body_p: &str) -> Report {
    let r = run(body, body_p);
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{body}\n{:#?}\n{:#?}",
        r.findings,
        r.unknown_reasons
    );
    assert!(r.unknown_reasons.is_empty(), "{:?}", r.unknown_reasons);
    r
}

fn not_equal(body: &str, body_p: &str) -> Report {
    let r = run(body, body_p);
    assert_ne!(r.verdict(), Verdict::Equal, "{body}\n{body_p}");
    assert_ne!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{body}\n{body_p}\n{:?}",
        ids(&r)
    );
    r
}

#[test]
fn an_informational_message_with_a_string_value() {
    let r = equal(
        "  RAISERROR('%s', 10, 1, @m) WITH NOWAIT;",
        "  RAISE NOTICE '%', COALESCE(v_m, '(null)');",
    );
    // nvarchar(max): the text may pass 2,047 characters.
    assert!(ids(&r).contains(&"ASM-RAISERROR-TEXT"), "{:?}", ids(&r));
    // T-SQL shows a NULL value as (null), PostgreSQL as <NULL>.
    not_equal(
        "  RAISERROR('%s', 10, 1, @m) WITH NOWAIT;",
        "  RAISE NOTICE '%', v_m;",
    );
    not_equal(
        "  RAISERROR('%s', 10, 1, @m);",
        "  RAISE NOTICE '%', COALESCE(v_m, '<NULL>');",
    );
    // An informational message is not an error.
    not_equal(
        "  RAISERROR('%s', 10, 1, @m);",
        "  RAISE EXCEPTION '%', COALESCE(v_m, '(null)') USING ERRCODE = 'P0001';",
    );
}

#[test]
fn literal_pieces_integers_and_percent_signs() {
    let r = equal(
        "  RAISERROR('[%s] n=%d 100%% %s', 0, 1, @e, @n, N'lit');",
        "  RAISE NOTICE '[%] n=% 100%% lit', COALESCE(v_e, '(null)'), COALESCE(CAST(v_n AS text), '(null)');",
    );
    // nvarchar(100), an int and 15 characters of text stay under 2,047.
    assert!(!ids(&r).contains(&"ASM-RAISERROR-TEXT"), "{:?}", ids(&r));
    not_equal(
        "  RAISERROR('[%s] n=%d', 0, 1, @e, @n);",
        "  RAISE NOTICE '(%) n=%', COALESCE(v_e, '(null)'), COALESCE(CAST(v_n AS text), '(null)');",
    );
    // The values must stay in their places.
    not_equal(
        "  RAISERROR('%s %s', 10, 1, @e, @m);",
        "  RAISE NOTICE '% %', COALESCE(v_m, '(null)'), COALESCE(v_e, '(null)');",
    );
    // A literal message against the same text, with or without a value.
    equal(
        "  RAISERROR('Adding Constraints', 0, 1) WITH NOWAIT;",
        "  RAISE NOTICE 'Adding Constraints';",
    );
    equal(
        "  RAISERROR('Adding %s', 0, 1, 'Constraints');",
        "  RAISE NOTICE '%', 'Adding Constraints';",
    );
    not_equal(
        "  RAISERROR('Adding Constraints', 0, 1);",
        "  RAISE NOTICE 'Adding constraints';",
    );
}

#[test]
fn values_whose_text_differs_are_not_shown_equal() {
    // %d of a bigint is error 2786 in T-SQL.
    not_equal(
        "  RAISERROR('%d', 10, 1, @b);",
        "  RAISE NOTICE '%', COALESCE(CAST(v_b AS text), '(null)');",
    );
    // %s of an int is error 2786 too.
    not_equal(
        "  RAISERROR('%s', 10, 1, @n);",
        "  RAISE NOTICE '%', COALESCE(CAST(v_n AS text), '(null)');",
    );
    // A blank-padded char is not compared.
    not_equal(
        "  DECLARE @c char(10);\n  RAISERROR('%s', 10, 1, @c);",
        "  DECLARE\n    v_c char(10);\n  BEGIN\n    RAISE NOTICE '%', COALESCE(v_c, '(null)');\n  END;",
    );
    // Specifications other than %s, %d, %i and %% are not read.
    not_equal(
        "  RAISERROR('%5s', 10, 1, @e);",
        "  RAISE NOTICE '%', COALESCE(v_e, '(null)');",
    );
}

#[test]
fn a_format_variable_needs_its_assumption() {
    let r = equal(
        "  RAISERROR(@e, 10, 1) WITH NOWAIT;",
        "  RAISE NOTICE '%', COALESCE(v_e, '');",
    );
    assert!(ids(&r).contains(&"ASM-RAISERROR-FORMAT"), "{:?}", ids(&r));
    // A NULL format is an empty message in T-SQL.
    not_equal(
        "  RAISERROR(@e, 10, 1);",
        "  RAISE NOTICE '%', COALESCE(v_e, '(null)');",
    );
}

#[test]
fn an_error_is_compared_with_an_exception() {
    let r = equal(
        "  RAISERROR('boom %s', 16, 1, @e);",
        "  RAISE EXCEPTION 'boom %', COALESCE(v_e, '(null)') USING ERRCODE = 'P0001';",
    );
    assert!(!ids(&r).contains(&"ASM-RAISERROR-TEXT"), "{:?}", ids(&r));
    not_equal("  RAISERROR('boom', 16, 1);", "  RAISE NOTICE 'boom';");
    // Another SQLSTATE is not read.
    not_equal(
        "  RAISERROR('boom', 16, 1);",
        "  RAISE EXCEPTION 'boom' USING ERRCODE = '22012';",
    );
    // A severity that is not a literal could be either.
    not_equal(
        "  DECLARE @s int = 16;\n  RAISERROR('boom', @s, 1);",
        "  RAISE EXCEPTION 'boom' USING ERRCODE = 'P0001';",
    );
}

#[test]
fn t_sql_carries_on_after_an_error() {
    // RAISERROR of severity 11 to 18 outside TRY does not stop the
    // procedure, even under XACT_ABORT ON; PL/pgSQL stops at RAISE.
    let r = not_equal(
        "  RAISERROR('boom', 16, 1);\n  UPDATE dbo.t SET v = 1 WHERE id = @id;",
        "  RAISE EXCEPTION 'boom' USING ERRCODE = 'P0001';\n  UPDATE t SET v = 1 WHERE id = p_id;",
    );
    assert!(
        r.findings.iter().any(|f| f.verdict == Verdict::Different),
        "{:#?}",
        r.findings
    );
    // An informational message carries on on both sides.
    equal(
        "  RAISERROR('step', 10, 1);\n  UPDATE dbo.t SET v = 1 WHERE id = @id;",
        "  RAISE NOTICE 'step';\n  UPDATE t SET v = 1 WHERE id = p_id;",
    );
}
