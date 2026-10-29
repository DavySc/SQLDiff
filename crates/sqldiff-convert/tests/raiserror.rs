//! T-SQL RAISERROR with a literal severity becomes PL/pgSQL RAISE (rule
//! `tsql.stmt.raiserror`): 0 to 10 an informational `RAISE NOTICE`, 11 to
//! 18 `RAISE EXCEPTION … USING ERRCODE = 'P0001'`. Values are shown as
//! T-SQL shows them (`(null)` for NULL), and the engine compares the
//! message with what it became. Forms whose meaning depends on a value
//! (a variable severity, a message number) or that the rule does not read
//! (width, precision, flags, WITH LOG) stay refused, as does Db2 for i.

use sqldiff_convert::{check, rewrite_in, Conversion, Status, Unit};
use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{Artifact, Dialect, Options};

const TABLES: &str = "CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, v int NULL);\n";

/// A procedure with `body` after declarations of `@m nvarchar(max)`,
/// `@e nvarchar(100)`, `@n int` and `@b bigint`, converted and checked with
/// the table in both catalogs.
fn convert_to(body: &str, to: Dialect) -> Conversion {
    let src = format!(
        "CREATE PROCEDURE dbo.p @id int\nAS\nBEGIN\n  SET NOCOUNT ON;\n  DECLARE @m nvarchar(max) = N'start';\n  DECLARE @e nvarchar(100) = N'e';\n  DECLARE @n int = 3;\n  DECLARE @b bigint = 4;\n{body}\nEND\n"
    );
    let mut cat = Catalog::default();
    let root = sqldiff_syntax::parse(TABLES, Dialect::Tsql).syntax();
    ddl::load_tree(&mut cat, &root, Dialect::Tsql);
    let target = rewrite_in(TABLES, Dialect::Tsql, to, None)
        .output()
        .to_string();
    let opts = Options {
        catalog_a: vec![Artifact::new("t.sql", Dialect::Tsql, TABLES)],
        catalog_b: vec![Artifact::new("t.sql", to, &target)],
        ..Options::default()
    };
    check(&src, rewrite_in(&src, Dialect::Tsql, to, Some(&cat)), &opts)
}

fn pg(body: &str) -> Conversion {
    convert_to(body, Dialect::Postgres)
}

/// The unit of the first statement on source line `line`.
fn at(c: &Conversion, line: usize) -> &Unit {
    c.units
        .iter()
        .find(|u| u.src.line == line)
        .unwrap_or_else(|| panic!("no unit on line {line}"))
}

/// The RAISERROR: the first statement of the body.
fn raise(c: &Conversion) -> &Unit {
    at(c, 9)
}

fn out<'c>(c: &'c Conversion, u: &Unit) -> &'c str {
    c.output[u.out.start..u.out.end].trim()
}

fn rules(u: &Unit) -> Vec<&str> {
    u.notes.iter().map(|n| n.rule.as_str()).collect()
}

/// Converted, and the engine showed the message equal: the unit is under
/// assumptions, never not proven.
fn proven(body: &str, want: &str) -> Conversion {
    let c = pg(body);
    let u = raise(&c);
    assert_eq!(out(&c, u), want, "{body}");
    assert_eq!(
        u.status,
        Status::Assumptions,
        "{body}\n{:#?}\n{:#?}",
        u.notes,
        c.report.unknown_reasons
    );
    assert!(
        c.report.unknown_reasons.is_empty(),
        "{:?}",
        c.report.unknown_reasons
    );
    c
}

fn refused(body: &str, why: &str) {
    for to in [Dialect::Postgres, Dialect::Db2i] {
        let c = convert_to(body, to);
        let u = raise(&c);
        assert_eq!(u.status, Status::NotConverted, "{to:?} {body}");
        let reason = &u.notes[0].text;
        if to == Dialect::Postgres {
            assert!(reason.contains(why), "{body}: {reason}");
        }
    }
}

