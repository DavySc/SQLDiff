//! `--proof` with a checker process. The checker here is a stand-in script
//! that speaks the sqldiff-check/2 protocol and decides like
//! formal/lean/Sqldiff/Gate.lean (confirmed only for identical sides);
//! formal-lean.yml tests the real one.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;

/// Writing a script and running processes from parallel test threads races
/// on Linux: a thread that forks while another holds the script open for
/// writing makes executing it fail ("text file busy"). The tests take turns.
static TURN: Mutex<()> = Mutex::new(());

fn turn() -> std::sync::MutexGuard<'static, ()> {
    TURN.lock().unwrap_or_else(|e| e.into_inner())
}

const STAND_IN: &str = r#"#!/usr/bin/env python3
import json, sys
if sys.argv[1:] == ["--version"]:
    print("stand-in 0 (sqldiff-check/2)")
    sys.exit(0)
for line in sys.stdin:
    f = line.rstrip("\n").split("\t")
    ok = len(f) == 10 and f[0] == "sqldiff-check/2" and f[1].isdigit() and f[9] == "end"
    if ok and f[3:6] == f[6:9]:
        a = {"id": f[1], "proof": "confirmed", "theorem": "Sqldiff.Gate.check_sound"}
    else:
        a = {"id": f[1] if ok else "0", "proof": "unconfirmed", "reason": "outside the verified fragment"}
    print(json.dumps(a), flush=True)
"#;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sqldiff-proof-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn stand_in(dir: &Path, body: &str) -> PathBuf {
    let p = dir.join("sqldiff-check");
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn write(dir: &Path, name: &str, text: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, text).unwrap();
    p.display().to_string()
}

const PROC: &str =
    "CREATE PROCEDURE dbo.p @id int AS\nBEGIN\n  UPDATE dbo.t SET x = 1 WHERE id = @id;\nEND\n";
const REFORMATTED: &str =
    "CREATE PROCEDURE dbo.p @id int AS\nBEGIN\n    -- set x\n    UPDATE dbo.t\n       SET x = 1\n     WHERE id = @id;\nEND\n";

fn diff(checker: Option<&Path>, a: &str, b: &str, mode: &str) -> (Output, serde_json::Value) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_sqldiff"));
    c.args([
        "diff",
        a,
        b,
        "--dialect",
        "tsql",
        "--isolated",
        "-f",
        "json",
        "--proof",
        mode,
    ]);
    match checker {
        Some(p) => c.env("SQLDIFF_CHECK", p),
        None => c.env("SQLDIFF_CHECK", "/nonexistent/sqldiff-check"),
    };
    let out = c.output().unwrap();
    let v = serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
    (out, v)
}

#[test]
fn a_reformatted_routine_is_confirmed_by_the_checker() {
    let _turn = turn();
    let d = scratch("confirm");
    let chk = stand_in(&d, STAND_IN);
    let (a, b) = (write(&d, "a.sql", PROC), write(&d, "b.sql", REFORMATTED));
    let (out, v) = diff(Some(&chk), &a, &b, "require");
    assert_eq!(out.status.code(), Some(0), "{v}");
    assert_eq!(v["verdict"], "equal");
    assert_eq!(v["proof"]["status"], "confirmed");
    assert_eq!(v["proof"]["theorem"], "Sqldiff.Gate.check_sound");
    assert!(v["proof"]["checker"]
        .as_str()
        .unwrap()
        .starts_with("stand-in 0"));
}

#[test]
fn require_turns_an_unconfirmed_equal_into_unknown() {
    let _turn = turn();
    let d = scratch("unconfirmed");
    let chk = stand_in(&d, STAND_IN);
    let lower = PROC.replace("UPDATE", "update");
    let (a, b) = (write(&d, "a.sql", PROC), write(&d, "b.sql", &lower));
    let (out, v) = diff(Some(&chk), &a, &b, "report");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(v["verdict"], "equal");
    assert_eq!(v["proof"]["status"], "unconfirmed");
    let (out, v) = diff(Some(&chk), &a, &b, "require");
    assert_eq!(v["verdict"], "unknown", "{v}");
    assert_eq!(v["verdict_before_proof"], "equal");
    assert_eq!(out.status.code(), Some(1), "{v}");
}

