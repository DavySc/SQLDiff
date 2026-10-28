use sqldiff_convert::{convert, Conversion, Status, Unit};
use sqldiff_core::{ChangeClass, Dialect};

fn tsql(text: &str) -> Conversion {
    convert(text, Dialect::Tsql, Dialect::Postgres).unwrap()
}

fn root(p: &str) -> String {
    format!("{}/../../{p}", env!("CARGO_MANIFEST_DIR"))
}

/// The unit whose input contains `needle`.
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

fn out_of<'a>(c: &'a Conversion, u: &Unit) -> &'a str {
    &c.output[u.out.start..u.out.end]
}

fn has_rule(u: &Unit, rule: &str) -> bool {
    u.notes.iter().any(|n| n.rule == rule)
}

/// The status the rules alone gave the unit, without the engine's notes.
fn rule_status(u: &Unit) -> Status {
    u.notes
        .iter()
        .filter(|n| n.status != Status::Unproven)
        .map(|n| n.status)
        .max()
        .unwrap_or(Status::Converted)
}

#[test]
fn pairs_without_rules_keep_every_statement_as_a_comment() {
    let src = "SELECT 1;\nUPDATE t SET a = 2;\n";
    let c = convert(src, Dialect::Postgres, Dialect::Db2i).unwrap();
    assert_eq!(c.units.len(), 2);
    assert_eq!(c.counts.not_converted, 2);
    assert!(
        c.output.lines().all(|l| l.starts_with("--")),
        "{}",
        c.output
    );
    assert!(c
        .output
        .contains("no conversion rules from PostgreSQL to Db2 for i yet"));
}

#[test]
fn select_rewrites() {
    let src = "SELECT TOP 10 [Name] + ' x' AS n, ISNULL(a, 0), LEN(b), NEWID()\nFROM dbo.Items WITH (NOLOCK)\nWHERE c = 1\nORDER BY n;\n";
    let c = tsql(src);
    let u = unit(&c, src, "SELECT");
    let out = out_of(&c, u);
    assert!(
        out.starts_with(
            "SELECT Name || ' x' AS n, COALESCE(a, 0), length(rtrim(b)), gen_random_uuid()"
        ),
        "{out}"
    );
    assert!(out.contains("FROM Items\nWHERE c = 1"), "{out}");
    assert!(out.trim_end().ends_with("ORDER BY n\nLIMIT 10;"), "{out}");
    // The rules need assumptions; the engine does not check table hints, so
    // it cannot confirm them either.
    assert_eq!(rule_status(u), Status::Assumptions);
    assert_eq!(u.status, Status::Unproven, "{:?}", u.notes);
    assert_eq!(u.checked, Status::Unproven);
    assert!(has_rule(u, "equiv.unsupported"), "{:?}", u.notes);
    for r in [
        "ASM-CONCAT-NULL",
        "ASM-ISNULL-TYPE",
        "ASM-DBO-SCHEMA",
        "ASM-NOLOCK",
    ] {
        assert!(has_rule(u, r), "missing {r}");
    }
}

#[test]
fn exact_rules_need_no_assumption() {
    let src = "SELECT LEN(b), NEWID(), IIF(a > 1, 'x', 'y') FROM t;";
    let c = tsql(src);
    assert_eq!(c.units.len(), 1);
    assert_eq!(
        rule_status(&c.units[0]),
        Status::Converted,
        "{:?}",
        c.units[0].notes
    );
    // The engine does not model LEN, NEWID or IIF yet: exact rules it
    // cannot confirm are not proven, not converted.
    assert_eq!(c.units[0].status, Status::Unproven);
    assert!(c.output.contains("(CASE WHEN a > 1 THEN 'x' ELSE 'y' END)"));
}

#[test]
fn procedure_with_control_flow() {
    let src = "CREATE PROCEDURE dbo.P @ItemId int, @Note nvarchar(20) = NULL OUTPUT AS\nBEGIN\n  DECLARE @i INT = 0;\n  WHILE @i < 3\n  BEGIN\n    SET @i += 1;\n    IF @i = 1 PRINT 'one' ELSE IF @i = 2 PRINT 'two' ELSE PRINT 'many';\n  END\n  SET @Note = 'n' + @Note;\n  RETURN 7;\nEND\nGO\n";
    let c = tsql(src);
    let o = &c.output;
    assert!(o.starts_with("CREATE OR REPLACE PROCEDURE P(p_item_id integer, INOUT p_note varchar(20) DEFAULT NULL, INOUT return_code integer DEFAULT NULL)\nLANGUAGE plpgsql\nAS $$\nDECLARE\n  v_i integer;\nBEGIN\n"), "{o}");
    assert!(o.contains("  v_i := 0;\n  WHILE v_i < 3 LOOP\n    v_i := v_i + 1;\n    IF v_i = 1 THEN\n      RAISE NOTICE '%', 'one';\n    ELSIF v_i = 2 THEN\n      RAISE NOTICE '%', 'two';\n    ELSE\n      RAISE NOTICE '%', 'many';\n    END IF;\n  END LOOP;\n"), "{o}");
    assert!(o.contains("p_note := 'n' || p_note;"), "{o}");
    assert!(o.contains("return_code := 7;\n  RETURN;\nEND;\n$$;"), "{o}");
    assert!(has_rule(
        unit(&c, src, "CREATE PROCEDURE"),
        "ASM-RETURN-CODE"
    ));
    assert!(sqldiff_syntax::parse(o, Dialect::Postgres)
        .diagnostics
        .is_empty());
}

#[test]
fn script_with_variables_becomes_a_do_block() {
    let src = "DECLARE @n INT;\nSELECT @n = COUNT(*) FROM t;\nPRINT @n;\n";
    let c = tsql(src);
    assert_eq!(
        c.output,
        "DO $$\nDECLARE\n  v_n integer;\nBEGIN\n  -- DECLARE @n: moved to the DECLARE section\n  SELECT COUNT(*) INTO v_n FROM t;\n  RAISE NOTICE '%', v_n;\nEND\n$$;\n"
    );
    // An aggregate without grouping returns exactly one row: nothing to assume.
    assert!(!has_rule(
        unit(&c, src, "SELECT"),
        "ASM-SELECT-ASSIGN-CARDINALITY"
    ));
}

#[test]
fn create_table() {
    let src = "CREATE TABLE dbo.T (\n    Id INT IDENTITY(1,1) NOT NULL PRIMARY KEY CLUSTERED,\n    Flag BIT NOT NULL DEFAULT 0,\n    At DATETIME2(3) NULL\n) ON [PRIMARY];";
    let c = tsql(src);
    assert_eq!(
        c.output,
        "CREATE TABLE T (\n    Id integer GENERATED ALWAYS AS IDENTITY (START WITH 1 INCREMENT BY 1) NOT NULL PRIMARY KEY,\n    Flag boolean NOT NULL DEFAULT FALSE,\n    At timestamp(3) NULL\n);\n"
    );
    let u = &c.units[0];
    assert_eq!(u.status, Status::Decision); // BIT → boolean
    assert!(has_rule(u, "ASM-IDENTITY"));
}

#[test]
fn rollback_rethrow_idiom_lets_the_error_propagate() {
    let src = std::fs::read_to_string(root("tests/fixtures/catch_rethrow.tsql.sql")).unwrap();
    let c = tsql(&src);
    assert!(!c.output.contains("EXCEPTION"), "{}", c.output);
    assert!(c
        .units
        .iter()
        .any(|u| has_rule(u, "idiom.rollback-rethrow")));
}