#[test]
fn an_informational_message_becomes_a_notice() {
    // Ola Hallengren's progress lines.
    let c = proven(
        "  RAISERROR('%s',10,1,@m) WITH NOWAIT;",
        "RAISE NOTICE '%', COALESCE(v_m, '(null)');",
    );
    let r = rules(raise(&c));
    assert!(r.contains(&"ASM-RAISERROR-INFO"), "{r:?}");
    // nvarchar(max): the message may be longer than 2,047 characters.
    assert!(r.contains(&"ASM-RAISERROR-TEXT"), "{r:?}");
    // A literal message: no value, nothing to cut.
    let c = proven(
        "  RAISERROR('Adding Constraints', 0, 1) WITH NOWAIT;",
        "RAISE NOTICE 'Adding Constraints';",
    );
    assert!(!rules(raise(&c)).contains(&"ASM-RAISERROR-TEXT"));
}

#[test]
fn substitutions_constants_and_percent_signs() {
    proven(
        "  RAISERROR('[%s] n=%i 100%% %s %d', 10, 1, @e, @n, N'it''s 5%', 7);",
        "RAISE NOTICE '[%] n=% 100%% it''s 5%% 7', COALESCE(v_e, '(null)'), COALESCE(CAST(v_n AS text), '(null)');",
    );
}

#[test]
fn a_format_variable_is_shown_as_it_is() {
    let c = proven(
        "  RAISERROR(@e, 10, 1) WITH NOWAIT;",
        "RAISE NOTICE '%', COALESCE(v_e, '');",
    );
    assert!(rules(raise(&c)).contains(&"ASM-RAISERROR-FORMAT"));
}

#[test]
fn an_error_becomes_an_exception() {
    let c = proven(
        "  RAISERROR('no row %d', 16, 1, @id);",
        "RAISE EXCEPTION 'no row %', COALESCE(CAST(p_id AS text), '(null)') USING ERRCODE = 'P0001';",
    );
    assert!(rules(raise(&c)).contains(&"ASM-THROW-ERRCODE"));
    // T-SQL carries on after RAISERROR (outside TRY, even under XACT_ABORT
    // ON); PostgreSQL stops. The write after it is not shown equal.
    let c = pg("  RAISERROR('no row', 16, 1);\n  UPDATE dbo.t SET v = 1 WHERE id = @id;");
    assert_eq!(
        out(&c, raise(&c)),
        "RAISE EXCEPTION 'no row' USING ERRCODE = 'P0001';"
    );
    let u = raise(&c);
    assert_eq!(u.status, Status::Differs, "{:#?}", u.notes);
    let found: Vec<&str> = u
        .findings
        .iter()
        .map(|&i| c.report.findings[i].rule.as_str())
        .collect();
    assert!(found.contains(&"equiv.error-continues"), "{found:?}");
}

#[test]
fn forms_that_depend_on_a_value_or_are_not_read_stay_refused() {
    refused(
        "  RAISERROR('%s', @n, 1, @e);",
        "severity that is not a literal",
    );
    refused("  RAISERROR('x', 20, 1) WITH LOG;", "WITH LOG");
    refused("  RAISERROR('x', 19, 1);", "severity 19 or more");
    refused("  RAISERROR('x', 16, 1) WITH SETERROR;", "SETERROR");
    refused("  RAISERROR(50001, 16, 1);", "message number");
    refused("  RAISERROR(@n, 16, 1);", "message number");
    refused(
        "  RAISERROR('%5s', 10, 1, @e);",
        "other than %s, %d, %i or %%",
    );
    refused(
        "  RAISERROR('%x', 10, 1, @n);",
        "other than %s, %d, %i or %%",
    );
    refused(
        "  RAISERROR('50% done', 10, 1);",
        "other than %s, %d, %i or %%",
    );
    refused(
        "  RAISERROR('%s %s', 10, 1, @e);",
        "more specifications than arguments",
    );
    refused("  RAISERROR('%s', 10, 1, @e, @m);", "more arguments");
    refused(
        "  RAISERROR(@e, 10, 1, @m);",
        "format variable and arguments",
    );
    // %d of a bigint and %s of an int are error 2786 in T-SQL.
    refused("  RAISERROR('%d', 10, 1, @b);", "error 2786");
    refused("  RAISERROR('%s', 10, 1, @n);", "error 2786");
    refused(
        "  RAISERROR('%s', 10, 1, LEFT(@e, 2));",
        "form not understood",
    );
    refused("  RAISERROR('café', 10, 1);", "code page");
}
