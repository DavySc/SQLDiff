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

fn no_recheck(mut j: Job) -> Job {
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
    convert_into(&no_recheck(f.job("o", false, false)), &Hooks::default()).unwrap();
    let first = tree_of(&f.out("o"));
    convert_into(&no_recheck(f.job("o", true, false)), &Hooks::default()).unwrap();
    assert_eq!(first, tree_of(&f.out("o")));
    assert!(reused(&f.out("o")).iter().all(|(_, r)| *r));
}

#[test]
fn an_edited_template_is_checked_again_and_the_tree_equals_a_fresh_one() {
    let f = Fixture::new("edit");
    convert_into(&no_recheck(f.job("o", false, false)), &Hooks::default()).unwrap();
    f.write("dml/u.sql", "UPDATE <%S%>.<%T%> SET a = 3 WHERE b = 'y';\n");
    convert_into(&no_recheck(f.job("o", true, false)), &Hooks::default()).unwrap();
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
    convert_into(&no_recheck(f.job("o", false, false)), &Hooks::default()).unwrap();
    f.write(
        "tables/t.sql",
        "CREATE TABLE <%S%>.<%T%> (a int NOT NULL, b varchar(10) NULL, c int NULL);\n",
    );
    convert_into(&no_recheck(f.job("o", true, false)), &Hooks::default()).unwrap();
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

fn select_next(n: usize) -> sqldiff_convert::plan::Select {
    sqldiff_convert::plan::Select {
        next: Some(n),
        ..Default::default()
    }
}

/// The conversion report without where and when it was made.
fn report_of(out: &Path) -> serde_json::Value {
    let mut v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(out.join("conversion-report.json")).unwrap())
            .unwrap();
    for k in ["out", "run", "progress"] {
        v.as_object_mut().unwrap().remove(k);
    }
    if let Some(t) = v["totals"].as_object_mut() {
        t.remove("progress");
    }
    v
}

fn outputs_of(dir: &Path) -> Vec<(String, String)> {
    tree_of(dir)
        .into_iter()
        .filter(|(p, _)| !p.starts_with("conversion-report") && !p.starts_with("review-queue"))
        .collect()
}

#[test]
fn chunked_increments_end_with_the_tree_of_a_full_run() {
    let f = Fixture::new("chunks");
    convert_into(&f.job("full", false, true), &Hooks::default()).unwrap();
    let mut j = f.job("inc", false, false);
    j.select = select_next(1);
    convert_into(&j, &Hooks::default()).unwrap();
    // After the first chunk something is not checked, and nothing reads as
    // better converted than in the full run.
    let first = report_of(&f.out("inc"));
    assert!(first["totals"]["files_not_checked"].as_u64().unwrap() > 0);
    let mut last = None;
    for _ in 0..8 {
        let mut j = f.job("inc", true, false);
        j.select = select_next(1);
        convert_into(&j, &Hooks::default()).unwrap();
        last = Some(report_of(&f.out("inc")));
    }
    let last = last.unwrap();
    assert_eq!(last["totals"]["files_not_checked"].as_u64().unwrap_or(0), 0);
    assert_eq!(outputs_of(&f.out("full")), outputs_of(&f.out("inc")));
    assert_eq!(report_of(&f.out("full")), last);
}

#[test]
fn a_pair_is_one_job_in_an_increment() {
    let f = Fixture::new("pairinc");
    let mut j = f.job("o", false, false);
    j.select = sqldiff_convert::plan::Select {
        paths: vec!["procs/head.sql".into()],
        ..Default::default()
    };
    convert_into(&j, &Hooks::default()).unwrap();
    let r = report_of(&f.out("o"));
    let checked = |p: &str| {
        r["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["path"] == p)
            .map(|x| x["checked"].as_bool().unwrap_or(true))
            .unwrap()
    };
    // Selecting one half checks both; the others are not checked.
    assert!(checked("procs/head.sql") && checked("procs/foot.sql"));
    assert!(!checked("dml/u.sql") && !checked("views/v.sql"));
}

#[test]
fn a_plan_of_a_template_tree_sees_names_with_placeholders() {
    let f = Fixture::new("plan");
    let j = f.job("o", false, false);
    let (plan, _) = sqldiff_convert::job::plan_into(&j.tree, &f.out("o")).unwrap();
    // The view uses the table through placeholders: it comes after it.
    let at = |p: &str| {
        plan.groups
            .iter()
            .position(|g| g.files.iter().any(|x| plan.files[*x].path == p))
            .unwrap()
    };
    assert!(at("tables/t.sql") < at("views/v.sql"), "{plan:?}");
}

#[test]
fn hand_written_output_in_the_ledger_is_never_stored_and_wins_on_a_resume() {
    use sqldiff_convert::review::{append, By};
    let f = Fixture::new("ledger");
    convert_into(&f.job("o", false, false), &Hooks::default()).unwrap();
    // Record a hand-written text for the unit of dml/u.sql.
    let j = f.job("o", true, false);
    let (inputs, unread) = tree::read(&j.tree).unwrap();
    let t = tree::convert(&j.tree, &inputs, unread, None).unwrap();
    let k = t
        .report
        .files
        .iter()
        .position(|x| x.path == "dml/u.sql")
        .unwrap();
    let id = t.report.files[k].units[0].id.clone();
    let r = tree::written_record(
        &t,
        &inputs,
        "dml/u.sql",
        &id,
        "UPDATE <%S%>.<%T%> SET a = 99;",
        By::Human,
        "dana",
        "by hand",
        "2026-11-07T12:00:00Z",
    )
    .unwrap();
    append(&f.out("o"), &r).unwrap();
    let before = results(&f.out("o")).len();
    convert_into(&no_recheck(f.job("o", true, false)), &Hooks::default()).unwrap();
    let resumed = outputs_of(&f.out("o"));
    let text = &resumed
        .iter()
        .find(|(p, _)| p.ends_with("u.sql"))
        .unwrap()
        .1;
    assert!(text.contains("99"), "{text}");
    // Its job was not stored; the others were reused.
    let by: std::collections::BTreeMap<_, _> = reused(&f.out("o")).into_iter().collect();
    assert!(!by["dml/u.sql"], "{by:?}");
    assert!(by["tables/t.sql"], "{by:?}");
    // Its old result is swept as no key of this tree; nothing new is stored.
    assert_eq!(results(&f.out("o")).len(), before - 1);
    // A fresh run gives the same files.
    convert_into(&f.job("o", true, true), &Hooks::default()).unwrap();
    assert_eq!(resumed, outputs_of(&f.out("o")));
}