#[test]
fn a_missing_or_broken_checker_never_confirms() {
    let _turn = turn();
    let d = scratch("broken");
    let (a, b) = (write(&d, "a.sql", PROC), write(&d, "b.sql", REFORMATTED));
    let (out, v) = diff(None, &a, &b, "require");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(v["verdict"], "unknown");
    // A checker that answers "confirmed" for the wrong request.
    let liar = STAND_IN.replace(
        r#""id": f[1], "proof": "confirmed""#,
        r#""id": "999", "proof": "confirmed""#,
    );
    let chk = stand_in(&d, &liar);
    let (_, v) = diff(Some(&chk), &a, &b, "require");
    assert_eq!(v["verdict"], "unknown");
    assert!(
        v["proof"]["reason"]
            .as_str()
            .unwrap()
            .contains("another request"),
        "{v}"
    );
    // One that dies.
    let chk = stand_in(
        &d,
        "#!/bin/sh\n[ \"$1\" = --version ] && echo 'x (sqldiff-check/2)' && exit 0\nexit 3\n",
    );
    let (_, v) = diff(Some(&chk), &a, &b, "require");
    assert_eq!(v["verdict"], "unknown");
    assert_eq!(v["proof"]["status"], "unconfirmed");
}

#[test]
fn off_is_the_default_and_leaves_no_proof() {
    let _turn = turn();
    let d = scratch("off");
    let (a, b) = (write(&d, "a.sql", PROC), write(&d, "b.sql", REFORMATTED));
    let out = Command::new(env!("CARGO_BIN_EXE_sqldiff"))
        .args([
            "diff",
            &a,
            &b,
            "--dialect",
            "tsql",
            "--isolated",
            "-f",
            "json",
        ])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["verdict"], "equal");
    assert!(v.get("proof").is_none());
}

/// The real checker, when `SQLDIFF_REAL_CHECK` names one (the Linux job of
/// `.github/workflows/formal-lean.yml` builds it and sets it): the Rust
/// trees, schema hash and protocol must be what the Lean side decodes.
#[test]
fn the_real_checker_confirms_a_reformatted_routine() {
    let Some(real) = std::env::var_os("SQLDIFF_REAL_CHECK") else {
        return;
    };
    let real = PathBuf::from(real);
    let _turn = turn();
    let d = scratch("real");
    let (a, b) = (write(&d, "a.sql", PROC), write(&d, "b.sql", REFORMATTED));
    let (out, v) = diff(Some(&real), &a, &b, "require");
    assert_eq!(out.status.code(), Some(0), "{v}");
    assert_eq!(v["verdict"], "equal", "{v}");
    assert_eq!(v["proof"]["status"], "confirmed", "{v}");
    assert_eq!(v["proof"]["theorem"], "Sqldiff.Gate.check_sound");
    let lower = write(&d, "c.sql", &PROC.replace("UPDATE", "update"));
    let (_, v) = diff(Some(&real), &a, &lower, "require");
    assert_eq!(v["verdict"], "unknown", "{v}");
    assert_eq!(
        v["proof"]["reason"],
        "the syntax trees differ: outside the verified fragment"
    );
}

/// C1 end to end with the real checker: a converted procedure with IF,
/// ELSE IF, WHILE and RETURN is confirmed from the text on disk; one with
/// an UPDATE is outside C1 and says so.
#[test]
fn the_real_checker_confirms_a_converted_procedure() {
    let Some(real) = std::env::var_os("SQLDIFF_REAL_CHECK") else {
        return;
    };
    let _turn = turn();
    let d = scratch("convert-real");
    let src = d.join("src");
    std::fs::create_dir_all(&src).unwrap();
    write(&src, "p.sql", "CREATE PROCEDURE dbo.p @a int, @b int = 2 OUTPUT\nAS\nBEGIN\n  DECLARE @x int = 1;\n  IF @a > 1\n    SET @x = 2;\n  ELSE IF @a < 0\n  BEGIN\n    SET @x = 3;\n  END\n  ELSE\n    SET @x = 4;\n  WHILE @x < 10\n    SET @x = @x + 1;\n  RETURN @x;\nEND\n");
    write(
        &src,
        "touch.sql",
        "CREATE PROCEDURE dbo.touch @id int AS\nBEGIN\n  UPDATE t SET x = 1 WHERE id = @id;\nEND\n",
    );
    let out = d.join("out");
    let o = Command::new(env!("CARGO_BIN_EXE_sqldiff"))
        .args([
            "convert",
            &src.display().to_string(),
            "--to",
            "postgres",
            "--out",
        ])
        .arg(&out)
        .args(["--isolated", "--proof", "report"])
        .env("SQLDIFF_CHECK", &real)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(
        stdout.contains("certificates: 1 of 2 routines confirmed"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let c: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("certificates.json")).unwrap())
            .unwrap();
    let routine = |path: &str| {
        c["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"] == path)
            .unwrap()["routines"][0]
            .clone()
    };
    let p = routine("p.sql");
    assert_eq!(p["answer"], "confirmed", "{p}");
    assert_eq!(p["theorems"][0], "Sqldiff.Mirror.check_sound");
    assert_eq!(p["assumptions"][0], "ASM-C1-OPERATORS", "{p}");
    let t = routine("touch.sql");
    assert_eq!(t["answer"], "unconfirmed", "{t}");
    assert_eq!(t["reason"], "the T-SQL routine is outside the C1 spec");
}

