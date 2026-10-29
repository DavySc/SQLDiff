//! Procedure calls and object comments compared across dialects: T-SQL
//! `EXEC` against PostgreSQL `CALL`, `sp_addextendedproperty` against
//! `COMMENT ON`. Each rule has an equal pair and a near miss; anything the
//! model does not read stays unknown.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;

const CAT_T: &str = "CREATE SCHEMA Sales;
GO
CREATE TABLE dbo.Things (
    Id int NOT NULL CONSTRAINT PK_Things PRIMARY KEY,
    Name nvarchar(50) NOT NULL,
    Qty int NOT NULL,
    Flag bit NOT NULL,
    CONSTRAINT UQ_Things_Name UNIQUE (Name)
);
GO
CREATE INDEX IX_Things_Qty ON dbo.Things (Qty);
GO
CREATE VIEW dbo.ThingNames AS SELECT Name FROM dbo.Things;
GO
CREATE PROCEDURE dbo.AddThing @Name nvarchar(50), @Qty int = 1, @Flag bit = 0, @Id int OUTPUT
AS
BEGIN
    SET @Id = 1;
END;
GO
CREATE PROCEDURE dbo.Touch @When date, @Amount decimal(10,2)
AS
BEGIN
    RETURN;
END;
GO
CREATE PROCEDURE dbo.Fetch @Id int, @Name nvarchar(50) OUTPUT
AS
BEGIN
    SET @Name = N'x';
END;
";

const CAT_P: &str = "CREATE SCHEMA sales;
CREATE TABLE Things (
    Id integer NOT NULL CONSTRAINT PK_Things PRIMARY KEY,
    Name varchar(50) NOT NULL,
    Qty integer NOT NULL,
    Flag boolean NOT NULL,
    CONSTRAINT UQ_Things_Name UNIQUE (Name)
);
CREATE INDEX IX_Things_Qty ON Things (Qty);
CREATE VIEW ThingNames AS SELECT Name FROM Things;
CREATE PROCEDURE AddThing(p_name varchar(50), p_qty integer DEFAULT 1, p_flag boolean DEFAULT FALSE, INOUT p_id integer DEFAULT NULL)
LANGUAGE plpgsql AS $$ BEGIN p_id := 1; END $$;
CREATE PROCEDURE Touch(p_when date, p_amount numeric(10,2))
LANGUAGE plpgsql AS $$ BEGIN RETURN; END $$;
CREATE PROCEDURE Fetch(p_id integer, OUT p_name varchar(50))
LANGUAGE plpgsql AS $$ BEGIN p_name := 'x'; END $$;
";

fn opts(cat_t: &str, cat_p: &str) -> Options {
    Options {
        catalog_a: vec![Artifact::new("cat.sql", T, cat_t)],
        catalog_b: vec![Artifact::new("cat.pg.sql", P, cat_p)],
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

fn run(a: &str, b: &str) -> Report {
    run_with(a, b, &opts(CAT_T, CAT_P))
}

fn rules(r: &Report) -> Vec<&str> {
    r.findings.iter().map(|f| f.rule.as_str()).collect()
}

fn asms(r: &Report) -> Vec<&str> {
    r.assumptions.iter().map(|a| a.id.as_str()).collect()
}

/// No call or comment left undecided, nothing different.
fn equal(a: &str, b: &str) -> Report {
    let r = run(a, b);
    assert!(
        !r.unknown_reasons
            .iter()
            .any(|x| x.contains("call") || x.contains("comment")),
        "{:?}",
        r.unknown_reasons
    );
    assert!(
        !r.findings.iter().any(|f| f.verdict == Verdict::Different),
        "{:#?}",
        r.findings
    );
    r
}

/// Different by `rule`, and the witness mentions `needle`.
fn differs(a: &str, b: &str, rule: &str, needle: &str) {
    let r = run(a, b);
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.unknown_reasons);
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == rule)
        .unwrap_or_else(|| panic!("{rule}: {:?} {:?}", rules(&r), r.unknown_reasons));
    assert!(f.witness.contains(needle), "{}", f.witness);
}

/// Not decided, and a reason mentions `needle`.
fn unknown_with(a: &str, b: &str, o: &Options, needle: &str) {
    let r = run_with(a, b, o);
    assert!(
        r.unknown_reasons.iter().any(|x| x.contains(needle)),
        "{needle}: {:?} {:?}",
        r.unknown_reasons,
        rules(&r)
    );
    assert_ne!(r.verdict(), Verdict::Equal);
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
}

