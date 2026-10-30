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
        stdout.contains("proof: 1 of 2 routines confirmed"),
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
        stdout.contains("proof: 1 of 2 routines confirmed"),
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
        stdout.contains("proof: 1 of 1 routines confirmed"),
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
        // `@a int`: callers pass values of the parameter's type.
        "[deployment]\naccept = [\"ASM-RETURN-CODE\", \"ASM-NO-CALLER-TXN\", \"ASM-TSQL-STRING-ARGS\"]\n",
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
    // The UPDATE can overflow after the INSERT wrote; a caller's SET
    // ROWCOUNT would limit the rows the statements touch (eval/session).
    for a in [
        "ASM-STATE-AFTER-ERROR",
        "ASM-TABLES-AS-DECLARED",
        "ASM-OBSERVER-ROWCOUNT",
        "ASM-TSQL-ROWCOUNT-0",
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
    // taken as declared; the reads run under the caller's SET ROWCOUNT.
    // Reads send no rows-affected count.
    for a in [
        "ASM-TSQL-ARITH-ERRORS",
        "ASM-TSQL-ANSI-NULLS",
        "ASM-TABLES-AS-DECLARED",
        "ASM-TSQL-ROWCOUNT-0",
    ] {
        assert!(asm.contains(&a), "{a}: {r}");
    }
    assert!(!asm.contains(&"ASM-OBSERVER-ROWCOUNT"), "{r}");
    write(&src, "look.sql", &look("Qty = @id"));
    let r = run();
    assert_eq!(r["answer"], "unconfirmed", "{r}");
}

