use sqldiff_core::{check, diff, Artifact, ChangeClass, Dialect, Options, Report, Verdict};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn run(a: &str, da: Dialect, b: &str, db: Dialect) -> Report {
    diff(
        &Artifact::new("a.sql", da, a),
        &Artifact::new("b.sql", db, b),
        &Options::default(),
    )
}

fn rules(r: &Report) -> Vec<&str> {
    r.findings.iter().map(|f| f.rule.as_str()).collect()
}

#[test]
fn worked_example_tsql_to_db2() {
    let r = run(
        &fixture("movestock.tsql.sql"),
        Dialect::Tsql,
        &fixture("movestock.db2i.sql"),
        Dialect::Db2i,
    );
    assert_eq!(r.verdict, Verdict::Different);
    let rs = rules(&r);
    for expected in ["txn.atomicity", "txn.caller-transaction", "error.contract"] {
        assert!(rs.contains(&expected), "missing {expected}: {rs:?}");
    }
    let atom = r
        .findings
        .iter()
        .find(|f| f.rule == "txn.atomicity")
        .unwrap();
    assert_eq!(atom.class, ChangeClass::Transactional);
    assert_eq!(atom.verdict, Verdict::Different);
    assert!(
        atom.witness.contains("INSERT into StockLog"),
        "{}",
        atom.witness
    );
    assert!(atom.witness.contains("UPDATE of Stock"), "{}", atom.witness);
    // Under XACT_ABORT ON T-SQL is still atomic, so the finding does not
    // rest on the caller's setting.
    assert!(!atom
        .assumptions
        .contains(&"ASM-TSQL-CALLER-XACT-ABORT-OFF".to_string()));
    // Every write is paired across dialects, and the converted side has no residue.
    assert!(!rs.contains(&"write.unpaired"), "{rs:?}");
    assert!(!rs.iter().any(|r| r.starts_with("residue.")), "{rs:?}");
}

#[test]
fn fingerprints_survive_reformatting() {
    let db2 = fixture("movestock.db2i.sql");
    let r1 = run(
        &fixture("movestock.tsql.sql"),
        Dialect::Tsql,
        &db2,
        Dialect::Db2i,
    );
    let r2 = run(
        &fixture("movestock_reformatted.tsql.sql"),
        Dialect::Tsql,
        &db2,
        Dialect::Db2i,
    );
    let mut f1: Vec<_> = r1.findings.iter().map(|f| f.fingerprint.clone()).collect();
    let mut f2: Vec<_> = r2.findings.iter().map(|f| f.fingerprint.clone()).collect();
    f1.sort();
    f2.sort();
    assert_eq!(f1, f2);
}

#[test]
fn reformat_only_is_equal() {
    let r = run(
        &fixture("movestock.tsql.sql"),
        Dialect::Tsql,
        &fixture("movestock_reformatted.tsql.sql"),
        Dialect::Tsql,
    );
    assert_eq!(r.verdict, Verdict::Equal, "{:#?}", r.findings);
    assert!(r.findings.is_empty());
    assert!(r.noise.cosmetic > 0);
}

#[test]
fn tsql_identifier_case_needs_an_assumption() {
    let r = run(
        "UPDATE Stock SET Qty = 1",
        Dialect::Tsql,
        "UPDATE stock SET qty = 1",
        Dialect::Tsql,
    );
    assert_eq!(r.verdict, Verdict::EqualUnderAssumptions);
    assert!(r
        .assumptions
        .iter()
        .any(|a| a.id == "ASM-TSQL-CI-COLLATION"));
    // PostgreSQL folds unquoted identifiers, so the same change is plainly equal.
    let r = run(
        "UPDATE Stock SET Qty = 1;",
        Dialect::Postgres,
        "update stock set qty = 1;",
        Dialect::Postgres,
    );
    assert_eq!(r.verdict, Verdict::Equal);
    // ...but a quoted identifier keeps its case.
    let r = run(
        "UPDATE \"Stock\" SET qty = 1;",
        Dialect::Postgres,
        "UPDATE stock SET qty = 1;",
        Dialect::Postgres,
    );
    assert_ne!(r.verdict, Verdict::Equal);
}

