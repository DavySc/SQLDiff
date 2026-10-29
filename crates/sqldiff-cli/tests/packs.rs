//! Packs and layered config in `diff`, `check` and `tree`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sqldiff-packs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

const PACK: &str = r#"[pack]
name = "t"
version = "1"

[[pairing.rules]]
name = "strip-usp"
strip_prefix = ["usp_"]

[[limits]]
id = "update"
message = "UPDATE statement"
pattern = "(?i)\\bupdate\\b"

[[overrides]]
rule = "limit.*"
severity = "high"
"#;

fn pack(dir: &Path) -> PathBuf {
    let p = dir.join("pack.toml");
    std::fs::write(&p, PACK).unwrap();
    p
}

fn sqldiff(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sqldiff"))
        .args(args)
        .arg("--isolated")
        .env(
            "SQLDIFF_DATA_DIR",
            std::env::temp_dir().join(format!("sqldiff-packs-data-{}", std::process::id())),
        )
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

fn f(name: &str) -> String {
    fixtures().join(name).display().to_string()
}

#[test]
fn diff_infers_dialects_from_paths() {
    let out = sqldiff(&[
        "diff",
        &f("movestock.tsql.sql"),
        &f("movestock.db2i.sql"),
        "-f",
        "json",
    ]);
    let v = json(&out);
    assert_eq!(v["a"]["dialect"], "tsql");
    assert_eq!(v["b"]["dialect"], "db2i");
}

#[test]
fn check_applies_pack_limits_and_overrides() {
    let dir = scratch("check");
    let p = pack(&dir);
    let out = sqldiff(&[
        "check",
        &f("movestock.tsql.sql"),
        "--pack",
        p.to_str().unwrap(),
        "--dialect-version",
        "tsql=2019",
        "-f",
        "json",
    ]);
    let v = json(&out);
    assert_eq!(v["a"]["dialect_version"], "2019");
    let limits: Vec<_> = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["rule"] == "limit.update")
        .collect();
    assert!(!limits.is_empty());
    assert!(limits.iter().all(|f| f["severity"] == "high"));
}

#[test]
fn diff_without_pack_has_no_limits() {
    let out = sqldiff(&[
        "diff",
        &f("movestock.tsql.sql"),
        &f("movestock_reformatted.tsql.sql"),
        "-f",
        "json",
    ]);
    let v = json(&out);
    assert!(v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .all(|f| f["class"] != "limit"));
}

#[test]
fn tree_pairs_by_pack_rules() {
    let dir = scratch("tree");
    let (a, b) = (dir.join("a"), dir.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::copy(
        fixtures().join("movestock.tsql.sql"),
        a.join("usp_move_stock.sql"),
    )
    .unwrap();
    std::fs::copy(
        fixtures().join("movestock.pg.sql"),
        b.join("move_stock.sql"),
    )
    .unwrap();
    let args = |extra: &[&str]| {
        let mut v = vec![
            "tree",
            a.to_str().unwrap(),
            b.to_str().unwrap(),
            "--dialect-a",
            "tsql",
            "--dialect-b",
            "postgres",
            "-f",
            "json",
        ];
        v.extend_from_slice(extra);
        v.into_iter().map(String::from).collect::<Vec<_>>()
    };
    let run = |extra: &[&str]| {
        let a = args(extra);
        json(&sqldiff(&a.iter().map(String::as_str).collect::<Vec<_>>()))
    };

    // Built-in rule: `usp_` is kept, so nothing pairs.
    let v = run(&[]);
    assert_eq!(v["pairs"].as_array().unwrap().len(), 0);
    assert_eq!(v["only_a"].as_array().unwrap().len(), 1);

    let p = pack(&dir);
    let v = run(&["--pack", p.to_str().unwrap()]);
    assert_eq!(v["pairs"].as_array().unwrap().len(), 1);
    assert!(v["only_a"].as_array().unwrap().is_empty());
    // `--pack` loads a private pack, named by its file.
    assert!(v["provenance"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["kind"] == "private_pack" && l["name"] == p.to_str().unwrap()));
    assert!(v["pairs"][0]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["rule"] == "limit.update"));

    // Two A objects with one key: reported, not silently overwritten.
    std::fs::copy(
        fixtures().join("movestock.tsql.sql"),
        a.join("move_stock.sql"),
    )
    .unwrap();
    let out = sqldiff(
        &args(&["--pack", p.to_str().unwrap()])
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    let v = json(&out);
    assert_eq!(v["ambiguous"].as_array().unwrap().len(), 1);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn template_and_diff_agree_on_known_parameters() {
    let dir = scratch("template-known");
    std::fs::write(
        dir.join("pack.toml"),
        "[pack]\nname = \"t\"\nversion = \"1\"\n\n[template]\nopen = \"[\"\nclose = \"]\"\n\
         name_pattern = \"[A-Z][A-Z0-9_]*\"\nknown_parameters = [\"SCHEMA\"]\n",
    )
    .unwrap();
    let tpl = dir.join("t.tsql.sql");
    std::fs::write(&tpl, "select x from [SCHEMA].[TAB];\n").unwrap();
    let pack = dir.join("pack.toml");
    // `[SCHEMA]` comes from the pack, `[TAB]` from --param: neither is unresolved.
    let out = sqldiff(&[
        "template",
        tpl.to_str().unwrap(),
        "-d",
        "tsql",
        "--pack",
        pack.to_str().unwrap(),
        "--param",
        "TAB=t",
        "--known-only",
    ]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("unresolved"), "{stdout}");
    assert!(stdout.contains("placeholder SCHEMA"), "{stdout}");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