/// C3d end to end: a procedure that reads two plain tables through a join
/// by key (`SELECT @x = o.Qty, @s = c.Score FROM dbo.Ord AS o JOIN dbo.Cust
/// c ON c.Id = o.CustId WHERE o.Id = @id`, which becomes the PL/pgSQL `FOR`
/// block over the same join), takes a scalar subquery over an inner join,
/// counts the rows of a comma list and tests `EXISTS` over a cross join is
/// confirmed by `check_sound_c2a`; both tables are sent. The near miss,
/// a join on a column that is no key, may keep several rows, of which T-SQL
/// assigns an unspecified one: refused with `stmt:select:many-rows`.
#[test]
fn the_real_checker_confirms_a_join_read_by_key() {
    let Some(real) = std::env::var_os("SQLDIFF_REAL_CHECK") else {
        return;
    };
    let _turn = turn();
    let d = scratch("c3d-real");
    let src = d.join("src");
    std::fs::create_dir_all(&src).unwrap();
    write(
        &src,
        "tables.sql",
        "CREATE TABLE dbo.Ord (Id int NOT NULL, Qty int NULL, CustId int NULL, CONSTRAINT PK_Ord PRIMARY KEY (Id));\nGO\n\
         CREATE TABLE dbo.Cust (Id int NOT NULL, Score int NULL, CONSTRAINT PK_Cust PRIMARY KEY (Id));\nGO\n",
    );
    let look = |on: &str| {
        format!(
            "CREATE PROCEDURE dbo.look @id int, @q int OUTPUT, @s int OUTPUT, @n int OUTPUT\nAS\nBEGIN\n  SET XACT_ABORT ON;\n  SET NOCOUNT ON;\n  SELECT @q = o.Qty, @s = c.Score FROM dbo.Ord AS o JOIN dbo.Cust c ON {on} WHERE o.Id = @id;\n  SET @s = (SELECT c.Score FROM dbo.Ord o INNER JOIN dbo.Cust AS c ON o.CustId = c.Id WHERE o.Qty > @id);\n  SELECT @n = COUNT(*) FROM dbo.Ord, dbo.Cust WHERE Ord.CustId = Cust.Id;\n  IF EXISTS (SELECT 1 FROM dbo.Ord o CROSS JOIN dbo.Cust c WHERE o.Qty = c.Score)\n    SET @n = 0;\nEND\n"
        )
    };
    write(&src, "look.sql", &look("c.Id = o.CustId"));
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
    let r = run();
    // The converter keeps the FROM as written: aliases, qualifiers, joins.
    let out = std::fs::read_to_string(d.join("out/look.sql")).unwrap();
    for shape in [
        "FOR sqldiff_r IN SELECT o.Qty AS sqldiff_c1, c.Score AS sqldiff_c2 FROM Ord AS o JOIN Cust c ON c.Id = o.CustId WHERE o.Id = p_id LOOP",
        "p_s := (SELECT c.Score FROM Ord o INNER JOIN Cust AS c ON o.CustId = c.Id WHERE o.Qty > p_id);",
        "SELECT COUNT(*) INTO p_n FROM Ord, Cust WHERE Ord.CustId = Cust.Id;",
        "IF EXISTS (SELECT 1 FROM Ord o CROSS JOIN Cust c WHERE o.Qty = c.Score) THEN",
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
    // The scalar subquery over the join can raise `cardinality` under
    // `XACT_ABORT ON`, which dooms a caller's transaction
    // (ASM-NO-CALLER-TXN, eval/queries Q15); every read is limited by a
    // caller's `SET ROWCOUNT` (ASM-TSQL-ROWCOUNT-0, eval/session).
    for a in [
        "ASM-TSQL-ARITH-ERRORS",
        "ASM-TSQL-ANSI-NULLS",
        "ASM-TABLES-AS-DECLARED",
        "ASM-NO-CALLER-TXN",
        "ASM-TSQL-ROWCOUNT-0",
    ] {
        assert!(asm.contains(&a), "{a}: {r}");
    }
    assert!(!asm.contains(&"ASM-OBSERVER-ROWCOUNT"), "{r}");
    // A join on a column that is no key: `Cust` is not determined.
    write(&src, "look.sql", &look("c.Score = o.Qty"));
    let r = run();
    assert_eq!(r["answer"], "unconfirmed", "{r}");
    let why: Vec<&str> = r["why"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert_eq!(why, ["stmt:select:many-rows"], "{r}");
    // The table named in another case than its DDL: a case-sensitive SQL
    // Server database raises 208 where PostgreSQL folds the name, so the
    // table is not sent and the read is refused (review of C3d).
    write(
        &src,
        "look.sql",
        &look("c.Id = o.CustId").replace("FROM dbo.Ord AS o JOIN", "FROM dbo.ORD AS o JOIN"),
    );
    let r = run();
    assert_eq!(r["answer"], "unconfirmed", "{r}");
    assert!(
        r["why"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "stmt:select:table:unknown"),
        "{r}"
    );
}

/// C3d part 2 end to end: a converted procedure reads, by key across a
/// join, by `COUNT(*)`, `EXISTS` and a scalar subquery, tables with a foreign
/// key, identity, `CHECK`, `decimal`, `char`, `datetime2` (that is,
/// `datetime2(7)`) columns and an expression default, declared in the tree
/// or in base catalogs (`--catalog`, with or without `--catalog-b`); it is
/// confirmed by `check_sound_c2a`. A write to such a table, a read of a
/// column outside the slice, and a table under a security policy are
/// refused.
#[test]
fn the_real_checker_confirms_reads_of_a_read_only_table() {
    let Some(real) = std::env::var_os("SQLDIFF_REAL_CHECK") else {
        return;
    };
    let _turn = turn();
    let d = scratch("c3d2-real");
    let src = d.join("src");
    std::fs::create_dir_all(&src).unwrap();
    const TABLES: &str = "CREATE TABLE dbo.Item (Id int IDENTITY(1,1) NOT NULL PRIMARY KEY,\n  \
         Name varchar(20) NOT NULL, Code char(4) NOT NULL, Price decimal(12,2) NOT NULL,\n  \
         Added datetime2 NOT NULL DEFAULT SYSDATETIME());\nGO\n\
         CREATE TABLE dbo.Line (OrdId int NOT NULL, LineNo int NOT NULL,\n  \
         ItemId int NOT NULL REFERENCES dbo.Item (Id), Qty int NOT NULL CHECK (Qty > 0),\n  \
         Price decimal(12,2) NOT NULL, CONSTRAINT PK_Line PRIMARY KEY (OrdId, LineNo));\nGO\n\
         CREATE INDEX IX_Line_Item ON dbo.Line (ItemId);\nGO\n";
    let read = |extra: &str| {
        format!(
            "CREATE PROCEDURE dbo.look @ord int, @line int, @q int OUTPUT, @n int OUTPUT, @i int OUTPUT\nAS\nBEGIN\n  \
             SET XACT_ABORT ON;\n  SET NOCOUNT ON;\n  \
             SELECT @q = l.Qty FROM dbo.Line l JOIN dbo.Item i ON i.Id = l.ItemId WHERE l.OrdId = @ord AND l.LineNo = @line;\n  \
             SELECT @n = COUNT(*) FROM dbo.Line WHERE OrdId = @ord;\n  \
             IF EXISTS (SELECT 1 FROM dbo.Item WHERE Id = @q)\n    SET @n = @n + 1;\n  \
             SET @i = (SELECT l.ItemId FROM dbo.Line AS l WHERE l.OrdId = @ord AND l.LineNo = @line);\n{extra}END\n"
        )
    };
    let run = |catalogs: &[&str]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_sqldiff"));
        c.args([
            "convert",
            &src.display().to_string(),
            "--to",
            "postgres",
            "--out",
        ])
        .arg(d.join("out"))
        .args(["--overwrite", "--isolated", "--proof", "report"])
        .args(catalogs)
        .env("SQLDIFF_CHECK", &real);
        let o = c.output().unwrap();
        assert!(d.join("out/certificates.json").exists(), "{o:?}");
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
    let why = |r: &serde_json::Value| -> Vec<String> {
        r["why"]
            .as_array()
            .map(|a| a.iter().map(|x| x.as_str().unwrap().to_string()).collect())
            .unwrap_or_default()
    };
    let confirmed = |r: &serde_json::Value| {
        assert_eq!(r["answer"], "confirmed", "{r}");
        assert_eq!(r["theorems"][0], "Sqldiff.Mirror.check_sound_c2a", "{r}");
        assert_eq!(r["statements"], "c3b", "{r}");
        assert!(
            r["assumptions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a == "ASM-TABLES-AS-DECLARED"),
            "{r}"
        );
    };
    // Declared in the tree.
    write(&src, "tables.sql", TABLES);
    write(&src, "look.sql", &read(""));
    confirmed(&run(&[]));
    // A write to a read-only table is refused.
    write(
        &src,
        "look.sql",
        &read("  UPDATE dbo.Line SET Qty = 2 WHERE OrdId = @ord AND LineNo = @line;\n"),
    );
    let r = run(&[]);
    assert_eq!(r["answer"], "unconfirmed", "{r}");
    assert!(
        why(&r).contains(&"stmt:update:table:read-only".to_string()),
        "{r}"
    );
    // A column outside the slice (decimal) is a name only.
    write(
        &src,
        "look.sql",
        &read("  IF EXISTS (SELECT 1 FROM dbo.Line WHERE Price > 0)\n    SET @n = 0;\n"),
    );
    let r = run(&[]);
    assert_eq!(r["answer"], "unconfirmed", "{r}");
    assert_eq!(why(&r), ["stmt:select:column:opaque"], "{r}");
    // A security policy's filter predicate changes what a read returns
    // (eval/queries Q18): the table stays out.
    write(
        &src,
        "tables.sql",
        &format!(
            "{TABLES}CREATE FUNCTION dbo.pred (@n varchar(20)) RETURNS TABLE WITH SCHEMABINDING AS\n  \
             RETURN SELECT 1 AS ok WHERE @n = USER_NAME();\nGO\n\
             CREATE SECURITY POLICY dbo.P ADD FILTER PREDICATE dbo.pred(Name) ON dbo.Item;\nGO\n"
        ),
    );
    write(&src, "look.sql", &read(""));
    let r = run(&[]);
    assert_eq!(r["answer"], "unconfirmed", "{r}");
    assert!(
        why(&r).contains(&"stmt:select:table:unknown".to_string()),
        "{r}"
    );
    // The same tables from base catalogs: the T-SQL DDL with `--catalog`,
    // the PostgreSQL DDL with `--catalog-b`, or converted when it is not
    // given.
    std::fs::remove_file(src.join("tables.sql")).unwrap();
    let cat = d.join("cat");
    std::fs::create_dir_all(&cat).unwrap();
    let a = write(&cat, "tsql.sql", TABLES);
    let b = write(
        &cat,
        "pg.sql",
        "CREATE TABLE item (id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,\n  \
         name varchar(20) NOT NULL, code char(4) NOT NULL, price numeric(12,2) NOT NULL,\n  \
         added timestamp NOT NULL DEFAULT CURRENT_TIMESTAMP);\n\
         CREATE TABLE line (ordid integer NOT NULL, lineno integer NOT NULL,\n  \
         itemid integer NOT NULL REFERENCES item (id), qty integer NOT NULL CHECK (qty > 0),\n  \
         price numeric(12,2) NOT NULL, PRIMARY KEY (ordid, lineno));\n\
         CREATE INDEX ix_line_item ON line (itemid);\n",
    );
    let r = run(&[]);
    assert_eq!(r["answer"], "unconfirmed", "without the catalogs: {r}");
    confirmed(&run(&["--catalog", &a, "--catalog-b", &b]));
    // Without `--catalog-b` the PostgreSQL side is the base catalog as
    // converted, without the definitions of suspect base objects
    // (ASM-BASE-SUSPECT): `dbo.Item`'s conversion is not proven, and
    // `dbo.Line` names it, so neither is sent ...
    let r = run(&["--catalog", &a]);
    assert_eq!(r["answer"], "unconfirmed", "{r}");
    assert!(
        why(&r).contains(&"stmt:select:table:unknown".to_string()),
        "{r}"
    );
    // ... while a base table whose conversion is not suspect is.
    let a1 = write(
        &cat,
        "tsql1.sql",
        "CREATE TABLE dbo.Line (OrdId int NOT NULL, LineNo int NOT NULL, ItemId int NOT NULL,
           Qty int NOT NULL CHECK (Qty > 0), Price decimal(12,2) NOT NULL,
           CONSTRAINT PK_Line PRIMARY KEY (OrdId, LineNo));
GO
",
    );
    write(
        &src,
        "look.sql",
        "CREATE PROCEDURE dbo.look @ord int, @line int, @q int OUTPUT, @n int OUTPUT
AS
BEGIN
           SET XACT_ABORT ON;
  SET NOCOUNT ON;
           SELECT @q = l.Qty FROM dbo.Line l WHERE l.OrdId = @ord AND l.LineNo = @line;
           SELECT @n = COUNT(*) FROM dbo.Line WHERE OrdId = @ord;
END
",
    );
    confirmed(&run(&["--catalog", &a1]));
    write(&src, "look.sql", &read(""));
    // A `--catalog-b` table of another shape (a column left out): no table.
    let b2 = write(
        &cat,
        "pg2.sql",
        &std::fs::read_to_string(&b)
            .unwrap()
            .replace(", qty integer NOT NULL CHECK (qty > 0)", ""),
    );
    let r = run(&["--catalog", &a, "--catalog-b", &b2]);
    assert_eq!(r["answer"], "unconfirmed", "{r}");
    assert!(
        why(&r).contains(&"stmt:select:table:unknown".to_string()),
        "{r}"
    );
}

/// C3d part 2, review end to end: each false confirmation the review found
/// is refused, with the base catalogs of the test above. A column named
/// `exclude` added by `ALTER TABLE` (F1), a generated column followed by a
/// constraint named `identity` (F2), a key `INITIALLY DEFERRED` (F3), DDL
/// run where no scan sees it (F4: `EXEC [sp_executesql]`, a `DO` block, a
/// quoted SQL function body called at the top level), a `pg_` table (F5)
/// and another schema's table of the same name (F7). An extended property
/// set by `EXEC` changes nothing.
#[test]
fn the_real_checker_refuses_what_the_review_found() {
    let Some(real) = std::env::var_os("SQLDIFF_REAL_CHECK") else {
        return;
    };
    let _turn = turn();
    let d = scratch("c3d2-review");
    let src = d.join("src");
    let cat = d.join("cat");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&cat).unwrap();
    const TABLES: &str = "CREATE TABLE dbo.Item (Id int IDENTITY(1,1) NOT NULL PRIMARY KEY,\n  \
         Name varchar(20) NOT NULL, Price decimal(12,2) NOT NULL);\nGO\n\
         CREATE TABLE dbo.Line (OrdId int NOT NULL, LineNo int NOT NULL,\n  \
         ItemId int NOT NULL REFERENCES dbo.Item (Id), Qty int NOT NULL,\n  \
         CONSTRAINT PK_Line PRIMARY KEY (OrdId, LineNo));\nGO\n";
    const PG: &str = "CREATE TABLE item (id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,\n  \
         name varchar(20) NOT NULL, price numeric(12,2) NOT NULL);\n\
         CREATE TABLE line (ordid integer NOT NULL, lineno integer NOT NULL,\n  \
         itemid integer NOT NULL REFERENCES item (id), qty integer NOT NULL,\n  \
         PRIMARY KEY (ordid, lineno));\n";
    write(
        &src,
        "look.sql",
        "CREATE PROCEDURE dbo.look @ord int, @line int, @q int OUTPUT, @n int OUTPUT\nAS\nBEGIN\n  \
         SET XACT_ABORT ON;\n  SET NOCOUNT ON;\n  \
         SELECT @q = l.Qty FROM dbo.Line l JOIN dbo.Item i ON i.Id = l.ItemId \
         WHERE l.OrdId = @ord AND l.LineNo = @line;\n  \
         SELECT @n = COUNT(*) FROM dbo.Line WHERE OrdId = @ord;\nEND\n",
    );
    let run = |a: &str, b: &str| {
        let a = write(&cat, "tsql.sql", a);
        let b = write(&cat, "pg.sql", b);
        let o = Command::new(env!("CARGO_BIN_EXE_sqldiff"))
            .args([
                "convert",
                &src.display().to_string(),
                "--to",
                "postgres",
                "--out",
            ])
            .arg(d.join("out"))
            .args(["--overwrite", "--isolated", "--proof", "report"])
            .args(["--catalog", &a, "--catalog-b", &b])
            .env("SQLDIFF_CHECK", &real)
            .output()
            .unwrap();
        assert!(d.join("out/certificates.json").exists(), "{o:?}");
        let c: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(d.join("out/certificates.json")).unwrap(),
        )
        .unwrap();
        let r = c["files"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|f| f["routines"].as_array().unwrap().clone())
            .find(|r| r["routine"] == "dbo.look")
            .unwrap();
        let why: Vec<String> = r["why"]
            .as_array()
            .map(|a| a.iter().map(|x| x.as_str().unwrap().to_string()).collect())
            .unwrap_or_default();
        (r["answer"].as_str().unwrap().to_string(), why)
    };
    let refused = |a: &str, b: &str, code: &str| {
        let (answer, why) = run(a, b);
        assert_eq!(answer, "unconfirmed", "{a}\n{b}");
        assert!(
            why.iter().any(|c| c == code),
            "{code} not in {why:?}: {a}\n{b}"
        );
    };
    // The pair is confirmed as it stands, and with an extended property.
    assert_eq!(run(TABLES, PG).0, "confirmed");
    let prop = format!(
        "{TABLES}EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = N'x', \
         @level0type = N'SCHEMA', @level0name = 'dbo';\nGO\n"
    );
    assert_eq!(run(&prop, PG).0, "confirmed");
    let unknown = "stmt:select:table:unknown";
    // F1: PostgreSQL `ADD exclude integer` adds a column.
    refused(
        TABLES,
        &format!("{PG}ALTER TABLE line ADD exclude integer;\n"),
        unknown,
    );
    // F2: a virtual generated column, not an identity.
    refused(
        TABLES,
        &PG.replace(
            "qty integer NOT NULL",
            "qty integer GENERATED ALWAYS AS (1 / (ordid - ordid)) CONSTRAINT identity CHECK (true)",
        ),
        "stmt:select:column:opaque",
    );
    // F3: a key that is initially deferred may hold two equal keys.
    refused(
        TABLES,
        &PG.replace(
            "PRIMARY KEY (ordid, lineno)",
            "PRIMARY KEY (ordid, lineno) INITIALLY DEFERRED",
        ),
        "stmt:select:many-rows",
    );
    // F4: DDL no scan sees.
    refused(
        &format!(
            "{TABLES}DECLARE @s nvarchar(100) = N'ALTER TABLE dbo.' + NCHAR(76) + N'ine DROP COLUMN Qty';\n\
             EXEC [sp_executesql] @s;\nGO\n"
        ),
        PG,
        unknown,
    );
    refused(
        TABLES,
        &format!("{PG}DO $$ BEGIN EXECUTE 'CREATE POLICY p ON ' || chr(108) || 'ine USING (false)'; END $$;\n"),
        unknown,
    );
    refused(
        TABLES,
        &format!(
            "{PG}CREATE FUNCTION f() RETURNS void LANGUAGE sql AS \
             'ALTER TABLE line ENABLE ROW LEVEL SECURITY';\nSELECT f();\n"
        ),
        unknown,
    );
    // F7: another schema's table of the same name.
    refused(
        TABLES,
        &format!("{PG}CREATE SCHEMA s;\nCREATE TABLE s.line (ordid integer NOT NULL);\n"),
        unknown,
    );
    // F5: a `pg_` table is read from `pg_catalog` first; the same count of
    // `line` is confirmed.
    let count = |t: &str| {
        write(
            &src,
            "look.sql",
            &format!(
                "CREATE PROCEDURE dbo.look @ord int, @n int OUTPUT\nAS\nBEGIN\n  \
                 SET XACT_ABORT ON;\n  SET NOCOUNT ON;\n  \
                 SELECT @n = COUNT(*) FROM dbo.{t} WHERE OrdId = @ord;\nEND\n"
            ),
        );
    };
    count("Line");
    assert_eq!(run(TABLES, PG).0, "confirmed");
    count("pg_line");
    refused(
        &TABLES.replace("dbo.Line", "dbo.pg_line"),
        &PG.replace("TABLE line", "TABLE pg_line"),
        unknown,
    );
}

