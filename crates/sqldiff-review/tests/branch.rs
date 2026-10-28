//! `branch::review`, the code `sqldiff changes` and the app's branch runs
//! share, on a scratch git repository.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use sqldiff_core::hooks::{Hooks, CANCELLED};
use sqldiff_core::{Dialect, Verdict};
use sqldiff_review::branch;
use sqldiff_review::pipeline::{Pipeline, Settings};
use sqldiff_source::changes::ChangeScope;
use sqldiff_source::Resolved;

fn git(repo: &Path, args: &[&str]) {
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
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn put(repo: &Path, path: &str, text: &str) {
    let p = repo.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// A repository with two SQL files changed and one added on a branch.
fn repo(name: &str) -> (PathBuf, Pipeline) {
    let root = std::env::temp_dir().join(format!("sqldiff-branch-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    put(&repo, "a.tsql.sql", "SELECT a FROM t WHERE b = 1;\n");
    put(&repo, "b.tsql.sql", "SELECT a FROM t WHERE b = 1;\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(&repo, &["checkout", "-q", "-b", "topic"]);
    put(&repo, "a.tsql.sql", "SELECT a FROM t WHERE b = 2;\n");
    put(&repo, "b.tsql.sql", "SELECT   a   FROM t WHERE b = 1;\n");
    put(&repo, "c.tsql.sql", "SELECT 1;\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "change"]);
    let mut cfg = Resolved::builtin();
    cfg.data_dir = root.join("data");
    let pipe = Pipeline::new(
        cfg,
        &Settings {
            no_cache: true,
            ..Settings::default()
        },
    )
    .unwrap();
    (repo, pipe)
}

fn scope(repo: &Path) -> ChangeScope<'_> {
    ChangeScope {
        repo,
        base: "main",
        head: "topic",
        dialect: Some(Dialect::Tsql),
        paths: &[],
    }
}

#[test]
fn reviews_the_changed_files_most_severe_first() {
    let (repo, pipe) = repo("review");
    let steps = Arc::new(Mutex::new(Vec::new()));
    let s = Arc::clone(&steps);
    let hooks = Hooks {
        progress: Some(Box::new(move |n, total| s.lock().unwrap().push((n, total)))),
        cancel: None,
    };
    let r = branch::review(&scope(&repo), &pipe, &hooks).unwrap();
    assert_eq!(r.verdict(), Verdict::Different);
    let json: serde_json::Value = serde_json::from_str(&r.json()).unwrap();
    let heads: Vec<_> = json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["head"].as_str().unwrap())
        .collect();
    assert_eq!(heads[0], "a.tsql.sql", "{heads:?}");
    assert_eq!(heads.len(), 3);
    // One step per changed file, the last one is the total.
    assert_eq!(steps.lock().unwrap().clone(), [(1, 3), (2, 3), (3, 3)]);
    assert!(r.markdown().starts_with(branch::MARKER));
    assert!(r.text().contains("Overall: different"), "{}", r.text());
    assert!(r.sarif().contains("\"version\": \"2.1.0\""));
}

#[test]
fn a_cancel_stops_before_the_next_file() {
    let (repo, pipe) = repo("cancel");
    let cancel = Arc::new(AtomicBool::new(false));
    let c = Arc::clone(&cancel);
    let hooks = Hooks {
        progress: Some(Box::new(move |n, _| {
            if n == 1 {
                c.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        })),
        cancel: Some(cancel),
    };
    let e = branch::review(&scope(&repo), &pipe, &hooks).err().unwrap();
    assert_eq!(e, CANCELLED);
}

#[test]
fn redaction_is_refused() {
    let (repo, _) = repo("redact");
    let mut cfg = Resolved::builtin();
    cfg.data_dir = repo.parent().unwrap().join("data");
    let pipe = Pipeline::new(
        cfg,
        &Settings {
            redact: true,
            no_cache: true,
            ..Settings::default()
        },
    )
    .unwrap();
    let e = branch::review(&scope(&repo), &pipe, &Hooks::default())
        .err()
        .unwrap();
    assert!(e.contains("--redact"), "{e}");
}
