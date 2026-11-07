//! Fault scenarios of a joined explorer versus a plain one, on shapes where
//! the dropped branch has statements of its own (found by the #330 review) (so a replay could merge into a tick of the
//! dropped segment whose subtree slice is empty or partial).

use std::collections::BTreeSet;

use sqldiff_core::artifact::Analyzed;
use sqldiff_core::ir::{self, ErrorKind, Explorer, Fault, KeyUniverse, Outcome, Scenario};
use sqldiff_core::model::Side;
use sqldiff_core::{Artifact, Dialect};

fn cfg_of(d: Dialect, sql: &str) -> ir::Cfg {
    let name = match d {
        Dialect::Tsql => "a.tsql.sql",
        Dialect::Postgres => "a.pg.sql",
        _ => "a.db2i.sql",
    };
    let art = Artifact::new(name, d, sql);
    ir::lower(&Analyzed::new(&art, Side::A, 3))
}

fn stripped(o: &Outcome) -> String {
    let mut o = o.clone();
    o.trace = ir::interp::Trace::default();
    o.signature.clear();
    o.fault_index = None;
    format!("{o:?}")
}

fn set(v: &[Outcome]) -> BTreeSet<String> {
    v.iter().map(stripped).collect()
}

/// Multiset view too: a lost duplicate is not a verdict change, but shows
/// where slices went missing.
fn multiset(v: &[Outcome]) -> Vec<String> {
    let mut m: Vec<String> = v.iter().map(stripped).collect();
    m.sort();
    m
}

fn check(name: &str, d: Dialect, sql: &str) -> (usize, usize) {
    let cfg = cfg_of(d, sql);
    let keys = KeyUniverse::of(&[&cfg]);
    let mut joined_total = 0;
    let mut multiset_diffs = 0;
    for (caller_txn, xact_abort) in [(false, false), (true, false), (false, true)] {
        let base = Scenario {
            caller_txn,
            xact_abort,
            ..Scenario::default()
        };
        let plain = Explorer::in_universe_joined(&cfg, &base, &keys, false);
        let joined = Explorer::in_universe_joined(&cfg, &base, &keys, true);
        joined_total += joined.joined();
        assert_eq!(set(plain.base()), set(joined.base()), "{name} base");
        for n in 0..cfg.nodes.len() {
            if cfg.node(n).stmt().is_none() {
                continue;
            }
            assert_eq!(plain.reaches(n), joined.reaches(n), "{name}: node {n}");
            for kind in ErrorKind::ALL {
                let sc = Scenario {
                    faults: vec![Fault { node: n, kind }],
                    ..base.clone()
                };
                let (p, j) = (plain.explore(&cfg, &sc), joined.explore(&cfg, &sc));
                if multiset(&p) != multiset(&j) {
                    multiset_diffs += 1;
                }
                assert_eq!(
                    set(&p),
                    set(&j),
                    "{name}: fault {kind:?} at node {n}, caller_txn {caller_txn}, xact_abort {xact_abort}\nplain {} joined {}\n{}",
                    p.len(),
                    j.len(),
                    cfg.dump()
                );
            }
        }
    }
    (joined_total, multiset_diffs)
}

fn tsql(body: &str) -> String {
    format!("CREATE PROCEDURE dbo.p @p int AS\nBEGIN\n{body}END\nGO\n")
}

fn pg(body: &str) -> String {
    format!("CREATE PROCEDURE p(p int) LANGUAGE plpgsql AS $$\nBEGIN\n{body}END;\n$$;\n")
}

fn db2(body: &str) -> String {
    format!("CREATE PROCEDURE p(IN p INT)\nLANGUAGE SQL\nBEGIN\n{body}END\n")
}