/// C3c end to end: a procedure over a table with a `varchar` and an
/// `nvarchar` column, two constant defaults and a non-unique index inserts
/// (the left-out columns get their defaults), updates the string columns
/// with values that fit and reads one into a variable it fits; it is
/// confirmed by `check_sound_c2a` and names `ASM-TSQL-ANSI-PADDING`. A
/// value that may not fit its column, and a UNIQUE index, are refused.
#[test]
fn the_real_checker_confirms_a_table_with_strings_defaults_and_an_index() {
    let Some(real) = std::env::var_os("SQLDIFF_REAL_CHECK") else {
        return;
    };
    let _turn = turn();
    let d = scratch("c3c-real");
    let src = d.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let ddl = |index: &str| {
        format!(
            "CREATE TABLE dbo.Cust (Id int NOT NULL, Name varchar(20) NOT NULL, \
             City nvarchar(30) NULL CONSTRAINT DF_Cust_City DEFAULT 'Paris', \
             Qty int NOT NULL CONSTRAINT DF_Cust_Qty DEFAULT ((0)), \
             CONSTRAINT PK_Cust PRIMARY KEY (Id));\nGO\n{index}\nGO\n"
        )
    };
    let proc = |set_name: &str| {
        format!(
            "CREATE PROCEDURE dbo.addc @id int, @c nvarchar(30) OUTPUT\nAS\nBEGIN\n  SET XACT_ABORT ON;\n  SET NOCOUNT ON;\n  DECLARE @n varchar(20) = 'abc';\n  INSERT INTO dbo.Cust (Id, Name) VALUES (@id, @n);\n  UPDATE dbo.Cust SET Name = {set_name}, City = 'Lyon' WHERE Id = @id;\n  SELECT @c = City FROM dbo.Cust WHERE Id = @id;\nEND\n"
        )
    };
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
            .find(|r| r["routine"] == "dbo.addc")
            .unwrap()
    };
    write(
        &src,
        "cust.sql",
        &ddl("CREATE INDEX IX_Cust_Name ON dbo.Cust (Name);"),
    );
    write(&src, "addc.sql", &proc("RTRIM(Name)"));
    let r = run();
    // The converter writes the table, its defaults and the index as the
    // checker's catalog takes them.
    let out = std::fs::read_to_string(d.join("out/cust.sql")).unwrap();
    for shape in [
        "Name varchar(20) NOT NULL",
        "City varchar(30) NULL CONSTRAINT DF_Cust_City DEFAULT 'Paris'",
        "CREATE INDEX IX_Cust_Name ON Cust (Name);",
    ] {
        assert!(out.contains(shape), "{shape}:\n{out}");
    }
    assert_eq!(r["answer"], "confirmed", "{r}");
    assert_eq!(r["theorems"][0], "Sqldiff.Mirror.check_sound_c2a", "{r}");
    assert_eq!(r["scalars"], "c2a", "{r}");
    assert_eq!(r["statements"], "c3b", "{r}");
    let asm: Vec<&str> = r["assumptions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    for a in [
        "ASM-TABLES-AS-DECLARED",
        "ASM-TSQL-ANSI-PADDING",
        "ASM-STATE-AFTER-ERROR",
        "ASM-TSQL-STRING-ARGS",
        "ASM-TSQL-ROWCOUNT-0",
    ] {
        assert!(asm.contains(&a), "{a}: {r}");
    }
    // 21 characters may not fit Name: SQL Server raises 2628 or truncates,
    // PostgreSQL raises 22001 (eval/tables T2).
    write(&src, "addc.sql", &proc("RTRIM(Name) + 'x'"));
    let r = run();
    assert_eq!(r["answer"], "unconfirmed", "{r}");
    // A UNIQUE index keeps the table out of the catalog.
    write(&src, "addc.sql", &proc("RTRIM(Name)"));
    write(
        &src,
        "cust.sql",
        &ddl("CREATE UNIQUE INDEX UX_Cust_Name ON dbo.Cust (Name);"),
    );
    let r = run();
    assert_eq!(r["answer"], "unconfirmed", "{r}");
}

