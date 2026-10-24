//! `sqldiff changes` (the pull request workflow) and same-name pairing in
//! `sqldiff tree`, end to end on scratch git repositories.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sqldiff-cli-changes-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn put(repo: &Path, path: &str, text: impl AsRef<[u8]>) {
    let p = repo.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn commit(repo: &Path, msg: &str) {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "--allow-empty", "-m", msg]);
}

fn new_repo(name: &str) -> (PathBuf, PathBuf) {
    let root = scratch(name);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    (root, repo)
}

/// Runs `sqldiff changes` with an isolated config and a scratch data dir.
fn changes(root: &Path, repo: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sqldiff"))
        .arg("changes")
        .arg("--repo")
        .arg(repo)
        .args(["--isolated", "--data-dir"])
        .arg(root.join("data"))
        .args(extra)
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

const MOVE: &str = "CREATE PROCEDURE dbo.MoveStock @id int AS
BEGIN
    BEGIN TRANSACTION;
    UPDATE dbo.Stock SET qty = qty - 1 WHERE id = @id;
    INSERT INTO dbo.StockLog (id) VALUES (@id);
    COMMIT;
END;
";

const CITIES_TABLE: &str = "CREATE TABLE Application.Cities (
    CityID int NOT NULL,
    CityName nvarchar(50) NOT NULL
);
";

const CITIES_VIEW: &str = "CREATE VIEW WebApi.Cities
AS
SELECT c.CityID, c.CityName
FROM Application.Cities AS c;
";

const MOVER_OLD: &str = "CREATE PROCEDURE dbo.Mover AS
BEGIN
    DELETE FROM dbo.Queue WHERE done = 1;
END;
";

const MOVER_NEW: &str = "-- Rewritten: archive first, then delete.
CREATE PROCEDURE dbo.Mover
AS
BEGIN
    SET NOCOUNT ON;
    INSERT INTO dbo.QueueArchive (id, payload)
    SELECT id, payload FROM dbo.Queue WHERE done = 1 AND archived = 0;
    UPDATE dbo.Queue SET archived = 1 WHERE done = 1;
    DELETE FROM dbo.Queue WHERE done = 1 AND archived = 1;
END;
";

/// A base commit and a head commit that exercise every kind of change.
fn scenario(name: &str) -> (PathBuf, PathBuf) {
    let (root, repo) = new_repo(name);
    put(&repo, "tsql/MoveStock.sql", MOVE);
    put(&repo, "tsql/Application/Tables/Cities.sql", CITIES_TABLE);
    put(&repo, "tsql/WebApi/Views/Cities.sql", CITIES_VIEW);
    put(
        &repo,
        "tsql/Report.sql",
        "CREATE PROCEDURE dbo.Report AS\nBEGIN\n    SELECT id, qty FROM dbo.Stock WHERE qty > 0;\nEND;\n",
    );
    put(&repo, "tsql/old/Mover.sql", MOVER_OLD);
    put(
        &repo,
        "tsql/Gone.sql",
        "CREATE PROCEDURE dbo.Gone AS SELECT 1;\n",
    );
    // Not valid text: reading it would make it "unreadable". It never
    // changes, so it must never be read.
    put(&repo, "tsql/Unchanged.sql", [0xC3u8, 0x28, 0x0A]);
    put(&repo, "README.md", "docs\n");
    commit(&repo, "base");
    git(&repo, &["branch", "base"]);

    // Drop the COMMIT: a real behaviour change.
    put(
        &repo,
        "tsql/MoveStock.sql",
        MOVE.replace("    COMMIT;\n", ""),
    );
    // Same file name in two schema folders, both changed.
    put(
        &repo,
        "tsql/Application/Tables/Cities.sql",
        CITIES_TABLE.replace("NOT NULL\n", "NOT NULL,\n    Population bigint NULL\n"),
    );
    put(
        &repo,
        "tsql/WebApi/Views/Cities.sql",
        CITIES_VIEW.replace("SELECT c", "SELECT\n\tc"),
    );
    // A rename git detects, with a reformat.
    std::fs::create_dir_all(repo.join("tsql/reports")).unwrap();
    git(&repo, &["mv", "tsql/Report.sql", "tsql/reports/Report.sql"]);
    put(
        &repo,
        "tsql/reports/Report.sql",
        "CREATE PROCEDURE dbo.Report AS\nBEGIN\n    SELECT id, qty FROM dbo.Stock WHERE qty > 0;\nEND;\n-- moved\n",
    );
    // A move git does not detect (rewritten), paired by object name.
    git(&repo, &["rm", "-q", "tsql/old/Mover.sql"]);
    put(&repo, "tsql/new/Mover.sql", MOVER_NEW);
    git(&repo, &["rm", "-q", "tsql/Gone.sql"]);
    put(
        &repo,
        "tsql/Added.sql",
        "CREATE PROCEDURE dbo.Added AS\nBEGIN\n    SELECT 1 AS x;\nEND;\n",
    );
    put(&repo, "README.md", "more docs\n");
    commit(&repo, "head");
    (root, repo)
}

