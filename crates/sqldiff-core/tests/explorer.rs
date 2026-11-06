//! The explorer resumes fault scenarios from the fault-free run instead of
//! running each from the start: its outcomes must be exactly those of a full
//! exploration, for every statement and error kind, on the corpus routines
//! and on generated ones with branches, loops and savepoints (long enough to
//! thin the checkpoints out).

use std::path::Path;

use sqldiff_core::artifact::Analyzed;
use sqldiff_core::ir::{self, ErrorKind, Explorer, Fault, Scenario};
use sqldiff_core::model::Side;
use sqldiff_core::{Artifact, Dialect};

fn dialect_of(name: &str) -> Option<Dialect> {
    let stem = name.split('.').next()?;
    match stem.split('_').next()? {
        "tsql" => Some(Dialect::Tsql),
        "pg" => Some(Dialect::Postgres),
        "db2i" => Some(Dialect::Db2i),
        _ => match name.rsplit('.').nth(1)? {
            "tsql" => Some(Dialect::Tsql),
            "pg" => Some(Dialect::Postgres),
            "db2i" => Some(Dialect::Db2i),
            _ => None,
        },
    }
}

fn corpus() -> Vec<(String, Dialect, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut out = Vec::new();
    for dir in ["corpus/pairs", "corpus/mutations"] {
        let mut cases: Vec<_> = std::fs::read_dir(root.join(dir))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        cases.sort();
        for c in cases {
            let mut files: Vec<_> = std::fs::read_dir(&c)
                .unwrap()
                .flatten()
                .map(|e| e.path())
                .collect();
            files.sort();
            for f in files {
                let name = f.file_name().unwrap().to_string_lossy().to_string();
                if let (true, Some(d)) = (name.ends_with(".sql"), dialect_of(&name)) {
                    out.push((
                        f.display().to_string(),
                        d,
                        std::fs::read_to_string(&f).unwrap(),
                    ));
                }
            }
        }
    }
    out
}

/// A long T-SQL routine with branches, a loop, savepoints, temporary state
/// and a handler, so faults land after forks and after checkpoints thin out.
fn generated_tsql(n: usize) -> String {
    let mut s = String::from(
        "CREATE PROCEDURE dbo.g @p int AS\nBEGIN\n  DECLARE @i int = 0;\n  CREATE TABLE #t (a int);\n  BEGIN TRY\n    BEGIN TRANSACTION;\n",
    );
    for i in 0..n {
        let t = i % 7;
        s += &match i % 9 {
            0 | 5 if i % 27 != i % 9 => format!("    UPDATE dbo.t{t} SET b = {i} WHERE a = @p;\n"),
            0 => format!("    IF @p > {i}\n    BEGIN\n      UPDATE dbo.t{t} SET b = {i} WHERE a = @p;\n    END\n    ELSE\n      DELETE FROM dbo.t{t} WHERE a = @p;\n"),
            1 => format!("    SAVE TRANSACTION s{t};\n    INSERT INTO dbo.t{t} (a) VALUES ({i});\n"),
            2 => format!("    IF @@ERROR <> 0 ROLLBACK TRANSACTION s{t};\n"),
            3 => "    INSERT INTO #t (a) VALUES (@p);\n".to_string(),
            4 => format!("    UPDATE dbo.t{t} SET b = (SELECT COUNT(*) FROM #t) WHERE a = {i};\n"),
            5 => format!("    WHILE @i < 2\n    BEGIN\n      UPDATE dbo.t{t} SET b = b + 1 WHERE a = {i};\n      SET @i = @i + 1;\n    END\n"),
            6 => format!("    MERGE dbo.t{t} AS x USING dbo.src AS y ON x.a = y.a\n    WHEN MATCHED THEN UPDATE SET b = y.b\n    WHEN NOT MATCHED THEN INSERT (a, b) VALUES (y.a, y.b);\n"),
            7 => format!("    EXEC dbo.other @p = {i};\n"),
            _ => format!("    INSERT INTO dbo.t{t} (a, b) VALUES (@p, {i});\n"),
        };
    }
    s += "    COMMIT TRANSACTION;\n  END TRY\n  BEGIN CATCH\n    IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION;\n    THROW;\n  END CATCH\nEND\n";
    s
}

