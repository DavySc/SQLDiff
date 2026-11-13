//! The work total of a replay-heavy file is pinned: one procedure of 20
//! statement pairs (an UPDATE and an `IF @@ROWCOUNT = 0 INSERT`, the shape of
//! `scripts/gen-adversarial.py` section A, one procedure of twenty), converted
//! to PostgreSQL with `SQLDIFF_WORK=1`.
//!
//! The merge test of a replay reads the fault-free run at each candidate tick.
//! It counts the walk to the tick (one step per tick) and restores the machine
//! from a checkpoint instead of walking it (`Explorer::try_merge`). A change
//! to how that machine is made must not move the count; the value below was
//! measured on the version that walked the machine, so a speed-up of the
//! replay keeps it and a change of what is counted does not.

use std::path::Path;
use std::process::Command;

const SCHEMA: &str = "CREATE TABLE dbo.T (Id int NOT NULL PRIMARY KEY, Name nvarchar(50) NULL, Qty int NOT NULL, Amount decimal(18,2) NOT NULL, Created datetime2(7) NOT NULL);\nGO\nCREATE TABLE dbo.Log (Id int IDENTITY(1,1) PRIMARY KEY, Msg nvarchar(200) NULL, At datetime2(7) NOT NULL);\nGO\n";

fn proc_text(i: usize, n: usize) -> String {
    let body: String = (0..n)
        .map(|k| {
            format!(
                "    UPDATE dbo.T SET Qty = Qty + {k} WHERE Id = @id + {k};\n    IF @@ROWCOUNT = 0 INSERT dbo.Log (Msg, At) VALUES (N'none {i}-{k}', SYSUTCDATETIME());\n"
            )
        })
        .collect();
    format!("CREATE PROCEDURE dbo.P{i} @id int\nAS\nBEGIN\n    SET NOCOUNT ON;\n{body}END\nGO\n")
}

fn work_line(src: &Path, out: &Path) -> String {
    let r = Command::new(env!("CARGO_BIN_EXE_sqldiff"))
        .arg("convert")
        .arg(src)
        .arg("-o")
        .arg(out)
        .args(["--from", "tsql", "--to", "postgres", "-j", "1", "--fresh"])
        .env("SQLDIFF_WORK", "1")
        .output()
        .unwrap();
    assert!(r.status.code().is_some_and(|c| c <= 1), "{r:?}");
    String::from_utf8_lossy(&r.stderr)
        .lines()
        .filter_map(|l| l.strip_prefix("work: "))
        .find(|l| l.starts_with("p1x20.sql: "))
        .unwrap_or_else(|| panic!("no work line: {r:?}"))
        .to_string()
}

#[test]
fn a_procedure_of_twenty_pairs_counts_the_same_work() {
    let d = std::env::temp_dir().join(format!("sqldiff-replay-pin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let src = d.join("p1x20.sql");
    std::fs::write(&src, format!("{SCHEMA}{}", proc_text(0, 20))).unwrap();
    let line = work_line(&src, &d.join("out"));
    let _ = std::fs::remove_dir_all(&d);
    assert!(line.contains(" total=789180 "), "{line}");
    assert!(line.contains(" explorer_steps=761619 "), "{line}");
}