fn entry<'a>(r: &'a serde_json::Value, head: &str) -> &'a serde_json::Value {
    r["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["head"] == head)
        .unwrap_or_else(|| panic!("no entry for {head}: {r:#}"))
}

#[test]
fn every_changed_file_is_paired_or_reported() {
    let (root, repo) = scenario("pairing");
    let out = changes(&root, &repo, &["--base", "base", "-f", "json"]);
    assert_eq!(out.status.code(), Some(1));
    let r = json(&out);

    assert_eq!(entry(&r, "tsql/MoveStock.sql")["change"], "modified");
    assert_eq!(entry(&r, "tsql/MoveStock.sql")["verdict"], "different");
    // Both Cities.sql files pair by path, not by file name.
    let table = entry(&r, "tsql/Application/Tables/Cities.sql");
    assert_eq!(table["base"], "tsql/Application/Tables/Cities.sql");
    let view = entry(&r, "tsql/WebApi/Views/Cities.sql");
    assert_eq!(view["base"], "tsql/WebApi/Views/Cities.sql");
    // Reindenting a view is not a change.
    assert_eq!(view["verdict"], "equal");
    let report = entry(&r, "tsql/reports/Report.sql");
    assert_eq!(report["base"], "tsql/Report.sql");
    assert!(report["change"].as_str().unwrap().starts_with("renamed"));
    let mover = entry(&r, "tsql/new/Mover.sql");
    assert_eq!(mover["base"], "tsql/old/Mover.sql");
    assert_eq!(mover["change"], "replaced (same object)");
    assert_eq!(entry(&r, "tsql/Added.sql")["change"], "added");
    // An added file is checked, but not compared: at best unknown.
    assert_eq!(entry(&r, "tsql/Added.sql")["verdict"], "unknown");

    let not = r["not_compared"].as_array().unwrap();
    assert_eq!(not.len(), 1, "{not:#?}");
    assert_eq!(not[0]["what"], "removed");
    assert_eq!(not[0]["path"], "tsql/Gone.sql");
    assert_eq!(r["entries"].as_array().unwrap().len(), 6);
    // The unchanged file was never read, and the README is not SQL.
    let all = r.to_string();
    assert!(!all.contains("Unchanged.sql"), "{all}");
    assert!(!all.contains("README"), "{all}");
    assert_eq!(r["verdict"], "different");
}

/// Every SARIF result has one location, in a file that exists at head,
/// on lines that exist there: code scanning drops anything else.
#[test]
fn every_sarif_result_is_located_at_head() {
    let (root, repo) = scenario("sarif");
    let sarif = root.join("out.sarif");
    let out = changes(
        &root,
        &repo,
        &["--base", "base", "--sarif", sarif.to_str().unwrap()],
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sarif).unwrap()).unwrap();
    let results = doc["runs"][0]["results"].as_array().unwrap();
    assert!(!results.is_empty());
    for res in results {
        let locs = res["locations"].as_array().unwrap();
        assert_eq!(locs.len(), 1, "{res:#}");
        let phys = &locs[0]["physicalLocation"];
        let uri = phys["artifactLocation"]["uri"].as_str().unwrap();
        let text = git(&repo, &["show", &format!("HEAD:{uri}")]);
        let lines = text.lines().count() as u64;
        let (start, end) = (
            phys["region"]["startLine"].as_u64().unwrap(),
            phys["region"]["endLine"].as_u64().unwrap(),
        );
        assert!(
            1 <= start && start <= end && end <= lines,
            "{uri}: {start}-{end} of {lines}: {res:#}"
        );
    }
    // The dropped COMMIT points at the head line now in its place.
    let removed = results
        .iter()
        .find(|r| r["ruleId"] == "stmt.removed")
        .expect("a stmt.removed result");
    let phys = &removed["locations"][0]["physicalLocation"];
    assert_eq!(phys["artifactLocation"]["uri"], "tsql/MoveStock.sql");
    assert_eq!(phys["region"]["startLine"], 6);
    let msg = removed["message"]["text"].as_str().unwrap();
    assert!(
        msg.starts_with("(Base line 6 was removed just above this line.)"),
        "{msg}"
    );
    // Where it was at base stays a related location.
    let rel = &removed["relatedLocations"][0]["physicalLocation"];
    assert_eq!(rel["artifactLocation"]["uri"], "tsql/MoveStock.sql");
    assert_eq!(rel["region"]["startLine"], 6);
}