/// C2c end to end: a procedure with a `bigint` and a `bit` variable, a
/// `date` column written from a literal and compared, `DATEADD(day, ...)`
/// and `DATEADD(month, ...)` on a date, is confirmed by `check_sound_c2a`
/// and names the two DATEADD facts; a date literal outside the ISO form
/// and a `DATEADD(hour, ...)` are refused.
#[test]
fn the_real_checker_confirms_typed_values() {
    let Some(real) = std::env::var_os("SQLDIFF_REAL_CHECK") else {
        return;
    };
    let _turn = turn();
    let d = scratch("c2c-real");
    let src = d.join("src");
    std::fs::create_dir_all(&src).unwrap();
    write(
        &src,
        "ev.sql",
        "CREATE TABLE dbo.Ev (Id int NOT NULL, D date NULL, B bit NOT NULL, N bigint NULL, \
         CONSTRAINT PK_Ev PRIMARY KEY (Id));\nGO\n",
    );
    let proc = |tail: &str| {
        format!(
            "CREATE PROCEDURE dbo.mark @id int, @big bigint, @flag bit, @when date OUTPUT\nAS\nBEGIN\n  \
             SET XACT_ABORT ON;\n  SET NOCOUNT ON;\n  DECLARE @b bit = 1;\n  DECLARE @n bigint;\n  \
             DECLARE @d date = '2024-01-31';\n  SET @n = @big + @id;\n  \
             SET @d = DATEADD(day, 1, @d);\n  SET @d = DATEADD(month, 1, @d);\n  \
             IF @flag = 1 SET @b = 0;\n  \
             IF @b = 1 AND @d < '2024-12-31' SET @when = @d;\n  \
             INSERT INTO dbo.Ev (Id, D, B, N) VALUES (@id, @d, @b, @n);\n  \
             UPDATE dbo.Ev SET D = DATEADD(day, 7, D), B = 1 WHERE Id = @id;\n  \
             SELECT @when = D FROM dbo.Ev WHERE Id = @id;\n{tail}END\n"
        )
    };
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
            .find(|r| r["routine"] == "dbo.mark")
            .unwrap()
    };
    write(&src, "mark.sql", &proc(""));
    let r = run();
    // The converter writes the forms the checker reads: boolean literals,
    // date arithmetic as `+ n` and `make_interval`, the typed columns.
    let out = std::fs::read_to_string(d.join("out/mark.sql")).unwrap();
    for shape in [
        "v_b := TRUE;",
        "IF p_flag = TRUE THEN",
        "v_d := (v_d + 1);",
        "v_d := (v_d + make_interval(months => 1))::date;",
        "IF v_b = TRUE AND v_d < '2024-12-31' THEN",
        "UPDATE Ev SET D = (D + 7), B = TRUE WHERE Id = p_id;",
    ] {
        assert!(out.contains(shape), "{shape}:\n{out}");
    }
    assert_eq!(r["answer"], "confirmed", "{r}");
    assert_eq!(r["theorems"][0], "Sqldiff.Mirror.check_sound_c2a", "{r}");
    assert_eq!(r["scalars"], "c2a", "{r}");
    assert_eq!(r["statements"], "c3b", "{r}");
    let asm: Vec<&str> = r["assumptions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    for a in [
        "ASM-TSQL-ARITH-ERRORS",
        "ASM-TSQL-ANSI-NULLS",
        "ASM-DATEADD-RANGE",
        "ASM-TABLES-AS-DECLARED",
        "ASM-TSQL-ROWCOUNT-0",
    ] {
        assert!(asm.contains(&a), "{a}: {r}");
    }
    // No year is added: the year-end fact is not named.
    assert!(!asm.contains(&"ASM-DATEADD-YEAR-END"), "{r}");
    // A date literal T-SQL reads by DATEFORMAT, or DATEADD(hour, ...) on a
    // date (which T-SQL itself refuses): outside the spec.
    write(&src, "mark.sql", &proc("  SET @d = '01/02/2024';\n"));
    let r = run();
    assert_eq!(r["answer"], "unconfirmed", "{r}");
    write(
        &src,
        "mark.sql",
        &proc("  SET @d = DATEADD(hour, 1, @d);\n"),
    );
    let r = run();
    assert_eq!(r["answer"], "unconfirmed", "{r}");
}

