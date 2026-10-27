//! `sqldiff fmt`: printing, `--check`, `--write` and layout from `[format]`.

use std::path::PathBuf;
use std::process::{Command, Output};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sqldiff-cli-fmt-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn sqldiff(dir: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sqldiff"))
        .current_dir(dir)
        .env("SQLDIFF_CONFIG", dir.join("no-user-config.toml"))
        .args(args)
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}

const SRC: &str = "select a,b from t where x=1\n";

#[test]
fn prints_checks_and_writes() {
    let d = scratch("modes");
    std::fs::write(d.join("q.pg.sql"), SRC).unwrap();
    let o = sqldiff(&d, &["fmt", "q.pg.sql"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(stdout(&o), "SELECT a, b\nFROM t\nWHERE x = 1\n");

    let o = sqldiff(&d, &["fmt", "--check", "q.pg.sql"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(stdout(&o), "q.pg.sql\n");
    assert_eq!(std::fs::read_to_string(d.join("q.pg.sql")).unwrap(), SRC);

    let o = sqldiff(&d, &["fmt", "--write", "q.pg.sql"]);
    assert!(o.status.success());
    let o = sqldiff(&d, &["fmt", "--check", "q.pg.sql"]);
    assert_eq!(o.status.code(), Some(0), "{}", stdout(&o));

    // Several files need --write or --check.
    std::fs::write(d.join("r.pg.sql"), SRC).unwrap();
    let o = sqldiff(&d, &["fmt", "q.pg.sql", "r.pg.sql"]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn layout_comes_from_config_then_flags() {
    let d = scratch("config");
    std::fs::write(d.join("q.pg.sql"), SRC).unwrap();
    std::fs::write(
        d.join("sqldiff.toml"),
        "[format]\nkeyword_case = \"lower\"\n\n[format.postgres]\none_item_per_line = true\n",
    )
    .unwrap();
    let o = sqldiff(&d, &["fmt", "q.pg.sql"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(stdout(&o), "select a,\n    b\nfrom t\nwhere x = 1\n");
    let o = sqldiff(
        &d,
        &[
            "fmt",
            "--keyword-case",
            "upper",
            "--indent",
            "2",
            "q.pg.sql",
        ],
    );
    assert_eq!(stdout(&o), "SELECT a,\n  b\nFROM t\nWHERE x = 1\n");
    // A T-SQL file does not get the PostgreSQL table.
    std::fs::write(d.join("q.tsql.sql"), SRC).unwrap();
    let o = sqldiff(&d, &["fmt", "q.tsql.sql"]);
    assert_eq!(stdout(&o), "select a, b\nfrom t\nwhere x = 1\n");

    std::fs::write(d.join("sqldiff.toml"), "[format]\nindnet = 2\n").unwrap();
    let o = sqldiff(&d, &["fmt", "q.pg.sql"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("indnet"));
}