fn unknown(a: &str, b: &str, needle: &str) {
    unknown_with(a, b, &opts(CAT_T, CAT_P), needle)
}

fn caller_t(body: &str) -> String {
    format!(
        "CREATE PROCEDURE dbo.Caller @n nvarchar(50)\nAS\nBEGIN\n    DECLARE @newid int;\n    DECLARE @nm nvarchar(50);\n    {body}\nEND;"
    )
}

fn caller_p(body: &str) -> String {
    format!(
        "CREATE PROCEDURE Caller(p_n varchar(50))\nLANGUAGE plpgsql\nAS $$\nDECLARE\n  v_newid integer;\n  v_nm varchar(50);\nBEGIN\n  {body}\nEND;\n$$;"
    )
}

#[test]
fn named_arguments_defaults_and_output() {
    let r = equal(
        &caller_t("EXEC dbo.AddThing @Name = @n, @Id = @newid OUTPUT;"),
        &caller_p("CALL AddThing(p_name => p_n, p_id => v_newid);"),
    );
    assert!(asms(&r).contains(&"ASM-CALLEE-CONVERTED"), "{:?}", asms(&r));
    // Positional, with every default spelled out on one side.
    equal(
        &caller_t("EXEC dbo.AddThing N'x', @Id = @newid OUTPUT;"),
        &caller_p("CALL AddThing('x', 1, FALSE, v_newid);"),
    );
    equal(
        &caller_t("EXEC dbo.AddThing N'x', DEFAULT, 1, @newid OUTPUT;"),
        &caller_p("CALL AddThing('x', p_flag => TRUE, p_id => v_newid);"),
    );
}

#[test]
fn argument_values_are_compared() {
    differs(
        &caller_t("EXEC dbo.AddThing N'x', @Id = @newid OUTPUT;"),
        &caller_p("CALL AddThing('x', 2, FALSE, v_newid);"),
        "equiv.call-argument",
        "parameter 2",
    );
    differs(
        &caller_t("EXEC dbo.AddThing N'x', @Flag = 1, @Id = @newid OUTPUT;"),
        &caller_p("CALL AddThing('x', p_flag => FALSE, p_id => v_newid);"),
        "equiv.call-argument",
        "parameter 3",
    );
    differs(
        "EXEC dbo.Touch '20200101', 12.5;",
        "CALL Touch('20200102', 12.5);",
        "equiv.call-argument",
        "parameter 1",
    );
    // Variables bound to other parameters.
    unknown(
        &caller_t("EXEC dbo.AddThing @Name = @n, @Id = @newid OUTPUT;"),
        &caller_p("CALL AddThing(p_name => p_n, p_id => v_nm);"),
        "not shown the same",
    );
}

#[test]
fn literal_conversions() {
    let r = equal(
        "EXEC dbo.Touch '20200101', 12.5;",
        "CALL Touch('20200101', 12.5);",
    );
    // PostgreSQL keeps 12.5 as written, SQL Server makes it 12.50.
    assert!(
        asms(&r).contains(&"ASM-PG-PARAMETER-TYPMOD"),
        "{:?}",
        asms(&r)
    );
    // SQL Server rounds to the parameter's scale, PostgreSQL does not.
    unknown(
        "EXEC dbo.Touch '20200101', 1.005;",
        "CALL Touch('20200101', 1.005);",
        "not modelled",
    );
    // SQL Server truncates to nvarchar(50); PostgreSQL ignores the length.
    let long = "x".repeat(51);
    unknown(
        &caller_t(&format!(
            "EXEC dbo.AddThing N'{long}', @Id = @newid OUTPUT;"
        )),
        &caller_p(&format!("CALL AddThing('{long}', p_id => v_newid);")),
        "not modelled",
    );
    // An integer is no boolean argument in PostgreSQL.
    unknown(
        &caller_t("EXEC dbo.AddThing N'x', @Flag = 1, @Id = @newid OUTPUT;"),
        &caller_p("CALL AddThing('x', p_flag => 1, p_id => v_newid);"),
        "not modelled",
    );
}

#[test]
fn modes_and_output_arguments() {
    // T-SQL does not copy back without OUTPUT; PostgreSQL always does.
    unknown(
        &caller_t("EXEC dbo.AddThing @Name = @n, @Id = @newid;"),
        &caller_p("CALL AddThing(p_name => p_n, p_id => v_newid);"),
        "output is not passed",
    );
    // An OUT parameter does not receive the value T-SQL's OUTPUT passes in.
    unknown(
        &caller_t("EXEC dbo.Fetch 1, @nm OUTPUT;"),
        &caller_p("CALL Fetch(1, v_nm);"),
        "does not",
    );
    unknown(
        &caller_t("EXEC dbo.Touch '20200101', 1 OUTPUT;"),
        &caller_p("CALL Touch('20200101', 1);"),
        "input parameter",
    );
}

