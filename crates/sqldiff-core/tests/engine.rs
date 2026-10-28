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
fn tsql_identifier_case_in_a_rewritten_statement_needs_an_assumption() {
    // The statement also changed in a way statement equivalence proves
    // (operand order, `DELETE` without `FROM`), so it is not matched as a
    // whole; the lowering folds the names, which needs the same assumption.
    let proc_ = |body: &str| format!("CREATE PROCEDURE dbo.P @p int AS\nBEGIN\n  {body}\nEND\n");
    for (a, b) in [
        (
            "UPDATE dbo.Orders SET Total = 1 WHERE OrderId = @p AND Status = 2;",
            "UPDATE dbo.orders SET total = 1 WHERE status = 2 AND orderid = @p;",
        ),
        (
            "DELETE FROM dbo.Orders WHERE OrderId = @p;",
            "DELETE dbo.orders WHERE orderid = @p;",
        ),
    ] {
        let r = run(&proc_(a), Dialect::Tsql, &proc_(b), Dialect::Tsql);
        assert_ne!(r.verdict, Verdict::Equal, "{a}");
        assert!(
            r.assumptions
                .iter()
                .any(|x| x.id == "ASM-TSQL-CI-COLLATION"),
            "{a} / {b}: {:?}",
            r.assumptions.iter().map(|x| &x.id).collect::<Vec<_>>()
        );
    }
    // Same spelling everywhere: nothing is folded, so no assumption.
    let a = "UPDATE dbo.Orders SET Total = 1 WHERE OrderId = @p AND Status = 2;";
    let b = "UPDATE dbo.Orders SET Total = 1 WHERE Status = 2 AND OrderId = @p;";
    let r = run(&proc_(a), Dialect::Tsql, &proc_(b), Dialect::Tsql);
    assert!(
        !r.assumptions
            .iter()
            .any(|x| x.id == "ASM-TSQL-CI-COLLATION"),
        "{:?}",
        r.assumptions.iter().map(|x| &x.id).collect::<Vec<_>>()
    );
    // PostgreSQL folds unquoted names itself: no assumption there.
    let pg = |body: &str| {
        format!("CREATE PROCEDURE p(p_p int) LANGUAGE plpgsql AS $$\nBEGIN\n  {body}\nEND $$;\n")
    };
    let r = run(
        &pg("UPDATE Orders SET Total = 1 WHERE OrderId = p_p AND Status = 2;"),
        Dialect::Postgres,
        &pg("UPDATE orders SET total = 1 WHERE status = 2 AND orderid = p_p;"),
        Dialect::Postgres,
    );
    assert!(!r
        .assumptions
        .iter()
        .any(|x| x.id == "ASM-TSQL-CI-COLLATION"));
}

#[test]
fn modified_statement_is_never_equal() {
    let a = "CREATE PROCEDURE p AS\nBEGIN\n  UPDATE t SET x = 1 WHERE EXISTS (SELECT 1 FROM s WHERE s.id = t.id);\n  SELECT 1;\nEND";
    let b = "CREATE PROCEDURE p AS\nBEGIN\n  UPDATE t SET x = 1;\n  SELECT 1;\nEND";
    let r = run(a, Dialect::Tsql, b, Dialect::Tsql);
    // The edit is evaluated: with no matching row in s, only B updates t.
    assert_eq!(r.verdict, Verdict::Different);
    let f = &r.findings[0];
    assert_eq!(f.rule, "equiv.edit");
    assert!(f.witness.contains("s has no rows"), "{}", f.witness);
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
    let original = fixture("catch_rethrow.tsql.sql");
    // Either line ending: a CRLF checkout must get the fix too.
    let fixed = original
        .replace("ROLLBACK\r\n", "ROLLBACK;\r\n")
        .replace("ROLLBACK\n", "ROLLBACK;\n");
    assert_ne!(fixed, original, "the fixture has a bare ROLLBACK line");
    let r = check(
        &Artifact::new("c.sql", Dialect::Tsql, fixed),
        &Options::default(),
    );
    assert!(r.findings.is_empty(), "{:#?}", r.findings);
}

