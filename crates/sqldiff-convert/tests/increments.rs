//! Increments (`plan`, `job::Job::select`): a tree checked in any split of
//! increments, with the whole-tree passes rerun each time, ends with the
//! files and report a full run gives; a file no run checked counts as not
//! converted and never as converted.

use std::path::{Path, PathBuf};

use sqldiff_convert::job::{convert_into, Job};
use sqldiff_convert::plan::{Plan, Select};
use sqldiff_convert::tree::{self, Hooks};
use sqldiff_core::Dialect;

const FILES: &[(&str, &str)] = &[
    ("tables/t.sql", "CREATE TABLE dbo.t (a int NOT NULL);\n"),
    (
        "funcs/f.sql",
        "CREATE FUNCTION dbo.f(@x int) RETURNS int AS\nBEGIN\n  RETURN @x + 1;\nEND\n",
    ),
    (
        "views/v.sql",
        "CREATE VIEW dbo.v AS SELECT a FROM dbo.t WHERE a > 1;\n",
    ),
    (
        "procs/p.sql",
        "CREATE PROCEDURE dbo.p AS\nBEGIN\n  UPDATE dbo.t SET a = dbo.f(a);\nEND\n",
    ),
    ("load.sql", "INSERT INTO dbo.t (a) VALUES (1);\n"),
];

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("sqldiff-incr-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (p, text) in FILES {
            let f = dir.join("src").join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        Fixture { dir }
    }

    fn job(&self, out: &str, select: Select) -> Job {
        Job {
            tree: tree::Args {
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
            overwrite: true,
            discard_edits: false,
            fresh: false,
            select,
            instances: None,
            queue_all: false,
            proof: Default::default(),
            row_counts: false,
        }
    }

    fn out(&self, out: &str) -> PathBuf {
        self.dir.join(out)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn next(n: usize) -> Select {
    Select {
        next: Some(n),
        ..Select::default()
    }
}

fn path(p: &str) -> Select {
    Select {
        paths: vec![p.to_string()],
        ..Select::default()
    }
}

/// A JSON value without what differs between runs by design.
fn plain(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(m) => {
            for k in [
                "seconds",
                "reused",
                "checked",
                "progress",
                "files_not_checked",
                "out",
            ] {
                m.remove(k);
            }
            m.values_mut().for_each(plain);
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(plain),
        _ => {}
    }
}

/// Every file under `dir` except the store, the plan and the markdown
/// reports (which quote timings and the increment line).
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
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap();
            if rel.ends_with(".md") {
                continue;
            }
            let text = if rel.ends_with(".json") {
                let mut j: serde_json::Value = serde_json::from_str(&text).unwrap();
                plain(&mut j);
                j.to_string()
            } else {
                text
            };
            v.push((rel, text));
        }
    }
    v.sort();
    v
}

fn same(a: &[(String, String)], b: &[(String, String)], what: &str) {
    let names = |v: &[(String, String)]| v.iter().map(|x| x.0.clone()).collect::<Vec<_>>();
    assert_eq!(names(a), names(b), "{what}: files");
    for (x, y) in a.iter().zip(b) {
        if x.1 != y.1 {
            let at =
                x.1.bytes()
                    .zip(y.1.bytes())
                    .position(|(p, q)| p != q)
                    .unwrap_or(0);
            let (lo, hi) = (at.saturating_sub(150), at + 250);
            panic!(
                "{what}: {} differs at {at}\n full: {}\n incr: {}",
                x.0,
                &x.1[lo..hi.min(x.1.len())],
                &y.1[lo..hi.min(y.1.len())]
            );
        }
    }
}

fn report(out: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(out.join("conversion-report.json")).unwrap()).unwrap()
}