fn generated_pg(n: usize) -> String {
    let mut s = String::from(
        "CREATE OR REPLACE PROCEDURE g(p integer) LANGUAGE plpgsql AS $$\nBEGIN\n  CREATE TEMP TABLE t (a integer);\n",
    );
    for i in 0..n {
        let t = i % 5;
        s += &match i % 6 {
            0 if i % 24 != 0 => format!("  UPDATE t{t} SET b = {i} WHERE a = p;\n"),
            0 => format!("  IF p > {i} THEN\n    UPDATE t{t} SET b = {i} WHERE a = p;\n  END IF;\n"),
            1 => format!("  BEGIN\n    INSERT INTO t{t} (a) VALUES ({i});\n  EXCEPTION WHEN OTHERS THEN\n    RAISE NOTICE 'x';\n  END;\n"),
            2 => "  INSERT INTO t (a) VALUES (p);\n".to_string(),
            3 => "  COMMIT;\n".to_string(),
            4 => format!("  UPDATE t{t} SET b = (SELECT COUNT(*) FROM t) WHERE a = {i};\n"),
            _ => format!("  DELETE FROM t{t} WHERE a = {i};\n"),
        };
    }
    s + "END;\n$$;\n"
}

/// Returns the number of fault scenarios compared and how many of them
/// merged into the fault-free run.
fn check(name: &str, d: Dialect, sql: &str) -> (usize, usize) {
    let art = Artifact::new(name, d, sql);
    let a = Analyzed::new(&art, Side::A, 3);
    let cfg = ir::lower(&a);
    let stmts: Vec<_> = (0..cfg.nodes.len())
        .filter(|&n| cfg.node(n).stmt().is_some())
        .collect();
    let mut runs = 0;
    let mut merged = 0;
    for (caller_txn, xact_abort) in [(false, false), (true, false), (false, true)] {
        let base = Scenario {
            caller_txn,
            xact_abort,
            ..Scenario::default()
        };
        let x = Explorer::new(&cfg, &base);
        assert_eq!(x.base(), ir::explore(&cfg, &base).as_slice(), "{name}");
        for &node in &stmts {
            for kind in ErrorKind::ALL {
                let sc = Scenario {
                    faults: vec![Fault { node, kind }],
                    ..base.clone()
                };
                assert_eq!(
                    x.explore(&cfg, &sc),
                    ir::explore(&cfg, &sc),
                    "{name}: fault {kind:?} at node {node}, caller_txn {caller_txn}, xact_abort {xact_abort}\n{}",
                    cfg.dump()
                );
                runs += 1;
            }
        }
        merged += x.merges();
    }
    (runs, merged)
}

#[test]
fn explorer_matches_full_exploration_on_the_corpus() {
    let files = corpus();
    assert!(files.len() > 40, "corpus not found");
    let (mut runs, mut merged) = (0, 0);
    for (n, d, s) in &files {
        let (r, m) = check(n, *d, s);
        runs += r;
        merged += m;
    }
    assert!(runs > 1000);
    // The merge is exercised, not just allowed.
    assert!(merged > 100, "only {merged} of {runs} scenarios merged");
    eprintln!("{merged} of {runs} fault scenarios merged");
}

#[test]
fn explorer_matches_full_exploration_on_long_branchy_routines() {
    for n in [5, 30, 70] {
        let (r, m) = check(&format!("g{n}.tsql.sql"), Dialect::Tsql, &generated_tsql(n));
        eprintln!("tsql {n}: {m} of {r} merged");
        let (r, m) = check(&format!("g{n}.pg.sql"), Dialect::Postgres, &generated_pg(n));
        eprintln!("pg {n}: {m} of {r} merged");
    }
}

