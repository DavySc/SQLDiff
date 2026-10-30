//! Scalar user functions across T-SQL and PostgreSQL: a call inside an
//! expression is the same call on both sides when the catalogs hold the
//! function and every argument reaches its parameter unchanged; and a
//! function's own comparison minds that PostgreSQL ignores the length,
//! precision and scale of its parameter and return types.
//!
//! Facts (SQL Server 2022 and PostgreSQL 18, 2026-10-30,
//! eval/user-functions): `RETURNS decimal(10,2)` returns 1.005 as 1.01 in
//! SQL Server and as 1.005 in PostgreSQL; `RETURNS varchar(5)` returns
//! 'abcdefgh' as 'abcde' and 'abcdefgh'; a `varchar(5)` parameter receives
//! 'abcde' and 'abcdefgh'; a `char(5)` parameter receives 'ab   ' and 'ab'.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;

const CAT_T: &str = "CREATE SCHEMA App;
GO
CREATE TABLE App.Items (Id int NOT NULL PRIMARY KEY, CodeId int NULL, Price decimal(10,2) NULL);
GO
CREATE FUNCTION App.GetCode(@Code nvarchar(5))
RETURNS int
AS
BEGIN
    RETURN 1;
END;
GO
CREATE FUNCTION App.Price(@Qty int, @Unit decimal(10,2))
RETURNS decimal(10,2)
AS
BEGIN
    RETURN @Qty * @Unit;
END;
";

const CAT_P: &str = "CREATE SCHEMA app;
CREATE TABLE app.items (id integer NOT NULL PRIMARY KEY, codeid integer NULL, price numeric(10,2) NULL);
CREATE FUNCTION app.getcode(p_code varchar(5)) RETURNS integer
LANGUAGE plpgsql AS $$ BEGIN RETURN 1; END $$;
CREATE FUNCTION app.price(p_qty integer, p_unit numeric(10,2)) RETURNS numeric(10,2)
LANGUAGE plpgsql AS $$ BEGIN RETURN p_qty * p_unit; END $$;
";

fn opts() -> Options {
    Options {
        catalog_a: vec![Artifact::new("cat.sql", T, CAT_T)],
        catalog_b: vec![Artifact::new("cat.pg.sql", P, CAT_P)],
        ..Options::default()
    }
}

fn run_with(a: &str, b: &str, o: &Options) -> Report {
    diff(
        &Artifact::new("a.sql", T, a),
        &Artifact::new("b.sql", P, b),
        o,
    )
}

fn asms(r: &Report) -> Vec<&str> {
    r.assumptions.iter().map(|a| a.id.as_str()).collect()
}

fn caller_t(body: &str) -> String {
    format!(
        "CREATE PROCEDURE App.Caller @Id int, @Code nvarchar(5), @Long nvarchar(20), @Qty int\nAS\nBEGIN\n    SET NOCOUNT ON;\n    SET XACT_ABORT ON;\n    {body}\nEND;"
    )
}

fn caller_p(body: &str) -> String {
    format!(
        "CREATE PROCEDURE app.caller(p_id integer, p_code varchar(5), p_long varchar(20), p_qty integer)\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  {body}\nEND;\n$$;"
    )
}

fn compare(t: &str, p: &str) -> Report {
    run_with(&caller_t(t), &caller_p(p), &opts())
}

fn not_shown(r: &Report, needle: &str) {
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.verdict()
    );
    let all: Vec<&str> = r
        .findings
        .iter()
        .map(|f| f.witness.as_str())
        .chain(r.unknown_reasons.iter().map(String::as_str))
        .collect();
    assert!(all.iter().any(|x| x.contains(needle)), "{needle}: {all:#?}");
}

#[test]
fn a_call_whose_arguments_fit_is_the_same_call() {
    for (t, p) in [
        (
            "UPDATE App.Items SET CodeId = App.GetCode(N'AB') WHERE Id = @Id;",
            "UPDATE app.items SET codeid = app.getcode('AB') WHERE id = p_id;",
        ),
        (
            "UPDATE App.Items SET CodeId = App.GetCode(@Code) WHERE Id = @Id;",
            "UPDATE app.items SET codeid = app.getcode(p_code) WHERE id = p_id;",
        ),
        (
            "UPDATE App.Items SET CodeId = [App].[GetCode](@Code) WHERE Id = @Id;",
            "UPDATE app.items SET codeid = app.getcode(p_code) WHERE id = p_id;",
        ),
    ] {
        let r = compare(t, p);
        assert_eq!(
            r.verdict(),
            Verdict::EqualUnderAssumptions,
            "{t}\n{:#?}\n{:?}",
            r.findings,
            r.unknown_reasons
        );
        let ids = asms(&r);
        assert!(ids.contains(&"ASM-UDF-CALLEE:app.getcode"), "{ids:?}");
        assert!(ids.contains(&"ASM-UDF-CALL-CONTEXT"), "{ids:?}");
    }
}

