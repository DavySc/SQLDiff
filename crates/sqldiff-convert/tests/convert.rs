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
    assert_eq!(u.status, Status::Assumptions);
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
        c.units[0].status,
        Status::Converted,
        "{:?}",
        c.units[0].notes
    );
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
    assert!(has_rule(
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
        c.output
            .contains("SELECT Name INTO v_n FROM t ORDER BY Name LIMIT (1);"),
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