/// A script of plain statements with no routine around it: nearly every
/// fault scenario merges.
fn straight_line(n: usize) -> String {
    (0..n)
        .map(|i| match i % 4 {
            0 => format!("INSERT INTO dbo.t{} (a, b) VALUES ({i}, {i});\n", i % 3),
            1 => format!(
                "UPDATE dbo.t{} SET b = {i} WHERE a = {};\nGO\n",
                i % 3,
                i - 1
            ),
            2 => format!("DELETE FROM dbo.t{} WHERE a = {i};\n", i % 3),
            _ => "IF @@ERROR <> 0 PRINT 'x';\n".to_string(),
        })
        .collect()
}

/// Loops that run past the step budget and branches that run past the path
/// budget, so the cut-off paths and what a merge must not hide are compared.
fn budget_edges() -> Vec<(&'static str, Dialect, String)> {
    let mut branches = String::from("CREATE PROCEDURE dbo.b @p int AS\nBEGIN\n");
    for i in 0..9 {
        branches += &format!(
            "  IF @p > {i} UPDATE dbo.t{} SET b = {i};\n  ELSE DELETE FROM dbo.t{};\n",
            i % 3,
            i % 3
        );
    }
    branches += "  INSERT INTO dbo.done (a) VALUES (1);\nEND\n";
    let mut nested = String::from(
        "CREATE PROCEDURE dbo.n @p int AS\nBEGIN\n  DECLARE @i int = 0, @j int = 0;\n",
    );
    nested += "  WHILE @i < 100\n  BEGIN\n    SET @j = 0;\n    WHILE @j < 100\n    BEGIN\n      UPDATE dbo.t SET b = @j WHERE a = @i;\n      SET @j = @j + 1;\n    END\n    SET @i = @i + 1;\n  END\n  INSERT INTO dbo.done (a) VALUES (1);\nEND\n";
    let handler = "CREATE PROCEDURE dbo.h @p int AS\nBEGIN\n  BEGIN TRY\n    UPDATE dbo.a SET b = 1;\n    BEGIN TRY\n      INSERT INTO dbo.b (a) VALUES (1);\n    END TRY\n    BEGIN CATCH\n      INSERT INTO dbo.log (m) VALUES (ERROR_MESSAGE());\n    END CATCH\n    DELETE FROM dbo.c;\n  END TRY\n  BEGIN CATCH\n    THROW;\n  END CATCH\nEND\n".to_string();
    let db2 = "CREATE PROCEDURE h (IN p INTEGER)\nLANGUAGE SQL\nBEGIN\n  DECLARE CONTINUE HANDLER FOR SQLEXCEPTION SET p = 0;\n  UPDATE a SET b = 1;\n  INSERT INTO b (a) VALUES (1);\n  BEGIN\n    DECLARE EXIT HANDLER FOR SQLSTATE '23505' ROLLBACK;\n    DELETE FROM c;\n    COMMIT;\n  END;\n  DELETE FROM d;\nEND\n".to_string();
    vec![
        ("branches.tsql.sql", Dialect::Tsql, branches),
        ("nested.tsql.sql", Dialect::Tsql, nested),
        ("handler.tsql.sql", Dialect::Tsql, handler),
        ("handler.db2i.sql", Dialect::Db2i, db2),
    ]
}

#[test]
fn explorer_matches_full_exploration_at_the_budget_edges_and_on_data_scripts() {
    for (name, d, sql) in budget_edges() {
        let (r, m) = check(name, d, &sql);
        eprintln!("{name}: {m} of {r} merged");
    }
    for n in [3, 40] {
        let (r, m) = check(
            &format!("data{n}.tsql.sql"),
            Dialect::Tsql,
            &straight_line(n),
        );
        eprintln!("data {n}: {m} of {r} merged");
        if n > 3 {
            assert!(m > 0, "no fault scenario of a data script merged");
        }
    }
}

