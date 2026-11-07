//! The result store for template trees (`convert --template`): a second run
//! reuses every stored job whose key matches and gives the same tree and
//! report as a fresh run; an edit re-checks what its key covers; a stored
//! result a fresh check contradicts fails the run and empties the store.

use std::path::{Path, PathBuf};

use sqldiff_convert::job::{convert_into, Job};
use sqldiff_convert::tree::{self, Hooks, Recheck, TemplateMode};
use sqldiff_core::Dialect;

const FILES: &[(&str, &str)] = &[
    (
        "tables/t.sql",
        "CREATE TABLE <%S%>.<%T%> (a int NOT NULL, b varchar(10) NULL);\n",
    ),
    ("dml/u.sql", "UPDATE <%S%>.<%T%> SET a = 1 WHERE b = 'x';\n"),
    (
        "views/v.sql",
        "CREATE VIEW <%S%>.<%T%>_v AS SELECT a FROM <%S%>.<%T%> WHERE a > 1;\n",
    ),
    (
        "procs/head.sql",
        "CREATE PROCEDURE <%S%>.<%T%>_p AS\nBEGIN\n  SET NOCOUNT ON;\n",
    ),
    ("procs/foot.sql", "  UPDATE <%S%>.<%T%> SET a = 2;\nEND\n"),
];

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir =
            std::env::temp_dir().join(format!("sqldiff-tstore-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (p, text) in FILES {
            let f = dir.join("src").join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        Fixture { dir }
    }

    fn job(&self, out: &str, overwrite: bool, fresh: bool) -> Job {
        let def: sqldiff_source::pack::TemplateSyntax = serde_json::from_value(serde_json::json!({
            "open": "<%",
            "close": "%>",
            "pairs": [{ "head": "procs/head.sql", "foot": "procs/foot.sql" }],
        }))
        .unwrap();
        Job {
            tree: tree::Args {
                rules: Default::default(),
                src: self.dir.join("src"),
                from: Dialect::Tsql,
                to: Dialect::Postgres,
                jobs: Some(2),
                no_catalog: false,
                recheck: Recheck::default(),
                template: Some(TemplateMode::new(&def, None).unwrap()),
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

/// Every file under `dir` except the store, with timings and reuse marks
/// taken out.
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

fn results(out: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(out.join(".sqldiff/results"))
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

fn recheck_all(mut j: Job) -> Job {
    j.tree.recheck = Recheck::None;
    j
}

#[test]
fn a_second_run_reuses_the_jobs_and_gives_the_same_tree() {
    let f = Fixture::new("same");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    // Four jobs: three templates and one pair.
    assert_eq!(results(&f.out("o")).len(), 4);
    assert!(reused(&f.out("o")).iter().all(|(_, r)| !r), "first run");
    let first = tree_of(&f.out("o"));
    convert_into(&f.job("o", true, false), &Hooks::default()).unwrap();
    assert_eq!(first, tree_of(&f.out("o")), "a resumed run differs");
    let r = reused(&f.out("o"));
    assert!(r.iter().filter(|(_, x)| *x).count() >= 3, "{r:?}");
    // The pair is reused as one job: both halves or neither.
    let by: std::collections::BTreeMap<_, _> = r.iter().cloned().collect();
    assert_eq!(by["procs/head.sql"], by["procs/foot.sql"], "{r:?}");
    // A fresh run gives the same tree and reuses nothing.
    convert_into(&f.job("o", true, true), &Hooks::default()).unwrap();
    assert_eq!(first, tree_of(&f.out("o")));
    assert!(reused(&f.out("o")).iter().all(|(_, r)| !r));
}

#[test]
fn recheck_none_reuses_every_job() {
    let f = Fixture::new("none");
    convert_into(&recheck_all(f.job("o", false, false)), &Hooks::default()).unwrap();
    let first = tree_of(&f.out("o"));
    convert_into(&recheck_all(f.job("o", true, false)), &Hooks::default()).unwrap();
    assert_eq!(first, tree_of(&f.out("o")));
    assert!(reused(&f.out("o")).iter().all(|(_, r)| *r));
}

#[test]
fn an_edited_template_is_checked_again_and_the_tree_equals_a_fresh_one() {
    let f = Fixture::new("edit");
    convert_into(&recheck_all(f.job("o", false, false)), &Hooks::default()).unwrap();
    f.write("dml/u.sql", "UPDATE <%S%>.<%T%> SET a = 3 WHERE b = 'y';\n");
    convert_into(&recheck_all(f.job("o", true, false)), &Hooks::default()).unwrap();
    let resumed = tree_of(&f.out("o"));
    let r = reused(&f.out("o"));
    let by: std::collections::BTreeMap<_, _> = r.iter().cloned().collect();
    assert!(!by["dml/u.sql"], "{r:?}");
    // The catalogs did not change (an UPDATE defines nothing), so the rest
    // is reused.
    assert!(by["tables/t.sql"] && by["views/v.sql"], "{r:?}");
    convert_into(&f.job("fresh", false, true), &Hooks::default()).unwrap();
    let fresh = tree_of(&f.out("fresh"));
    assert_eq!(resumed.len(), fresh.len());
    for ((pa, a), (pb, b)) in resumed.iter().zip(&fresh) {
        assert_eq!(pa, pb);
        if pa == "conversion-report.json" {
            // Everything but where the tree was written and when.
            let read = |o: &str| -> serde_json::Value {
                let mut v: serde_json::Value = serde_json::from_slice(
                    &std::fs::read(f.out(o).join("conversion-report.json")).unwrap(),
                )
                .unwrap();
                for k in ["out", "run"] {
                    v.as_object_mut().unwrap().remove(k);
                }
                v
            };
            assert_eq!(read("o"), read("fresh"), "{pa}");
        } else if !pa.starts_with("conversion-report") && !pa.starts_with("review-queue") {
            assert_eq!(a, b, "{pa}");
        }
    }
}

#[test]
fn a_changed_table_changes_every_key_which_is_conservative_and_correct() {
    let f = Fixture::new("catalog");
    convert_into(&recheck_all(f.job("o", false, false)), &Hooks::default()).unwrap();
    f.write(
        "tables/t.sql",
        "CREATE TABLE <%S%>.<%T%> (a int NOT NULL, b varchar(10) NULL, c int NULL);\n",
    );
    convert_into(&recheck_all(f.job("o", true, false)), &Hooks::default()).unwrap();
    let r = reused(&f.out("o"));
    assert!(r.iter().all(|(_, x)| !x), "{r:?}");
}

#[test]
fn a_stored_result_that_a_fresh_check_contradicts_fails_the_run_and_empties_the_store() {
    let f = Fixture::new("mismatch");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    for p in results(&f.out("o")) {
        let raw = std::fs::read(&p).unwrap();
        let mut e: serde_json::Value = serde_json::from_slice(&raw[65..]).unwrap();
        e["conversion"]["output"] = "-- tampered\n".into();
        let body = serde_json::to_vec(&e).unwrap();
        use sha2::{Digest, Sha256};
        let hex: String = Sha256::digest(&body)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        std::fs::write(&p, [hex.as_bytes(), b"\n", &body].concat()).unwrap();
    }
    let mut j = f.job("o", true, false);
    j.tree.recheck = Recheck::All;
    let err = convert_into(&j, &Hooks::default())
        .err()
        .expect("a mismatch fails the run");
    assert!(err.contains("differs from a fresh check"), "{err}");
    assert!(results(&f.out("o")).is_empty(), "the store is emptied");
    convert_into(&f.job("o", true, false), &Hooks::default()).unwrap();
    assert_eq!(results(&f.out("o")).len(), 4);
}
