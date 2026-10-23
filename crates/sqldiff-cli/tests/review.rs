//! `sqldiff review`, suppression, `--redact` and the cache, end to end.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
        .display()
        .to_string()
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sqldiff-cli-review-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn sqldiff(data: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sqldiff"))
        .args(args)
        .arg("--isolated")
        .arg("--data-dir")
        .arg(data)
        .output()
        .unwrap()
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

#[test]
fn accept_suppresses_until_cleared() {
    let data = scratch("accept");
    let (a, b) = (fixture("movestock.tsql.sql"), fixture("movestock.pg.sql"));
    let r = json(&sqldiff(&data, &["diff", &a, &b, "-f", "json"]));
    let fp = r["findings"][0]["fingerprint"]
        .as_str()
        .unwrap()
        .to_string();
    let n = r["findings"].as_array().unwrap().len();

    let out = sqldiff(
        &data,
        &[
            "review", "accept", &fp, "--pair", &a, &b, "-c", "intended", "--author", "dana",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let r = json(&sqldiff(&data, &["diff", &a, &b, "-f", "json"]));
    assert_eq!(r["findings"].as_array().unwrap().len(), n - 1);
    assert_eq!(r["suppressed"][0]["finding"]["fingerprint"], fp.as_str());
    assert_eq!(r["suppressed"][0]["kind"], "review");

    let r = json(&sqldiff(
        &data,
        &["diff", &a, &b, "-f", "json", "--no-suppress"],
    ));
    assert_eq!(r["findings"].as_array().unwrap().len(), n);

    let list = json(&sqldiff(&data, &["review", "list", "-f", "json"]));
    assert_eq!(list[0]["fingerprint"], fp.as_str());
    assert_eq!(list[0]["decision"]["author"], "dana");

    let snap = list[0]["snapshot"].as_str().unwrap().to_string();
    assert!(
        sqldiff(&data, &["review", "clear", &fp, "--snapshot", &snap])
            .status
            .success()
    );
    let r = json(&sqldiff(&data, &["diff", &a, &b, "-f", "json"]));
    assert_eq!(r["findings"].as_array().unwrap().len(), n);
}

#[test]
fn accepting_everything_never_yields_equal_exit() {
    let data = scratch("gate");
    let (a, b) = (
        fixture("catch_rethrow.tsql.sql"),
        fixture("movestock.tsql.sql"),
    );
    let r = json(&sqldiff(&data, &["diff", &a, &b, "-f", "json"]));
    for f in r["findings"].as_array().unwrap() {
        let fp = f["fingerprint"].as_str().unwrap();
        assert!(sqldiff(&data, &["review", "accept", fp]).status.success());
    }
    let out = sqldiff(&data, &["diff", &a, &b, "-f", "json"]);
    let r = json(&out);
    assert!(r["findings"].as_array().unwrap().is_empty());
    assert_eq!(r["verdict"], "different", "the computed verdict is kept");
    // Open verdict is at best "equal under assumptions" (exit 0), or unknown.
    let code = out.status.code().unwrap();
    assert!(code == 0 || code == 1);
}

#[test]
fn redacted_output_hides_names_in_every_format() {
    let data = scratch("redact");
    let (a, b) = (fixture("movestock.tsql.sql"), fixture("movestock.pg.sql"));
    for fmt in ["text", "json", "sarif"] {
        let out = sqldiff(&data, &["diff", &a, &b, "-f", fmt, "--redact"]);
        let s = String::from_utf8_lossy(&out.stdout).to_lowercase();
        assert!(!s.is_empty());
        for term in ["movestock", "move_stock", "fixtures"] {
            assert!(!s.contains(term), "{fmt}: {term} in {s}");
        }
    }
    let t = scratch("tree");
    for (side, f) in [("a", "movestock.tsql.sql"), ("b", "movestock.pg.sql")] {
        std::fs::create_dir_all(t.join(side)).unwrap();
        std::fs::copy(fixture(f), t.join(side).join("movestock.sql")).unwrap();
    }
    std::fs::write(t.join("b").join("onlyhere_secretname.sql"), "SELECT 1;\n").unwrap();
    let out = sqldiff(
        &data,
        &[
            "tree",
            t.join("a").to_str().unwrap(),
            t.join("b").to_str().unwrap(),
            "--dialect-a",
            "tsql",
            "--dialect-b",
            "postgres",
            "-f",
            "json",
            "--redact",
        ],
    );
    let s = String::from_utf8_lossy(&out.stdout).to_lowercase();
    assert!(
        s.contains("\"pairs\""),
        "{s}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for term in ["movestock", "secretname", "sqldiff-cli-review"] {
        assert!(!s.contains(term), "{term} in {s}");
    }
}

#[test]
fn cache_is_written_and_can_be_cleared() {
    let data = scratch("cache");
    let (a, b) = (fixture("movestock.tsql.sql"), fixture("movestock.pg.sql"));
    let first = sqldiff(&data, &["diff", &a, &b, "-f", "json"]);
    assert!(data.join("cache/report").is_dir());
    let second = sqldiff(&data, &["diff", &a, &b, "-f", "json"]);
    assert_eq!(first.stdout, second.stdout);
    let nocache = scratch("nocache");
    sqldiff(&nocache, &["diff", &a, &b, "--no-cache"]);
    assert!(!nocache.join("cache").exists());
    assert!(sqldiff(&data, &["review", "clear-cache"]).status.success());
    assert!(!data.join("cache").exists());
}