#[test]
fn try_catch_becomes_an_exception_block() {
    let src = "CREATE PROCEDURE p AS\nBEGIN\n  BEGIN TRY\n    DELETE FROM t;\n  END TRY\n  BEGIN CATCH\n    PRINT ERROR_MESSAGE();\n  END CATCH\nEND\n";
    let c = tsql(src);
    assert!(c.output.contains("  BEGIN\n    DELETE FROM t;\n  EXCEPTION WHEN OTHERS THEN\n    RAISE NOTICE '%', SQLERRM;\n  END;\n"), "{}", c.output);
    assert!(has_rule(
        unit(&c, src, "BEGIN TRY"),
        "ASM-TRY-SUBTRANSACTION"
    ));
}

#[test]
fn constructs_without_rules_are_kept_as_comments() {
    let src = "MERGE t USING s ON t.id = s.id WHEN MATCHED THEN DELETE;\nUPDATE t SET a = s.a FROM t JOIN s ON s.id = t.id;\nSELECT DATEDIFF(week, a, b) FROM t;\n";
    let c = tsql(src);
    for needle in ["MERGE", "UPDATE", "DATEDIFF"] {
        let u = unit(&c, src, needle);
        assert_eq!(u.status, Status::NotConverted, "{needle}");
        assert!(
            out_of(&c, u).lines().all(|l| l.starts_with("--")),
            "{needle}: {}",
            out_of(&c, u)
        );
    }
    // The engine sees the MERGE write missing on the PostgreSQL side.
    assert!(c.report.findings.iter().any(|f| f.rule == "write.unpaired"));
}

#[test]
fn error_checks_are_not_guessed() {
    let src = std::fs::read_to_string(root("tests/fixtures/movestock.tsql.sql")).unwrap();
    let c = tsql(&src);
    let u = unit(&c, &src, "IF @@ERROR");
    assert_eq!(u.status, Status::NotConverted);
    // ... and the engine reports the changed error contract.
    assert!(c.report.findings.iter().any(|f| f.rule == "error.contract"));
}

#[test]
fn procedures_cannot_return_result_sets() {
    let src = "CREATE PROCEDURE p AS\nBEGIN\n  SELECT a FROM t;\nEND\n";
    let c = tsql(src);
    assert_eq!(unit(&c, src, "SELECT").status, Status::NotConverted);
}

#[test]
fn datediff_day_matches_the_corpus_conversion() {
    let src = std::fs::read_to_string(root("corpus/pairs/p12-days-between/tsql.sql")).unwrap();
    let c = tsql(&src);
    assert!(
        c.output
            .contains("((shipped_at)::date - (created_at)::date) AS ship_days"),
        "{}",
        c.output
    );
}

/// Every corpus T-SQL file converts without leaving T-SQL in live output, and
/// the output parses as PostgreSQL.
#[test]
fn corpus_outputs_parse_and_carry_no_residue() {
    let mut files = Vec::new();
    for dir in ["corpus/pairs", "corpus/mutations"] {
        for e in std::fs::read_dir(root(dir)).unwrap() {
            let d = e.unwrap().path();
            if !d.is_dir() {
                continue;
            }
            for f in std::fs::read_dir(d).unwrap() {
                let p = f.unwrap().path();
                let n = p.file_name().unwrap().to_string_lossy().to_string();
                if n == "tsql.sql" || n.ends_with(".tsql.sql") {
                    files.push(p);
                }
            }
        }
    }
    files.push(root("corpus/catalog/tsql.sql").into());
    files.push(root("tests/fixtures/movestock.tsql.sql").into());
    assert!(files.len() > 20);
    for p in files {
        let src = std::fs::read_to_string(&p).unwrap();
        let c = tsql(&src);
        let diags = sqldiff_syntax::parse(&c.output, Dialect::Postgres).diagnostics;
        assert!(diags.is_empty(), "{}: {diags:?}\n{}", p.display(), c.output);
        let residue: Vec<_> = c
            .report
            .findings
            .iter()
            .filter(|f| f.class == ChangeClass::Residue && f.b.is_some())
            .collect();
        assert!(
            residue.is_empty(),
            "{}: {residue:?}\n{}",
            p.display(),
            c.output
        );
        let total = c.counts.converted
            + c.counts.assumptions
            + c.counts.decision
            + c.counts.unproven
            + c.counts.differs
            + c.counts.not_converted;
        assert_eq!(total, c.units.len());
        for u in &c.units {
            assert!(u.src.start <= u.src.end && u.src.end <= src.len());
            assert!(u.out.start <= u.out.end && u.out.end <= c.output.len());
            assert!(u.status >= u.checked);
        }
    }
}

// ----- cross-review fixes ------------------------------------------------------

#[test]
fn commit_inside_try_is_refused() {
    let src = "CREATE PROCEDURE p AS\nBEGIN\n  BEGIN TRY\n    BEGIN TRAN;\n    DELETE FROM t;\n    COMMIT;\n  END TRY\n  BEGIN CATCH\n    PRINT ERROR_MESSAGE();\n  END CATCH\nEND\n";
    let c = tsql(src);
    let u = unit(&c, src, "COMMIT");
    assert_eq!(u.status, Status::NotConverted, "{}", c.output);
    assert!(
        !c.output.lines().any(|l| l.trim() == "COMMIT;"),
        "{}",
        c.output
    );
}

#[test]
fn alter_column_is_refused() {
    let src = "ALTER TABLE t ALTER COLUMN Name nvarchar(100) NOT NULL;";
    let c = tsql(src);
    assert_eq!(c.units[0].status, Status::NotConverted, "{}", c.output);
    let add = "ALTER TABLE t ADD Note varchar(10) NULL;";
    let c = tsql(add);
    assert_eq!(
        c.output,
        "ALTER TABLE t ADD COLUMN Note varchar(10) NULL;\n"
    );
}

#[test]
fn top_in_parentheses_with_assignment() {
    let src = "DECLARE @n nvarchar(10);\nSELECT TOP (1) @n = Name FROM t ORDER BY Name;\n";
    let c = tsql(src);
    assert!(
        c.output.contains(
            "FOR sqldiff_r IN SELECT Name AS sqldiff_c1 FROM t ORDER BY Name LIMIT (1) LOOP\n      v_n := sqldiff_r.sqldiff_c1;\n    END LOOP;"
        ),
        "{}",
        c.output
    );
}

#[test]
fn units_beside_a_routine_are_kept() {
    let src = "CREATE TABLE x (a int);\nCREATE VIEW v AS SELECT a FROM x;\nGO\n";
    let c = tsql(src);
    assert!(c.output.contains("CREATE TABLE x"), "{}", c.output);
    assert!(
        c.output.contains("CREATE OR REPLACE VIEW v AS"),
        "{}",
        c.output
    );
    assert_eq!(c.units.len(), 2);
}

#[test]
fn type_defaults_follow_tsql() {
    let c = tsql("CREATE TABLE t (a decimal, b varchar, c nchar, d varchar(5));");
    assert!(c.output.contains("a numeric(18,0)"), "{}", c.output);
    assert!(c.output.contains("b varchar(1)"), "{}", c.output);
    assert!(c.output.contains("c char(1)"), "{}", c.output);
    let c = tsql("SELECT CAST(a AS varchar) FROM t;");
    assert!(c.output.contains("CAST(a AS varchar(30))"), "{}", c.output);
    let c = tsql("CREATE TABLE t (a datetime2, b datetime2(3));");
    assert!(c.output.contains("a timestamp,"), "{}", c.output);
    assert!(c.output.contains("b timestamp(3)"), "{}", c.output);
    assert!(has_rule(&c.units[0], "ASM-FRACTIONAL-SECONDS"));
}