#[test]
fn markdown_names_every_changed_file() {
    let (root, repo) = scenario("markdown");
    let md = root.join("out.md");
    changes(
        &root,
        &repo,
        &["--base", "base", "--markdown", md.to_str().unwrap()],
    );
    let md = std::fs::read_to_string(md).unwrap();
    assert!(
        md.starts_with("<!-- sqldiff:changes -->\n### SQLDiff: behaviour differs"),
        "{md}"
    );
    for p in [
        "tsql/MoveStock.sql",
        "tsql/Application/Tables/Cities.sql",
        "tsql/WebApi/Views/Cities.sql",
        "tsql/reports/Report.sql",
        "tsql/new/Mover.sql",
        "tsql/Added.sql",
        "tsql/Gone.sql",
    ] {
        assert!(md.contains(&format!("`{p}`")), "{p} missing:\n{md}");
    }
    assert!(md.contains("Transaction behaviour differs."), "{md}");
    assert!(md.contains("removed `tsql/Gone.sql`"), "{md}");
}

#[test]
fn reformatting_passes_and_fail_on_decides_the_exit_code() {
    let (root, repo) = new_repo("reformat");
    put(&repo, "tsql/MoveStock.sql", MOVE);
    commit(&repo, "base");
    git(&repo, &["branch", "base"]);
    put(
        &repo,
        "tsql/MoveStock.sql",
        MOVE.replace("    ", "\t")
            .replace("BEGIN\n", "BEGIN -- moved\n"),
    );
    commit(&repo, "reformat");
    let out = changes(&root, &repo, &["--base", "base", "-f", "json"]);
    let r = json(&out);
    assert_eq!(r["verdict"], "equal", "{r:#}");
    assert_eq!(out.status.code(), Some(0));

    // An added file is unknown: it fails by default, not with --fail-on different.
    put(
        &repo,
        "tsql/New.sql",
        "CREATE PROCEDURE dbo.New AS SELECT 1;\n",
    );
    commit(&repo, "add");
    assert_eq!(
        changes(&root, &repo, &["--base", "base"]).status.code(),
        Some(1)
    );
    let out = changes(&root, &repo, &["--base", "base", "--fail-on", "different"]);
    assert_eq!(out.status.code(), Some(0));
    let out = changes(&root, &repo, &["--base", "base", "--fail-on", "never"]);
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn the_diff_starts_at_the_merge_base() {
    let (root, repo) = new_repo("merge-base");
    put(&repo, "tsql/A.sql", "CREATE PROCEDURE dbo.A AS SELECT 1;\n");
    put(&repo, "tsql/B.sql", "CREATE PROCEDURE dbo.B AS SELECT 1;\n");
    commit(&repo, "base");
    git(&repo, &["checkout", "-q", "-b", "feature"]);
    put(&repo, "tsql/A.sql", "CREATE PROCEDURE dbo.A AS SELECT 2;\n");
    commit(&repo, "feature");
    // The target branch moves on; the feature branch did not touch B.
    git(&repo, &["checkout", "-q", "main"]);
    put(&repo, "tsql/B.sql", "CREATE PROCEDURE dbo.B AS SELECT 3;\n");
    commit(&repo, "main moves");
    let r = json(&changes(
        &root,
        &repo,
        &["--base", "main", "--head", "feature", "-f", "json"],
    ));
    let entries = r["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{r:#}");
    assert_eq!(entries[0]["head"], "tsql/A.sql");
}

#[test]
fn unknown_dialect_unreadable_ignored_and_config_changes_are_reported() {
    let (root, repo) = new_repo("reported");
    put(&repo, "sqldiff.toml", "ignore = [\"vendor/**\"]\n");
    commit(&repo, "base");
    git(&repo, &["branch", "base"]);
    put(&repo, "misc/NoDialect.sql", "SELECT 1;\n");
    put(&repo, "tsql/Latin1.sql", [b'S', b'E', 0xE9, b'\n']);
    let mut utf16 = vec![0xFF, 0xFE];
    for u in "CREATE PROCEDURE dbo.Wide AS SELECT 1;\n".encode_utf16() {
        utf16.extend(u.to_le_bytes());
    }
    put(&repo, "tsql/Wide.sql", utf16);
    put(&repo, "vendor/tsql/Lib.sql", "SELECT 1;\n");
    put(
        &repo,
        "sqldiff.toml",
        "ignore = [\"vendor/**\", \"tsql/**\"]\n",
    );
    commit(&repo, "head");
    let config = repo.join("sqldiff.toml");
    // Read the base's config, so the head's new ignore pattern does not apply.
    std::fs::write(&config, "ignore = [\"vendor/**\"]\n").unwrap();
    let out = changes(
        &root,
        &repo,
        &[
            "--base",
            "base",
            "--config",
            config.to_str().unwrap(),
            "-f",
            "json",
        ],
    );
    let r = json(&out);
    assert_eq!(out.status.code(), Some(1));
    let not: Vec<(String, String)> = r["not_compared"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            (
                n["what"].as_str().unwrap().to_string(),
                n["path"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    for want in [
        ("no dialect", "misc/NoDialect.sql"),
        ("unreadable", "tsql/Latin1.sql"),
        ("config changed", "sqldiff.toml"),
    ] {
        assert!(
            not.contains(&(want.0.to_string(), want.1.to_string())),
            "{want:?} not in {not:?}"
        );
    }
    assert_eq!(r["ignored"], serde_json::json!(["vendor/tsql/Lib.sql"]));
    // UTF-16 with a BOM is read.
    assert_eq!(entry(&r, "tsql/Wide.sql")["change"], "added");
    assert_eq!(r["verdict"], "unknown");
}

#[test]
fn tree_pairs_same_named_files_by_the_object_they_define() {
    let root = scratch("tree");
    for side in ["a", "b"] {
        let d = root.join(side);
        put(&d, "Application/Tables/Cities.sql", CITIES_TABLE);
        put(&d, "WebApi/Views/Cities.sql", CITIES_VIEW);
        // Same name, and no single object: stays ambiguous.
        put(&d, "x/Misc.sql", "SELECT 1;\n");
        put(&d, "y/Misc.sql", "SELECT 2;\n");
    }
    let out = Command::new(env!("CARGO_BIN_EXE_sqldiff"))
        .arg("tree")
        .arg(root.join("a"))
        .arg(root.join("b"))
        .args(["--dialect-a", "tsql", "--dialect-b", "tsql", "-f", "json"])
        .args(["--isolated", "--data-dir"])
        .arg(root.join("data"))
        .output()
        .unwrap();
    let r = json(&out);
    let pairs: Vec<(String, String)> = r["pairs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            let rel = |k: &str| {
                let s = p[k]["path"].as_str().unwrap().replace('\\', "/");
                s.split_once("/a/")
                    .or_else(|| s.split_once("/b/"))
                    .unwrap()
                    .1
                    .to_string()
            };
            (rel("a"), rel("b"))
        })
        .collect();
    assert!(pairs.contains(&(
        "Application/Tables/Cities.sql".into(),
        "Application/Tables/Cities.sql".into()
    )));
    assert!(pairs.contains(&(
        "WebApi/Views/Cities.sql".into(),
        "WebApi/Views/Cities.sql".into()
    )));
    assert_eq!(pairs.len(), 2, "{r:#}");
    let amb = r["ambiguous"].as_array().unwrap();
    assert_eq!(amb.len(), 2, "{r:#}");
    assert!(amb.iter().all(|a| a["key"] == "misc"));
}

#[test]
fn a_rename_from_an_unknown_dialect_still_checks_the_head_file() {
    let (root, repo) = new_repo("from-no-dialect");
    put(&repo, "misc/p.sql", "CREATE PROCEDURE dbo.P AS SELECT 1;\n");
    commit(&repo, "base");
    git(&repo, &["branch", "base"]);
    std::fs::create_dir_all(repo.join("tsql")).unwrap();
    git(&repo, &["mv", "misc/p.sql", "tsql/p.sql"]);
    put(
        &repo,
        "tsql/p.sql",
        "CREATE PROCEDURE dbo.P AS SELECT 1;\n-- moved\n",
    );
    commit(&repo, "head");
    let r = json(&changes(&root, &repo, &["--base", "base", "-f", "json"]));
    assert_eq!(entry(&r, "tsql/p.sql")["change"], "added");
    let not = r["not_compared"].as_array().unwrap();
    assert!(
        not.iter()
            .any(|n| n["what"] == "no dialect" && n["path"] == "misc/p.sql"),
        "{r:#}"
    );
    assert_eq!(r["verdict"], "unknown");
}
