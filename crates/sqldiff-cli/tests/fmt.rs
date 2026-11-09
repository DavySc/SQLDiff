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

#[test]
fn a_folder_stands_for_its_sql_files() {
    let d = scratch("folder");
    std::fs::create_dir_all(d.join("db/sub")).unwrap();
    std::fs::create_dir_all(d.join("db/target")).unwrap();
    std::fs::write(d.join("db/a.pg.sql"), SRC).unwrap();
    std::fs::write(d.join("db/sub/b.tsql.sql"), "SELECT a, b\nFROM t;\n").unwrap();
    std::fs::write(d.join("db/notes.txt"), SRC).unwrap();
    // Tool folders are not listed, as a tree source does not list them.
    std::fs::write(d.join("db/target/c.pg.sql"), SRC).unwrap();
    // A folder is several files: it needs --check or --write.
    let o = sqldiff(&d, &["fmt", "db"]);
    assert_eq!(o.status.code(), Some(2));
    let o = sqldiff(&d, &["fmt", "--check", "db"]);
    assert_eq!(
        o.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let a = std::path::Path::new("db")
        .join("a.pg.sql")
        .display()
        .to_string();
    assert_eq!(stdout(&o), format!("{a}\n"));
    let o = sqldiff(&d, &["fmt", "--write", "db"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(stdout(&o), format!("formatted {a}\n"));
    assert_eq!(
        std::fs::read_to_string(d.join("db/a.pg.sql")).unwrap(),
        "SELECT a, b\nFROM t\nWHERE x = 1\n"
    );
    assert_eq!(
        std::fs::read_to_string(d.join("db/notes.txt")).unwrap(),
        SRC
    );
    assert_eq!(
        std::fs::read_to_string(d.join("db/target/c.pg.sql")).unwrap(),
        SRC
    );
    let o = sqldiff(&d, &["fmt", "--check", "db"]);
    assert_eq!(o.status.code(), Some(0), "{}", stdout(&o));
    // A folder without SQL files is an error, not a pass.
    std::fs::create_dir_all(d.join("empty")).unwrap();
    let o = sqldiff(&d, &["fmt", "--check", "empty"]);
    assert_eq!(o.status.code(), Some(2));
}

#[cfg(unix)]
#[test]
fn a_link_in_a_folder_is_not_written_through() {
    let d = scratch("link");
    std::fs::create_dir_all(d.join("db")).unwrap();
    std::fs::write(d.join("outside.pg.sql"), SRC).unwrap();
    std::os::unix::fs::symlink(d.join("outside.pg.sql"), d.join("db/l.pg.sql")).unwrap();
    let o = sqldiff(&d, &["fmt", "--write", "db"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("not formatted: a symbolic link"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(d.join("outside.pg.sql")).unwrap(),
        SRC
    );
}

fn with_stdin(dir: &PathBuf, args: &[&str], input: &[u8]) -> Output {
    use std::io::Write;
    use std::process::Stdio;
    let mut c = Command::new(env!("CARGO_BIN_EXE_sqldiff"))
        .current_dir(dir)
        .env("SQLDIFF_CONFIG", dir.join("no-user-config.toml"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(input).unwrap();
    c.wait_with_output().unwrap()
}

#[test]
fn stdin_to_stdout_with_filepath_dialect_and_config() {
    let d = scratch("stdin");
    std::fs::create_dir_all(d.join("db")).unwrap();
    // Config is found from the --stdin-filepath directory, not the cwd.
    std::fs::write(
        d.join("db").join("sqldiff.toml"),
        "[format]\nkeyword_case = \"lower\"\n",
    )
    .unwrap();
    let o = with_stdin(
        &d,
        &["fmt", "-", "--stdin-filepath", "db/q.pg.sql"],
        SRC.as_bytes(),
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(stdout(&o), "select a, b\nfrom t\nwhere x = 1\n");
    // Without the filepath there is no dialect.
    let o = with_stdin(&d, &["fmt", "-"], SRC.as_bytes());
    assert_eq!(o.status.code(), Some(2));
    // -d works without a path.
    let o = with_stdin(
        &d,
        &["fmt", "-", "-d", "postgres", "--keyword-case", "upper"],
        SRC.as_bytes(),
    );
    assert_eq!(stdout(&o), "SELECT a, b\nFROM t\nWHERE x = 1\n");
}

#[test]
fn stdin_keeps_crlf_and_rejects_bad_utf8() {
    let d = scratch("stdin-crlf");
    let o = with_stdin(
        &d,
        &["fmt", "-", "-d", "postgres"],
        b"select a,b\r\nfrom t\r\n",
    );
    assert_eq!(o.stdout, b"SELECT a, b\r\nFROM t\r\n");
    let o = with_stdin(&d, &["fmt", "-", "-d", "postgres"], b"select '\xff'");
    assert_eq!(o.status.code(), Some(2));
    assert!(o.stdout.is_empty());
}

#[test]
fn stdin_check_and_write() {
    let d = scratch("stdin-check");
    let o = with_stdin(
        &d,
        &["fmt", "-", "--check", "-d", "postgres"],
        SRC.as_bytes(),
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(o.stdout.is_empty());
    let o = with_stdin(
        &d,
        &["fmt", "-", "--write", "-d", "postgres"],
        SRC.as_bytes(),
    );
    assert_eq!(o.status.code(), Some(2));
    let o = with_stdin(&d, &["fmt", "x.sql", "--stdin-filepath", "y.sql"], b"");
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn check_reads_stdin_with_the_filepath_shown() {
    let d = scratch("stdin-lint");
    let o = with_stdin(
        &d,
        &[
            "check",
            "-",
            "--stdin-filepath",
            "sub/x.tsql.sql",
            "-f",
            "json",
        ],
        b"select 1\nfrom t limit 3\n",
    );
    let out = stdout(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("\"path\": \"sub/x.tsql.sql\""), "{out}");
    assert!(out.contains("residue.pg.limit"), "{out}");
    assert!(out.contains("\"start_line\": 2"), "{out}");
}

#[test]
fn stdin_filepath_matches_dialect_map_when_absolute_or_dotdot() {
    let d = scratch("stdin-map");
    let proj = d.join("proj");
    std::fs::create_dir_all(proj.join("db").join("sub")).unwrap();
    std::fs::create_dir_all(d.join("other")).unwrap();
    std::fs::write(
        proj.join("sqldiff.toml"),
        "[[dialect_map]]\nglob = \"db/**/*.sql\"\ndialect = \"postgres\"\n",
    )
    .unwrap();
    let abs = proj.join("db").join("sub").join("x.sql");
    let rel_from_proj = "db/sub/x.sql".to_string();
    let abs = abs.to_string_lossy().to_string();
    let dotdot = "../proj/db/sub/x.sql".to_string();
    let cases: [(&PathBuf, &str); 3] = [
        (&proj, abs.as_str()),
        (&proj, rel_from_proj.as_str()),
        (&d.join("other"), dotdot.as_str()),
    ];
    for (cwd, path) in cases {
        let o = with_stdin(cwd, &["fmt", "-", "--stdin-filepath", path], SRC.as_bytes());
        assert!(
            o.status.success(),
            "{path}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert_eq!(stdout(&o), "SELECT a, b\nFROM t\nWHERE x = 1\n", "{path}");
        let o = with_stdin(
            cwd,
            &["check", "-", "--stdin-filepath", path, "-f", "json"],
            b"select 1\n",
        );
        assert!(
            o.status.code() != Some(2),
            "{path}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
    // A path outside the project does not match the glob.
    let o = with_stdin(
        &proj,
        &["fmt", "-", "--stdin-filepath", "../x.sql"],
        SRC.as_bytes(),
    );
    assert_eq!(o.status.code(), Some(2));
}