/// PostgreSQL rounds a seventh fractional digit of a date and time literal
/// (the SQL Server period end becomes 10000-01-01 00:00:00); six are kept.
#[test]
fn seven_digit_time_literals_are_noted() {
    let c = tsql("SELECT k FROM t WHERE e = '9999-12-31 23:59:59.9999999';");
    assert!(has_rule(&c.units[0], "ASM-LITERAL-FRACTION"));
    for src in [
        "SELECT k FROM t WHERE e = '9999-12-31 23:59:59.999999';",
        "SELECT k FROM t WHERE e = 'abc.1234567';",
    ] {
        assert!(
            !has_rule(&tsql(src).units[0], "ASM-LITERAL-FRACTION"),
            "{src}"
        );
    }
}

#[test]
fn alter_procedure_notes_the_overload_risk() {
    let src = "ALTER PROCEDURE dbo.q @a int AS\nBEGIN\n  DELETE FROM t WHERE id = @a;\nEND\n";
    let c = tsql(src);
    assert!(c.output.starts_with("CREATE OR REPLACE PROCEDURE q("));
    assert!(has_rule(
        unit(&c, src, "ALTER PROCEDURE"),
        "ASM-ROUTINE-REPLACE"
    ));
}

#[test]
fn result_set_check_is_structural() {
    // ' INTO ' in a literal must not make a result set look like an assignment.
    let src = "CREATE PROCEDURE p AS\nBEGIN\n  SELECT 'x INTO y' AS a, ':=' AS b FROM t;\nEND\n";
    let c = tsql(src);
    assert_eq!(
        unit(&c, src, "SELECT").status,
        Status::NotConverted,
        "{}",
        c.output
    );
}

#[test]
fn declare_notes_are_counted_once() {
    let src = "DECLARE @d datetime = GETDATE();\nPRINT @d;\n";
    let c = tsql(src);
    let decl = c
        .units
        .iter()
        .filter(|u| has_rule(u, "ASM-DATETIME-PRECISION"))
        .count();
    let time = c
        .units
        .iter()
        .filter(|u| has_rule(u, "ASM-TIME-SOURCE"))
        .count();
    assert_eq!((decl, time), (1, 1), "{:#?}", c.units);
}

const IDIOMS: &str = "CREATE PROCEDURE dbo.Feed @Item int, @Price decimal(12,2), @Customer int AS
BEGIN
  DECLARE @OrderId int, @rc int;
  MERGE dbo.item_price WITH (HOLDLOCK) AS t USING (SELECT @Item AS item_id, @Price AS unit_price) AS s ON t.item_id = s.item_id WHEN MATCHED THEN UPDATE SET unit_price = s.unit_price WHEN NOT MATCHED THEN INSERT (item_id, unit_price) VALUES (s.item_id, s.unit_price);
  DELETE pf FROM dbo.price_feed AS pf LEFT JOIN dbo.item AS i ON i.item_id = pf.item_id WHERE i.item_id IS NULL;
  INSERT INTO dbo.orders (customer_id) VALUES (@Customer);
  SET @OrderId = SCOPE_IDENTITY();
  EXEC @rc = sp_getapplock @Resource = 'price-feed', @LockMode = 'Exclusive', @LockOwner = 'Transaction', @LockTimeout = -1;
  IF @rc < 0 BEGIN ROLLBACK; THROW 50020, 'lock not granted', 1; END
END
";

#[test]
fn idiom_rules_convert_what_the_token_rules_refuse() {
    let c = tsql(IDIOMS);
    let cases = [
        (
            "MERGE",
            "ON CONFLICT (item_id) DO UPDATE SET unit_price = p_price",
        ),
        (
            "DELETE pf",
            "NOT EXISTS (SELECT 1 FROM item WHERE item.item_id = price_feed.item_id)",
        ),
        (
            "EXEC @rc",
            "PERFORM pg_advisory_xact_lock(hashtext('price-feed'));",
        ),
    ];
    for (needle, want) in cases {
        let u = unit(&c, IDIOMS, needle);
        // The rules' own verdict; the engine may still find the procedure's
        // autocommit-per-statement behaviour different around it.
        assert!(
            u.notes.iter().all(|n| n.status < Status::Differs),
            "{needle}: {:?}",
            u.notes
        );
        assert!(has_rule(u, "idiom-rules"), "{needle}");
        assert!(out_of(&c, u).contains(want), "{needle}: {}", out_of(&c, u));
    }
    assert_eq!(c.counts.not_converted, 0, "{}", c.output);
}

#[test]
fn idiom_rules_join_statements_the_token_rules_see_apart() {
    let c = tsql(IDIOMS);
    let insert = unit(&c, IDIOMS, "INSERT INTO dbo.orders");
    let identity = unit(&c, IDIOMS, "SCOPE_IDENTITY");
    // One unit for INSERT + SCOPE_IDENTITY, named with the procedure's variables.
    assert_eq!(insert.src, identity.src);
    assert!(
        out_of(&c, insert).contains("v_order_id := lastval();"),
        "{}",
        out_of(&c, insert)
    );
    // Without a catalog the rules cannot prove RETURNING, and say so.
    assert!(
        has_rule(insert, "ASM-LASTVAL-SAME-INSERT"),
        "{:?}",
        insert.notes
    );
    // The lock check after sp_getapplock is part of the lock unit.
    assert_eq!(
        unit(&c, IDIOMS, "IF @rc").src,
        unit(&c, IDIOMS, "EXEC @rc").src
    );
}

#[test]
fn idiom_rules_use_the_declared_names() {
    // The DECLARE spells it @orderid, the use site @OrderId: one variable, one name.
    let src = "CREATE PROCEDURE p @Customer int AS\nBEGIN\n  DECLARE @orderid int;\n  INSERT INTO dbo.orders (customer_id) VALUES (@Customer);\n  SET @OrderId = SCOPE_IDENTITY();\nEND\n";
    let c = tsql(src);
    assert!(c.output.contains("v_orderid integer;"), "{}", c.output);
    assert!(c.output.contains("v_orderid := lastval();"), "{}", c.output);
    assert!(!has_rule(
        unit(&c, src, "SCOPE_IDENTITY"),
        "ASM-UNDECLARED-VAR"
    ));
    // Undeclared variables are named as the token rules name them, and noted.
    let src = "CREATE PROCEDURE p @Customer int AS\nBEGIN\n  INSERT INTO dbo.orders (customer_id) VALUES (@Customer);\n  SET @NewId = SCOPE_IDENTITY();\nEND\n";
    let c = tsql(src);
    let u = unit(&c, src, "SCOPE_IDENTITY");
    assert!(
        out_of(&c, u).contains("v_new_id := lastval();"),
        "{}",
        c.output
    );
    assert!(has_rule(u, "ASM-UNDECLARED-VAR"), "{:?}", u.notes);
}

