//! T-SQL → Db2 for i: output shape, notes, and the engine check.

use sqldiff_convert::{convert, supported, Conversion, Status, Unit};
use sqldiff_core::{ChangeClass, Dialect};

fn db2(text: &str) -> Conversion {
    convert(text, Dialect::Tsql, Dialect::Db2i).unwrap()
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

fn has_rule(u: &Unit, rule: &str) -> bool {
    u.notes.iter().any(|n| n.rule == rule)
}

fn any_rule(c: &Conversion, rule: &str) -> bool {
    c.units.iter().any(|u| has_rule(u, rule))
}

#[test]
fn tsql_to_db2_is_supported() {
    assert!(supported(Dialect::Tsql, Dialect::Db2i));
    assert!(!supported(Dialect::Postgres, Dialect::Db2i));
    let c = db2("SELECT 1;\n");
    assert_eq!(c.to, "db2i");
}

#[test]
fn select_rewrites() {
    let src = "SELECT TOP 10 [Name] + ' x' AS n, ISNULL(a, 0), LEN(b), NEWID(), GETDATE()\nFROM dbo.Items WITH (NOLOCK)\nWHERE c = 1\nORDER BY n;\n";
    let c = db2(src);
    let o = &c.output;
    assert!(o.contains("Name || ' x' AS n"), "{o}");
    assert!(o.contains("COALESCE(a, 0)"), "{o}");
    assert!(o.contains("LENGTH(RTRIM(b))"), "{o}");
    assert!(o.contains("GENERATE_UUID()"), "{o}");
    assert!(o.contains("CURRENT TIMESTAMP"), "{o}");
    assert!(o.contains("FROM Items\n"), "{o}");
    assert!(o.trim_end().ends_with("FETCH FIRST 10 ROWS ONLY;"), "{o}");
    let u = &c.units[0];
    for r in [
        "ASM-NOLOCK",
        "ASM-UUID-TEXT",
        "ASM-TIME-SOURCE",
        "ASM-DB2-LENGTH",
    ] {
        assert!(has_rule(u, r), "{r}: {:?}", u.notes);
    }
}

#[test]
fn procedure_becomes_sql_pl_with_the_commit_level_written_out() {
    let src = "CREATE PROCEDURE dbo.Bump @Id int, @Total int OUTPUT AS\nBEGIN\n  SET NOCOUNT ON;\n  DECLARE @n int = 0;\n  WHILE @n < 3\n  BEGIN\n    SET @n += 1;\n    IF @n = 2 BREAK;\n  END\n  IF @Id > 10\n    SET @Total = @n;\n  ELSE IF @Id > 5\n    SET @Total = 1;\n  ELSE\n    RETURN 4;\nEND\n";
    let c = db2(src);
    let o = &c.output;
    assert!(
        o.starts_with("CREATE OR REPLACE PROCEDURE Bump (IN p_id INTEGER, INOUT p_total INTEGER)\nLANGUAGE SQL\nSET OPTION COMMIT = *CS, USRPRF = *USER, DYNUSRPRF = *USER\nBEGIN\n  DECLARE v_n INTEGER;\n"),
        "{o}"
    );
    assert!(o.contains("SET v_n = 0;"), "{o}");
    assert!(o.contains("loop_1: WHILE v_n < 3 DO"), "{o}");
    assert!(o.contains("SET v_n = v_n + 1;"), "{o}");
    assert!(o.contains("LEAVE loop_1;"), "{o}");
    assert!(o.contains("END WHILE loop_1;"), "{o}");
    assert!(o.contains("ELSEIF p_id > 5 THEN"), "{o}");
    assert!(o.contains("RETURN 4;"), "{o}");
    assert!(o.trim_end().ends_with("END;"), "{o}");
    assert!(any_rule(&c, "ASM-DB2-COMMIT-CS"));
    assert!(any_rule(&c, "ASM-RETURN-STATUS"));
    let diags = sqldiff_syntax::parse(o, Dialect::Db2i).diagnostics;
    assert!(diags.is_empty(), "{diags:?}\n{o}");
}

#[test]
fn script_with_variables_becomes_a_compound_statement() {
    let src = "DECLARE @c int;\nSELECT @c = COUNT(*) FROM t;\nIF @c > 0 DELETE FROM t;\n";
    let c = db2(src);
    let o = &c.output;
    assert!(o.starts_with("BEGIN\n  DECLARE v_c INTEGER;\n"), "{o}");
    assert!(o.contains("SELECT COUNT(*) INTO v_c FROM t;"), "{o}");
    assert!(o.trim_end().ends_with("END;"), "{o}");
    assert!(!o.contains("$$"), "{o}");
}

#[test]
fn xact_abort_on_becomes_a_rollback_handler() {
    let src = "CREATE PROCEDURE p @x int AS\nBEGIN\n  SET NOCOUNT ON;\n  SET XACT_ABORT ON;\n  DECLARE @y int;\n  UPDATE t SET a = @x;\nEND\n";
    let c = db2(src);
    let o = &c.output;
    let decl = o.find("DECLARE v_y INTEGER;").expect(o);
    let handler = o
        .find(
            "DECLARE EXIT HANDLER FOR SQLEXCEPTION\n  BEGIN\n    ROLLBACK;\n    RESIGNAL;\n  END;",
        )
        .expect(o);
    assert!(decl < handler, "variables come before handlers:\n{o}");
    assert!(has_rule(unit(&c, src, "XACT_ABORT"), "db2.xact-abort"));

    // After other work it would not cover what ran before: refused.
    let late = "CREATE PROCEDURE p AS\nBEGIN\n  DELETE FROM t;\n  SET XACT_ABORT ON;\nEND\n";
    let c = db2(late);
    assert_eq!(unit(&c, late, "XACT_ABORT").status, Status::NotConverted);
}

#[test]
fn try_catch_becomes_an_exit_handler() {
    let src = "CREATE PROCEDURE p AS\nBEGIN\n  BEGIN TRY\n    DELETE FROM t;\n  END TRY\n  BEGIN CATCH\n    INSERT INTO log (msg, num) VALUES (ERROR_MESSAGE(), ERROR_NUMBER());\n  END CATCH\nEND\n";
    let c = db2(src);
    let o = &c.output;
    assert!(o.contains("  BEGIN\n    DECLARE EXIT HANDLER FOR SQLEXCEPTION\n    BEGIN\n      DECLARE v_error_message VARCHAR(32740);\n      DECLARE v_error_number INTEGER;\n      GET DIAGNOSTICS CONDITION 1 v_error_message = MESSAGE_TEXT, v_error_number = DB2_RETURNED_SQLCODE;\n      INSERT INTO log (msg, num) VALUES (v_error_message, v_error_number);\n    END;\n    DELETE FROM t;\n  END;"), "{o}");
    assert!(any_rule(&c, "ASM-TRY-HANDLER"));
    assert!(any_rule(&c, "db2.sqlcode"));
    let diags = sqldiff_syntax::parse(o, Dialect::Db2i).diagnostics;
    assert!(diags.is_empty(), "{diags:?}\n{o}");
}

#[test]
fn rollback_rethrow_becomes_rollback_resignal() {
    let src = "CREATE PROCEDURE p AS\nBEGIN\n  BEGIN TRY\n    BEGIN TRAN;\n    DELETE FROM t;\n    COMMIT;\n  END TRY\n  BEGIN CATCH\n    IF @@TRANCOUNT > 0 ROLLBACK;\n    THROW;\n  END CATCH\nEND\n";
    let c = db2(src);
    let o = &c.output;
    assert!(
        o.contains("BEGIN\n      ROLLBACK;\n      RESIGNAL;\n    END;"),
        "{o}"
    );
    // Db2 allows COMMIT inside the compound statement (it is not ATOMIC).
    assert_ne!(unit(&c, src, "COMMIT").status, Status::NotConverted, "{o}");
    assert!(any_rule(&c, "idiom.rollback-rethrow"));
    assert!(!o.contains("@@TRANCOUNT"), "{o}");
}

#[test]
fn throw_becomes_signal() {
    let src =
        "CREATE PROCEDURE p @x int AS\nBEGIN\n  IF @x IS NULL THROW 50001, 'missing', 1;\nEND\n";
    let c = db2(src);
    assert!(
        c.output
            .contains("SIGNAL SQLSTATE '75000' SET MESSAGE_TEXT = 'missing';"),
        "{}",
        c.output
    );
    assert!(any_rule(&c, "ASM-THROW-ERRCODE"));
}

#[test]
fn result_sets_become_return_cursors() {
    let src = "CREATE PROCEDURE p @id int AS\nBEGIN\n  DECLARE @n int;\n  SELECT a, b FROM t WHERE id = @id;\nEND\n";
    let c = db2(src);
    let o = &c.output;
    assert!(o.contains("DYNAMIC RESULT SETS 1\nLANGUAGE SQL"), "{o}");
    let var = o.find("DECLARE v_n INTEGER;").expect(o);
    let cur = o
        .find("DECLARE result_1 INSENSITIVE CURSOR WITH RETURN TO CLIENT FOR\n    SELECT a, b FROM t WHERE id = p_id;")
        .expect(o);
    assert!(var < cur, "variables come before cursors:\n{o}");
    assert!(o.contains("  OPEN result_1;"), "{o}");
    assert!(any_rule(&c, "ASM-DB2-RESULT-SET"));

    // A function cannot return a result set.
    let f = "CREATE FUNCTION f() RETURNS int AS\nBEGIN\n  SELECT a FROM t;\n  RETURN 1;\nEND\n";
    let c = db2(f);
    assert_eq!(unit(&c, f, "SELECT a").status, Status::NotConverted);
}

#[test]
fn transactions_and_savepoints() {
    let src = "CREATE PROCEDURE p AS\nBEGIN\n  BEGIN TRAN;\n  SAVE TRAN sp1;\n  DELETE FROM t;\n  ROLLBACK TRAN sp1;\n  COMMIT;\nEND\n";
    let c = db2(src);
    let o = &c.output;
    assert!(
        o.contains("SAVEPOINT sp1 ON ROLLBACK RETAIN CURSORS;"),
        "{o}"
    );
    assert!(o.contains("ROLLBACK TO SAVEPOINT sp1;"), "{o}");
    assert!(any_rule(&c, "ASM-FACT-V14"));
    assert_eq!(unit(&c, src, "COMMIT").status, Status::Decision);

    // An unknown name is never guessed to be the whole transaction.
    let named = "CREATE PROCEDURE p AS\nBEGIN\n  ROLLBACK TRAN outer_tx;\nEND\n";
    let c = db2(named);
    assert_eq!(unit(&c, named, "ROLLBACK").status, Status::NotConverted);
    let c = convert(named, Dialect::Tsql, Dialect::Postgres).unwrap();
    assert_eq!(unit(&c, named, "ROLLBACK").status, Status::NotConverted);
}

#[test]
fn create_table_types_and_defaults() {
    let src = "CREATE TABLE dbo.Item (\n    Id INT IDENTITY(1,1),\n    Code NVARCHAR(20) NOT NULL DEFAULT (('x')),\n    Active BIT NOT NULL DEFAULT ((1)),\n    Price MONEY,\n    Notes NVARCHAR(MAX),\n    Guid UNIQUEIDENTIFIER,\n    At DATETIME2 DEFAULT GETDATE(),\n    CONSTRAINT PK_Item PRIMARY KEY CLUSTERED (Id)\n) ON [PRIMARY];\n";
    let c = db2(src);
    let o = &c.output;
    assert!(
        o.contains(
            "Id INTEGER GENERATED ALWAYS AS IDENTITY (START WITH 1 INCREMENT BY 1) NOT NULL"
        ),
        "{o}"
    );
    assert!(o.contains("Code NVARCHAR(20) NOT NULL DEFAULT 'x'"), "{o}");
    assert!(o.contains("Active SMALLINT NOT NULL DEFAULT 1"), "{o}");
    assert!(o.contains("Price DECIMAL(19, 4)"), "{o}");
    assert!(o.contains("Notes NCLOB(1G)"), "{o}");
    assert!(o.contains("Guid CHAR(36)"), "{o}");
    assert!(
        o.contains("At TIMESTAMP(7) DEFAULT CURRENT TIMESTAMP"),
        "{o}"
    );
    assert!(o.contains("CONSTRAINT PK_Item PRIMARY KEY (Id)"), "{o}");
    assert!(!o.contains("PRIMARY]"), "{o}");
    let u = &c.units[0];
    // Db2 for i column types are not compared by the engine yet (no verified
    // facts): not proven, and NVARCHAR(MAX) is a decision besides.
    assert_eq!(u.status, Status::Unproven, "{:?}", u.notes);
    assert!(u
        .notes
        .iter()
        .any(|n| n.status == Status::Decision && n.rule == "db2.lob"));
    for r in [
        "ASM-IDENTITY",
        "ASM-BIT-SMALLINT",
        "ASM-UUID-CHAR",
        "db2.lob",
    ] {
        assert!(has_rule(u, r), "{r}: {:?}", u.notes);
    }
    let diags = sqldiff_syntax::parse(o, Dialect::Db2i).diagnostics;
    assert!(diags.is_empty(), "{diags:?}\n{o}");

    // A function default has no Db2 form: refused rather than guessed.
    let f = "CREATE TABLE t (id UNIQUEIDENTIFIER DEFAULT NEWID());\n";
    let c = db2(f);
    assert_eq!(c.units[0].status, Status::NotConverted, "{}", c.output);
}

#[test]
fn temp_tables_and_table_variables_are_global_temporary_tables() {
    let src = "CREATE PROCEDURE p AS\nBEGIN\n  CREATE TABLE #work (id INT);\n  DECLARE @seen TABLE (id INT PRIMARY KEY);\n  INSERT INTO #work (id) SELECT id FROM @seen;\n  DROP TABLE #work;\nEND\n";
    let c = db2(src);
    let o = &c.output;
    assert!(
        o.contains("DECLARE GLOBAL TEMPORARY TABLE SESSION.work (\n"),
        "{o}"
    );
    assert!(
        o.contains(") ON COMMIT PRESERVE ROWS LOGGED WITH REPLACE;"),
        "{o}"
    );
    // V16: only NOT LOGGED ON ROLLBACK PRESERVE ROWS keeps a table variable's rows.
    // The name carries the routine's, so a called routine's @seen is another table.
    assert!(
        o.contains("DECLARE GLOBAL TEMPORARY TABLE SESSION.tv_p_seen ("),
        "{o}"
    );
    assert!(o.contains("id INTEGER NOT NULL PRIMARY KEY"), "{o}");
    assert!(
        o.contains(") ON COMMIT PRESERVE ROWS NOT LOGGED ON ROLLBACK PRESERVE ROWS WITH REPLACE;"),
        "{o}"
    );
    assert!(
        o.contains("INSERT INTO SESSION.work (id) SELECT id FROM SESSION.tv_p_seen;"),
        "{o}"
    );
    assert!(o.contains("DROP TABLE SESSION.work;"), "{o}");
    assert!(any_rule(&c, "ASM-TABLE-VARIABLE"));
    assert!(any_rule(&c, "ASM-TEMP-TABLE"));
}

#[test]
fn date_functions() {
    let src = "CREATE TABLE t (d date, a date, b date);\nGO\nSELECT DATEADD(day, 3, d), DATEADD(month, 1, d), DATEPART(hour, d), DATEDIFF(day, a, b), DATEDIFF(month, a, b) FROM t;\nSELECT DATEDIFF(week, a, b) FROM t;\n";
    let c = db2(src);
    let o = &c.output;
    assert!(o.contains("(d + 3 DAYS)"), "{o}");
    assert!(o.contains("(d + 1 MONTHS)"), "{o}");
    assert!(o.contains("HOUR(d)"), "{o}");
    assert!(o.contains("(DAYS(b) - DAYS(a))"), "{o}");
    assert!(
        o.contains("((YEAR(b) - YEAR(a)) * 12 + MONTH(b) - MONTH(a))"),
        "{o}"
    );
    assert_ne!(c.units[1].status, Status::NotConverted, "{o}");
    // Week boundaries are not guessed.
    assert_eq!(c.units[2].status, Status::NotConverted, "{o}");
}

#[test]
fn datediff_day_matches_the_corpus_conversion() {
    let src = std::fs::read_to_string(root("corpus/pairs/p12-days-between/tsql.sql")).unwrap();
    let c = db2(&src);
    assert!(
        c.output.contains("(DAYS(shipped_at) - DAYS(created_at))"),
        "{}",
        c.output
    );
    assert!(
        c.units.iter().all(|u| u.checked != Status::Differs),
        "{:#?}",
        c.report.findings
    );
}

#[test]
fn unverified_vendor_facts_surface_as_assumptions() {
    // V9: CAST('' AS INT) on Db2 for i was never checked on a real system.
    let src = "SELECT CAST(code AS INT), CAST(5 AS BIGINT) FROM t;\n";
    let c = db2(src);
    assert!(any_rule(&c, "ASM-FACT-V9"), "{:?}", c.units);
    let src = "SELECT CAST(5 AS BIGINT) FROM t;\n";
    assert!(!any_rule(&db2(src), "ASM-FACT-V9"));
    // V4/V5: a script's transaction statements depend on the job's commit level.
    let src = "BEGIN TRAN;\nDELETE FROM t;\nCOMMIT;\n";
    assert!(any_rule(&db2(src), "ASM-DB2-SCRIPT-COMMIT"));
    // V15: lock hints point at the unverified lock clause.
    let src = "SELECT a FROM t WITH (UPDLOCK) WHERE id = 1;\n";
    let c = db2(src);
    let n = c.units[0]
        .notes
        .iter()
        .find(|n| n.rule == "tsql.lock-hint")
        .unwrap();
    assert!(n.text.contains("V15"), "{}", n.text);
}

#[test]
fn constructs_without_rules_are_kept_as_comments() {
    for src in [
        "MERGE INTO t USING s ON t.id = s.id WHEN NOT MATCHED BY SOURCE THEN DELETE;\n",
        "UPDATE t SET a = s.a FROM t JOIN s ON s.id = t.id;\n",
        "SELECT FORMAT(d, 'yyyy') FROM t;\n",
        "CREATE TABLE t (d DATETIMEOFFSET);\n",
        "DROP TABLE IF EXISTS t;\n",
    ] {
        let c = db2(src);
        assert_eq!(
            c.units[0].status,
            Status::NotConverted,
            "{src}\n{}",
            c.output
        );
        assert!(c.output.starts_with("-- [not converted]"), "{}", c.output);
    }
}

#[test]
fn merge_is_kept_without_by_target() {
    let src = "CREATE PROCEDURE p @id int, @price decimal(12,2) AS\nBEGIN\n  MERGE dbo.item_price AS t\n  USING (SELECT @id AS item_id, @price AS unit_price) AS s\n  ON t.item_id = s.item_id\n  WHEN MATCHED THEN UPDATE SET unit_price = s.unit_price\n  WHEN NOT MATCHED BY TARGET THEN INSERT (item_id, unit_price) VALUES (s.item_id, s.unit_price);\nEND\n";
    let c = db2(src);
    let o = &c.output;
    assert!(o.contains("MERGE INTO item_price AS t"), "{o}");
    // Db2 for i needs a FROM in the source query.
    assert!(
        o.contains(
            "USING (SELECT p_id AS item_id, p_price AS unit_price FROM SYSIBM.SYSDUMMY1) AS s"
        ),
        "{o}"
    );
    assert!(o.contains("WHEN NOT MATCHED THEN INSERT"), "{o}");
    let u = unit(&c, src, "MERGE");
    assert!(has_rule(u, "db2.merge"), "{:?}", u.notes);
    // With FROM SYSIBM.SYSDUMMY1 read as no FROM, the engine compares the
    // MERGEs and finds only the race of a T-SQL MERGE without HOLDLOCK.
    let different: Vec<&str> = u
        .findings
        .iter()
        .map(|&i| &c.report.findings[i])
        .filter(|f| f.verdict == sqldiff_core::Verdict::Different)
        .map(|f| f.rule.as_str())
        .collect();
    assert!(
        different.iter().all(|r| *r == "equiv.upsert-race"),
        "{:#?}",
        c.report.findings
    );
    // Outside a procedure the commit level is the job's: fact V2 is assumed.
    let script = "MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN DELETE;\n";
    assert!(any_rule(&db2(script), "ASM-FACT-V2"));
}

#[test]
fn select_without_from_gets_the_dummy_table() {
    let src = "SELECT 1 AS one;\nINSERT INTO t (a) SELECT 2;\nGO\nIF EXISTS (SELECT 1 WHERE 1 = 1) PRINT 'x';\n";
    let c = db2(src);
    let o = &c.output;
    assert!(o.contains("SELECT 1 AS one FROM SYSIBM.SYSDUMMY1;"), "{o}");
    assert!(
        o.contains("INSERT INTO t (a) SELECT 2 FROM SYSIBM.SYSDUMMY1;"),
        "{o}"
    );
    assert!(
        o.contains("EXISTS (SELECT 1 FROM SYSIBM.SYSDUMMY1 WHERE 1 = 1)"),
        "{o}"
    );
    // The dropped PRINT leaves the IF without a statement: Db2 needs one.
    assert!(
        o.contains("    -- PRINT 'x': dropped\n    BEGIN END;\n  END IF;"),
        "{o}"
    );
}

/// Every corpus T-SQL file converts without leaving T-SQL in live output, and
/// the output parses as Db2 for i.
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
        let c = db2(&src);
        let diags = sqldiff_syntax::parse(&c.output, Dialect::Db2i).diagnostics;
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
        for u in &c.units {
            assert!(u.src.start <= u.src.end && u.src.end <= src.len());
            assert!(u.out.start <= u.out.end && u.out.end <= c.output.len());
            assert!(u.status >= u.checked);
        }
    }
}