/// C4 end to end with the real checker: the converter writes TRY/CATCH as
/// a PL/pgSQL block with EXCEPTION WHEN OTHERS. A TRY body that writes
/// nothing is confirmed (the block's rollback changes nothing); one that
/// writes and can then fail, with a handler that carries on, is refused
/// (PostgreSQL rolls the write back, T-SQL keeps it).
#[test]
fn the_real_checker_confirms_a_converted_try_catch() {
    let Some(real) = std::env::var_os("SQLDIFF_REAL_CHECK") else {
        return;
    };
    let _turn = turn();
    let d = scratch("try-real");
    let src = d.join("src");
    std::fs::create_dir_all(&src).unwrap();
    write(&src, "p.sql", "CREATE PROCEDURE dbo.p @a int, @b int OUTPUT\nAS\nBEGIN\n  SET XACT_ABORT ON;\n  BEGIN TRY\n    SET @b = @a / 0;\n  END TRY\n  BEGIN CATCH\n    SET @b = -1;\n  END CATCH\nEND\n");
    write(&src, "q.sql", "CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, n int NULL);\nGO\nCREATE PROCEDURE dbo.q @a int, @b int OUTPUT\nAS\nBEGIN\n  SET XACT_ABORT ON;\n  SET NOCOUNT ON;\n  BEGIN TRY\n    INSERT INTO dbo.t (id, n) VALUES (@a, 1);\n    SET @b = @a / @b;\n  END TRY\n  BEGIN CATCH\n    SET @b = -1;\n  END CATCH\nEND\n");
    let out = d.join("out");
    let o = Command::new(env!("CARGO_BIN_EXE_sqldiff"))
        .args([
            "convert",
            &src.display().to_string(),
            "--to",
            "postgres",
            "--out",
        ])
        .arg(&out)
        .args(["--isolated", "--proof", "report"])
        .env("SQLDIFF_CHECK", &real)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(
        stdout.contains("certificates: 1 of 2 routines confirmed"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let c: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("certificates.json")).unwrap())
            .unwrap();
    let routine = |path: &str| {
        c["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"] == path)
            .unwrap()["routines"][0]
            .clone()
    };
    let p = routine("p.sql");
    assert_eq!(p["answer"], "confirmed", "{p}");
    assert_eq!(p["theorems"][0], "Sqldiff.Mirror.check_sound_c2a", "{p}");
    let q = routine("q.sql");
    assert_eq!(q["answer"], "unconfirmed", "{q}");
    assert_eq!(
        q["reason"], "the lowered routines differ beyond the renaming",
        "{q}"
    );
}