#[test]
fn context_refusals_are_not_handed_to_the_idiom_rules() {
    let cases = [
        // RETURN with a value in a DO block, THROW; outside CATCH.
        ("DECLARE @x int = 1;\nRETURN 1;\n", "RETURN 1"),
        ("DECLARE @x int = 1;\nTHROW;\n", "THROW"),
        // RETURN; in a scalar function.
        (
            "CREATE FUNCTION f() RETURNS int AS\nBEGIN\n  RETURN;\nEND\n",
            "RETURN;",
        ),
        // A result set in a procedure.
        (
            "CREATE PROCEDURE p AS\nBEGIN\n  SELECT a FROM t;\nEND\n",
            "SELECT",
        ),
    ];
    for (src, needle) in cases {
        let c = tsql(src);
        let u = unit(&c, src, needle);
        assert_eq!(u.status, Status::NotConverted, "{needle}: {}", c.output);
        assert!(!has_rule(u, "idiom-rules"), "{needle}");
    }
}

#[test]
fn statement_pairs_keep_the_context_checks() {
    // Beside a routine in one batch: plain SQL, no variables, so no pair.
    let src = "INSERT INTO t (a) VALUES (1);\nSET @id = SCOPE_IDENTITY();\nCREATE VIEW v AS SELECT a FROM t;\nGO\n";
    let c = tsql(src);
    let set = unit(&c, src, "SET @id");
    assert_eq!(set.status, Status::NotConverted, "{}", c.output);
    assert!(!c.output.contains("lastval"), "{}", c.output);
    // Inside TRY (a subtransaction) the lock and its ROLLBACK are not joined.
    let src = "CREATE PROCEDURE p AS\nBEGIN\n  DECLARE @rc int;\n  BEGIN TRY\n    EXEC @rc = sp_getapplock @Resource = 'r', @LockMode = 'Exclusive', @LockOwner = 'Transaction', @LockTimeout = -1;\n    IF @rc < 0 BEGIN ROLLBACK; THROW 50020, 'no lock', 1; END\n  END TRY\n  BEGIN CATCH\n    PRINT 'x';\n  END CATCH\nEND\n";
    let c = tsql(src);
    assert!(!c.output.contains("pg_advisory_xact_lock"), "{}", c.output);
    assert_eq!(unit(&c, src, "EXEC @rc").status, Status::NotConverted);
    // A comment between the two statements keeps them apart.
    let src = "CREATE PROCEDURE p @Customer int AS\nBEGIN\n  DECLARE @id int;\n  INSERT INTO dbo.orders (customer_id) VALUES (@Customer);\n  -- the key\n  SET @id = SCOPE_IDENTITY();\nEND\n";
    let c = tsql(src);
    assert_ne!(unit(&c, src, "INSERT").src, unit(&c, src, "SET @id").src);
}

#[test]
fn only_the_refused_statement_goes_to_the_idiom_rules() {
    let src = "CREATE PROCEDURE p @Item int, @Price decimal(12,2) AS\nBEGIN\n  IF @Price > 0\n  BEGIN\n    MERGE dbo.item_price WITH (HOLDLOCK) AS t USING (SELECT @Item AS item_id, @Price AS unit_price) AS s ON t.item_id = s.item_id WHEN MATCHED THEN UPDATE SET unit_price = s.unit_price WHEN NOT MATCHED THEN INSERT (item_id, unit_price) VALUES (s.item_id, s.unit_price);\n    RETURN 1;\n  END\nEND\n";
    let c = tsql(src);
    // The IF and RETURN keep the token rules' spelling; only the MERGE is the rules'.
    assert!(c.output.contains("IF p_price > 0 THEN"), "{}", c.output);
    assert!(c.output.contains("return_code := 1;"), "{}", c.output);
    assert!(c.output.contains("INOUT return_code"), "{}", c.output);
    let m = unit(&c, src, "MERGE");
    assert!(has_rule(m, "idiom-rules"), "{:?}", m.notes);
    assert!(
        out_of(&c, m).contains("ON CONFLICT (item_id)"),
        "{}",
        out_of(&c, m)
    );
    assert!(!has_rule(unit(&c, src, "RETURN 1"), "idiom-rules"));
}

fn template(text: &str) -> sqldiff_syntax::template::Template {
    use sqldiff_syntax::template::{Compiled, Template, TemplateSyntax};
    let syntax = Compiled::new(&TemplateSyntax {
        open: "[".into(),
        close: "]".into(),
        name_pattern: "[A-Z][A-Z0-9_]*".into(),
        special_forms: vec![("include".into(), r"<%@include\s+(\S+)\s*%>".into())],
        ignore_case: false,
    })
    .unwrap();
    Template::parse(text, &syntax, None)
}

fn convert_template(t: &sqldiff_syntax::template::Template) -> Conversion {
    use sqldiff_core::composition::PlaceholderBasis;
    sqldiff_convert::convert_template(
        t,
        Dialect::Tsql,
        Dialect::Postgres,
        PlaceholderBasis::DeclaredPattern,
        ("[", "]"),
    )
    .unwrap()
}

const TEMPLATE: &str = "<%@include header.sql %>
CREATE PROCEDURE [SCHEMA].Save[ENTITY] @Id int, @[KEY]Name nvarchar(50) AS
BEGIN
  SET NOCOUNT ON;
  UPDATE [SCHEMA].[TABLE] SET [Name] = @[KEY]Name WHERE Id = @Id;
END
";

#[test]
fn templates_keep_their_placeholders() {
    let t = template(TEMPLATE);
    let c = convert_template(&t);
    println!("{}", c.output);
    let info = c.template.as_ref().unwrap();
    assert_eq!(info.parameters, ["SCHEMA", "ENTITY", "KEY", "TABLE"]);
    assert!(
        c.output.contains("<%@include header.sql %>"),
        "{}",
        c.output
    );
    assert!(
        c.output.contains("PROCEDURE [SCHEMA].Save[ENTITY]("),
        "{}",
        c.output
    );
    assert!(
        c.output.contains("UPDATE [SCHEMA].[TABLE] SET Name = "),
        "{}",
        c.output
    );
    assert!(
        !c.output.to_ascii_lowercase().contains("tpl_"),
        "{}",
        c.output
    );
    // Spans point into the template, and units with placeholders say what they assume.
    let u = unit(&c, TEMPLATE, "UPDATE");
    assert_eq!(&TEMPLATE[u.src.start..u.src.start + 6], "UPDATE");
    assert!(has_rule(u, "ASM-TEMPLATE-PLACEHOLDER"), "{:?}", u.notes);
    assert!(out_of(&c, u).contains("[TABLE]"), "{}", out_of(&c, u));
    // The include's text was not converted with it: no unit counts as converted.
    for u in c.units.iter().filter(|u| u.status != Status::NotConverted) {
        assert!(has_rule(u, "ASM-TEMPLATE-DIRECTIVE"), "{:?}", u.notes);
        assert_ne!(u.status, Status::Converted);
    }
}

#[test]
fn a_template_without_directives_needs_no_directive_assumption() {
    let c = convert_angle("UPDATE t SET a = 1;\n");
    assert!(c
        .units
        .iter()
        .all(|u| !has_rule(u, "ASM-TEMPLATE-DIRECTIVE")));
    // Only what the engine assumes without a catalog: the table and its
    // column types.
    assert_eq!(c.units[0].status, Status::Assumptions);
    assert!(
        c.units[0]
            .notes
            .iter()
            .all(|n| n.rule.starts_with("ASM-BIND-") || n.rule == "ASM-UNTYPED-STORE"),
        "{:?}",
        c.units[0].notes
    );
}

