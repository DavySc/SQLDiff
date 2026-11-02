//! T-SQL without `SET XACT_ABORT ON` goes on after a statement fails. A
//! PostgreSQL procedure stops at the first error, so each statement that
//! can fail gets its own `BEGIN … EXCEPTION WHEN <errors that end the T-SQL
//! batch> THEN RAISE; WHEN OTHERS THEN RAISE WARNING … END` and a COMMIT where
//! T-SQL commits it on its own (`tsql.txn.continue`,
//! `ASM-ERROR-CONTINUES-WARNING`). Conversion errors (245, 8114, 241), lock
//! errors and missing objects end the T-SQL batch and so the procedure here
//! (SQL Server 2022 and PostgreSQL 18, `eval/xact-abort/`); every other error
//! is reported and the next statement runs.

use std::collections::BTreeMap;

use sqldiff_convert::{check, rewrite_in, Conversion, Status};
use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{Artifact, Dialect, Options, Verdict};

const TABLES: &str = "CREATE TABLE dbo.A (Id int NOT NULL PRIMARY KEY, Qty int NOT NULL);
CREATE TABLE dbo.B (Id int NOT NULL PRIMARY KEY, Qty int NOT NULL);
CREATE TABLE dbo.C (Id int NOT NULL PRIMARY KEY, Qty int NOT NULL);
";

fn pg(src: &str) -> Conversion {
    let mut cat = Catalog::default();
    for f in [TABLES, src] {
        let root = sqldiff_syntax::parse(f, Dialect::Tsql).syntax();
        ddl::load_tree(&mut cat, &root, Dialect::Tsql);
    }
    let target = rewrite_in(TABLES, Dialect::Tsql, Dialect::Postgres, None)
        .output()
        .to_string();
    let opts = Options {
        catalog_a: vec![Artifact::new("t.sql", Dialect::Tsql, TABLES)],
        catalog_b: vec![Artifact::new("t.sql", Dialect::Postgres, &target)],
        flips: BTreeMap::new(),
        ..Options::default()
    };
    check(
        src,
        rewrite_in(src, Dialect::Tsql, Dialect::Postgres, Some(&cat)),
        &opts,
    )
}

fn count(c: &Conversion, needle: &str) -> usize {
    c.output
        .lines()
        .filter(|l| l.trim_start().starts_with(needle))
        .count()
}

fn has_note(c: &Conversion, rule: &str) -> bool {
    c.units
        .iter()
        .flat_map(|u| &u.notes)
        .any(|n| n.rule == rule)
}

const THREE: &str = "CREATE PROCEDURE dbo.Spread @Id int AS
BEGIN
    SET NOCOUNT ON;
    INSERT dbo.A (Id, Qty) VALUES (@Id, 1);
    UPDATE dbo.B SET Qty = Qty + 1 WHERE Id = @Id;
    DELETE FROM dbo.C WHERE Id = @Id;
END;
";

#[test]
fn each_statement_gets_a_block_and_a_commit() {
    let c = pg(THREE);
    assert_eq!(count(&c, "BEGIN"), 4, "{}", c.output); // routine + three blocks
    assert_eq!(count(&c, "EXCEPTION WHEN"), 3, "{}", c.output);
    assert_eq!(count(&c, "RAISE WARNING"), 3, "{}", c.output);
    assert_eq!(count(&c, "COMMIT;"), 3, "{}", c.output);
    assert!(
        c.output.contains("T-SQL (XACT_ABORT OFF)"),
        "the continuation is named in the output:\n{}",
        c.output
    );
    assert!(has_note(&c, "ASM-ERROR-CONTINUES-WARNING"));
    // Conversion errors are re-raised: T-SQL ends the batch at one. Every
    // other error is reported and the procedure goes on.
    assert_eq!(count(&c, "WHEN OTHERS THEN"), 3, "{}", c.output);
    assert_eq!(count(&c, "RAISE;"), 3, "{}", c.output);
    assert_eq!(
        count(&c, "EXCEPTION WHEN invalid_text_representation OR"),
        3,
        "{}",
        c.output
    );
}

#[test]
fn dml_that_may_raise_a_conversion_error_keeps_its_commits_unproven() {
    let c = pg(THREE);
    assert_eq!(
        c.report.verdict(),
        Verdict::Unknown,
        "conversion errors keep the COMMITs unproven for now:\n{:#?}",
        c.report
    );
}

