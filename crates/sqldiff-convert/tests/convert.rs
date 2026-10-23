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
fn unsupported_pairs_are_refused() {
    assert!(convert("SELECT 1", Dialect::Postgres, Dialect::Tsql).is_err());
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