/// Loops whose condition is unknown, run inside a transaction that the loop
/// body ends: the loop is summarized after two iterations unless its state
/// changes between them, and what changes it includes the transaction's log.
fn loops_in_transactions() -> Vec<(&'static str, Dialect, String)> {
    let commit_in_loop = "CREATE PROCEDURE dbo.l @p int AS\nBEGIN\n  BEGIN TRANSACTION;\n  INSERT INTO dbo.a (x) VALUES (1);\n  INSERT INTO dbo.a (x) VALUES (2);\n  WHILE EXISTS (SELECT 1 FROM dbo.q)\n  BEGIN\n    DELETE FROM dbo.q WHERE id = @p;\n    COMMIT TRANSACTION;\n    BEGIN TRANSACTION;\n    INSERT INTO dbo.b (x) VALUES (2);\n  END\n  COMMIT TRANSACTION;\nEND\n";
    let rollback_in_loop = "CREATE PROCEDURE dbo.l @p int AS\nBEGIN\n  BEGIN TRANSACTION;\n  INSERT INTO dbo.a (x) VALUES (1);\n  SAVE TRANSACTION s1;\n  INSERT INTO dbo.a (x) VALUES (2);\n  WHILE EXISTS (SELECT 1 FROM dbo.q)\n  BEGIN\n    INSERT INTO dbo.b (x) VALUES (2);\n    ROLLBACK TRANSACTION s1;\n    DELETE FROM dbo.q WHERE id = @p;\n  END\n  COMMIT TRANSACTION;\nEND\n";
    let savepoint_after_fault = "CREATE PROCEDURE dbo.s @p int AS\nBEGIN\n  BEGIN TRANSACTION;\n  INSERT INTO dbo.a (x) VALUES (1);\n  INSERT INTO dbo.a (x) VALUES (2);\n  SAVE TRANSACTION s1;\n  INSERT INTO dbo.a (x) VALUES (3);\n  ROLLBACK TRANSACTION s1;\n  INSERT INTO dbo.a (x) VALUES (4);\n  SAVE TRANSACTION s2;\n  INSERT INTO dbo.a (x) VALUES (5);\n  ROLLBACK TRANSACTION s2;\n  COMMIT TRANSACTION;\nEND\n";
    let late_commit = "CREATE PROCEDURE dbo.l @p int AS\nBEGIN\n  BEGIN TRANSACTION;\n  INSERT INTO dbo.a (x) VALUES (1);\n  PRINT 'inserted';\n  WHILE EXISTS (SELECT 1 FROM dbo.q)\n  BEGIN\n    IF EXISTS (SELECT 1 FROM dbo.r)\n    BEGIN\n      COMMIT TRANSACTION;\n      BEGIN TRANSACTION;\n    END\n    ELSE\n      PRINT 'again';\n  END\n  COMMIT TRANSACTION;\nEND\n";
    let pg = "CREATE OR REPLACE PROCEDURE l(p integer) LANGUAGE plpgsql AS $$\nBEGIN\n  INSERT INTO a (x) VALUES (1);\n  INSERT INTO a (x) VALUES (2);\n  WHILE EXISTS (SELECT 1 FROM q) LOOP\n    DELETE FROM q WHERE id = p;\n    COMMIT;\n    INSERT INTO b (x) VALUES (2);\n  END LOOP;\nEND;\n$$;\n";
    // The unsafe loop is reachable only through a continue handler's resume.
    let resume_loop = "CREATE PROCEDURE t (IN p INTEGER)\nLANGUAGE SQL\nBEGIN\n  DECLARE CONTINUE HANDLER FOR SQLSTATE '75000'\n  BEGIN\n    SET p = 0;\n    SET p = 1;\n  END;\n  BEGIN\n    DECLARE EXIT HANDLER FOR SQLEXCEPTION SET p = 2;\n    INSERT INTO a (x) VALUES (1);\n    SET p = 3;\n  END;\n  SIGNAL SQLSTATE '75000';\n  WHILE p > 0 DO\n    IF p > 5 THEN\n      COMMIT;\n    END IF;\n    SET p = p - 1;\n  END WHILE;\n  INSERT INTO d (x) VALUES (1);\nEND\n";
    vec![
        (
            "resume_loop.db2i.sql",
            Dialect::Db2i,
            resume_loop.to_string(),
        ),
        (
            "commit_in_loop.tsql.sql",
            Dialect::Tsql,
            commit_in_loop.to_string(),
        ),
        (
            "rollback_in_loop.tsql.sql",
            Dialect::Tsql,
            rollback_in_loop.to_string(),
        ),
        (
            "savepoint_after_fault.tsql.sql",
            Dialect::Tsql,
            savepoint_after_fault.to_string(),
        ),
        (
            "late_commit.tsql.sql",
            Dialect::Tsql,
            late_commit.to_string(),
        ),
        ("commit_in_loop.pg.sql", Dialect::Postgres, pg.to_string()),
    ]
}

