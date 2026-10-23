//! `queue`, `assumptions`, `comment` and `open` mirror the MCP review tools.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

/// A fresh directory per call: tests run in parallel, so no two may share one.
fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("sqldiff-queue-{name}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn sqldiff(args: &[&str]) -> (i32, String) {
    sqldiff_in(&scratch("data"), args)
}

/// Runs with its own data directory (review store and cache).
fn sqldiff_in(data: &std::path::Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sqldiff"))
        .args(args)
        .arg("--isolated")
        .env("SQLDIFF_DATA_DIR", data)
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
    // T-SQL ↔ Db2 diverges with XACT_ABORT ON as well, so only the PG pair
    // rests on the assumption.
    assert_eq!(pairs.len(), 1);
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
    // Equivalence adds an `equiv.unproven` finding for the UPDATE (ItemId vs item_id).
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

#[test]
fn exit_code_covers_pairs_beyond_the_limit() {
    let dir = fixtures().display().to_string();
    let (code, out) = sqldiff(&["queue", &dir, "--limit", "0"]);
    assert_eq!(code, 1, "{out}");
}

#[test]
fn accepted_findings_are_suppressed_and_unknown_pairs_still_need_review() {
    let data = scratch("accept");
    let dir = fixtures().display().to_string();
    let (a, b) = (f("movestock.tsql.sql"), f("movestock.pg.sql"));
    let (_, json) = sqldiff(&["diff", &a, &b, "-f", "json"]);
    let r: serde_json::Value = serde_json::from_str(&json).unwrap();
    let fps: Vec<String> = r["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["fingerprint"].as_str().unwrap().to_string())
        .collect();
    for fp in &fps {
        let (code, _) = sqldiff_in(&data, &["review", "accept", fp, "--pair", &a, &b]);
        assert_eq!(code, 0);
    }
    let (_, out) = sqldiff_in(&data, &["queue", &dir, "-f", "json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let pairs = v["pairs"].as_array().unwrap();
    let pg = pairs
        .iter()
        .find(|p| p["a"] == "movestock.tsql.sql" && p["b"] == "movestock.pg.sql")
        .unwrap();
    assert_eq!(pg["findings"], 0);
    assert_eq!(pg["review"]["accepted"], fps.len());
    // No finding left, but the verdict is still unknown: it needs review.
    assert_eq!(pg["verdict"], "unknown");
    assert_eq!(pg["needs_review"], true);
    let (_, out) = sqldiff_in(
        &data,
        &["queue", &dir, "--review", "reviewed", "-f", "json"],
    );
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v["pairs"].as_array().unwrap().is_empty(), "{v}");
}

#[test]
fn limit_findings_give_the_same_verdict_in_diff_and_assumptions() {
    let dir = scratch("limits");
    let pack = dir.join("pack.toml");
    std::fs::write(
        &pack,
        "[pack]\nname = \"t\"\nversion = \"1\"\n\n[[limits]]\nid = \"update\"\nmessage = \"UPDATE statement\"\npattern = \"(?i)\\\\bupdate\\\\b\"\n",
    )
    .unwrap();
    let pack = pack.display().to_string();
    let (a, b) = (f("movestock.tsql.sql"), f("movestock_reformatted.tsql.sql"));
    let (code, out) = sqldiff(&["diff", &a, &b, "--pack", &pack, "-f", "json"]);
    let d: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(d["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["class"] == "limit"));
    assert_eq!(d["verdict"], "unknown");
    assert_eq!(code, 1);
    let (_, out) = sqldiff(&["assumptions", &a, &b, "--pack", &pack, "-f", "json"]);
    let l: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(l["verdict"], d["verdict"]);
}

#[test]
fn open_quotes_paths_with_spaces() {
    let dir = scratch("open space");
    for n in ["movestock.tsql.sql", "movestock.pg.sql"] {
        std::fs::copy(fixtures().join(n), dir.join(n)).unwrap();
    }
    let a = dir.join("movestock.tsql.sql").display().to_string();
    let b = dir.join("movestock.pg.sql").display().to_string();
    let out = Command::new(env!("CARGO_BIN_EXE_sqldiff"))
        .args(["open", &a, &b, "--isolated"])
        .env("SQLDIFF_DATA_DIR", scratch("open-data"))
        .output()
        .unwrap();
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains(&format!("'{a}' '{b}'")), "{err}");
}

// Regressions from the review of PR #23.

#[test]
fn assumptions_runs_the_diff_pipeline() {
    let dir = scratch("asm-pipeline");
    let pack = dir.join("pack.toml");
    std::fs::write(
        &pack,
        "[pack]\nname = \"t\"\nversion = \"1\"\n\n[[author_markers]]\nkind = \"assumption\"\npattern = \"--\\\\s*ASSUMES:\\\\s*(.*)\"\n",
    )
    .unwrap();
    let a = dir.join("a.tsql.sql");
    let b = dir.join("b.tsql.sql");
    let body = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  -- ASSUMES: stock rows exist\n  UPDATE dbo.stock SET qty = 0;\nEND\n";
    std::fs::write(&a, body).unwrap();
    std::fs::write(&b, body).unwrap();
    let (pack, a, b) = (
        pack.display().to_string(),
        a.display().to_string(),
        b.display().to_string(),
    );
    let (_, out) = sqldiff(&["diff", &a, &b, "--pack", &pack, "-f", "json"]);
    let d: serde_json::Value = serde_json::from_str(&out).unwrap();
    let asm = d["assumptions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["source"] == "author")
        .unwrap_or_else(|| panic!("{d}"))["id"]
        .as_str()
        .unwrap()
        .to_string();
    // The author's assumption is in the ledger, with the same verdict.
    let (_, out) = sqldiff(&["assumptions", &a, &b, "--pack", &pack, "-f", "json"]);
    let l: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(l["verdict"], d["verdict"]);
    assert!(
        l["assumptions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["id"] == asm.as_str()),
        "{l}"
    );
    // Denying it reaches the marker's assumption.
    let (_, out) = sqldiff(&[
        "diff", &a, &b, "--pack", &pack, "--deny", &asm, "-f", "json",
    ]);
    let r: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(r["verdict"], "unknown", "{r}");
}