#[test]
fn modified_statement_is_never_equal() {
    let a = "CREATE PROCEDURE p AS\nBEGIN\n  UPDATE t SET x = 1 WHERE EXISTS (SELECT 1 FROM s WHERE s.id = t.id);\n  SELECT 1;\nEND";
    let b = "CREATE PROCEDURE p AS\nBEGIN\n  UPDATE t SET x = 1;\n  SELECT 1;\nEND";
    let r = run(a, Dialect::Tsql, b, Dialect::Tsql);
    assert_eq!(r.verdict, Verdict::Unknown);
    let f = &r.findings[0];
    assert_eq!(f.rule, "stmt.modified");
    assert!(f.witness.contains("was removed"), "{}", f.witness);
    assert_eq!(f.a.as_ref().unwrap().start_line, 3);
}

#[test]
fn moved_commit_is_transactional() {
    let a = "BEGIN TRAN;\nUPDATE a SET x = 1;\nINSERT INTO b VALUES (1);\nCOMMIT;";
    let b = "BEGIN TRAN;\nUPDATE a SET x = 1;\nCOMMIT;\nINSERT INTO b VALUES (1);";
    let r = run(a, Dialect::Tsql, b, Dialect::Tsql);
    assert_eq!(r.verdict, Verdict::Different);
    assert!(
        r.findings
            .iter()
            .any(|f| f.class == ChangeClass::Transactional),
        "{:#?}",
        r.findings
    );
}

#[test]
fn removed_write_is_different() {
    let a = "UPDATE a SET x = 1;\nINSERT INTO log VALUES (1);";
    let b = "UPDATE a SET x = 1;";
    let r = run(a, Dialect::Postgres, b, Dialect::Postgres);
    assert_eq!(r.verdict, Verdict::Different);
    assert_eq!(rules(&r), vec!["stmt.removed"]);
}

#[test]
fn commit_level_change_is_transactional() {
    let a = "CREATE PROCEDURE p () LANGUAGE SQL SET OPTION COMMIT = *CHG\nBEGIN ATOMIC\n  UPDATE a SET x = 1;\n  UPDATE b SET y = 1;\nEND";
    let b = a.replace("*CHG", "*NONE");
    let r = run(a, Dialect::Db2i, &b, Dialect::Db2i);
    // ATOMIC under *NONE rests on vendor fact V4, untested on a real IBM i:
    // the witness is shown, the verdict capped (milestone M5b).
    assert_eq!(r.verdict, Verdict::Unknown);
    assert!(rules(&r).contains(&"txn.atomicity"), "{:?}", rules(&r));
    assert!(r
        .findings
        .iter()
        .any(|f| f.rule == "txn.atomicity" && f.witness.contains("vendor fact V4")));
    assert!(r
        .findings
        .iter()
        .any(|f| f.rule == "header.modified" && f.class == ChangeClass::Transactional));
}

#[test]
fn pg_function_vs_tsql_without_transaction() {
    let pg = "CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$\nBEGIN\n  UPDATE a SET x = 1;\n  INSERT INTO b VALUES (1);\nEND;\n$$;";
    let ts =
        "CREATE PROCEDURE f AS\nBEGIN\n  UPDATE a SET x = 1;\n  INSERT INTO b VALUES (1);\nEND";
    let r = run(pg, Dialect::Postgres, ts, Dialect::Tsql);
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == "txn.atomicity")
        .expect("atomicity finding");
    assert_eq!(f.verdict, Verdict::Different);
    assert!(
        f.witness.contains("side A (PostgreSQL) undoes"),
        "{}",
        f.witness
    );
}

