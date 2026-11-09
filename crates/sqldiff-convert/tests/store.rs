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
                recheck: Default::default(),
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
    r["run"]["files"]
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

/// A stored result is its body's SHA-256 in hex, a newline, the body.
fn read_result(p: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(p).unwrap()[65..]).unwrap()
}

/// Writes `v` as a result with a digest that matches it: a stored result that
/// is wrong but whole, as an engine bug would leave one.
fn write_result(p: &Path, v: &serde_json::Value) {
    std::fs::write(p, framed(&serde_json::to_vec(v).unwrap())).unwrap();
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
    // The edit is in the output, and the result of the file as it was is
    // gone (`Store::gc`): no later run of this tree can use it.
    assert!(tree_of(&f.out("o"))
        .iter()
        .any(|(n, t)| n == "sub/p.sql" && t.contains("= 2")));
    assert_eq!(results(&f.out("o")).len(), 4);
    // The tree is what a fresh run gives.
    let resumed = tree_of(&f.out("o"));
    convert_into(&f.job("o", true, true), &Hooks::default()).unwrap();
    assert_eq!(resumed, tree_of(&f.out("o")));
}

#[test]
fn an_increment_keeps_the_results_of_the_files_outside_it() {
    let f = Fixture::standard("gcincr");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    let before = results(&f.out("o"));
    assert_eq!(before.len(), 4);
    let mut j = f.job("o", true, false);
    j.select = sqldiff_convert::plan::Select {
        next: Some(1),
        ..Default::default()
    };
    convert_into(&j, &Hooks::default()).unwrap();
    let mut after = results(&f.out("o"));
    let mut before = before;
    before.sort();
    after.sort();
    assert_eq!(before, after);
}

#[test]
fn a_file_that_is_not_a_result_is_left_in_the_results_folder() {
    let f = Fixture::standard("gcother");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    let other = f.out("o").join(".sqldiff/results/notes.txt");
    std::fs::write(&other, "mine").unwrap();
    f.write("a.sql", "CREATE TABLE t (a int NOT NULL, b int NULL);\n");
    convert_into(&f.job("o", true, false), &Hooks::default()).unwrap();
    assert!(other.is_file());
}

#[test]
fn a_write_killed_long_ago_is_swept_but_one_in_flight_is_not() {
    let f = Fixture::standard("gctmp");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    let dir = f.out("o").join(".sqldiff/results");
    let key = "ab".repeat(32);
    let old = dir.join(format!("{key}.tmp1-ThreadId(2)"));
    let fresh = dir.join(format!("{key}.tmp2-ThreadId(3)"));
    std::fs::write(&old, "x").unwrap();
    std::fs::write(&fresh, "x").unwrap();
    let two_hours = std::time::Duration::from_secs(7200);
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - two_hours)
        .unwrap();
    f.write("a.sql", "CREATE TABLE t (a int NOT NULL, b int NULL);\n");
    convert_into(&f.job("o", true, false), &Hooks::default()).unwrap();
    assert!(!old.exists(), "a stale temporary file stays");
    assert!(fresh.exists(), "a recent temporary file was removed");
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
        read_result(&p);
    }
}