#[test]
fn templates_converted_together_share_stand_ins() {
    use sqldiff_convert::{template_fragment, StandIns};
    let a = angle_template("CREATE TABLE <%S%>.<%T%> (id int);\n");
    let b = angle_template("UPDATE <%T%> SET id = 1; UPDATE <%S%>.<%T%> SET id = 2;\n");
    let stand = StandIns::new([&a, &b]);
    let (fa, fb) = (template_fragment(&a, &stand), template_fragment(&b, &stand));
    let stem = stand.stem();
    assert_eq!(fa, format!("CREATE TABLE {stem}0z.{stem}1z (id int);\n"));
    assert_eq!(
        fb,
        format!("UPDATE {stem}1z SET id = 1; UPDATE {stem}0z.{stem}1z SET id = 2;\n")
    );
}

#[test]
fn output_check_findings_quote_the_template() {
    use sqldiff_convert::{check_template, rewrite_template, template_fragment, StandIns};
    use sqldiff_core::catalog::{ddl, Catalog};
    use sqldiff_core::composition::PlaceholderBasis;
    let table = angle_template("CREATE TABLE <%S%>.<%T%> (id int);\n");
    let proc = angle_template("UPDATE <%S%>.<%T%> SET cost = 1;\n");
    let stand = StandIns::new([&table, &proc]);
    let catalog = |text: &str, d: Dialect| {
        let mut c = Catalog::default();
        ddl::load_tree(&mut c, &sqldiff_syntax::parse(text, d).syntax(), d);
        std::sync::Arc::new(c)
    };
    let tdraft = rewrite_template(&table, Dialect::Tsql, Dialect::Postgres, &stand, None);
    let opts = sqldiff_core::Options {
        facts_a: vec![catalog(&template_fragment(&table, &stand), Dialect::Tsql)],
        facts_b: vec![catalog(tdraft.output(), Dialect::Postgres)],
        ..Default::default()
    };
    let d = rewrite_template(&proc, Dialect::Tsql, Dialect::Postgres, &stand, None);
    let c = check_template(
        &proc,
        d,
        &stand,
        &opts,
        PlaceholderBasis::DeclaredPattern,
        ("<%", "%>"),
    );
    let u = &c.units[0];
    assert_eq!(u.status, Status::Differs, "{:?}", u.notes);
    let note = u
        .notes
        .iter()
        .find(|n| n.rule == "bind.unknown-column")
        .unwrap();
    assert!(note.text.contains("<%S%>.<%T%>"), "{}", note.text);
    let f = &c.output_check.findings[u.output_findings[0]];
    assert!(
        !f.witness.to_ascii_lowercase().contains("sqldifftpl"),
        "{}",
        f.witness
    );
    let at = f.a.as_ref().unwrap();
    // Located in the restored output, where the stand-ins are gone.
    assert_eq!(&c.output[at.byte_start..at.byte_end], "cost", "{at:?}");
}

fn angle_template(text: &str) -> sqldiff_syntax::template::Template {
    use sqldiff_syntax::template::{Compiled, Template, TemplateSyntax};
    let syntax = Compiled::new(&TemplateSyntax {
        open: "<%".into(),
        close: "%>".into(),
        name_pattern: "[A-Z][A-Z0-9_.]*".into(),
        special_forms: vec![("include".into(), r"<%@include\s+(\S+)\s*%>".into())],
        ignore_case: false,
    })
    .unwrap();
    Template::parse(text, &syntax, None)
}

fn convert_angle(text: &str) -> Conversion {
    use sqldiff_core::composition::PlaceholderBasis;
    sqldiff_convert::convert_template(
        &angle_template(text),
        Dialect::Tsql,
        Dialect::Postgres,
        PlaceholderBasis::DeclaredPattern,
        ("<%", "%>"),
    )
    .unwrap()
}

#[test]
fn template_restore_leaves_the_users_own_names_alone() {
    // A placeholder named T next to the user's tpl_* names, variables and strings.
    let src = "CREATE PROCEDURE p @tpl_total int AS\nBEGIN\n  UPDATE <%T%> SET tpl_type = 'TPL_TEST', n = @tpl_total FROM tpl_settings;\nEND\n";
    let c = convert_angle(src);
    let low = c.output.to_ascii_lowercase();
    assert!(
        c.output.contains("UPDATE <%T%> SET tpl_type = 'TPL_TEST'"),
        "{}",
        c.output
    );
    assert!(low.contains("p_tpl_total"), "{}", c.output);
    assert!(!low.contains("sqldifftpl"), "{}", c.output);
    // No false alarm for the user's own tpl_ names.
    for u in &c.units {
        assert!(!has_rule(u, "template.placeholder-lost"), "{:?}", u.notes);
    }
}

#[test]
fn similar_placeholder_names_stay_apart() {
    let src = "UPDATE <%A.B%> SET x = 1;\nUPDATE <%A_B%> SET x = 2;\n";
    let c = convert_angle(src);
    assert!(
        c.output.contains("UPDATE <%A.B%> SET x = 1;"),
        "{}",
        c.output
    );
    assert!(
        c.output.contains("UPDATE <%A_B%> SET x = 2;"),
        "{}",
        c.output
    );
}

#[test]
fn template_findings_point_into_the_template() {
    // A multi-line directive before the statements: fragment lines would drift.
    let src = "<%@include\n   header.sql %>\nCREATE PROCEDURE <%S%>.p @Id int AS\nBEGIN\n  UPDATE <%S%>.t SET a = 1 WHERE id = @Id;\n  IF @@ERROR <> 0 RETURN 1;\nEND\n";
    let c = convert_angle(src);
    assert!(!c.report.findings.is_empty());
    let lines: Vec<&str> = src.lines().collect();
    for f in &c.report.findings {
        assert!(
            !f.witness.to_ascii_lowercase().contains("sqldifftpl"),
            "{}",
            f.witness
        );
        if let Some(a) = &f.a {
            let first = a.excerpt.first().map(|s| s.trim()).unwrap_or_default();
            assert!(lines[a.start_line - 1].contains(first), "{a:?}");
        }
        if let Some(b) = &f.b {
            let out: Vec<&str> = c.output.lines().collect();
            let first = b.excerpt.first().map(|s| s.trim()).unwrap_or_default();
            assert!(out[b.start_line - 1].contains(first), "{b:?}");
        }
    }
}

#[test]
fn a_lost_directive_is_reported() {
    // The directive sits inside a statement the rules turn into a comment.
    let src = "CREATE PROCEDURE p AS\nBEGIN\n  SET NOCOUNT ON <%@include x.sql %>;\n  UPDATE t SET a = 1;\nEND\n";
    let c = convert_angle(src);
    assert!(
        !c.output.contains("<%@include x.sql %>"),
        "precondition: {}",
        c.output
    );
    let u = unit(&c, src, "SET NOCOUNT");
    assert!(has_rule(u, "template.special-form-lost"), "{:?}", u.notes);
    assert_eq!(u.status, Status::Differs);
    assert!(
        c.report
            .unknown_reasons
            .iter()
            .any(|r| r.contains("include")),
        "{:?}",
        c.report.unknown_reasons
    );
    assert_ne!(c.report.verdict, sqldiff_core::Verdict::Equal);
}

#[test]
fn a_placeholder_decided_by_pattern_is_an_open_question() {
    use sqldiff_core::composition::PlaceholderBasis;
    // [T] in T-SQL could be a quoted name; without a parameter list the report says so.
    let c = sqldiff_convert::convert_template(
        &template("UPDATE [T] SET a = 1;\n"),
        Dialect::Tsql,
        Dialect::Postgres,
        PlaceholderBasis::Pattern,
        ("[", "]"),
    )
    .unwrap();
    assert!(
        c.report.unknown_reasons.iter().any(|r| r.contains("[T]")),
        "{:?}",
        c.report.unknown_reasons
    );
    assert_ne!(c.report.verdict, sqldiff_core::Verdict::Equal);
}