#[test]
fn unpaired_cross_dialect_write() {
    let ts = "CREATE PROCEDURE p AS\nBEGIN\n  UPDATE Stock SET Qty = 0;\n  INSERT INTO StockLog VALUES (1);\nEND";
    let pg = "CREATE FUNCTION p() RETURNS void LANGUAGE plpgsql AS $$\nBEGIN\n  UPDATE stock SET qty = 0;\nEND;\n$$;";
    let r = run(ts, Dialect::Tsql, pg, Dialect::Postgres);
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == "write.unpaired")
        .expect("unpaired");
    assert!(f.witness.contains("StockLog"));
    assert_eq!(f.verdict, Verdict::Different);
}

#[test]
fn residue_in_converted_code() {
    let db2 = "CREATE PROCEDURE p (IN a INT) LANGUAGE SQL\nBEGIN\n  UPDATE dbo.t SET x = ISNULL(a, 0), d = GETDATE() WHERE id = @a;\nEND";
    let r = check(
        &Artifact::new("p.sql", Dialect::Db2i, db2),
        &Options::default(),
    );
    let rs = rules(&r);
    for e in [
        "residue.tsql.dbo-schema",
        "residue.tsql.variable",
        // T-SQL built-ins fail to bind in Db2: a proven binding error replaces the residue hit.
        "bind.unknown-builtin",
    ] {
        assert!(rs.contains(&e), "missing {e}: {rs:?}");
    }
    assert!(r
        .findings
        .iter()
        .all(|f| f.class == ChangeClass::Residue || f.rule == "bind.unknown-builtin"));
    let pg = "CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$\nBEGIN\n  SIGNAL SQLSTATE '75001';\n  UPDATE t SET ts = CURRENT TIMESTAMP;\nEND;\n$$;";
    let rs: Vec<String> = check(
        &Artifact::new("f.sql", Dialect::Postgres, pg),
        &Options::default(),
    )
    .findings
    .iter()
    .map(|f| f.rule.clone())
    .collect();
    assert!(rs.contains(&"residue.db2i.signal".to_string()), "{rs:?}");
    assert!(
        rs.contains(&"residue.db2i.special-register".to_string()),
        "{rs:?}"
    );
}

#[test]
fn clean_code_has_no_residue() {
    for (name, d) in [
        ("movestock.tsql.sql", Dialect::Tsql),
        ("movestock.pg.sql", Dialect::Postgres),
        ("movestock.db2i.sql", Dialect::Db2i),
    ] {
        let r = check(&Artifact::new(name, d, fixture(name)), &Options::default());
        assert!(r.findings.is_empty(), "{name}: {:#?}", r.findings);
    }
}

#[test]
fn missing_terminator_before_throw() {
    let r = check(
        &Artifact::new("c.sql", Dialect::Tsql, fixture("catch_rethrow.tsql.sql")),
        &Options::default(),
    );
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == "hazard.terminator-before-throw")
        .expect("hazard");
    assert!(
        f.witness.contains("transaction or savepoint name"),
        "{}",
        f.witness
    );
    let fixed = fixture("catch_rethrow.tsql.sql").replace("ROLLBACK\n", "ROLLBACK;\n");
    let r = check(
        &Artifact::new("c.sql", Dialect::Tsql, fixed),
        &Options::default(),
    );
    assert!(r.findings.is_empty(), "{:#?}", r.findings);
}

#[test]
fn hazard_introduced_in_new_version_is_reported() {
    let old =
        "BEGIN TRY\n  UPDATE t SET x = 1;\nEND TRY\nBEGIN CATCH\n  ROLLBACK;\n  THROW;\nEND CATCH";
    let new = old.replace("ROLLBACK;", "ROLLBACK");
    let r = run(old, Dialect::Tsql, &new, Dialect::Tsql);
    assert!(
        rules(&r).contains(&"hazard.terminator-before-throw"),
        "{:?}",
        rules(&r)
    );
}