#[test]
fn error_handler_writes_are_paired_by_target_not_count() {
    // Three steps, each recording a failure in its own CATCH block.
    let tsql = "BEGIN TRY\n  INSERT INTO parcel (id) SELECT id FROM old_parcel;\nEND TRY\n\
                BEGIN CATCH\n  UPDATE run_state SET failed = 1;\nEND CATCH;\n\
                BEGIN TRY\n  UPDATE parcel SET checked = 1;\nEND TRY\n\
                BEGIN CATCH\n  UPDATE run_state SET failed = 1;\nEND CATCH;\n\
                BEGIN TRY\n  DELETE FROM old_parcel;\nEND TRY\n\
                BEGIN CATCH\n  UPDATE run_state SET failed = 1;\nEND CATCH;";
    // One handler covering all three steps.
    let pg = "do $$\nbegin\n  insert into parcel (id) select id from old_parcel;\n  \
              update parcel set checked = 1;\n  delete from old_parcel;\n\
              exception when others then\n  update run_state set failed = true;\n  raise;\nend;\n$$;";
    let unpaired = |r: &Report| -> Vec<String> {
        r.findings
            .iter()
            .filter(|f| f.rule == "write.unpaired")
            .map(|f| f.witness.clone())
            .collect()
    };
    let r = run(tsql, Dialect::Tsql, pg, Dialect::Postgres);
    assert!(unpaired(&r).is_empty(), "{:?}", unpaired(&r));

    // No failure recording at all: one finding, not one per CATCH block.
    let db2 = "BEGIN\n  INSERT INTO parcel (id) SELECT id FROM old_parcel;\n  \
               UPDATE parcel SET checked = 1;\n  DELETE FROM old_parcel;\nEND;";
    let r = run(tsql, Dialect::Tsql, db2, Dialect::Db2i);
    let w = unpaired(&r);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(
        w[0].contains("UPDATE of run_state in an error handler (line 5, and 2 more)")
            && w[0].contains("no error handler writes it"),
        "{}",
        w[0]
    );
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == "write.unpaired")
        .unwrap();
    assert_eq!(f.verdict, Verdict::Different);

    // A Db2 condition handler that records the failure pairs with them.
    let db2_handler = "BEGIN\n  DECLARE EXIT HANDLER FOR SQLEXCEPTION\n    BEGIN\n      \
                       UPDATE run_state SET failed = 1;\n      RESIGNAL;\n    END;\n  \
                       INSERT INTO parcel (id) SELECT id FROM old_parcel;\n  \
                       UPDATE parcel SET checked = 1;\n  DELETE FROM old_parcel;\nEND;";
    let r = run(tsql, Dialect::Tsql, db2_handler, Dialect::Db2i);
    assert!(unpaired(&r).is_empty(), "{:?}", unpaired(&r));

    // Both sides write the log on the normal path (paired); only A also
    // writes it on the error path, so B has no error-path counterpart.
    let a = "BEGIN TRY\n  UPDATE run_state SET step = 1;\nEND TRY\n\
             BEGIN CATCH\n  UPDATE run_state SET failed = 1;\nEND CATCH;";
    let b = "BEGIN\n  UPDATE run_state SET step = 1;\nEND;";
    let r = run(a, Dialect::Tsql, b, Dialect::Db2i);
    let f: Vec<_> = r
        .findings
        .iter()
        .filter(|f| f.rule == "write.unpaired")
        .collect();
    assert_eq!(f.len(), 1, "{:?}", unpaired(&r));
    assert_eq!(f[0].verdict, Verdict::Different);
    assert!(
        f[0].witness.contains("no error handler writes it"),
        "{}",
        f[0].witness
    );

    // A normal-path write is not excused by a different kind of write to the
    // same table on the other side's error path.
    let a = "UPDATE parcel SET checked = 1;";
    let b = "BEGIN\n  DECLARE EXIT HANDLER FOR SQLEXCEPTION\n    BEGIN\n      \
             INSERT INTO parcel (id) VALUES (0);\n    END;\n  DELETE FROM old_parcel;\nEND;";
    let r = run(a, Dialect::Tsql, b, Dialect::Db2i);
    let main = r
        .findings
        .iter()
        .find(|f| f.rule == "write.unpaired" && f.witness.contains("UPDATE of parcel"))
        .expect("finding for the UPDATE");
    assert_eq!(main.verdict, Verdict::Different, "{}", main.witness);
}

