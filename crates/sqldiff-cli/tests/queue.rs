//! `queue`, `assumptions`, `comment` and `open` mirror the MCP review tools.

use std::path::PathBuf;
use std::process::Command;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn sqldiff(args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sqldiff"))
        .args(args)
        .arg("--isolated")
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
    )
}

fn f(name: &str) -> String {
    fixtures().join(name).display().to_string()
}

#[test]
fn queue_ranks_and_flips() {
    let dir = fixtures().display().to_string();
    let (code, out) = sqldiff(&["queue", &dir, "-f", "json"]);
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let pairs = v["pairs"].as_array().unwrap();
    assert_eq!(pairs.len(), 3);
    assert_eq!(pairs[0]["highest_severity"], "critical");

    let (_, out) = sqldiff(&[
        "queue",
        &dir,
        "--verdict",
        "unknown",
        "--deny",
        "ASM-TSQL-CALLER-XACT-ABORT-OFF",
        "-f",
        "json",
    ]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let pairs = v["pairs"].as_array().unwrap();
    assert_eq!(pairs.len(), 2);
    assert!(pairs.iter().any(|p| p["b"] == "movestock.pg.sql"));
}

#[test]
fn ledger_comment_and_link() {
    let (a, b) = (f("movestock.tsql.sql"), f("movestock.pg.sql"));
    let (_, out) = sqldiff(&["assumptions", &a, &b, "-f", "json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["assumptions"][0]["id"], "ASM-TSQL-CALLER-XACT-ABORT-OFF");
    assert_eq!(v["assumptions"][0]["flips_verdict"], true);
    assert_eq!(v["assumptions"][0]["reruns"], true);

    let (code, one) = sqldiff(&["comment", &a, &b]);
    let (_, two) = sqldiff(&["comment", &a, &b]);
    assert_eq!(code, 0);
    assert_eq!(one, two);
    assert!(
        one.starts_with("SQLDiff found 3 issues to review."),
        "{one}"
    );
    assert!(one.contains("Please confirm the new behaviour is intended."));

    let (_, denied) = sqldiff(&["comment", &a, &b, "--deny", "ASM-CALLER-CHECKS-RC"]);
    assert!(denied.contains("relies on what review says does not hold"));

    let (_, json) = sqldiff(&["diff", &a, &b, "-f", "json"]);
    let r: serde_json::Value = serde_json::from_str(&json).unwrap();
    let fp = r["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["rule"] == "error.contract")
        .unwrap()["fingerprint"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, link) = sqldiff(&["open", &a, &b, "--finding", &fp, "--gui-url", "http://h:9/"]);
    assert_eq!(code, 0);
    assert!(link.starts_with("http://h:9/?a="), "{link}");
    assert!(link.trim_end().ends_with(&format!("#f={fp}")));
    let (code, _) = sqldiff(&["open", &a, &b, "--finding", "nope"]);
    assert_eq!(code, 2);
}
