//! The result store (`store`, `job::Job::fresh`): a second run into the same
//! output reuses every stored result whose key matches, gives the same files
//! and report as a fresh run, re-checks only what changed, re-checks a sample
//! and fails on a mismatch.

use std::path::{Path, PathBuf};

use sqldiff_convert::job::{convert_into, Job};
use sqldiff_convert::tree::{self, Hooks};
use sqldiff_core::Dialect;

struct Fixture {
    dir: PathBuf,
}

const PROC: &str = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  UPDATE dbo.t SET a = 1;\nEND\n";

impl Fixture {
    fn new(name: &str, files: &[(&str, &str)]) -> Fixture {
        let dir = std::env::temp_dir().join(format!("sqldiff-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (p, text) in files {
            let f = dir.join("src").join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        Fixture { dir }
    }

    fn standard(name: &str) -> Fixture {
        Fixture::new(
            name,
            &[
                ("a.sql", "CREATE TABLE t (a int NOT NULL);\n"),
                ("b.sql", "SELECT TOP 1 a FROM t;\n"),
                ("sub/p.sql", PROC),
                (
                    "c.sql",
                    "CREATE VIEW dbo.v AS SELECT a FROM dbo.t WHERE a > 1;\n",
                ),
            ],
        )
    }

    fn job(&self, out: &str, overwrite: bool, fresh: bool) -> Job {
        Job {
            tree: tree::Args {
                rules: Default::default(),
                src: self.dir.join("src"),
                from: Dialect::Tsql,
                to: Dialect::Postgres,
                jobs: Some(2),
                no_catalog: false,
                template: None,
                extensions: tree::extensions(Vec::new()),
                helpers: Vec::new(),
                deployment: Default::default(),
                catalogs: Vec::new(),
                catalogs_b: Vec::new(),
            },
            out: self.dir.join(out),
            overwrite,
            discard_edits: false,
            fresh,
            select: Default::default(),
            instances: None,
            queue_all: false,
            proof: Default::default(),
            row_counts: false,
        }
    }

    fn out(&self, out: &str) -> PathBuf {
        self.dir.join(out)
    }

    fn write(&self, rel: &str, text: &str) {
        std::fs::write(self.dir.join("src").join(rel), text).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Every file under `dir` except the store, relative, with its text, the
/// timings and the reuse marks of the reports taken out.
fn tree_of(dir: &Path) -> Vec<(String, String)> {
    let mut v = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            let rel = p
                .strip_prefix(dir)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if rel.starts_with(".sqldiff") {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else {
                let text = std::fs::read_to_string(&p).unwrap();
                let text = text
                    .lines()
                    .filter(|l| !l.trim_start().starts_with("\"reused\":"))
                    .map(|l| {
                        let t = l.trim_start();
                        if t.starts_with("\"seconds\":")
                            || (t.contains(" files, ") && t.ends_with(" s"))
                        {
                            "(time)"
                        } else {
                            l
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                v.push((rel, text));
            }
        }
    }
    v.sort();
    v
}

fn reused(out: &Path) -> Vec<(String, bool)> {
    let r: serde_json::Value =
        serde_json::from_slice(&std::fs::read(out.join("conversion-report.json")).unwrap())
            .unwrap();
    r["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().to_string(),
                f["reused"].as_bool().unwrap_or(false),
            )
        })
        .collect()
}

fn results(out: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(out.join(".sqldiff/results"))
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

#[test]
fn a_second_run_reuses_the_results_and_gives_the_same_tree() {
    let f = Fixture::standard("same");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    assert_eq!(results(&f.out("o")).len(), 4);
    // The drafts stored as pass 1 went are dropped once the state is stored.
    assert!(!f.out("o").join(".sqldiff/drafts").exists());
    assert_eq!(states(&f.out("o")).len(), 1);
    assert!(reused(&f.out("o")).iter().all(|(_, r)| !r), "first run");
    let first = tree_of(&f.out("o"));
    convert_into(&f.job("o", true, false), &Hooks::default()).unwrap();
    let second = tree_of(&f.out("o"));
    assert_eq!(first, second, "a resumed run differs from the first");
    // The sampled re-check runs up to two files again; the rest is reused.
    let r = reused(&f.out("o"));
    assert!(r.iter().filter(|(_, x)| *x).count() >= 2, "{r:?}");
    // A fresh run gives the same tree too.
    convert_into(&f.job("o", true, true), &Hooks::default()).unwrap();
    assert_eq!(first, tree_of(&f.out("o")));
    assert!(reused(&f.out("o")).iter().all(|(_, r)| !r));
}

#[test]
fn fresh_reuses_nothing() {
    let f = Fixture::standard("fresh");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    convert_into(&f.job("o", true, true), &Hooks::default()).unwrap();
    assert!(reused(&f.out("o")).iter().all(|(_, r)| !r));
}

#[test]
fn editing_a_procedure_body_checks_that_file_again() {
    let f = Fixture::standard("edit");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    f.write(
        "sub/p.sql",
        "CREATE PROCEDURE dbo.p AS\nBEGIN\n  UPDATE dbo.t SET a = 2;\nEND\n",
    );
    convert_into(&f.job("o", true, false), &Hooks::default()).unwrap();
    let r = reused(&f.out("o"));
    let p = r.iter().find(|(p, _)| p == "sub/p.sql").unwrap();
    assert!(!p.1, "the edited file was reused: {r:?}");
    // The catalogs hold signatures only, so the other files keep their keys
    // (up to two of them are re-checked as the sample).
    assert!(r.iter().filter(|(_, x)| *x).count() >= 1, "{r:?}");
    // The edit is in the output, and the old result is still stored.
    assert!(tree_of(&f.out("o"))
        .iter()
        .any(|(n, t)| n == "sub/p.sql" && t.contains("= 2")));
    assert_eq!(results(&f.out("o")).len(), 5);
    // The tree is what a fresh run gives.
    let resumed = tree_of(&f.out("o"));
    convert_into(&f.job("o", true, true), &Hooks::default()).unwrap();
    assert_eq!(resumed, tree_of(&f.out("o")));
}

#[test]
fn editing_a_table_checks_every_file_again() {
    let f = Fixture::standard("table");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    f.write("a.sql", "CREATE TABLE t (a int NOT NULL, b int NULL);\n");
    convert_into(&f.job("o", true, false), &Hooks::default()).unwrap();
    // The catalogs changed, so no stored result matches (stage 1 of the
    // design: any change to the catalogs is a change to every key).
    let r = reused(&f.out("o"));
    assert!(r.iter().all(|(_, x)| !x), "{r:?}");
}

#[test]
fn a_damaged_result_is_a_miss() {
    let f = Fixture::standard("damaged");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    for p in results(&f.out("o")) {
        std::fs::write(p, b"{ not json").unwrap();
    }
    convert_into(&f.job("o", true, false), &Hooks::default()).unwrap();
    assert!(reused(&f.out("o")).iter().all(|(_, r)| !r));
    // And they are whole again.
    for p in results(&f.out("o")) {
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(p).unwrap()).unwrap();
    }
}

#[test]
fn a_stored_result_that_a_fresh_check_contradicts_fails_the_run_and_empties_the_store() {
    // One file: the sample is that file.
    let f = Fixture::new("mismatch", &[("p.sql", PROC)]);
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    let stored = results(&f.out("o"));
    assert_eq!(stored.len(), 1);
    let mut e: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&stored[0]).unwrap()).unwrap();
    e["conversion"]["output"] = "-- tampered\n".into();
    std::fs::write(&stored[0], serde_json::to_vec(&e).unwrap()).unwrap();
    let err = convert_into(&f.job("o", true, false), &Hooks::default())
        .err()
        .expect("a mismatch fails the run");
    assert!(err.contains("differs from a fresh check"), "{err}");
    assert!(results(&f.out("o")).is_empty(), "the store is emptied");
    let bundle: Vec<_> = std::fs::read_dir(f.out("o").join(".sqldiff/mismatch"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(bundle.len(), 1);
    // The next run works, from nothing.
    convert_into(&f.job("o", true, false), &Hooks::default()).unwrap();
    assert_eq!(results(&f.out("o")).len(), 1);
}

#[test]
fn a_directory_holding_only_the_store_is_empty_for_a_resume() {
    let f = Fixture::standard("onlystore");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    // As after a run killed before it wrote its tree.
    for e in std::fs::read_dir(f.out("o")).unwrap() {
        let p = e.unwrap().path();
        if p.file_name().unwrap() == ".sqldiff" {
            continue;
        }
        if p.is_dir() {
            std::fs::remove_dir_all(&p).unwrap();
        } else {
            std::fs::remove_file(&p).unwrap();
        }
    }
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    assert!(reused(&f.out("o")).iter().any(|(_, r)| *r));
}

fn states(out: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(out.join(".sqldiff/tree"))
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

fn modified(p: &Path) -> std::time::SystemTime {
    std::fs::metadata(p).unwrap().modified().unwrap()
}

#[test]
fn a_resume_takes_the_state_of_the_tree_from_the_store() {
    let f = Fixture::standard("treestate");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    let first = states(&f.out("o"));
    assert_eq!(first.len(), 1);
    let before = modified(&first[0]);
    std::thread::sleep(std::time::Duration::from_millis(50));
    convert_into(&f.job("o", true, false), &Hooks::default()).unwrap();
    // The same state, not written again.
    assert_eq!(states(&f.out("o")), first);
    assert_eq!(modified(&first[0]), before);
    // And the tree is what a fresh run gives.
    let resumed = tree_of(&f.out("o"));
    convert_into(&f.job("o", true, true), &Hooks::default()).unwrap();
    assert_eq!(resumed, tree_of(&f.out("o")));
}

#[test]
fn an_edit_anywhere_gives_a_new_tree_state_and_drops_the_old_one() {
    let f = Fixture::standard("treeedit");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    let old = states(&f.out("o"));
    // A change that touches no catalog and no other file's draft.
    f.write("b.sql", "SELECT TOP 2 a FROM t;\n");
    convert_into(&f.job("o", true, false), &Hooks::default()).unwrap();
    let new = states(&f.out("o"));
    assert_eq!(new.len(), 1);
    assert_ne!(new, old);
    let edited = tree_of(&f.out("o"));
    convert_into(&f.job("o", true, true), &Hooks::default()).unwrap();
    assert_eq!(edited, tree_of(&f.out("o")));
}

#[test]
fn a_damaged_tree_state_is_a_miss() {
    let f = Fixture::standard("treedamaged");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    let want = tree_of(&f.out("o"));
    for p in states(&f.out("o")) {
        std::fs::write(p, b"{ not json").unwrap();
    }
    convert_into(&f.job("o", true, false), &Hooks::default()).unwrap();
    assert_eq!(want, tree_of(&f.out("o")));
    for p in states(&f.out("o")) {
        read_state(&p);
    }
}

/// A stored tree state is gzip-compressed JSON.
fn read_state(p: &Path) -> serde_json::Value {
    let mut json = Vec::new();
    std::io::Read::read_to_end(
        &mut flate2::read::GzDecoder::new(std::fs::File::open(p).unwrap()),
        &mut json,
    )
    .unwrap();
    serde_json::from_slice(&json).unwrap()
}

fn write_state(p: &Path, v: &serde_json::Value) {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut enc, &serde_json::to_vec(v).unwrap()).unwrap();
    std::fs::write(p, enc.finish().unwrap()).unwrap();
}

/// A state that no longer matches what its tree builds: the first file's
/// draft text is replaced, the key kept.
fn tamper(out: &Path) {
    let p = states(out).remove(0);
    let mut st = read_state(&p);
    st["drafts"][0]["raw"]["text"] = "-- tampered\n".into();
    write_state(&p, &st);
}

#[test]
fn a_tree_state_that_a_rebuild_contradicts_fails_the_run_and_empties_the_store() {
    // One file: the sample is that file.
    let f = Fixture::new("treemismatch", &[("p.sql", PROC)]);
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    tamper(&f.out("o"));
    let err = convert_into(&f.job("o", true, false), &Hooks::default())
        .err()
        .expect("a mismatch fails the run");
    assert!(err.contains("stored tree state"), "{err}");
    assert!(states(&f.out("o")).is_empty() && results(&f.out("o")).is_empty());
    // The next run works, from nothing.
    convert_into(&f.job("o", true, false), &Hooks::default()).unwrap();
    assert_eq!(states(&f.out("o")).len(), 1);
}

#[test]
fn fresh_does_not_read_the_tree_state() {
    let f = Fixture::new("treefresh", &[("p.sql", PROC)]);
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    let want = tree_of(&f.out("o"));
    tamper(&f.out("o"));
    convert_into(&f.job("o", true, true), &Hooks::default()).unwrap();
    assert_eq!(want, tree_of(&f.out("o")));
}