#[test]
fn an_argument_the_parameter_changes_is_not_the_same_call() {
    // nvarchar(20) into nvarchar(5): SQL Server cuts it, PostgreSQL does not.
    let r = compare(
        "UPDATE App.Items SET CodeId = App.GetCode(@Long) WHERE Id = @Id;",
        "UPDATE app.items SET codeid = app.getcode(p_long) WHERE id = p_id;",
    );
    not_shown(&r, "varchar(5)");
    // 2.555 into decimal(10,2): SQL Server rounds it, PostgreSQL does not.
    let r = compare(
        "UPDATE App.Items SET Price = App.Price(@Qty, 2.555) WHERE Id = @Id;",
        "UPDATE app.items SET price = app.price(p_qty, 2.555) WHERE id = p_id;",
    );
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.verdict()
    );
}

#[test]
fn another_function_or_arguments_differ() {
    let r = compare(
        "UPDATE App.Items SET CodeId = App.GetCode(@Code) WHERE Id = @Id;",
        "UPDATE app.items SET codeid = app.getcode('AB') WHERE id = p_id;",
    );
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.verdict()
    );
    let r = compare(
        "UPDATE App.Items SET Price = App.Price(@Qty, 2.50) WHERE Id = @Id;",
        "UPDATE app.items SET price = app.price(p_id, 2.50) WHERE id = p_id;",
    );
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.verdict()
    );
}

#[test]
fn a_function_missing_from_a_catalog_is_not_compared() {
    let r = compare(
        "UPDATE App.Items SET CodeId = App.Missing(@Code) WHERE Id = @Id;",
        "UPDATE app.items SET codeid = app.missing(p_code) WHERE id = p_id;",
    );
    not_shown(&r, "app.missing");
}

#[test]
fn postgresql_ignores_a_return_types_precision_and_scale() {
    let t = "CREATE FUNCTION s.rn(@x decimal(10,3)) RETURNS decimal(10,2)\nAS\nBEGIN\n  RETURN @x;\nEND\n";
    let p = "CREATE FUNCTION s.rn(p_x numeric(10,3)) RETURNS numeric(10,2)\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  RETURN p_x;\nEND;\n$$;\n";
    let r = run_with(t, p, &Options::default());
    not_shown(&r, "decimal(10,2)");
    let t =
        "CREATE FUNCTION s.rv(@x varchar(10)) RETURNS varchar(5)\nAS\nBEGIN\n  RETURN @x;\nEND\n";
    let p = "CREATE FUNCTION s.rv(p_x varchar(10)) RETURNS varchar(5)\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  RETURN p_x;\nEND;\n$$;\n";
    let r = run_with(t, p, &Options::default());
    not_shown(&r, "varchar(5)");
    // A value that already has the type returns unchanged on both sides.
    let t =
        "CREATE FUNCTION s.ok(@x varchar(5)) RETURNS varchar(5)\nAS\nBEGIN\n  RETURN @x;\nEND\n";
    let p = "CREATE FUNCTION s.ok(p_x varchar(5)) RETURNS varchar(5)\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  RETURN p_x;\nEND;\n$$;\n";
    let r = run_with(t, p, &Options::default());
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:?}",
        r.unknown_reasons
    );
}

#[test]
fn postgresql_ignores_a_parameter_types_length() {
    let t =
        "CREATE FUNCTION s.pv(@x varchar(5)) RETURNS varchar(20)\nAS\nBEGIN\n  RETURN @x;\nEND\n";
    let p = "CREATE FUNCTION s.pv(p_x varchar(5)) RETURNS varchar(20)\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  RETURN p_x;\nEND;\n$$;\n";
    let r = run_with(t, p, &Options::default());
    assert_eq!(r.verdict(), Verdict::EqualUnderAssumptions);
    assert!(
        asms(&r).contains(&"ASM-PG-PARAMETER-TYPMOD"),
        "{:?}",
        asms(&r)
    );
    // No length, nothing to assume.
    let t = "CREATE FUNCTION s.pi(@x int) RETURNS int\nAS\nBEGIN\n  RETURN @x;\nEND\n";
    let p = "CREATE FUNCTION s.pi(p_x integer) RETURNS integer\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  RETURN p_x;\nEND;\n$$;\n";
    let r = run_with(t, p, &Options::default());
    assert!(
        !asms(&r).contains(&"ASM-PG-PARAMETER-TYPMOD"),
        "{:?}",
        asms(&r)
    );
}