#[test]
fn execute_as_owner_becomes_security_definer() {
    let src = "CREATE PROCEDURE dbo.Purge @Id int WITH RECOMPILE, EXECUTE AS OWNER AS\nBEGIN\n  DELETE FROM dbo.item WHERE item_id = @Id;\nEND\n";
    let c = tsql(src);
    assert!(
        c.output
            .contains("LANGUAGE plpgsql\nSECURITY DEFINER\nAS $$"),
        "{}",
        c.output
    );
    let u = unit(&c, src, "CREATE PROCEDURE");
    assert!(has_rule(u, "ASM-OWNER-PRIVILEGES"), "{:?}", u.notes);
    assert!(!has_rule(u, "tsql.routine-options"), "{:?}", u.notes);
    // EXECUTE AS CALLER is the default: nothing to write.
    let c = tsql(&src.replace("WITH RECOMPILE, EXECUTE AS OWNER", "WITH EXECUTE AS CALLER"));
    assert!(!c.output.contains("SECURITY DEFINER"), "{}", c.output);
    // Other principals have no PostgreSQL form.
    let c = tsql(&src.replace("WITH RECOMPILE, EXECUTE AS OWNER", "WITH EXECUTE AS 'app'"));
    assert!(!c.output.contains("SECURITY DEFINER"), "{}", c.output);
    assert!(c.units.iter().any(|u| has_rule(u, "tsql.routine-options")));
}

#[test]
fn security_definer_procedures_cannot_commit() {
    let src = "CREATE PROCEDURE dbo.Purge @Id int WITH EXECUTE AS OWNER AS\nBEGIN\n  BEGIN TRAN;\n  DELETE FROM dbo.item WHERE item_id = @Id;\n  COMMIT;\nEND\n";
    let c = tsql(src);
    assert!(!c.output.contains("SECURITY DEFINER"), "{}", c.output);
    assert!(c
        .units
        .iter()
        .any(|u| has_rule(u, "pg.security-definer-transaction")));
}

#[test]
fn delete_with_inner_joins_becomes_delete_using() {
    let cases = [
        (
            "DELETE o FROM dbo.orders o JOIN dbo.customer c ON c.customer_id = o.customer_id WHERE c.customer_id > 10 OR c.nick IS NULL;",
            "DELETE FROM orders o USING customer c WHERE (c.customer_id = o.customer_id) AND (c.customer_id > 10 OR c.nick IS NULL);",
        ),
        (
            "DELETE FROM o FROM dbo.orders AS o INNER JOIN dbo.customer AS c ON c.customer_id = o.customer_id;",
            "DELETE FROM orders AS o USING customer AS c WHERE (c.customer_id = o.customer_id);",
        ),
        (
            "DELETE orders FROM dbo.orders JOIN dbo.customer c ON c.customer_id = orders.customer_id, dbo.item i WHERE i.item_id = c.customer_id;",
            "DELETE FROM orders USING customer c, item i WHERE (c.customer_id = orders.customer_id) AND (i.item_id = c.customer_id);",
        ),
        (
            "DELETE o FROM dbo.orders o CROSS JOIN (SELECT MAX(order_id) AS m FROM dbo.orders) x WHERE o.order_id < x.m;",
            "DELETE FROM orders o USING (SELECT MAX(order_id) AS m FROM orders) x WHERE (o.order_id < x.m);",
        ),
        (
            "DELETE o FROM dbo.orders o JOIN dbo.orders o2 ON o2.customer_id = o.customer_id AND o2.order_id > o.order_id;",
            "DELETE FROM orders o USING orders o2 WHERE (o2.customer_id = o.customer_id AND o2.order_id > o.order_id);",
        ),
        (
            "DELETE o FROM dbo.orders o JOIN dbo.customer c ON LEFT(c.nick, 1) IS NOT NULL AND c.customer_id = o.customer_id;",
            "DELETE FROM orders o USING customer c WHERE (LEFT(c.nick, 1) IS NOT NULL AND c.customer_id = o.customer_id);",
        ),
        (
            "DELETE o FROM dbo.orders o WHERE o.order_id = 1;",
            "DELETE FROM orders o WHERE (o.order_id = 1);",
        ),
    ];
    for (src, want) in cases {
        let c = tsql(src);
        assert_eq!(c.output.trim(), want, "{src}");
        assert!(
            c.units[0].status < Status::Differs,
            "{src}: {:?}",
            c.units[0].notes
        );
        // The engine checks the rewrite itself, not only the output's binding.
        assert!(
            !c.report
                .findings
                .iter()
                .any(|f| f.rule == "equiv.unsupported"),
            "{src}: {:?}",
            c.report.findings
        );
    }
}

#[test]
fn delete_joins_that_do_not_only_filter_are_refused() {
    for src in [
        // An outer join that is not the anti-join idiom.
        "DELETE o FROM dbo.orders o LEFT JOIN dbo.customer c ON c.customer_id = o.customer_id WHERE c.nick = 'a';",
        "DELETE o FROM dbo.orders o CROSS APPLY dbo.f(o.order_id) f;",
        "DELETE o FROM dbo.orders o INNER HASH JOIN dbo.customer c ON c.customer_id = o.customer_id;",
        // Nested ON clauses.
        "DELETE o FROM dbo.orders o JOIN dbo.customer c JOIN dbo.item i ON i.item_id = c.customer_id ON c.customer_id = o.customer_id;",
        // The target binds to no FROM table, or by a rule that is not modelled.
        "DELETE FROM dbo.orders FROM dbo.customer c WHERE c.customer_id = orders.customer_id;",
        "DELETE FROM dbo.orders FROM dbo.orders o WHERE o.order_id = 1;",
        "DELETE orders FROM dbo.orders o JOIN dbo.customer c ON c.customer_id = o.customer_id;",
        "DELETE o FROM dbo.orders o JOIN dbo.customer c ON c.customer_id = o.customer_id OPTION (MAXDOP 1);",
        "DELETE TOP (10) FROM dbo.orders WHERE order_id < 5;",
        "DELETE FROM dbo.orders OUTPUT deleted.order_id WHERE order_id = 5;",
    ] {
        let c = tsql(src);
        assert_eq!(c.units[0].status, Status::NotConverted, "{src}: {}", c.output);
    }
}