#[test]
fn variable_types_must_pass_unchanged() {
    let t = "CREATE PROCEDURE dbo.Caller @n nvarchar(80)\nAS\nBEGIN\n    DECLARE @newid int;\n    EXEC dbo.AddThing @Name = @n, @Id = @newid OUTPUT;\nEND;";
    let p = "CREATE PROCEDURE Caller(p_n varchar(80))\nLANGUAGE plpgsql\nAS $$\nDECLARE\n  v_newid integer;\nBEGIN\n  CALL AddThing(p_name => p_n, p_id => v_newid);\nEND;\n$$;";
    unknown(t, p, "does not pass unchanged");
    let t = "CREATE PROCEDURE dbo.Caller @n nvarchar(50)\nAS\nBEGIN\n    DECLARE @newid bigint;\n    EXEC dbo.AddThing @Name = @n, @Id = @newid OUTPUT;\nEND;";
    let p = "CREATE PROCEDURE Caller(p_n varchar(50))\nLANGUAGE plpgsql\nAS $$\nDECLARE\n  v_newid bigint;\nBEGIN\n  CALL AddThing(p_name => p_n, p_id => v_newid);\nEND;\n$$;";
    unknown(t, p, "does not pass unchanged");
    // PostgreSQL refuses an INOUT variable that needs a cast, even
    // varchar(50) to text ("argument is not writable").
    let cat_t = format!(
        "{CAT_T}\nGO\nCREATE PROCEDURE dbo.Fix @s nvarchar(50) OUTPUT AS BEGIN SET @s = @s; END;"
    );
    let fix_p = |ty: &str| {
        format!("{CAT_P}\nCREATE PROCEDURE Fix(INOUT p_s {ty}) LANGUAGE plpgsql AS $$ BEGIN p_s := p_s; END $$;")
    };
    let t = caller_t("EXEC dbo.Fix @nm OUTPUT;");
    let p = caller_p("CALL Fix(v_nm);");
    unknown_with(
        &t,
        &p,
        &opts(&cat_t, &fix_p("text")),
        "only of the parameter's own type",
    );
    let r = run_with(&t, &p, &opts(&cat_t, &fix_p("character varying(50)")));
    assert!(
        !r.unknown_reasons.iter().any(|x| x.contains("call")),
        "{:?}",
        r.unknown_reasons
    );
    assert!(
        asms(&r).contains(&"ASM-PG-PARAMETER-TYPMOD"),
        "{:?}",
        asms(&r)
    );
}

#[test]
fn calls_not_read_stay_unknown() {
    // The return code: not even read as a call.
    unknown(
        "DECLARE @rc int;\nEXEC @rc = dbo.Touch '20200101', 1;",
        "CALL Touch('20200101', 1);",
        "`EXEC @RC =` is not covered",
    );
    unknown(
        "EXEC dbo.Touch '20200101', 1 WITH RECOMPILE;",
        "CALL Touch('20200101', 1);",
        "WITH",
    );
    // T-SQL accepts only constants and variables as arguments.
    unknown(
        "EXEC dbo.Touch '20200101', 1 + 1;",
        "CALL Touch('20200101', 1 + 1);",
        "only constants",
    );
    unknown(
        "EXEC dbo.Touch '20200101', 1;",
        "CALL Touch(p_when := '20200101', p_amount := 1);",
        "name := value",
    );
    unknown(
        "EXEC dbo.Touch @When = '20200101', 1;",
        "CALL Touch('20200101', 1);",
        "positional after a named",
    );
    unknown(
        "EXEC dbo.Touch @When = '20200101', @Nope = 1;",
        "CALL Touch('20200101', 1);",
        "no parameter @nope",
    );
    // Not in the catalogs, or no catalogs at all.
    unknown(
        "EXEC dbo.Missing 1;",
        "CALL Missing(1);",
        "catalog has no procedure",
    );
    unknown_with(
        "EXEC dbo.Touch '20200101', 1;",
        "CALL Touch('20200101', 1);",
        &Options::default(),
        "has no catalog",
    );
    // Overloaded in PostgreSQL (same number of parameters, other types).
    let cat_p = format!(
        "{CAT_P}\nCREATE PROCEDURE Touch(p_when text, p_amount numeric) LANGUAGE plpgsql AS $$ BEGIN RETURN; END $$;"
    );
    unknown_with(
        "EXEC dbo.Touch '20200101', 1;",
        "CALL Touch('20200101', 1);",
        &opts(CAT_T, &cat_p),
        "several procedures",
    );
    // Parameter types that differ: the callee's own comparison says so.
    let cat_p = CAT_P.replace("Touch(p_when date", "Touch(p_when text");
    unknown_with(
        "EXEC dbo.Touch '20200101', 1;",
        "CALL Touch('20200101', 1);",
        &opts(CAT_T, &cat_p),
        "parameter types differ",
    );
    // A call on one side only.
    unknown(
        "EXEC dbo.Touch '20200101', 1;\nEXEC dbo.Touch '20200102', 1;",
        "CALL Touch('20200101', 1);",
        "no counterpart",
    );
}