/// C2c review: the converter's calls are confirmed only when each argument
/// has exactly its callee's parameter type (an `int` into a `smallint`
/// parameter finds no procedure in PostgreSQL, 42883); `@b <> 1` against
/// `b <> TRUE` names `ASM-TSQL-ANSI-NULLS`; a nested `DATEADD`, which the
/// converter writes with `::date`, is confirmed.
#[test]
fn the_real_checker_holds_call_arguments_and_bit_comparisons() {
    let Some(real) = std::env::var_os("SQLDIFF_REAL_CHECK") else {
        return;
    };
    let _turn = turn();
    let d = scratch("c2c-review");
    let src = d.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let callee = |name: &str, ty: &str| {
        format!(
            "CREATE PROCEDURE dbo.{name} @x {ty} AS\nBEGIN\n  SET XACT_ABORT ON;\n  SET NOCOUNT ON;\n  \
             DECLARE @y {ty};\n  SET @y = @x;\nEND\nGO\n"
        )
    };
    write(
        &src,
        "c.sql",
        &format!(
            "{}{}{}CREATE PROCEDURE dbo.callers @i int, @b bit, @r int OUTPUT AS\nBEGIN\n  \
             SET XACT_ABORT ON;\n  SET NOCOUNT ON;\n  EXEC dbo.ci @i;\n  EXEC dbo.cb @b;\n  \
             EXEC dbo.cb 1;\n  SET @r = 0;\n  IF @b <> 1 SET @r = 1;\nEND\nGO\n\
             CREATE PROCEDURE dbo.narrow @i int AS\nBEGIN\n  SET XACT_ABORT ON;\n  SET NOCOUNT ON;\n  \
             EXEC dbo.cs @i;\nEND\nGO\n\
             CREATE PROCEDURE dbo.nd @id int, @d date, @w date, @r date OUTPUT, @i int OUTPUT AS\nBEGIN\n  \
             SET XACT_ABORT ON;\n  SET NOCOUNT ON;\n  SET @r = DATEADD(day, 1, DATEADD(month, @id, @d));\n  \
             SET @i = DATEDIFF(day, @w, DATEADD(month, @id, @d));\nEND\nGO\n",
            callee("cs", "smallint"),
            callee("cb", "bit"),
            callee("ci", "int")
        ),
    );
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
    let out = std::fs::read_to_string(d.join("out/c.sql")).unwrap();
    for shape in [
        "CALL cs(p_i);",
        "CALL cb(TRUE);",
        "IF p_b <> TRUE THEN",
        "(p_d + make_interval(months => p_id))::date + 1",
    ] {
        assert!(out.contains(shape), "{shape}:\n{out}");
    }
    let c: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(d.join("out/certificates.json")).unwrap())
            .unwrap();
    let routine = |name: &str| {
        c["files"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|f| f["routines"].as_array().unwrap().clone())
            .find(|r| r["routine"] == name)
            .unwrap()
    };
    let r = routine("dbo.narrow");
    assert_eq!(r["answer"], "unconfirmed", "{r}");
    assert_eq!(r["why"][0], "node:call:type", "{r}");
    let r = routine("dbo.callers");
    assert_eq!(r["answer"], "confirmed", "{r}");
    assert_eq!(r["theorems"][0], "Sqldiff.Mirror.check_sound_c2a", "{r}");
    let asm: Vec<&str> = r["assumptions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    for a in ["ASM-TSQL-ANSI-NULLS", "ASM-TSQL-STRING-ARGS"] {
        assert!(asm.contains(&a), "{a}: {r}");
    }
    for name in ["dbo.cs", "dbo.cb", "dbo.ci", "dbo.nd"] {
        let r = routine(name);
        assert_eq!(r["answer"], "confirmed", "{r}");
    }
}