#[test]
fn dropped_segment_with_statements_tsql() {
    let shapes = [
        (
            "else with two prints",
            tsql("  INSERT INTO dbo.a(x) VALUES (1);\n  IF @p > 0\n    PRINT 'a';\n  ELSE\n    BEGIN\n      PRINT 'b';\n      PRINT 'b2';\n    END\n  UPDATE dbo.t SET a = 1;\n  IF @p > 1\n    PRINT 'c';\n  DELETE FROM dbo.u;\n"),
        ),
        (
            "else with select and raiserror",
            tsql("  IF @p > 0\n    PRINT 'a';\n  ELSE\n    BEGIN\n      SELECT 1;\n      RAISERROR('note', 10, 1);\n      SELECT 2;\n    END\n  UPDATE dbo.t SET a = 1;\n  IF @p > 1\n    PRINT 'c';\n  DELETE FROM dbo.u;\n"),
        ),
        (
            "else with nested if",
            tsql("  INSERT INTO dbo.a(x) VALUES (1);\n  IF @p > 0\n    PRINT 'a';\n  ELSE\n    BEGIN\n      PRINT 'b';\n      IF @p > 5\n        PRINT 'd';\n      PRINT 'b2';\n    END\n  UPDATE dbo.t SET a = 1;\n  IF @p > 1\n    PRINT 'c';\n  DELETE FROM dbo.u;\n"),
        ),
        (
            "writes in both branches of a diamond then a join",
            tsql("  IF @p > 0\n    BEGIN\n      UPDATE dbo.a SET x = 1;\n      UPDATE dbo.a SET x = 2;\n    END\n  ELSE\n    BEGIN\n      UPDATE dbo.a SET x = 3;\n      UPDATE dbo.a SET x = 4;\n    END\n  UPDATE dbo.t SET a = 1;\n  IF @p > 1\n    PRINT 'c';\n  DELETE FROM dbo.u;\n"),
        ),
        (
            "try catch around a diamond",
            tsql("  BEGIN TRY\n    IF @p > 0\n      BEGIN\n        UPDATE dbo.a SET x = 1;\n        PRINT 'a';\n      END\n    ELSE\n      BEGIN\n        UPDATE dbo.a SET x = 3;\n        PRINT 'b';\n      END\n    UPDATE dbo.t SET a = 1;\n    IF @p > 1\n      PRINT 'c';\n  END TRY\n  BEGIN CATCH\n    IF @p > 2\n      PRINT 'c';\n    UPDATE dbo.l SET x = 1;\n  END CATCH\n  DELETE FROM dbo.u;\n"),
        ),
        (
            "transaction with diamonds",
            tsql("  BEGIN TRANSACTION;\n  IF @p > 0\n    BEGIN\n      UPDATE dbo.a SET x = 1;\n      PRINT 'a';\n    END\n  ELSE\n    BEGIN\n      UPDATE dbo.a SET x = 3;\n      PRINT 'b';\n    END\n  UPDATE dbo.t SET a = 1;\n  IF @p > 1\n    PRINT 'b';\n  COMMIT;\n  IF @p > 2\n    PRINT 'c';\n  DELETE FROM dbo.u;\n"),
        ),
        (
            "loop with diamond inside",
            tsql("  WHILE @p > 0\n  BEGIN\n    IF @p > 5\n      BEGIN\n        UPDATE dbo.a SET x = 1;\n        PRINT 'a';\n      END\n    ELSE\n      PRINT 'b';\n    UPDATE dbo.t SET a = 1;\n    SET @p = @p - 1;\n  END\n  UPDATE dbo.z SET a = 1;\n  IF @p > 1\n    PRINT 'c';\n  DELETE FROM dbo.u;\n"),
        ),
        (
            "temp table in both branches",
            tsql("  CREATE TABLE #t (a int);\n  IF @p > 0\n    BEGIN\n      INSERT INTO #t VALUES (1);\n      PRINT 'a';\n    END\n  ELSE\n    BEGIN\n      INSERT INTO #t VALUES (2);\n      PRINT 'b';\n    END\n  SELECT a FROM #t;\n  UPDATE dbo.t SET a = 1;\n  IF @p > 1\n    PRINT 'c';\n  DELETE FROM dbo.u;\n"),
        ),
        (
            "rowcount test after the join",
            tsql("  IF @p > 0\n    PRINT 'a';\n  ELSE\n    BEGIN\n      PRINT 'b';\n      PRINT 'b2';\n    END\n  UPDATE dbo.t SET a = 1;\n  IF @@ROWCOUNT = 0\n    PRINT 'none';\n  DELETE FROM dbo.u;\n"),
        ),
        (
            "return in a branch",
            tsql("  IF @p > 0\n    BEGIN\n      PRINT 'a';\n      RETURN;\n    END\n  ELSE\n    BEGIN\n      PRINT 'b';\n      PRINT 'b2';\n    END\n  UPDATE dbo.t SET a = 1;\n  IF @p > 1\n    PRINT 'c';\n  DELETE FROM dbo.u;\n"),
        ),
        (
            "three-way join",
            tsql("  INSERT INTO dbo.a(x) VALUES (1);\n  IF @p > 0\n    PRINT 'a';\n  ELSE IF @p > 1\n    BEGIN\n      PRINT 'b';\n      PRINT 'b2';\n    END\n  ELSE\n    BEGIN\n      PRINT 'c';\n      PRINT 'c2';\n      PRINT 'c3';\n    END\n  UPDATE dbo.t SET a = 1;\n  IF @p > 2\n    PRINT 'd';\n  DELETE FROM dbo.u;\n"),
        ),
    ];
    let mut total = 0;
    for (name, sql) in &shapes {
        let (j, md) = check(name, Dialect::Tsql, sql);
        eprintln!("{name}: joined {j}, scenarios with a lost duplicate {md}");
        total += j;
    }
    assert!(total > 0);
}