/// PostgreSQL takes a WITH list in front of INSERT, UPDATE and DELETE with the
/// same meaning, unless the statement changes a CTE (T-SQL then changes the
/// CTE's base table).
#[test]
fn with_data_change() {
    let kept = [
        (
            "WITH c AS (SELECT order_id FROM dbo.orders) INSERT INTO dbo.orders (order_id) SELECT order_id + 100 FROM c;",
            "WITH c AS (SELECT order_id FROM orders) INSERT INTO orders (order_id) SELECT order_id + 100 FROM c;",
        ),
        (
            "WITH c (k) AS (SELECT order_id FROM dbo.orders), d AS (SELECT k FROM c) UPDATE dbo.orders SET nick = 0 WHERE order_id IN (SELECT k FROM d);",
            "WITH c (k) AS (SELECT order_id FROM orders), d AS (SELECT k FROM c) UPDATE orders SET nick = 0 WHERE order_id IN (SELECT k FROM d);",
        ),
        (
            "WITH c AS (SELECT order_id FROM dbo.orders WHERE nick = 1) DELETE FROM dbo.orders WHERE order_id IN (SELECT order_id FROM c);",
            "WITH c AS (SELECT order_id FROM orders WHERE nick = 1) DELETE FROM orders WHERE order_id IN (SELECT order_id FROM c);",
        ),
    ];
    for (src, want) in kept {
        let c = tsql(src);
        assert_eq!(c.units.len(), 1, "{src}");
        assert_eq!(c.output.trim(), want, "{src}");
        assert!(
            c.units[0].status < Status::Differs,
            "{src}: {:?}",
            c.units[0].notes
        );
    }
    for src in [
        "WITH c AS (SELECT order_id FROM dbo.orders) DELETE c FROM c JOIN dbo.orders o ON o.order_id = c.order_id;",
        "WITH c AS (SELECT order_id FROM dbo.orders) DELETE FROM c WHERE order_id > 1;",
        "WITH c AS (SELECT order_id AS a FROM dbo.orders WHERE nick = 1) INSERT INTO c (a) VALUES (1);",
        "WITH c AS (SELECT order_id AS a FROM dbo.orders WHERE nick = 1) INSERT c (a) VALUES (1);",
        "WITH [c] AS (SELECT order_id FROM dbo.orders) UPDATE dbo.c SET order_id = 2;",
        "WITH c AS (SELECT order_id FROM dbo.orders) DELETE o FROM dbo.orders o JOIN c ON c.order_id = o.order_id;",
        "WITH c AS (SELECT order_id FROM dbo.orders) MERGE dbo.orders AS t USING c ON t.order_id = c.order_id WHEN MATCHED THEN DELETE;",
    ] {
        let c = tsql(src);
        assert_eq!(c.units.len(), 1, "{src}");
        assert_eq!(c.units[0].status, Status::NotConverted, "{src}");
    }
    // In a procedure it is a data change, not a result set.
    let src = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  WITH c AS (SELECT order_id FROM dbo.orders) INSERT INTO dbo.orders (order_id) SELECT order_id + 100 FROM c;\nEND\n";
    let c = tsql(src);
    assert!(
        c.output.contains("  WITH c AS (SELECT order_id FROM orders) INSERT INTO orders (order_id) SELECT order_id + 100 FROM c;"),
        "{}",
        c.output
    );
    assert!(unit(&c, src, "WITH c").status < Status::Differs);
}

/// A recursive CTE needs WITH RECURSIVE, and T-SQL's recursion limit is noted.
#[test]
fn recursive_cte_gets_recursive() {
    let src =
        "WITH r AS (SELECT 1 AS n UNION ALL SELECT n + 1 FROM r WHERE n < 5) SELECT n FROM r;";
    let c = tsql(src);
    assert_eq!(
        c.output.trim(),
        "WITH RECURSIVE r AS (SELECT 1 AS n UNION ALL SELECT n + 1 FROM r WHERE n < 5) SELECT n FROM r;"
    );
    assert!(has_rule(&c.units[0], "ASM-CTE-MAXRECURSION"));
    // RECURSIVE makes later CTEs visible to earlier ones; T-SQL's `b` is the table.
    let fwd = tsql("WITH r AS (SELECT 1 AS n UNION ALL SELECT n + 1 FROM r WHERE n < 5), a AS (SELECT x FROM dbo.b), b AS (SELECT 5 AS x) SELECT n FROM r;");
    assert_eq!(fwd.units[0].status, Status::NotConverted, "{}", fwd.output);
    let back = tsql("WITH b AS (SELECT 5 AS x), r AS (SELECT x AS n FROM b UNION ALL SELECT n + 1 FROM r WHERE n < 5) SELECT n FROM r;");
    assert!(
        back.output.starts_with("WITH RECURSIVE b AS"),
        "{}",
        back.output
    );
    let plain = tsql("WITH c AS (SELECT 1 AS n) SELECT n FROM c;");
    assert!(plain.output.starts_with("WITH c AS"), "{}", plain.output);
    assert!(!has_rule(&plain.units[0], "ASM-CTE-MAXRECURSION"));
}

fn convert_kind(text: &str, kind: &str) -> Conversion {
    convert_kind_with(text, kind, |t| sqldiff_convert::StandIns::new(t))
}

fn convert_kind_with(
    text: &str,
    kind: &str,
    stand: impl Fn([&sqldiff_syntax::template::Template; 1]) -> sqldiff_convert::StandIns,
) -> Conversion {
    use sqldiff_convert::{check_template, rewrite_template_as, FragmentKind};
    use sqldiff_core::composition::PlaceholderBasis;
    let t = angle_template(text);
    let stand = stand([&t]);
    let kind = FragmentKind::parse(kind).unwrap();
    let d = rewrite_template_as(&t, kind, Dialect::Tsql, Dialect::Postgres, &stand, None);
    check_template(
        &t,
        d,
        &stand,
        &sqldiff_core::Options::default(),
        PlaceholderBasis::DeclaredPattern,
        ("<%", "%>"),
    )
}

#[test]
fn fragments_convert_inside_a_stand_in_statement() {
    for (kind, src, out) in [
        (
            "predicate",
            "ISNULL(<%T%>.qty, 0) > 1 AND name + 'x' = N'y'",
            "COALESCE(<%T%>.qty, 0) > 1 AND name || 'x' = 'y'",
        ),
        (
            "expression",
            "LEN(a), GETDATE()",
            "length(rtrim(a)), statement_timestamp()::timestamp",
        ),
        (
            "set-list",
            "a = ISNULL(b, 1), [c] = 2",
            "a = COALESCE(b, 1), c = 2",
        ),
        (
            "routine-body",
            "UPDATE <%T%> SET a = ISNULL(b, 0);\nDELETE FROM <%T%> WHERE a = 1;",
            "UPDATE <%T%> SET a = COALESCE(b, 0);\n  DELETE FROM <%T%> WHERE a = 1;",
        ),
    ] {
        let c = convert_kind(src, kind);
        assert_eq!(c.output.trim(), out, "{kind}");
        assert!(!c.units.is_empty(), "{kind}");
        for u in &c.units {
            assert!(
                has_rule(u, "ASM-TEMPLATE-FRAGMENT"),
                "{kind}: {:?}",
                u.notes
            );
            assert_ne!(u.status, Status::Converted, "{kind}");
            assert!(u.out.end <= c.output.len(), "{kind}");
            assert!(u.src.end <= src.len(), "{kind}");
        }
    }
}

#[test]
fn a_fragment_whose_conversion_crosses_its_edge_is_not_converted() {
    // PL/pgSQL declares variables before BEGIN: the declaration leaves the body.
    let src = "DECLARE @x int = 1;\nUPDATE t SET a = @x;\n";
    let c = convert_kind(src, "routine-body");
    assert_eq!(c.output, src);
    assert_eq!(c.units.len(), 1);
    assert!(
        has_rule(&c.units[0], "template.context-leak"),
        "{:?}",
        c.units[0].notes
    );
    assert_eq!(c.units[0].status, Status::Differs);
    assert!(c
        .report
        .unknown_reasons
        .iter()
        .any(|r| r.contains("routine-body")));
}