fn not_checked(out: &Path) -> Vec<String> {
    report(out)["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["checked"].as_bool() == Some(false))
        .map(|f| f["path"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_split_into_increments_gives_the_full_runs_tree() {
    let f = Fixture::new("split");
    convert_into(&f.job("full", Select::default()), &Hooks::default()).unwrap();
    let full = tree_of(&f.out("full"));
    // One group at a time, in plan order.
    for step in 1..=5 {
        convert_into(&f.job("inc", next(1)), &Hooks::default()).unwrap();
        let left = not_checked(&f.out("inc"));
        assert_eq!(left.len(), 5 - step, "step {step}: {left:?}");
        if step < 5 {
            assert_ne!(tree_of(&f.out("inc")), full, "step {step}");
        }
    }
    same(&full, &tree_of(&f.out("inc")), "next 1 x5");
}

#[test]
fn any_order_of_increments_gives_the_same_tree() {
    let f = Fixture::new("order");
    convert_into(&f.job("full", Select::default()), &Hooks::default()).unwrap();
    let full = tree_of(&f.out("full"));
    // Dependents first: the caller of a function before the function.
    for p in ["load.sql", "procs/**", "views/**", "funcs/**", "tables/**"] {
        convert_into(&f.job("rev", path(p)), &Hooks::default()).unwrap();
    }
    same(&full, &tree_of(&f.out("rev")), "reverse");
    // Two at a time.
    for _ in 0..3 {
        convert_into(&f.job("two", next(2)), &Hooks::default()).unwrap();
    }
    same(&full, &tree_of(&f.out("two")), "two at a time");
}

#[test]
fn a_file_nobody_checked_counts_as_not_converted() {
    let f = Fixture::new("notchecked");
    convert_into(&f.job("o", path("tables/**")), &Hooks::default()).unwrap();
    let r = report(&f.out("o"));
    let files = r["files"].as_array().unwrap();
    assert_eq!(not_checked(&f.out("o")).len(), 4);
    for x in files {
        let checked = x["checked"].as_bool().unwrap_or(true);
        if checked {
            assert_ne!(x["status"], "not_converted", "{x}");
        } else {
            assert_eq!(x["status"], "not_converted", "{x}");
            for u in x["units"].as_array().unwrap() {
                assert_eq!(u["status"], "not_converted");
                assert_eq!(u["rules"][0], "plan.not-checked");
            }
            assert!(x["converted_units"].as_array().unwrap().is_empty());
        }
    }
    assert_eq!(r["totals"]["files_not_checked"], 4);
    let p = &r["totals"]["progress"];
    assert_eq!(
        (p["files"].as_u64(), p["files_checked"].as_u64()),
        (Some(5), Some(1))
    );
    // The caller of a function nobody checked is not proven or worse, never
    // converted.
    convert_into(&f.job("o", path("procs/**")), &Hooks::default()).unwrap();
    let r = report(&f.out("o"));
    let p = r["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["path"] == "procs/p.sql")
        .unwrap();
    assert_ne!(p["status"], "converted");
    assert_ne!(p["status"], "assumptions", "{p}");
}

#[test]
fn next_zero_reports_the_store_and_checks_nothing() {
    let f = Fixture::new("status");
    convert_into(&f.job("o", next(2)), &Hooks::default()).unwrap();
    let before: Vec<_> = std::fs::read_dir(f.out("o").join(".sqldiff/results"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(before.len(), 2);
    convert_into(&f.job("o", next(0)), &Hooks::default()).unwrap();
    let r = report(&f.out("o"));
    assert_eq!(r["totals"]["progress"]["files_checked"], 2);
    assert_eq!(r["totals"]["files_not_checked"], 3);
    let after = std::fs::read_dir(f.out("o").join(".sqldiff/results"))
        .unwrap()
        .count();
    assert_eq!(after, 2, "nothing was checked, nothing was stored");
}

#[test]
fn the_plan_is_written_with_an_increment() {
    let f = Fixture::new("plan");
    convert_into(&f.job("o", next(1)), &Hooks::default()).unwrap();
    let plan: Plan =
        serde_json::from_slice(&std::fs::read(f.out("o").join(".sqldiff/plan.json")).unwrap())
            .unwrap();
    assert_eq!(plan.files.len(), 5);
    let first = &plan.files[plan.groups[0].files[0]];
    assert_eq!(first.path, "tables/t.sql");
}

#[test]
fn a_selection_that_matches_nothing_fails() {
    let f = Fixture::new("nothing");
    let err = convert_into(&f.job("o", path("nope/**")), &Hooks::default())
        .err()
        .expect("fails");
    assert!(err.contains("--path nope/**"), "{err}");
    let err = convert_into(
        &f.job(
            "o",
            Select {
                objects: vec!["missing".into()],
                ..Select::default()
            },
        ),
        &Hooks::default(),
    )
    .err()
    .expect("fails");
    assert!(err.contains("--object missing"), "{err}");
}