/// `--proof require` is the release gate: a routine outside the C2a slice
/// (`@a + 1` can overflow, and without XACT_ABORT ON T-SQL would carry on)
/// still rests on ASM-C1-OPERATORS, which no deployment can accept, so the
/// gate names it and the exit code is 1.
#[test]
fn the_real_checker_and_the_release_gate_name_what_needs_a_review() {
    let Some(real) = std::env::var_os("SQLDIFF_REAL_CHECK") else {
        return;
    };
    let _turn = turn();
    let d = scratch("gate-real");
    let src = d.join("src");
    std::fs::create_dir_all(&src).unwrap();
    write(
        &src,
        "p.sql",
        "CREATE PROCEDURE p @a int OUTPUT\nAS\nBEGIN\n  IF @a > 1\n    SET @a = @a + 1;\nEND\n",
    );
    let o = Command::new(env!("CARGO_BIN_EXE_sqldiff"))
        .args([
            "convert",
            &src.display().to_string(),
            "--to",
            "postgres",
            "--out",
        ])
        .arg(d.join("out"))
        .args(["--isolated", "--proof", "require"])
        .env("SQLDIFF_CHECK", &real)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("certificates: 1 of 1 routines confirmed"),
        "{stdout}"
    );
    assert!(
        stdout.contains("release gate: a review is still needed"),
        "{stdout}"
    );
    assert!(
        stdout.contains("p.sql: dbo.p: rests on ASM-C1-OPERATORS")
            || stdout.contains("p.sql: p: rests on ASM-C1-OPERATORS"),
        "{stdout}"
    );
}

/// A procedure whose expressions are all in the C2a slice (comparison,
/// ISNULL, CASE; nothing that can raise) is confirmed with its operators'
/// meaning, and with the deployment accepting what it still rests on, the
/// release gate passes: the proof stands where the engine did not compare.
#[test]
fn the_real_checker_lets_the_release_gate_pass_in_the_c2a_slice() {
    let Some(real) = std::env::var_os("SQLDIFF_REAL_CHECK") else {
        return;
    };
    let _turn = turn();
    let d = scratch("gate-c2a");
    let src = d.join("src");
    std::fs::create_dir_all(&src).unwrap();
    write(&src, "p.sql", "CREATE PROCEDURE p @a int, @b int OUTPUT\nAS\nBEGIN\n  IF @a > 1\n    SET @b = ISNULL(@a, 0);\n  ELSE\n    SET @b = CASE WHEN @a IS NULL THEN 0 ELSE @a END;\nEND\n");
    let config = write(
        &d,
        "config.toml",
        "[deployment]\naccept = [\"ASM-RETURN-CODE\", \"ASM-NO-CALLER-TXN\"]\n",
    );
    let run = |config: Option<&str>| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_sqldiff"));
        c.args([
            "convert",
            &src.display().to_string(),
            "--to",
            "postgres",
            "--out",
        ])
        .arg(d.join("out"))
        .args(["--overwrite", "--isolated", "--proof", "require"])
        .env("SQLDIFF_CHECK", &real);
        if let Some(f) = config {
            c.args(["--config", f]);
        }
        c.output().unwrap()
    };
    let o = run(Some(&config));
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{stdout}");
    assert!(stdout.contains("release gate: passed"), "{stdout}");
    let c: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(d.join("out/certificates.json")).unwrap())
            .unwrap();
    let r = &c["files"][0]["routines"][0];
    assert_eq!(r["theorems"][0], "Sqldiff.Mirror.check_sound_c2a", "{r}");
    assert_eq!(r["scalars"], "c2a", "{r}");
    assert!(
        !r["assumptions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a == "ASM-C1-OPERATORS"),
        "{r}"
    );
    // Without the deployment's acceptance the gate names what is open.
    let o = run(None);
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("rests on ASM-RETURN-CODE"), "{stdout}");
}