#[test]
fn explorer_matches_full_exploration_for_loops_and_savepoints_in_transactions() {
    for (name, d, sql) in loops_in_transactions() {
        let (r, m) = check(name, d, &sql);
        eprintln!("{name}: {m} of {r} merged");
    }
}

/// Reads of a temporary table that the fault-free path made and a faulted
/// one did not: at a statement that cannot run again they are let go of, in a
/// loop that reads again they are not.
fn temp_reads_after_faults() -> Vec<(&'static str, Dialect, String)> {
    let straight = "CREATE PROCEDURE dbo.t @p int AS\nBEGIN\n  CREATE TABLE #w (a int);\n  INSERT INTO #w (a) VALUES (1);\n  UPDATE dbo.t SET b = (SELECT COUNT(*) FROM #w);\n  PRINT 'x';\n  UPDATE dbo.t SET b = (SELECT COUNT(*) FROM #w);\n  DELETE FROM dbo.u WHERE a IN (SELECT a FROM #w);\nEND\n";
    let looped = "CREATE PROCEDURE dbo.t @p int AS\nBEGIN\n  CREATE TABLE #w (a int);\n  INSERT INTO #w (a) VALUES (1);\n  PRINT 'x';\n  WHILE EXISTS (SELECT 1 FROM dbo.q)\n  BEGIN\n    UPDATE dbo.t SET b = (SELECT COUNT(*) FROM #w);\n    PRINT 'y';\n  END\n  DELETE FROM dbo.u WHERE a IN (SELECT a FROM #w);\nEND\n";
    let in_txn = "CREATE PROCEDURE dbo.t @p int AS\nBEGIN\n  CREATE TABLE #w (a int);\n  BEGIN TRANSACTION;\n  INSERT INTO #w (a) VALUES (1);\n  UPDATE dbo.t SET b = (SELECT COUNT(*) FROM #w);\n  SAVE TRANSACTION s1;\n  INSERT INTO #w (a) VALUES (2);\n  ROLLBACK TRANSACTION s1;\n  UPDATE dbo.t SET b = (SELECT COUNT(*) FROM #w);\n  COMMIT TRANSACTION;\n  UPDATE dbo.t SET b = (SELECT COUNT(*) FROM #w);\nEND\n";
    let pg = "CREATE OR REPLACE PROCEDURE t(p integer) LANGUAGE plpgsql AS $$\nBEGIN\n  CREATE TEMP TABLE w (a integer);\n  INSERT INTO w (a) VALUES (1);\n  UPDATE t SET b = (SELECT COUNT(*) FROM w);\n  COMMIT;\n  UPDATE t SET b = (SELECT COUNT(*) FROM w);\nEND;\n$$;\n";
    vec![
        ("straight.tsql.sql", Dialect::Tsql, straight.to_string()),
        ("looped.tsql.sql", Dialect::Tsql, looped.to_string()),
        ("in_txn.tsql.sql", Dialect::Tsql, in_txn.to_string()),
        ("temp.pg.sql", Dialect::Postgres, pg.to_string()),
    ]
}

#[test]
fn explorer_matches_full_exploration_for_temp_reads_after_faults() {
    for (name, d, sql) in temp_reads_after_faults() {
        let (r, m) = check(name, d, &sql);
        eprintln!("{name}: {m} of {r} merged");
    }
}
