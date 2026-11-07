//! The work count of a file does not depend on how the tree was converted:
//! the thread count, the output folder, or whether the file was checked
//! fresh or again on a resumed run (`SQLDIFF_WORK=1` prints one line per file
//! checked). The counts are the same function of the file each time, because
//! the speed-ups that could change them (the explorer and flipped-scenario
//! caches, the checkpoints of a replay) are not counted twice and the join
//! drops are fixed by the file (see `crates/sqldiff-model/src/work.rs`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const FILES: &[(&str, &str)] = &[
    ("tables/t.sql", "CREATE TABLE dbo.t (a int NOT NULL, b varchar(20) NULL);\n"),
    (
        "procs/branchy.sql",
        "CREATE PROCEDURE dbo.branchy @x int AS\nBEGIN\n  UPDATE dbo.t SET a = 1 WHERE a = @x;\n  IF @@ROWCOUNT = 0\n    INSERT INTO dbo.t (a) VALUES (@x);\n  IF @x > 5\n    UPDATE dbo.t SET b = 'big' WHERE a = @x;\n  ELSE\n    UPDATE dbo.t SET b = 'small' WHERE a = @x;\n  IF @x > 9 PRINT 'a';\n  IF @x > 10 PRINT 'b';\n  IF @x > 11 PRINT 'c';\nEND\n",
    ),
    (
        "procs/loop.sql",
        "CREATE PROCEDURE dbo.cur AS\nBEGIN\n  DECLARE @v int = 0;\n  WHILE @v < 3\n  BEGIN\n    UPDATE dbo.t SET b = 'x' WHERE a = @v;\n    IF @@ROWCOUNT = 0 INSERT INTO dbo.t (a) VALUES (@v);\n    SET @v = @v + 1;\n  END\nEND\n",
    ),
    (
        "procs/try.sql",
        "CREATE PROCEDURE dbo.tr @x int AS\nBEGIN\n  BEGIN TRY\n    UPDATE dbo.t SET a = @x;\n    IF @x > 1 DELETE dbo.t WHERE a = @x;\n  END TRY\n  BEGIN CATCH\n    INSERT INTO dbo.t (a) VALUES (0);\n  END CATCH\nEND\n",
    ),
    ("load.sql", "INSERT INTO dbo.t (a) VALUES (1);\nINSERT INTO dbo.t (a) VALUES (2);\n"),
];

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!("sqldiff-work-totals-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    for (p, text) in FILES {
        let f = d.join("src").join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, text).unwrap();
    }
    d
}

/// `file -> work total` of the files this run checked.
fn totals(src: &Path, out: &Path, jobs: &str, mode: &str) -> BTreeMap<String, u64> {
    let r = Command::new(env!("CARGO_BIN_EXE_sqldiff"))
        .args(["convert"])
        .arg(src)
        .arg("-o")
        .arg(out)
        .args(["--from", "tsql", "--to", "postgres", "-j", jobs, mode])
        .env("SQLDIFF_WORK", "1")
        .output()
        .unwrap();
    assert!(r.status.code().is_some_and(|c| c <= 1), "{r:?}");
    String::from_utf8_lossy(&r.stderr)
        .lines()
        .filter_map(|l| l.strip_prefix("work: "))
        .filter_map(|l| {
            let (file, rest) = l.rsplit_once(": total=")?;
            let n = rest.split_whitespace().next()?.parse().ok()?;
            Some((file.replace('\\', "/"), n))
        })
        .collect()
}

#[test]
fn thread_count_folder_and_resume_never_change_a_files_work() {
    let d = scratch();
    let src = d.join("src");
    let a = totals(&src, &d.join("out-a"), "1", "--fresh");
    assert!(a.len() >= FILES.len(), "{a:?}");
    assert!(a.values().any(|&n| n > 100), "{a:?}");
    // Another thread count and folder.
    assert_eq!(a, totals(&src, &d.join("out-b"), "4", "--fresh"));
    assert_eq!(a, totals(&src, &d.join("out-c"), "2", "--fresh"));
    // Resumed on the first run's store: whatever is checked again (the store
    // answers the rest) counts the same as before.
    let again = totals(&src, &d.join("out-a"), "4", "--overwrite");
    for (file, n) in &again {
        assert_eq!(a.get(file), Some(n), "{file}");
    }
}