#[test]
fn trancount_guarded_rollback_becomes_rollback() {
    let src = "CREATE PROCEDURE p AS\nBEGIN\n  BEGIN TRY\n    DELETE FROM t;\n  END TRY\n  BEGIN CATCH\n    IF @@TRANCOUNT > 0 ROLLBACK TRAN;\n    INSERT INTO log (msg) VALUES (ERROR_MESSAGE());\n  END CATCH\nEND\n";
    let c = db2(src);
    let u = unit(&c, src, "@@TRANCOUNT");
    assert_eq!(u.status, Status::Decision, "{}", c.output);
    assert!(has_rule(u, "db2.proc-transaction"));
    assert!(
        c.output.contains("      ROLLBACK;\n      INSERT INTO log"),
        "{}",
        c.output
    );
    assert!(!c.output.contains("@@TRANCOUNT"), "{}", c.output);
}

#[test]
fn result_cursor_over_a_temp_table_notes_runtime_resolution() {
    let src =
        "CREATE PROCEDURE p AS\nBEGIN\n  CREATE TABLE #r (id INT);\n  SELECT id FROM #r;\nEND\n";
    assert!(any_rule(&db2(src), "ASM-DB2-SESSION-RUNTIME"));
}

#[test]
fn execute_as_owner_becomes_usrprf_owner() {
    let src = "CREATE PROCEDURE dbo.Purge @Id int WITH EXECUTE AS OWNER AS\nBEGIN\n  DELETE FROM dbo.item WHERE item_id = @Id;\nEND\n";
    let c = db2(src);
    assert!(
        c.output
            .contains("SET OPTION COMMIT = *CS, USRPRF = *OWNER, DYNUSRPRF = *OWNER\nBEGIN"),
        "{}",
        c.output
    );
    assert!(any_rule(&c, "ASM-OWNER-PRIVILEGES"));
    let f = "CREATE FUNCTION dbo.F (@Id int) RETURNS int AS\nBEGIN\n  RETURN @Id + 1;\nEND\n";
    let c = db2(f);
    assert!(
        c.output
            .contains("SET OPTION USRPRF = *USER, DYNUSRPRF = *USER"),
        "{}",
        c.output
    );
}