#[test]
fn dialect_code_values_need_a_value_per_dialect() {
    use sqldiff_convert::ValueKind;
    let c = convert_kind_with(
        "CREATE TABLE <%T%> (id <%TYPE%>, note varchar(10) DEFAULT '<%NOTE%>');\n",
        "statements",
        |t| {
            sqldiff_convert::StandIns::new(t).with_kinds([
                ("TYPE".to_string(), ValueKind::Type),
                ("NOTE".to_string(), ValueKind::Text),
            ])
        },
    );
    let u = &c.units[0];
    // A placeholder type is no type the engine can compare: not proven, and
    // a decision besides.
    assert_eq!(u.status, Status::Unproven, "{:?}", u.notes);
    let n = u
        .notes
        .iter()
        .find(|n| n.rule == "template.value-per-dialect")
        .unwrap();
    assert!(n.text.contains("<%TYPE%> holds a type"), "{}", n.text);
    // Only the name is converted as a name.
    let asm = u
        .notes
        .iter()
        .find(|n| n.rule == "ASM-TEMPLATE-PLACEHOLDER")
        .unwrap();
    assert!(asm.text.starts_with("<%T%> converted"), "{}", asm.text);
}

#[test]
fn fragments_convert_for_db2i_too() {
    use sqldiff_convert::{check_template, rewrite_template_as, FragmentKind, StandIns};
    use sqldiff_core::composition::PlaceholderBasis;
    for (kind, src) in [
        ("predicate", "ISNULL(a, 0) > 1"),
        ("expression", "LEN(a)"),
        ("set-list", "a = ISNULL(b, 1)"),
        ("routine-body", "UPDATE <%T%> SET a = ISNULL(b, 0);"),
    ] {
        let t = angle_template(src);
        let stand = StandIns::new([&t]);
        let k = FragmentKind::parse(kind).unwrap();
        let d = rewrite_template_as(&t, k, Dialect::Tsql, Dialect::Db2i, &stand, None);
        let c = check_template(
            &t,
            d,
            &stand,
            &sqldiff_core::Options::default(),
            PlaceholderBasis::DeclaredPattern,
            ("<%", "%>"),
        );
        assert!(
            c.units
                .iter()
                .all(|u| !has_rule(u, "template.context-leak")),
            "{kind}: {}",
            c.output
        );
        assert!(
            c.output.contains("COALESCE") || c.output.contains("LENGTH"),
            "{kind}: {}",
            c.output
        );
    }
}

#[test]
fn trailing_comma_in_a_column_list_is_refused_not_a_crash() {
    // sakila's SQL Server schema has `PRIMARY KEY NONCLUSTERED (film_id),\n)`.
    let src = "CREATE TABLE film_text (\n  film_id SMALLINT NOT NULL,\n  title VARCHAR(255) NOT NULL,\n  PRIMARY KEY NONCLUSTERED (film_id),\n);\n";
    for to in [Dialect::Postgres, Dialect::Db2i] {
        let c = convert(src, Dialect::Tsql, to).unwrap();
        let u = unit(&c, src, "CREATE TABLE");
        assert_eq!(u.status, Status::NotConverted, "{}", c.output);
        assert!(
            u.notes.iter().any(|n| n.text.contains("trailing comma")),
            "{:?}",
            u.notes
        );
    }
}

/// Incomplete statements (a file cut short, or typed half-way in the GUI)
/// are refused, not a crash of the whole run.
#[test]
fn incomplete_statements_are_not_converted() {
    for text in [
        "INSERT",
        "UPDATE",
        "DELETE",
        "SET",
        "SET @a =",
        "DECLARE",
        "CREATE",
        "PRINT",
        "CREATE PROCEDURE p",
        "SELECT '",
        "SELECT 'é",
    ] {
        for to in [Dialect::Postgres, Dialect::Db2i] {
            let c = convert(text, Dialect::Tsql, to).unwrap();
            assert!(
                c.units.iter().all(|u| u.status != Status::Converted),
                "{text:?} to {to:?}: {:#?}",
                c.units
            );
        }
    }
}

/// Deep nesting runs on the engine's stack instead of aborting the process.
#[test]
fn deep_nesting_does_not_overflow_the_engine_stack() {
    let text = format!("SELECT {}1{};", "(".repeat(5000), ")".repeat(5000));
    let c = sqldiff_core::with_engine_stack(|| tsql(&text));
    assert!(!c.units.is_empty());
}

#[test]
fn char_of_an_ascii_code_becomes_chr() {
    let src = "SELECT 'a' + CHAR(13) + CHAR(10) + 'b' AS s;";
    let c = tsql(src);
    let u = unit(&c, src, "SELECT");
    let out = out_of(&c, u);
    assert!(out.contains("chr(13)") && out.contains("chr(10)"), "{out}");
    assert!(!out.to_uppercase().contains("CHAR("), "{out}");
    // `+` between two calls is concatenation: PostgreSQL has no text + text.
    assert!(out.contains("chr(13) || chr(10)"), "{out}");
    assert!(!out.contains('+'), "{out}");
}

#[test]
fn plus_next_to_a_string_builtin_is_concatenation() {
    let src = "DECLARE @s varchar(200) = DB_NAME() + CHAR(9);";
    let c = tsql(src);
    let out = c.output.clone();
    assert!(out.contains("current_database() || chr(9)"), "{out}");
}

#[test]
fn plus_with_a_number_stays_addition() {
    // T-SQL converts the string side to a number: `CHAR(49) + 1` is 2 and
    // `'1' + 1` is 2. `||` would give '11'.
    for (src, keep) in [
        ("UPDATE t SET a = CHAR(49) + 1;", "chr(49) + 1"),
        ("UPDATE t SET a = '1' + 1;", "'1' + 1"),
        ("UPDATE t SET a = 1 + CHAR(49);", "1 + chr(49)"),
    ] {
        let c = tsql(src);
        let out = c.output.clone();
        assert!(out.contains(keep), "{src}: {out}");
        assert!(!out.contains("||"), "{src}: {out}");
    }
}

#[test]
fn qualified_char_is_a_user_function() {
    // `x.CHAR(5)` is a user function, so it is neither rewritten nor taken as
    // a string for `+`.
    let src = "UPDATE t SET a = x.CHAR(5) + x.CHAR(6);";
    let c = tsql(src);
    let out = c.output.clone();
    assert!(!out.contains("chr("), "{out}");
    assert!(!out.contains("||"), "{out}");
}

#[test]
fn char_of_other_codes_is_not_converted() {
    // 0 is an error in chr(), 128 to 255 depend on the code page, and a value
    // known only at run time may be either.
    for arg in ["0", "128", "200", "256", "@code"] {
        let src = format!("SELECT CHAR({arg}) AS c;");
        let c = tsql(&src);
        let u = unit(&c, &src, "SELECT");
        let out = out_of(&c, u);
        assert!(!out.contains("chr("), "CHAR({arg}): {out}");
        assert_ne!(u.status, Status::Converted, "CHAR({arg}): {out}");
    }
}

#[test]
fn db_name_without_an_argument_becomes_current_database() {
    let src = "SELECT DB_NAME() AS n;";
    let c = tsql(src);
    let u = unit(&c, src, "SELECT");
    assert!(
        out_of(&c, u).contains("current_database()"),
        "{}",
        out_of(&c, u)
    );
}

#[test]
fn db_name_of_an_id_is_not_converted() {
    let src = "SELECT DB_NAME(5) AS n;";
    let c = tsql(src);
    let u = unit(&c, src, "SELECT");
    let out = out_of(&c, u);
    assert!(!out.contains("current_database"), "{out}");
    assert_ne!(u.status, Status::Converted, "{out}");
}