#[test]
fn insert_column_count_mismatch_is_reported() {
    let lint = |sql: &str, d: Dialect| -> Vec<String> {
        check(&Artifact::new("i.sql", d, sql), &Options::default())
            .findings
            .into_iter()
            .filter(|f| f.rule == "hazard.insert-column-count")
            .map(|f| f.witness)
            .collect()
    };
    // Two large columns left out of the SELECT but kept in the column list.
    let short = "INSERT INTO shipment (id, label, payload, note)\nOVERRIDING SYSTEM VALUE\n\
                 SELECT s.id, s.label FROM old_shipment s WITH NC;";
    let w = lint(short, Dialect::Db2i);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(
        w[0].contains("names 4 columns but its SELECT list supplies 2"),
        "{}",
        w[0]
    );
    let rows = "insert into t (a, b) values (1, 2), (3);";
    for d in Dialect::ALL {
        assert_eq!(lint(rows, d).len(), 1, "{d}");
    }
    // Countable and matching, or not countable: no finding.
    for (sql, d) in [
        (
            "INSERT INTO shipment (id, label) OVERRIDING SYSTEM VALUE SELECT s.id, s.label FROM old_shipment s WITH NC;",
            Dialect::Db2i,
        ),
        (
            "insert into t (a, b) select coalesce(x.a, x.b), cast(x.c as int) from s x;",
            Dialect::Postgres,
        ),
        ("insert into t (a, b) select top (5) x.a, x.b from s x;", Dialect::Tsql),
        ("insert into t (a, b) select * from s;", Dialect::Tsql),
        ("insert into t (a, b) select x.* from s x;", Dialect::Postgres),
        ("insert into t values (1, 2);", Dialect::Tsql),
        ("insert into t (a, b) values (1, 2), (3, 4);", Dialect::Db2i),
        // Commas inside brackets, trailing clauses and keywords inside
        // expressions are not list items.
        ("INSERT INTO t (a, b) VALUES (1, ARRAY[1, 2]);", Dialect::Postgres),
        (
            "INSERT INTO t (a, b) SELECT 1, 2 ON CONFLICT (a) DO UPDATE SET a = 1, b = 2;",
            Dialect::Postgres,
        ),
        ("INSERT INTO t (a, b) SELECT $1, $2 RETURNING a, b;", Dialect::Postgres),
        ("INSERT INTO t (id, name) SELECT NEXT VALUE FOR seq, s.name FROM s;", Dialect::Tsql),
        ("INSERT INTO t (a, b) SELECT x IS DISTINCT FROM y, z FROM s;", Dialect::Postgres),
        // Targets that are not a plain table name.
        ("INSERT INTO t AS x (a, b) VALUES (1, 2, 3);", Dialect::Postgres),
        (
            "INSERT INTO OPENQUERY(srv, 'select a,b,c from t') VALUES (1, 2, 3);",
            Dialect::Tsql,
        ),
    ] {
        assert!(lint(sql, d).is_empty(), "{d}: {sql}");
    }
    // The fingerprint does not depend on the case or quoting of the target.
    let fp = |sql: &str| {
        check(
            &Artifact::new("i.sql", Dialect::Postgres, sql),
            &Options::default(),
        )
        .findings
        .into_iter()
        .find(|f| f.rule == "hazard.insert-column-count")
        .map(|f| f.fingerprint)
        .expect("hit")
    };
    assert_eq!(
        fp("insert into T (a) values (1, 2);"),
        fp("insert into \"t\" (a) values (1, 2);")
    );
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