#[test]
fn delete_with_inner_joins_becomes_delete_where_exists() {
    let cases = [
        (
            "DELETE o FROM dbo.orders o JOIN dbo.customer c ON c.customer_id = o.customer_id WHERE c.customer_id > 10 OR c.nick IS NULL;",
            "DELETE FROM orders o WHERE EXISTS (SELECT 1 FROM customer c WHERE (c.customer_id = o.customer_id) AND (c.customer_id > 10 OR c.nick IS NULL));",
        ),
        (
            "DELETE orders FROM dbo.orders JOIN dbo.customer c ON c.customer_id = orders.customer_id, dbo.item i WHERE i.item_id = c.customer_id;",
            "DELETE FROM orders WHERE EXISTS (SELECT 1 FROM customer c, item i WHERE (c.customer_id = orders.customer_id) AND (i.item_id = c.customer_id));",
        ),
        (
            "DELETE o FROM dbo.orders o JOIN dbo.orders o2 ON o2.customer_id = o.customer_id AND o2.order_id > o.order_id;",
            "DELETE FROM orders o WHERE EXISTS (SELECT 1 FROM orders o2 WHERE (o2.customer_id = o.customer_id AND o2.order_id > o.order_id));",
        ),
    ];
    for (src, want) in cases {
        let c = db2(src);
        assert_eq!(c.output.trim(), want, "{src}");
        assert!(
            c.units[0].status < Status::Differs,
            "{src}: {:?}",
            c.units[0].notes
        );
        assert!(
            !c.report
                .findings
                .iter()
                .any(|f| f.rule == "equiv.unsupported"),
            "{src}: {:?}",
            c.report.findings
        );
    }
    // Inside EXISTS an unqualified name would resolve to the joined table
    // first; T-SQL rejects one that both tables have (Msg 209). Without a
    // catalog to show which table owns it, it is refused.
    for src in [
        "DELETE o FROM dbo.orders o JOIN dbo.customer c ON c.customer_id = o.customer_id WHERE nick IS NULL;",
        "DELETE o FROM dbo.orders o JOIN dbo.customer c ON customer_id = o.customer_id;",
        "DELETE o FROM dbo.orders o JOIN dbo.customer c ON c.customer_id = o.customer_id WHERE c.nick IN (SELECT nick FROM dbo.item);",
    ] {
        assert_eq!(db2(src).units[0].status, Status::NotConverted, "{src}");
    }
    // Without other tables nothing moves into a subquery.
    let one = "DELETE o FROM dbo.orders o WHERE order_id = 1;";
    assert_eq!(
        db2(one).output.trim(),
        "DELETE FROM orders o WHERE (order_id = 1);"
    );
    let left = "DELETE o FROM dbo.orders o LEFT JOIN dbo.customer c ON c.customer_id = o.customer_id WHERE c.nick = 'a';";
    assert_eq!(db2(left).units[0].status, Status::NotConverted);
}