#[test]
fn dropped_segment_with_statements_pg() {
    let shapes = [
        (
            "else with two statements",
            pg("  INSERT INTO a(x) VALUES (1);\n  IF p > 0 THEN\n    RAISE NOTICE 'a';\n  ELSE\n    RAISE NOTICE 'b';\n    RAISE NOTICE 'b2';\n  END IF;\n  UPDATE t SET a = 1;\n  IF p > 1 THEN\n    RAISE NOTICE 'c';\n  END IF;\n  DELETE FROM u;\n"),
        ),
        (
            "exception block around a diamond",
            pg("  BEGIN\n    IF p > 0 THEN\n      UPDATE a SET x = 1;\n      RAISE NOTICE 'a';\n    ELSE\n      UPDATE a SET x = 3;\n      RAISE NOTICE 'b';\n    END IF;\n    UPDATE t SET a = 1;\n    IF p > 1 THEN\n      RAISE NOTICE 'c';\n    END IF;\n  EXCEPTION WHEN OTHERS THEN\n    IF p > 2 THEN\n      RAISE NOTICE 'c';\n    END IF;\n    UPDATE l SET x = 1;\n  END;\n  DELETE FROM u;\n"),
        ),
        (
            "writes in both branches",
            pg("  IF p > 0 THEN\n    UPDATE a SET x = 1;\n    UPDATE a SET x = 2;\n  ELSE\n    UPDATE a SET x = 3;\n    UPDATE a SET x = 4;\n  END IF;\n  UPDATE t SET a = 1;\n  IF p > 1 THEN\n    RAISE NOTICE 'c';\n  END IF;\n  DELETE FROM u;\n"),
        ),
        (
            "commit in a loop with a diamond",
            pg("  WHILE p > 0 LOOP\n    IF p > 5 THEN\n      UPDATE a SET x = 1;\n      RAISE NOTICE 'a';\n    ELSE\n      RAISE NOTICE 'b';\n    END IF;\n    UPDATE t SET a = 1;\n    COMMIT;\n    p := p - 1;\n  END LOOP;\n  UPDATE z SET a = 1;\n  IF p > 1 THEN\n    RAISE NOTICE 'c';\n  END IF;\n  DELETE FROM u;\n"),
        ),
        (
            "found test after the join",
            pg("  IF p > 0 THEN\n    RAISE NOTICE 'a';\n  ELSE\n    RAISE NOTICE 'b';\n    RAISE NOTICE 'b2';\n  END IF;\n  UPDATE t SET a = 1;\n  IF NOT FOUND THEN\n    RAISE NOTICE 'none';\n  END IF;\n  DELETE FROM u;\n"),
        ),
    ];
    let mut total = 0;
    for (name, sql) in &shapes {
        let (j, md) = check(name, Dialect::Postgres, sql);
        eprintln!("{name}: joined {j}, scenarios with a lost duplicate {md}");
        total += j;
    }
    assert!(total > 0);
}

#[test]
fn dropped_segment_with_statements_db2() {
    let shapes = [
        (
            "continue handler with a diamond",
            db2("  DECLARE CONTINUE HANDLER FOR SQLEXCEPTION\n    UPDATE l SET x = 1;\n  INSERT INTO a(x) VALUES (1);\n  IF p > 0 THEN\n    UPDATE a SET x = 1;\n    UPDATE a SET x = 2;\n  ELSE\n    UPDATE a SET x = 3;\n    UPDATE a SET x = 4;\n  END IF;\n  UPDATE t SET a = 1;\n  IF p > 1 THEN\n    UPDATE c SET x = 1;\n  END IF;\n  DELETE FROM u;\n"),
        ),
        (
            "exit handler with a diamond",
            db2("  DECLARE EXIT HANDLER FOR SQLEXCEPTION\n    UPDATE l SET x = 1;\n  INSERT INTO a(x) VALUES (1);\n  IF p > 0 THEN\n    UPDATE a SET x = 1;\n    UPDATE a SET x = 2;\n  ELSE\n    UPDATE a SET x = 3;\n    UPDATE a SET x = 4;\n  END IF;\n  UPDATE t SET a = 1;\n  IF p > 1 THEN\n    UPDATE c SET x = 1;\n  END IF;\n  COMMIT;\n  DELETE FROM u;\n"),
        ),
        (
            "no handler",
            db2("  INSERT INTO a(x) VALUES (1);\n  IF p > 0 THEN\n    UPDATE a SET x = 1;\n    UPDATE a SET x = 2;\n  ELSE\n    UPDATE a SET x = 3;\n    UPDATE a SET x = 4;\n  END IF;\n  UPDATE t SET a = 1;\n  IF p > 1 THEN\n    UPDATE c SET x = 1;\n  END IF;\n  DELETE FROM u;\n"),
        ),
    ];
    let mut total = 0;
    for (name, sql) in &shapes {
        let (j, md) = check(name, Dialect::Db2i, sql);
        eprintln!("{name}: joined {j}, scenarios with a lost duplicate {md}");
        total += j;
    }
    eprintln!("db2 joined total {total}");
}
