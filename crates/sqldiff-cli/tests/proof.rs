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