/// C3a end to end: a procedure that writes a plain table (declared in the
/// tree, nothing else on it) is confirmed with its data statements in the
/// slice; once a trigger is declared on the table it is not.
#[test]
fn the_real_checker_confirms_data_statements_on_a_plain_table() {
    let Some(real) = std::env::var_os("SQLDIFF_REAL_CHECK") else {
        return;
    };
    let _turn = turn();
    let d = scratch("c3a-real");
    let src = d.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let ddl = "CREATE TABLE dbo.Acct (Id int NOT NULL, Qty int NULL, Total int NOT NULL, CONSTRAINT PK_Acct PRIMARY KEY (Id));\n";
    write(&src, "acct.sql", ddl);
    write(&src, "p.sql", "CREATE PROCEDURE dbo.post @id int, @n int\nAS\nBEGIN\n  SET XACT_ABORT ON;\n  SET NOCOUNT ON;\n  IF @n > 0\n    INSERT INTO dbo.Acct (Id, Qty, Total) VALUES (@id, @n, 0);\n  UPDATE dbo.Acct SET Qty = Qty + 1 WHERE Id = @id;\n  DELETE FROM dbo.Acct WHERE Total < 0;\nEND\n");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_sqldiff"))
            .args([
                "convert",
                &src.display().to_string(),
                "--to",
                "postgres",
                "--out",
            ])
            .arg(d.join("out"))
            .args(["--overwrite", "--isolated", "--proof", "report"])
            .env("SQLDIFF_CHECK", &real)
            .output()
            .unwrap();
        let c: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(d.join("out/certificates.json")).unwrap(),
        )
        .unwrap();
        c["files"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|f| f["routines"].as_array().unwrap().clone())
            .find(|r| r["routine"] == "dbo.post")
            .unwrap()
    };
    let r = run();
    assert_eq!(r["answer"], "confirmed", "{r}");
    assert_eq!(r["theorems"][0], "Sqldiff.Mirror.check_sound_c2a", "{r}");
    assert_eq!(r["statements"], "c3a", "{r}");
    let asm: Vec<&str> = r["assumptions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    // The UPDATE can overflow after the INSERT wrote.
    for a in [
        "ASM-STATE-AFTER-ERROR",
        "ASM-TABLES-AS-DECLARED",
        "ASM-OBSERVER-ROWCOUNT",
    ] {
        assert!(asm.contains(&a), "{a}: {r}");
    }
    write(
        &src,
        "acct.sql",
        &format!(
            "{ddl}GO\nCREATE TRIGGER tr ON dbo.Acct AFTER INSERT AS BEGIN SET NOCOUNT ON; END\n"
        ),
    );
    let r = run();
    assert_eq!(r["answer"], "unconfirmed", "{r}");
}

/// C3b end to end: a procedure that reads a plain table in the three shapes
/// the converter writes (`SELECT @x = col` by key, which becomes a PL/pgSQL
/// `FOR` block; `IF [NOT] EXISTS`; a scalar subquery by key) and counts
/// rows is confirmed with `statements: c3b`; the table is sent although the
/// procedure only reads it. A `SELECT @x = col` whose `WHERE` does not fix
/// the key is refused: which row's value T-SQL keeps is unspecified.
#[test]
fn the_real_checker_confirms_reads_of_a_plain_table() {
    let Some(real) = std::env::var_os("SQLDIFF_REAL_CHECK") else {
        return;
    };
    let _turn = turn();
    let d = scratch("c3b-real");
    let src = d.join("src");
    std::fs::create_dir_all(&src).unwrap();
    write(
        &src,
        "acct.sql",
        "CREATE TABLE dbo.Acct (Id int NOT NULL, Qty int NULL, Total int NOT NULL, CONSTRAINT PK_Acct PRIMARY KEY (Id));\n",
    );
    let look = |wh: &str| {
        format!(
            "CREATE PROCEDURE dbo.look @id int, @q int OUTPUT, @t int OUTPUT, @e int OUTPUT\nAS\nBEGIN\n  SET XACT_ABORT ON;\n  SET NOCOUNT ON;\n  SELECT @q = Qty, @t = Total FROM dbo.Acct WHERE {wh};\n  IF EXISTS (SELECT 1 FROM dbo.Acct WHERE Qty > @id)\n    SET @e = 1;\n  IF NOT EXISTS (SELECT 1 FROM dbo.Acct WHERE Qty > @id)\n    SET @e = 2;\n  ELSE\n    SET @e = 3;\n  SET @t = (SELECT Total FROM dbo.Acct WHERE Id = @id);\n  SELECT @e = COUNT(*) FROM dbo.Acct WHERE Qty > 0;\nEND\n"
        )
    };
    write(&src, "look.sql", &look("Id = @id"));
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_sqldiff"))
            .args([
                "convert",
                &src.display().to_string(),
                "--to",
                "postgres",
                "--out",
            ])
            .arg(d.join("out"))
            .args(["--overwrite", "--isolated", "--proof", "report"])
            .env("SQLDIFF_CHECK", &real)
            .output()
            .unwrap();
        let c: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(d.join("out/certificates.json")).unwrap(),
        )
        .unwrap();
        c["files"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|f| f["routines"].as_array().unwrap().clone())
            .find(|r| r["routine"] == "dbo.look")
            .unwrap()
    };
    // The converter writes the shapes the checker reads.
    let r = run();
    let out = std::fs::read_to_string(d.join("out/look.sql")).unwrap();
    for shape in [
        "FOR sqldiff_r IN SELECT Qty AS sqldiff_c1, Total AS sqldiff_c2 FROM Acct WHERE Id = p_id LOOP",
        "IF NOT EXISTS (SELECT 1 FROM Acct WHERE Qty > p_id) THEN",
        "p_t := (SELECT Total FROM Acct WHERE Id = p_id);",
        "SELECT COUNT(*) INTO p_e FROM Acct WHERE Qty > 0;",
    ] {
        assert!(out.contains(shape), "{shape}:\n{out}");
    }
    assert_eq!(r["answer"], "confirmed", "{r}");
    assert_eq!(r["theorems"][0], "Sqldiff.Mirror.check_sound_c2a", "{r}");
    assert_eq!(r["statements"], "c3b", "{r}");
    let asm: Vec<&str> = r["assumptions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    // COUNT(*) can overflow; `Id = @id` compares with `=`; the table is
    // taken as declared. Reads send no rows-affected count.
    for a in [
        "ASM-TSQL-ARITH-ERRORS",
        "ASM-TSQL-ANSI-NULLS",
        "ASM-TABLES-AS-DECLARED",
    ] {
        assert!(asm.contains(&a), "{a}: {r}");
    }
    assert!(!asm.contains(&"ASM-OBSERVER-ROWCOUNT"), "{r}");
    write(&src, "look.sql", &look("Qty = @id"));
    let r = run();
    assert_eq!(r["answer"], "unconfirmed", "{r}");
}