#[test]
fn calls_keep_their_place() {
    unknown(
        &caller_t("EXEC dbo.AddThing @Name = @n, @Id = @newid OUTPUT;\n    SET @nm = @n;"),
        &caller_p("v_nm := p_n;\n  CALL AddThing(p_name => p_n, p_id => v_newid);"),
        "same place",
    );
}

#[test]
fn a_failed_call_lets_tsql_continue() {
    differs(
        &caller_t("EXEC dbo.AddThing @Name = @n, @Id = @newid OUTPUT;\n    UPDATE dbo.Things SET Qty = 0 WHERE Id = @newid;"),
        &caller_p("CALL AddThing(p_name => p_n, p_id => v_newid);\n  UPDATE Things SET Qty = 0 WHERE Id = v_newid;"),
        "equiv.error-continues",
        "continues",
    );
}

fn extprop(value: &str, levels: &str) -> String {
    format!("EXEC sys.sp_addextendedproperty @name = N'Description', @value = {value}, @level0type = N'SCHEMA', @level0name = N'dbo', {levels};")
}

#[test]
fn extended_properties_become_comments() {
    let cases = [
        (
            "@level1type = N'TABLE', @level1name = N'Things'",
            "COMMENT ON TABLE public.Things IS 'Kept things';",
        ),
        (
            "@level1type = N'TABLE', @level1name = N'Things', @level2type = N'COLUMN', @level2name = N'Qty'",
            "COMMENT ON COLUMN public.Things.qty IS 'Kept things';",
        ),
        (
            "@level1type = N'VIEW', @level1name = N'ThingNames'",
            "COMMENT ON VIEW public.ThingNames IS 'Kept things';",
        ),
        (
            "@level1type = N'TABLE', @level1name = N'Things', @level2type = N'INDEX', @level2name = N'IX_Things_Qty'",
            "COMMENT ON INDEX public.IX_Things_Qty IS 'Kept things';",
        ),
        (
            "@level1type = N'TABLE', @level1name = N'Things', @level2type = N'CONSTRAINT', @level2name = N'UQ_Things_Name'",
            "COMMENT ON CONSTRAINT UQ_Things_Name ON public.Things IS 'Kept things';",
        ),
        (
            "@level1type = N'PROCEDURE', @level1name = N'Touch'",
            "COMMENT ON PROCEDURE public.Touch IS 'Kept things';",
        ),
    ];
    for (levels, pg) in cases {
        let r = equal(&extprop("N'Kept things'", levels), pg);
        assert!(
            asms(&r).contains(&"ASM-EXTPROP-COMMENT"),
            "{pg}: {:?}",
            asms(&r)
        );
        assert_eq!(r.verdict(), Verdict::EqualUnderAssumptions, "{pg}");
    }
    let schema = "EXEC sys.sp_addextendedproperty @name = N'Description', @value = N'Sales data', @level0type = N'SCHEMA', @level0name = N'Sales';";
    equal(schema, "COMMENT ON SCHEMA Sales IS 'Sales data';");
}