#[test]
fn a_stored_result_that_a_fresh_check_contradicts_fails_the_run_and_empties_the_store() {
    // One file: the sample is that file.
    let f = Fixture::new("mismatch", &[("p.sql", PROC)]);
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    let stored = results(&f.out("o"));
    assert_eq!(stored.len(), 1);
    let mut e = read_result(&stored[0]);
    e["conversion"]["output"] = "-- tampered\n".into();
    write_result(&stored[0], &e);
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
fn the_sampled_recheck_reports_a_stored_result_that_differs_and_bundles_both() {
    // One file: the sample is that file. The stored result is read after the
    // fresh check, and the bundle holds the stored and the fresh result.
    let f = Fixture::new("sampled-differs", &[("p.sql", PROC)]);
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    let stored = results(&f.out("o"));
    assert_eq!(stored.len(), 1);
    let mut e = read_result(&stored[0]);
    let honest = e["conversion"]["output"].clone();
    e["conversion"]["output"] = "-- tampered\n".into();
    write_result(&stored[0], &e);
    let err = convert_into(&f.job("o", true, false), &Hooks::default())
        .err()
        .expect("a mismatch fails the run");
    assert!(err.contains("differs from a fresh check"), "{err}");
    assert!(results(&f.out("o")).is_empty(), "the store is emptied");
    let bundle = std::fs::read_dir(f.out("o").join(".sqldiff/mismatch"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let b: serde_json::Value = serde_json::from_slice(&std::fs::read(bundle).unwrap()).unwrap();
    assert_eq!(b["stored"]["output"], "-- tampered\n");
    assert_eq!(b["fresh"]["output"], honest);
}

#[test]
fn the_sampled_recheck_reports_a_stored_result_that_does_not_read_back_as_a_mismatch() {
    // The digest is right and the key is right, so the file is present when
    // the sample is decided; its units do not deserialise, so the read after
    // the fresh check fails. That is a mismatch, not a miss.
    let f = Fixture::new("sampled-unreadable", &[("p.sql", PROC)]);
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    let stored = results(&f.out("o"));
    assert_eq!(stored.len(), 1);
    let mut e = read_result(&stored[0]);
    e["conversion"]["units"] = serde_json::json!([{"status": "no such status"}]);
    write_result(&stored[0], &e);
    let err = convert_into(&f.job("o", true, false), &Hooks::default())
        .err()
        .expect("a stored result that does not read back fails the run");
    assert!(err.contains("does not read back whole"), "{err}");
    assert!(results(&f.out("o")).is_empty(), "the store is emptied");
    let bundle = std::fs::read_dir(f.out("o").join(".sqldiff/mismatch"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let b: serde_json::Value = serde_json::from_slice(&std::fs::read(bundle).unwrap()).unwrap();
    assert!(b["stored"].is_null() && b["fresh"]["output"].is_string());
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
    let raw = std::fs::read(p).unwrap();
    let mut json = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&raw[65..]), &mut json).unwrap();
    serde_json::from_slice(&json).unwrap()
}

fn write_state(p: &Path, v: &serde_json::Value) {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut enc, &serde_json::to_vec(v).unwrap()).unwrap();
    std::fs::write(p, framed(&enc.finish().unwrap())).unwrap();
}

/// A stored file is the SHA-256 of its body in hex, a newline, the body.
fn framed(body: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let hex: String = Sha256::digest(body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    [hex.as_bytes(), b"\n", body].concat()
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

#[test]
fn a_damaged_stored_draft_fails_the_run_cleanly_whichever_file_it_is() {
    // Whatever file the sample would pick, a draft whose ranges lie outside
    // its text is refused with a message, never indexed.
    let f = Fixture::standard("damageddraft");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    for idx in 0..4 {
        let p = states(&f.out("o")).remove(0);
        let mut st = read_state(&p);
        st["drafts"][idx]["raw"]["text"] = "".into();
        write_state(&p, &st);
        let err = convert_into(&f.job("o", true, false), &Hooks::default())
            .err()
            .expect("a damaged draft fails the run");
        assert!(err.contains("stored tree state"), "{err}");
        convert_into(&f.job("o", true, false), &Hooks::default()).unwrap();
    }
}

fn with_recheck(mut j: Job, r: tree::Recheck) -> Job {
    j.tree.recheck = r;
    j
}

fn tamper_every_result(out: &Path) {
    for p in results(out) {
        let mut e = read_result(&p);
        e["conversion"]["output"] = "-- tampered\n".into();
        write_result(&p, &e);
    }
}

#[test]
fn recheck_all_finds_a_wrong_stored_result_in_any_file() {
    // Four files: the default sample covers one of them, `all` every one.
    let f = Fixture::standard("recheckall");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    let stored = results(&f.out("o"));
    assert_eq!(stored.len(), 4);
    for p in &stored {
        let mut e = read_result(p);
        e["conversion"]["output"] = "-- tampered\n".into();
        write_result(p, &e);
        let err = convert_into(
            &with_recheck(f.job("o", true, false), tree::Recheck::All),
            &Hooks::default(),
        )
        .err()
        .expect("`all` builds every stored result again");
        assert!(err.contains("differs from a fresh check"), "{err}");
        // The store was emptied; build it again for the next file.
        convert_into(&f.job("o", true, false), &Hooks::default()).unwrap();
    }
}

#[test]
fn recheck_none_trusts_the_store_and_says_so() {
    let f = Fixture::standard("recheknone");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    tamper_every_result(&f.out("o"));
    let w = convert_into(
        &with_recheck(f.job("o", true, false), tree::Recheck::None),
        &Hooks::default(),
    )
    .expect("nothing is built again to compare");
    assert_eq!(w.tree.report.run.recheck, tree::Recheck::None);
    assert!(w.tree.report.run.files.iter().all(|r| r.reused));
    let summary = tree::summary(&w.tree.report);
    assert!(summary.contains("unverified reuse"), "{summary}");
    assert!(
        tree::markdown(&w.tree.report).contains("Unverified reuse"),
        "the markdown report says it too"
    );
    // The default and `all` do not claim it.
    let ok = convert_into(&f.job("o", true, true), &Hooks::default()).unwrap();
    assert!(!tree::summary(&ok.tree.report).contains("unverified reuse"));
}

#[test]
fn recheck_all_builds_the_whole_tree_state_again() {
    let f = Fixture::standard("recheckstate");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    let p = states(&f.out("o")).remove(0);
    let mut st = read_state(&p);
    // The last file's draft: the default sample (one file, by seed) may miss it.
    st["drafts"][3]["raw"]["text"] = "-- tampered\n".into();
    write_state(&p, &st);
    let err = convert_into(
        &with_recheck(f.job("o", true, false), tree::Recheck::All),
        &Hooks::default(),
    )
    .err()
    .expect("`all` builds the state again");
    assert!(err.contains("stored tree state"), "{err}");
}

#[test]
fn the_recheck_mode_never_changes_the_output() {
    let f = Fixture::standard("recheckmodes");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    // The mode itself is reported (`run.recheck`, and a line in the summary
    // for `none`); nothing else may differ.
    let plain = |out: &Path| {
        let mut t = tree_of(out);
        for (_, text) in &mut t {
            *text = text
                .lines()
                .filter(|l| {
                    !l.contains("\"recheck\":")
                        && !l.starts_with("unverified reuse")
                        && !l.starts_with("**Unverified reuse")
                        && !l.trim().is_empty()
                })
                .collect::<Vec<_>>()
                .join("\n");
        }
        t
    };
    let want = plain(&f.out("o"));
    for r in [
        tree::Recheck::None,
        tree::Recheck::Sample,
        tree::Recheck::All,
    ] {
        convert_into(&with_recheck(f.job("o", true, false), r), &Hooks::default()).unwrap();
        assert_eq!(want, plain(&f.out("o")), "{}", r.label());
    }
}

#[test]
fn recheck_none_says_so_when_only_the_tree_state_was_reused() {
    let f = Fixture::standard("recheckstateonly");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    // No per-file result is left to reuse; the tree state still is.
    for p in results(&f.out("o")) {
        std::fs::remove_file(p).unwrap();
    }
    let w = convert_into(
        &with_recheck(f.job("o", true, false), tree::Recheck::None),
        &Hooks::default(),
    )
    .unwrap();
    assert!(w.tree.report.run.files.iter().all(|r| !r.reused));
    assert!(tree::summary(&w.tree.report).contains("unverified reuse"));
}

#[test]
fn an_edited_result_that_is_still_valid_json_is_a_miss_and_converted_again() {
    let f = Fixture::standard("digest");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    let sql = |out: &Path| -> Vec<(String, String)> {
        // The reports say which recheck mode the run used; the files do not.
        tree_of(out)
            .into_iter()
            .filter(|(p, _)| p.ends_with(".sql"))
            .collect()
    };
    let base = sql(&f.out("o"));
    assert!(!base.is_empty());
    // Edited in place, the digest left as it was; no recheck samples it.
    for p in results(&f.out("o")) {
        let mut bytes = std::fs::read(&p).unwrap();
        let mut e: serde_json::Value = serde_json::from_slice(&bytes[65..]).unwrap();
        e["conversion"]["output"] = "-- tampered\n".into();
        bytes.truncate(65);
        bytes.extend(serde_json::to_vec(&e).unwrap());
        std::fs::write(&p, bytes).unwrap();
    }
    let w = convert_into(
        &with_recheck(f.job("o", true, false), tree::Recheck::None),
        &Hooks::default(),
    )
    .expect("an edited result is converted again, not trusted");
    assert!(w.tree.report.run.files.iter().all(|r| !r.reused));
    assert_eq!(base, sql(&f.out("o")), "the same files as a fresh run");
    // A truncated digest line and a bare body are misses too.
    for (i, p) in results(&f.out("o")).iter().enumerate() {
        let bytes = std::fs::read(p).unwrap();
        let bad = if i % 2 == 0 {
            bytes[65..].to_vec()
        } else {
            bytes[..40].to_vec()
        };
        std::fs::write(p, bad).unwrap();
    }
    convert_into(&f.job("o", true, false), &Hooks::default()).unwrap();
    assert_eq!(base, sql(&f.out("o")));
}

#[test]
fn a_damaged_stored_draft_fails_cleanly_even_when_nothing_is_rechecked() {
    // `--recheck none` trusts the store's content, never its ranges: a
    // damaged draft still fails the run instead of being indexed.
    let f = Fixture::standard("damagednone");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    let p = states(&f.out("o")).remove(0);
    let mut st = read_state(&p);
    st["drafts"][3]["raw"]["text"] = "".into();
    write_state(&p, &st);
    let err = convert_into(
        &with_recheck(f.job("o", true, false), tree::Recheck::None),
        &Hooks::default(),
    )
    .err()
    .expect("a damaged draft fails the run");
    assert!(err.contains("stored tree state"), "{err}");
}

#[test]
fn an_edited_tree_state_or_flag_set_is_a_miss_and_built_again() {
    let f = Fixture::standard("digest2");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    let want = tree_of(&f.out("o"));
    // The state is edited without a new digest (edited bytes after the line).
    for p in states(&f.out("o")) {
        let mut st = read_state(&p);
        st["drafts"][0]["raw"]["text"] = "-- tampered\n".into();
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut enc, &serde_json::to_vec(&st).unwrap()).unwrap();
        let mut bytes = std::fs::read(&p).unwrap();
        bytes.truncate(65);
        bytes.extend(enc.finish().unwrap());
        std::fs::write(&p, bytes).unwrap();
    }
    // `--recheck none` samples nothing: only the digest can refuse it.
    convert_into(
        &with_recheck(f.job("o", true, false), tree::Recheck::None),
        &Hooks::default(),
    )
    .unwrap();
    let sql = |t: Vec<(String, String)>| -> Vec<(String, String)> {
        t.into_iter().filter(|(p, _)| p.ends_with(".sql")).collect()
    };
    assert_eq!(sql(want), sql(tree_of(&f.out("o"))));
}

#[test]
fn a_stored_mix_that_the_draft_code_no_longer_builds_is_a_miss() {
    // The draft crate is not in a result's fingerprint (`CHECK_FINGERPRINT`),
    // so the mix stored with a result is what ties the result to the draft
    // code: when the mix built again differs from the stored one (here a
    // stored draft no code builds), the result is not used.
    let f = Fixture::standard("mix");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    let stored = results(&f.out("o"));
    assert_eq!(stored.len(), 4);
    // Nothing here needed a mix.
    for p in &stored {
        let e = read_result(p);
        assert!(e["mix"].is_null(), "{}", e["mix"]);
    }
    // Control: untouched results are reused (all but the re-checked sample).
    convert_into(&f.job("o", true, false), &Hooks::default()).unwrap();
    assert!(reused(&f.out("o")).iter().any(|(_, r)| *r));
    for p in &results(&f.out("o")) {
        let mut e = read_result(p);
        e["mix"] = serde_json::json!({ "plain_at": [0], "draft": "a draft no code builds" });
        write_result(p, &e);
    }
    convert_into(&f.job("o", true, false), &Hooks::default()).unwrap();
    assert!(
        reused(&f.out("o")).iter().all(|(_, r)| !*r),
        "a result whose mix is not rebuilt was reused"
    );
    // The results are stored again, with the mix this code gives (none).
    let again = results(&f.out("o"));
    assert_eq!(again.len(), 4);
    for p in &again {
        assert!(read_result(p)["mix"].is_null());
    }
}

/// Two routines of one tree whose table variables would map to one
/// temporary table (see `tests/table_variables.rs`): the later file is
/// refused. The refusal comes from the claims of the whole tree, which the
/// draft crate computes; it shows in the draft's text, so a file's stored
/// result follows an edit of *another* file that lifts the clash.
#[test]
fn a_clash_lifted_by_an_edit_of_another_file_changes_the_stored_result() {
    let first = "CREATE PROCEDURE [dbo].[a b%c d%e^f~g] AS\nBEGIN\n  DECLARE @t TABLE (i int);\n  INSERT INTO @t (i) VALUES (1);\nEND\n";
    let first_without = "CREATE PROCEDURE [dbo].[a b%c d%e^f~g] AS\nBEGIN\n  SELECT 1;\nEND\n";
    let second = "CREATE PROCEDURE [dbo].[a-b&c~d!e%f=g] AS\nBEGIN\n  DECLARE @t TABLE (i int);\n  INSERT INTO @t (i) VALUES (2);\nEND\n";
    let f = Fixture::new("claims", &[("a.sql", first), ("b.sql", second)]);
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    let refused = |out: &Path| {
        tree_of(out)
            .into_iter()
            .find(|(n, _)| n == "b.sql")
            .unwrap()
            .1
    };
    let before = refused(&f.out("o"));
    // A second, untouched run gives the same tree.
    convert_into(&f.job("o", true, false), &Hooks::default()).unwrap();
    assert_eq!(before, refused(&f.out("o")));
    // `a.sql` no longer declares the table variable: `b.sql` is not refused.
    f.write("a.sql", first_without);
    convert_into(&f.job("o", true, false), &Hooks::default()).unwrap();
    let resumed = tree_of(&f.out("o"));
    let b = reused(&f.out("o"))
        .into_iter()
        .find(|(p, _)| p == "b.sql")
        .unwrap();
    assert!(!b.1, "b.sql was reused after the clash with it was lifted");
    assert_ne!(before, refused(&f.out("o")));
    // And it is what a fresh run gives.
    convert_into(&f.job("o", true, true), &Hooks::default()).unwrap();
    assert_eq!(resumed, tree_of(&f.out("o")));
}