/// Re-review of C2c end to end. A `time` local is `time(7)` in SQL Server
/// and `time(6)` in PostgreSQL: `.1234567 < .1234568` is 1 against 0, so
/// a type outside the slice corresponds to nothing and a string stores
/// into it on neither side. A string argument must fit the callee's
/// declared length: `dbo.q @s varchar(3)` sees `abc` of a `varchar(10)`
/// holding `abcdef` in SQL Server, the whole value in PostgreSQL. An `int`
/// parameter names ASM-TSQL-STRING-ARGS (an outside caller's `1.5`).
#[test]
fn the_real_checker_refuses_time_locals_and_long_string_arguments() {
    let Some(real) = std::env::var_os("SQLDIFF_REAL_CHECK") else {
        return;
    };
    let _turn = turn();
    let d = scratch("c2c-rereview");
    let src = d.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let head = "AS\nBEGIN\n  SET XACT_ABORT ON;\n  SET NOCOUNT ON;\n";
    write(
        &src,
        "t.sql",
        &format!(
            "CREATE PROCEDURE dbo.tm @r int OUTPUT {head}  DECLARE @a time;\n  DECLARE @b time;\n  \
             SET @a = '12:00:00.1234567';\n  SET @b = '12:00:00.1234568';\n  \
             IF @a < @b SET @r = 1; ELSE SET @r = 0;\nEND\nGO\n\
             CREATE PROCEDURE dbo.tn @r int OUTPUT {head}  DECLARE @a time;\n  DECLARE @b time;\n  \
             IF @a < @b SET @r = 1; ELSE SET @r = 0;\nEND\nGO\n\
             CREATE PROCEDURE dbo.q @s varchar(3), @r int OUTPUT {head}  SET @r = LEN(@s);\nEND\nGO\n\
             CREATE PROCEDURE dbo.pc @r int OUTPUT {head}  DECLARE @v varchar(10);\n  \
             SET @v = 'abcdef';\n  EXEC dbo.q @v, @r OUTPUT;\nEND\nGO\n\
             CREATE PROCEDURE dbo.pl @r int OUTPUT {head}  EXEC dbo.q 'abcd', @r OUTPUT;\nEND\nGO\n\
             CREATE PROCEDURE dbo.p3 @r int OUTPUT {head}  DECLARE @v varchar(3);\n  \
             SET @v = 'abc';\n  EXEC dbo.q @v, @r OUTPUT;\nEND\nGO\n"
        ),
    );
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
    let out = std::fs::read_to_string(d.join("out/t.sql")).unwrap();
    for shape in ["v_a time;", "CALL q(v_v, p_r);", "p_s varchar(3)"] {
        assert!(out.contains(shape), "{shape}:\n{out}");
    }
    let c: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(d.join("out/certificates.json")).unwrap())
            .unwrap();
    let routine = |name: &str| {
        c["files"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|f| f["routines"].as_array().unwrap().clone())
            .find(|r| r["routine"] == name)
            .unwrap()
    };
    let r = routine("dbo.tm");
    assert_eq!(r["answer"], "unconfirmed", "{r}");
    assert_eq!(r["why"][0], "store:assign:time", "{r}");
    let r = routine("dbo.tn");
    assert_eq!(r["answer"], "unconfirmed", "{r}");
    for name in ["dbo.pc", "dbo.pl"] {
        let r = routine(name);
        assert_eq!(r["answer"], "unconfirmed", "{r}");
        assert_eq!(r["why"][0], "node:call:type", "{r}");
    }
    for name in ["dbo.q", "dbo.p3"] {
        let r = routine(name);
        assert_eq!(r["answer"], "confirmed", "{r}");
        let asm: Vec<&str> = r["assumptions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        assert!(asm.contains(&"ASM-TSQL-STRING-ARGS"), "{r}");
    }
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
        "ASM-TSQL-ROWCOUNT-0",
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
    assert!(good.contains("proof: 2 of 2 routines confirmed"), "{good}");
    assert!(!good.contains("ASM-CALLEE-CONFIRMED"), "{good}");
    let bad = run("CREATE PROCEDURE dbo.q @n int OUTPUT\nAS\nBEGIN\n  SAVE TRANSACTION s;\nEND\n");
    assert!(bad.contains("ASM-CALLEE-CONFIRMED:dbo.q"), "{bad}");
}
