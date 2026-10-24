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

fn check(name: &str, d: Dialect, sql: &str) -> usize {
    let art = Artifact::new(name, d, sql);
    let a = Analyzed::new(&art, Side::A, 3);
    let cfg = ir::lower(&a);
    let stmts: Vec<_> = (0..cfg.nodes.len())
        .filter(|&n| cfg.node(n).stmt().is_some())
        .collect();
    let mut runs = 0;
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
    }
    runs
}

#[test]
fn explorer_matches_full_exploration_on_the_corpus() {
    let files = corpus();
    assert!(files.len() > 40, "corpus not found");
    let runs: usize = files.iter().map(|(n, d, s)| check(n, *d, s)).sum();
    assert!(runs > 1000);
}

#[test]
fn explorer_matches_full_exploration_on_long_branchy_routines() {
    for n in [5, 30, 70] {
        check(&format!("g{n}.tsql.sql"), Dialect::Tsql, &generated_tsql(n));
        check(&format!("g{n}.pg.sql"), Dialect::Postgres, &generated_pg(n));
    }
}