#[test]
fn table_function_in_from_becomes_table_reference() {
    let cases = [
        (
            "SELECT f.order_id FROM dbo.f(1) f;",
            "SELECT f.order_id FROM TABLE(f(1)) f;",
        ),
        ("SELECT * FROM dbo.f(1);", "SELECT * FROM TABLE(f(1)) AS f;"),
        (
            "SELECT x.order_id FROM dbo.f(1) AS x JOIN dbo.orders o ON o.order_id = x.order_id;",
            "SELECT x.order_id FROM TABLE(f(1)) AS x JOIN orders o ON o.order_id = x.order_id;",
        ),
        (
            "SELECT a.order_id FROM dbo.orders a, dbo.f(2) g WHERE a.order_id = g.order_id;",
            "SELECT a.order_id FROM orders a, TABLE(f(2)) g WHERE a.order_id = g.order_id;",
        ),
        (
            "SELECT o.order_id FROM dbo.orders o WHERE EXISTS (SELECT 1 FROM dbo.f(o.order_id) z);",
            "SELECT o.order_id FROM orders o WHERE EXISTS (SELECT 1 FROM TABLE(f(o.order_id)) z);",
        ),
        (
            "DELETE FROM dbo.orders WHERE order_id IN (SELECT order_id FROM dbo.f(3));",
            "DELETE FROM orders WHERE order_id IN (SELECT order_id FROM TABLE(f(3)) AS f);",
        ),
        // Not a FROM item: a call in the select list, and TRIM's FROM.
        (
            "SELECT TRIM('x' FROM LEFT(nick, 2)), COALESCE(a, b) FROM dbo.orders;",
            "SELECT TRIM('x' FROM LEFT(nick, 2)), COALESCE(a, b) FROM orders;",
        ),
    ];
    for (src, want) in cases {
        let c = db2(src);
        assert_eq!(c.output.trim(), want, "{src}");
        assert!(
            !c.output_check
                .findings
                .iter()
                .any(|f| f.rule == "bind.table-function-form"),
            "{src}: {:?}",
            c.output_check.findings
        );
    }
    // An old-style table hint is not a call; the hint itself stays residue.
    let c = db2("SELECT o.order_id FROM dbo.orders o JOIN dbo.customer (NOLOCK) ON 1 = 1;");
    assert_eq!(
        c.output.trim(),
        "SELECT o.order_id FROM orders o JOIN customer (NOLOCK) ON 1 = 1;"
    );
    assert!(c.units[0].status >= Status::Differs);
    // T-SQL's rowset built-ins and sys functions have no rule.
    for src in [
        "SELECT value FROM STRING_SPLIT('a,b', ',');",
        "SELECT * FROM sys.fn_foo(1);",
    ] {
        let c = db2(src);
        assert_eq!(c.units[0].status, Status::NotConverted, "{src}");
        assert!(
            c.units[0]
                .notes
                .iter()
                .any(|n| n.text.contains("table function; no Db2 for i rule")),
            "{src}: {:?}",
            c.units[0].notes
        );
    }
}