/// C2b end to end: concatenation and ISNULL over bounded string variables
/// are confirmed in the slice, and the leading SET CONCAT_NULL_YIELDS_NULL
/// ON discharges ASM-CONCAT-NULL in the release gate. A string `=` is
/// refused once the collation is trusted: T-SQL ignores trailing spaces,
/// the converted comparison does not.
#[test]
fn the_real_checker_confirms_strings_and_refuses_a_padded_comparison() {
    let Some(real) = std::env::var_os("SQLDIFF_REAL_CHECK") else {
        return;
    };
    let _turn = turn();
    let d = scratch("c2b-real");
    let src = d.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let config = write(
        &d,
        "config.toml",
        "[deployment]\naccept = [\"ASM-BINARY-COLLATION\"]\n",
    );
    let run = |body: &str| {
        write(&src, "p.sql", &format!("CREATE PROCEDURE dbo.greet @n int, @out varchar(40) OUTPUT\nAS\nBEGIN\n  SET CONCAT_NULL_YIELDS_NULL ON;\n  DECLARE @v varchar(10) = 'abc';\n{body}END\n"));
        let o = Command::new(env!("CARGO_BIN_EXE_sqldiff"))
            .args([
                "convert",
                &src.display().to_string(),
                "--to",
                "postgres",
                "--out",
            ])
            .arg(d.join("out"))
            .args([
                "--overwrite",
                "--isolated",
                "--proof",
                "require",
                "--config",
            ])
            .arg(&config)
            .env("SQLDIFF_CHECK", &real)
            .output()
            .unwrap();
        let c: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(d.join("out/certificates.json")).unwrap(),
        )
        .unwrap();
        (
            String::from_utf8_lossy(&o.stdout).to_string(),
            c["files"][0]["routines"][0].clone(),
        )
    };
    let (stdout, r) = run("  IF @n > 0\n    SET @out = 'hello ' + @v;\n  ELSE\n    SET @out = ISNULL(@v, 'nobody');\n");
    assert_eq!(r["answer"], "confirmed", "{r}");
    assert_eq!(r["scalars"], "c2a", "{r}");
    assert!(!stdout.contains("ASM-CONCAT-NULL"), "{stdout}");
    assert!(!stdout.contains("ASM-ISNULL-TYPE"), "{stdout}");
    assert!(stdout.contains("ASM-TSQL-STRING-ARGS"), "{stdout}");
    let (_, r) = run("  IF @v = 'abc'\n    SET @out = @v;\n");
    assert_eq!(r["answer"], "unconfirmed", "{r}");
}

