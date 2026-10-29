//! Target versions are validated against `profiles/lifecycle.toml`, and a
//! run that analyses a version outside vendor support says so on stderr.

use std::path::PathBuf;
use std::process::{Command, Output};

fn fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
        .display()
        .to_string()
}

fn diff(version: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sqldiff"))
        .args([
            "diff",
            &fixture("movestock.tsql.sql"),
            &fixture("movestock_reformatted.tsql.sql"),
            "--dialect-version",
            version,
            "-f",
            "json",
        ])
        .output()
        .unwrap()
}

#[test]
fn an_unknown_version_is_an_error_that_lists_the_known_ones() {
    let out = diff("tsql=2012");
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unknown T-SQL version `2012`"), "{err}");
    assert!(err.contains("2022"), "{err}");
}

#[test]
fn the_report_carries_the_canonical_target() {
    let out = diff("tsql=2019/130");
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["a"]["dialect_version"], "2019/130");
    // The native level is not spelled out.
    let v: serde_json::Value = serde_json::from_slice(&diff("tsql=2019/150").stdout).unwrap();
    assert_eq!(v["a"]["dialect_version"], "2019");
}

#[test]
fn a_version_past_vendor_support_gets_a_note() {
    // SQL Server 2016 left extended support on 2026-07-14.
    let out = diff("tsql=2016");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("sqldiff: note: T-SQL 2016 is"), "{err}");
    // SQL Server 2022 is supported until 2033-01-11; this assertion starts
    // failing 90 days before that, when the note is due.
    let out = diff("tsql=2022");
    assert!(!String::from_utf8_lossy(&out.stderr).contains("note:"));
}