#[test]
fn with_data_change_is_refused() {
    for (src, verb) in [
        (
            "WITH c AS (SELECT order_id FROM dbo.orders) DELETE c FROM c JOIN dbo.orders o ON o.order_id = c.order_id;",
            "DELETE",
        ),
        (
            "WITH c AS (SELECT order_id FROM dbo.orders) DELETE FROM dbo.orders WHERE order_id IN (SELECT order_id FROM c);",
            "DELETE",
        ),
        ("WITH c AS (SELECT order_id FROM dbo.orders) UPDATE c SET order_id = 2;", "UPDATE"),
        (
            "WITH c AS (SELECT order_id FROM dbo.orders) INSERT INTO dbo.orders (order_id) SELECT order_id + 100 FROM c;",
            "INSERT",
        ),
    ] {
        let c = db2(src);
        assert_eq!(c.units.len(), 1, "{src}: {:?}", c.units);
        assert_eq!(c.units[0].status, Status::NotConverted, "{src}");
        assert!(
            c.units[0].notes[0].text.starts_with(&format!("WITH … {verb}")),
            "{src}: {:?}",
            c.units[0].notes
        );
        assert!(
            c.output.lines().all(|l| l.trim_start().starts_with("--")),
            "{src}: {}",
            c.output
        );
    }
    // In a procedure the statement is not turned into a result set.
    let src = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  WITH c AS (SELECT order_id FROM dbo.orders) DELETE c FROM c JOIN dbo.orders o ON o.order_id = c.order_id;\nEND\n";
    let c = db2(src);
    assert!(!c.output.contains("CURSOR"), "{}", c.output);
    assert!(c.output.contains("SIGNAL SQLSTATE '75001'"), "{}", c.output);
    let u = unit(&c, src, "WITH c");
    assert_eq!(u.status, Status::NotConverted);
}