/// C4 in the release gate: a TRY whose CATCH only re-raises is confirmed
/// with `errors: c4`, the proof settles the PL/pgSQL block's rollback, so
/// the converter's ASM-TRY-SUBTRANSACTION note is discharged, while what
/// the proof itself rests on stays listed. ASM-NO-CALLER-TXN is not
/// discharged by its absence from a residual.
#[test]
fn the_real_checker_discharges_the_try_subtransaction_note() {
    let Some(real) = std::env::var_os("SQLDIFF_REAL_CHECK") else {
        return;
    };
    let _turn = turn();
    let d = scratch("c4-gate");
    let src = d.join("src");
    std::fs::create_dir_all(&src).unwrap();
    write(&src, "acct.sql", "CREATE TABLE dbo.Acct (Id int NOT NULL, Qty int NULL, CONSTRAINT PK_Acct PRIMARY KEY (Id));\n");
    write(&src, "p.sql", "CREATE PROCEDURE dbo.post @id int, @n int\nAS\nBEGIN\n  SET XACT_ABORT ON;\n  SET NOCOUNT ON;\n  BEGIN TRY\n    INSERT INTO dbo.Acct (Id, Qty) VALUES (@id, @n);\n    UPDATE dbo.Acct SET Qty = Qty + 1 WHERE Id = @id;\n  END TRY\n  BEGIN CATCH\n    THROW;\n  END CATCH\nEND\n");
    let o = Command::new(env!("CARGO_BIN_EXE_sqldiff"))
        .args([
            "convert",
            &src.display().to_string(),
            "--to",
            "postgres",
            "--out",
        ])
        .arg(d.join("out"))
        .args(["--isolated", "--proof", "require"])
        .env("SQLDIFF_CHECK", &real)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout);
    let c: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(d.join("out/certificates.json")).unwrap())
            .unwrap();
    let r = c["files"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|f| f["routines"].as_array().unwrap().clone())
        .find(|r| r["routine"] == "dbo.post")
        .unwrap();
    assert_eq!(r["answer"], "confirmed", "{r}");
    assert_eq!(r["errors"], "c4", "{r}");
    assert!(!stdout.contains("ASM-TRY-SUBTRANSACTION"), "{stdout}");
    for open in [
        "ASM-CATCH-CORE-ERRORS",
        "ASM-STATE-AFTER-ERROR",
        "ASM-NO-CALLER-TXN",
    ] {
        assert!(stdout.contains(open), "{open}: {stdout}");
    }
}

/// A caller's confirmation takes its callee as its conversion: the release
/// gate accepts that only when the callee's own certificate is confirmed
/// (and calls nothing itself); otherwise it names the callee.
#[test]
fn the_real_checker_ties_a_caller_to_its_callee() {
    let Some(real) = std::env::var_os("SQLDIFF_REAL_CHECK") else {
        return;
    };
    let _turn = turn();
    let d = scratch("callee-gate");
    let src = d.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let caller = "CREATE PROCEDURE dbo.p @x int OUTPUT\nAS\nBEGIN\n  EXEC dbo.q @x OUTPUT;\nEND\n";
    let run = |q: &str| {
        write(&src, "q.sql", q);
        write(&src, "p.sql", caller);
        let o = Command::new(env!("CARGO_BIN_EXE_sqldiff"))
            .args([
                "convert",
                &src.display().to_string(),
                "--to",
                "postgres",
                "--out",
            ])
            .arg(d.join("out"))
            .args(["--overwrite", "--isolated", "--proof", "require"])
            .env("SQLDIFF_CHECK", &real)
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout).to_string()
    };
    let good =
        run("CREATE PROCEDURE dbo.q @n int OUTPUT\nAS\nBEGIN\n  SET @n = ISNULL(@n, 0);\nEND\n");
    assert!(
        good.contains("certificates: 2 of 2 routines confirmed"),
        "{good}"
    );
    assert!(!good.contains("ASM-CALLEE-CONFIRMED"), "{good}");
    let bad = run("CREATE PROCEDURE dbo.q @n int OUTPUT\nAS\nBEGIN\n  SAVE TRANSACTION s;\nEND\n");
    assert!(bad.contains("ASM-CALLEE-CONFIRMED:dbo.q"), "{bad}");
}