#[test]
fn comment_near_misses() {
    let table = "@level1type = N'TABLE', @level1name = N'Things'";
    differs(
        &extprop("N'Kept things'", table),
        "COMMENT ON TABLE public.Things IS 'Kept thing';",
        "equiv.comment-text",
        "Kept thing'",
    );
    // Another object: no counterpart.
    unknown(
        &extprop("N'Kept things'", table),
        "COMMENT ON TABLE public.ThingNames IS 'Kept things';",
        "no counterpart",
    );
    // A view commented as a table.
    unknown(
        &extprop(
            "N'Kept things'",
            "@level1type = N'VIEW', @level1name = N'ThingNames'",
        ),
        "COMMENT ON TABLE public.ThingNames IS 'Kept things';",
        "no counterpart",
    );
    // The object is not in the target catalog.
    unknown(
        &extprop(
            "N'Kept things'",
            "@level1type = N'TABLE', @level1name = N'Gone'",
        ),
        "COMMENT ON TABLE public.Gone IS 'Kept things';",
        "catalog has no table",
    );
    // A quoted PostgreSQL name with capitals is another object.
    unknown(
        &extprop("N'Kept things'", table),
        "COMMENT ON TABLE public.\"Things\" IS 'Kept things';",
        "catalog has no table public.Things",
    );
    // Commented twice: PostgreSQL keeps only the last comment.
    let two = format!(
        "{}\nGO\n{}",
        extprop("N'Kept things'", table),
        extprop("N'Other'", table).replace("N'Description'", "N'Note'")
    );
    unknown(
        &two,
        "COMMENT ON TABLE public.Things IS 'Kept things';\nCOMMENT ON TABLE public.Things IS 'Other';",
        "more than once",
    );
    // Something after the comment could change the object again.
    unknown(
        &format!(
            "{}\nGO\nDROP TABLE dbo.Things;",
            extprop("N'Kept things'", table)
        ),
        "COMMENT ON TABLE public.Things IS 'Kept things';\nDROP TABLE Things;",
        "last change",
    );
    unknown(
        &extprop("N'Kept things'", table),
        "COMMENT ON TABLE public.Things IS E'Kept things';",
        "plain string literal",
    );
    unknown(
        &extprop("N'Kept\\things'", table),
        "COMMENT ON TABLE public.Things IS 'Kept\\things';",
        "backslash",
    );
    // A property value that is not a string.
    unknown(
        &extprop("42", table),
        "COMMENT ON TABLE public.Things IS '42';",
        "not a string literal",
    );
}

#[test]
fn comments_follow_the_tables_they_describe() {
    // The usual SSDT shape: the table, then its properties, after GO.
    let t = format!(
        "CREATE TABLE dbo.Notes (Id int NOT NULL);\nGO\n{}",
        extprop("N'Notes'", "@level1type = N'TABLE', @level1name = N'Notes'")
    );
    let p = "CREATE TABLE Notes (Id integer NOT NULL);\nCOMMENT ON TABLE public.Notes IS 'Notes';";
    let o = opts(
        &format!("{CAT_T}\nGO\nCREATE TABLE dbo.Notes (Id int NOT NULL);"),
        &format!("{CAT_P}\nCREATE TABLE Notes (Id integer NOT NULL);"),
    );
    let r = run_with(&t, p, &o);
    assert!(r.unknown_reasons.is_empty(), "{:?}", r.unknown_reasons);
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:#?}",
        r.findings
    );
}

/// `diff` with a trace that links the two routines and the two calls only:
/// the statements around the calls pair through the routines' anchor.
fn traced(a: &str, b: &str, call: (&str, &str)) -> Report {
    let span = |t: &str, s: &str| {
        let at = t.find(s).unwrap();
        (at, at + s.len())
    };
    let mut o = opts(CAT_T, CAT_P);
    o.anchors = vec![
        sqldiff_core::equiv::Anchor {
            a: (0, a.len()),
            b: (0, b.len()),
        },
        sqldiff_core::equiv::Anchor {
            a: span(a, call.0),
            b: span(b, call.1),
        },
    ];
    run_with(a, b, &o)
}

#[test]
fn a_traced_call_keeps_its_place_among_the_paired_statements() {
    let call = (
        "EXEC dbo.Touch '20200101', 1;",
        "CALL Touch('20200101', 1);",
    );
    let place = |r: &Report| r.unknown_reasons.iter().any(|x| x.contains("same place"));
    // A statement only one side has does not move the call.
    let a = caller_t(&format!("SET @nm = @n;\n    {}", call.0));
    let b = caller_p(&format!("v_nm := p_n;\n  v_newid := 0;\n  {}", call.1));
    let r = traced(&a, &b, call);
    assert!(!place(&r), "{:?}", r.unknown_reasons);
    // A statement that pairs through the routines' anchor and sits on the
    // other side of the call does.
    let a = caller_t(&format!(
        "UPDATE dbo.Things SET Qty = 1 WHERE Id = 1;\n    {}",
        call.0
    ));
    let b = caller_p(&format!(
        "{}\n  UPDATE Things SET Qty = 1 WHERE Id = 1;",
        call.1
    ));
    let r = traced(&a, &b, call);
    assert!(place(&r), "{:?}", r.unknown_reasons);
    assert_ne!(r.verdict(), Verdict::Equal);
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
}