#[test]
fn delete_join_with_a_table_function_uses_table() {
    let c = db2("DELETE o FROM dbo.orders o JOIN dbo.f(1) x ON x.order_id = o.order_id;");
    assert_eq!(
        c.output.trim(),
        "DELETE FROM orders o WHERE EXISTS (SELECT 1 FROM TABLE(f(1)) x WHERE (x.order_id = o.order_id));"
    );
    assert!(
        !c.output_check
            .findings
            .iter()
            .any(|f| f.rule == "bind.table-function-form"),
        "{:?}",
        c.output_check.findings
    );
    let c = db2("DELETE o FROM dbo.orders o, dbo.f(1) WHERE f.order_id = o.order_id;");
    assert!(
        c.output.contains("FROM TABLE(f(1)) AS f WHERE"),
        "{}",
        c.output
    );
}

/// A view's query, CTEs included, gets the dummy table like a procedure's.
#[test]
fn view_select_without_from_gets_the_dummy_table() {
    let src = "CREATE VIEW dbo.v AS\nWITH r (n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 5) SELECT n FROM r;\n";
    let c = db2(src);
    assert!(
        c.output.contains("WITH r (n) AS (SELECT 1 FROM SYSIBM.SYSDUMMY1 UNION ALL SELECT n + 1 FROM r WHERE n < 5) SELECT n FROM r;"),
        "{}",
        c.output
    );
    assert!(
        !c.output_check
            .findings
            .iter()
            .any(|f| f.rule == "residue.db2i.select-without-from"),
        "{:?}",
        c.output_check.findings
    );
    // Run twice over the same tokens (statement, then query), it adds one FROM.
    let c =
        db2("CREATE PROCEDURE p AS\nBEGIN\n  WITH c AS (SELECT 1 AS a) SELECT a FROM c;\nEND\n");
    assert_eq!(c.output.matches("SYSDUMMY1").count(), 1, "{}", c.output);
}