#[test]
fn a_call_is_never_wrapped() {
    // A callee's own COMMIT cannot run inside the subtransaction of an
    // exception block (2D000), and its error path is its own: a body that
    // calls is converted without the blocks.
    let c = pg("CREATE PROCEDURE dbo.Two AS
BEGIN
    SET NOCOUNT ON;
    UPDATE dbo.A SET Qty = 1;
    EXEC dbo.One @n = 2;
END;
GO
CREATE PROCEDURE dbo.One @n int AS
BEGIN
    SET NOCOUNT ON;
    UPDATE dbo.A SET Qty = @n;
    UPDATE dbo.B SET Qty = @n;
    DELETE FROM dbo.C WHERE Id = @n;
END;
");
    assert_eq!(count(&c, "EXCEPTION WHEN"), 3, "{}", c.output);
    // Only the callee's three statements are wrapped.
    let caller = c.output.split("PROCEDURE One").next().unwrap();
    assert!(!caller.contains("EXCEPTION"), "{}", c.output);
}

#[test]
fn many_routines_in_one_file_all_get_their_blocks() {
    // The behaviour model compares one routine per artifact; the blocks must
    // still win over the plain conversion, which the engine shows wrong.
    let mut src = String::new();
    for i in 0..4 {
        src.push_str(&format!(
            "CREATE PROCEDURE dbo.P{i} @Id int AS
BEGIN
    SET NOCOUNT ON;
    INSERT dbo.A (Id, Qty) VALUES (@Id, 1);
    UPDATE dbo.B SET Qty = Qty + 1 WHERE Id = @Id;
    DELETE FROM dbo.C WHERE Id = @Id;
END;
GO
"
        ));
    }
    let c = pg(&src);
    assert_eq!(count(&c, "EXCEPTION WHEN"), 12, "{}", c.output);
    assert!(
        c.units.iter().all(|u| u.status != Status::Differs),
        "{:#?}",
        c.units
            .iter()
            .map(|u| (u.status, &u.notes))
            .collect::<Vec<_>>()
    );
}

#[test]
fn xact_abort_on_keeps_the_plain_autocommit_conversion() {
    let c = pg(&THREE.replace(
        "    SET NOCOUNT ON;",
        "    SET NOCOUNT ON;\n    SET XACT_ABORT ON;",
    ));
    assert_eq!(count(&c, "EXCEPTION WHEN"), 0, "{}", c.output);
    assert_eq!(count(&c, "COMMIT;"), 3, "{}", c.output);
}

#[test]
fn a_single_write_is_not_wrapped() {
    let c = pg("CREATE PROCEDURE dbo.One @Id int AS
BEGIN
    SET NOCOUNT ON;
    UPDATE dbo.A SET Qty = 0 WHERE Id = @Id;
END;
");
    assert_eq!(count(&c, "EXCEPTION WHEN"), 0, "{}", c.output);
    assert_eq!(count(&c, "COMMIT;"), 0, "{}", c.output);
}

#[test]
fn bodies_whose_error_path_is_not_known_are_not_wrapped() {
    for body in [
        // an explicit transaction
        "BEGIN TRAN; UPDATE dbo.A SET Qty = 1; UPDATE dbo.B SET Qty = 1; COMMIT;",
        // an IF around a write
        "UPDATE dbo.A SET Qty = 1; IF @Id > 1 UPDATE dbo.B SET Qty = 1;",
        // TRY/CATCH
        "BEGIN TRY UPDATE dbo.A SET Qty = 1; END TRY BEGIN CATCH PRINT 'x'; END CATCH; UPDATE dbo.B SET Qty = 1;",
        // a result set
        "UPDATE dbo.A SET Qty = 1; SELECT Qty FROM dbo.A; UPDATE dbo.B SET Qty = 1;",
        // an error read by the procedure
        "UPDATE dbo.A SET Qty = 1; IF @@ERROR <> 0 PRINT 'x'; UPDATE dbo.B SET Qty = 1;",
        // a loop
        "WHILE @Id < 3 BEGIN UPDATE dbo.A SET Qty = 1; SET @Id += 1; END",
    ] {
        let c = pg(&format!("CREATE PROCEDURE dbo.P @Id int AS\nBEGIN\n    {body}\nEND;\n"));
        assert_eq!(count(&c, "RAISE WARNING"), 0, "{body}\n{}", c.output);
    }
}

#[test]
fn an_owner_procedure_is_not_wrapped() {
    // PostgreSQL 18 refuses COMMIT in a SECURITY DEFINER procedure.
    let c = pg(&THREE.replace(" AS\nBEGIN", "\nWITH EXECUTE AS OWNER\nAS\nBEGIN"));
    assert_eq!(count(&c, "EXCEPTION WHEN"), 0, "{}", c.output);
}

#[test]
fn a_refused_statement_drops_the_blocks() {
    // The stand-in for a refused statement stops the procedure, where T-SQL
    // goes on: the plain conversion is kept, with the refusal.
    let c = pg("CREATE PROCEDURE dbo.R @Id int AS
BEGIN
    SET NOCOUNT ON;
    UPDATE dbo.A SET Qty = ABS(CHECKSUM(NEWID())) WHERE Id = @Id;
    UPDATE dbo.B SET Qty = 1 WHERE Id = @Id;
    UPDATE dbo.C SET Qty = 1 WHERE Id = @Id;
END;
");
    assert!(
        c.units.iter().any(|u| u.status == Status::NotConverted),
        "{}",
        c.output
    );
    assert_eq!(count(&c, "EXCEPTION WHEN"), 0, "{}", c.output);
    assert_eq!(count(&c, "COMMIT;"), 0, "{}", c.output);
}

#[test]
fn a_trailing_return_is_allowed() {
    let c = pg(&THREE.replace("END;", "    RETURN\nEND;"));
    assert_eq!(count(&c, "EXCEPTION WHEN"), 3, "{}", c.output);
}

#[test]
fn a_script_is_not_wrapped() {
    // A DO block is compared as a script: the client decides whether it
    // goes on after an error.
    let c = pg("DECLARE @n int = 1;\nINSERT dbo.A (Id, Qty) VALUES (@n, 1);\nINSERT dbo.B (Id, Qty) VALUES (@n, 1);\n");
    assert_eq!(count(&c, "EXCEPTION WHEN"), 0, "{}", c.output);
}
