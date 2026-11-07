//! T1 and T2 of the recorded-reads design (W4.3): after a catalog edit, a
//! resume on the old store gives byte-for-byte the tree and report that
//! `--fresh` gives, and the report says how many files it reused.
//!
//! Today a result's key holds the digest of both whole catalogs, so an edit
//! that changes a catalog reuses nothing and this test passes without
//! reusing; it is the gate the replay of recorded reads (PR 3b) has to pass
//! while reusing. The edits come from `support::edits`, a generator that
//! knows nothing about any corpus; every failure prints the seed and the
//! edit.

mod support;

use std::path::{Path, PathBuf};

use sqldiff_convert::job::{convert_into, Job};
use sqldiff_convert::plan::Select;
use sqldiff_convert::tree::{self, Hooks};
use sqldiff_core::Dialect;
use support::edits::{Edit, Model, Rng, ALL_EDITS};

/// The seeds the test runs; a failure names the one that broke.
const SEEDS: &[u64] = &[7, 21];

struct Fixture {
    dir: PathBuf,
    to: Dialect,
}

impl Fixture {
    fn new(name: &str, to: Dialect) -> Fixture {
        let dir = std::env::temp_dir().join(format!("sqldiff-t1-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Fixture { dir, to }
    }

    /// Writes the model's tree and base catalogs, replacing what was there.
    fn write(&self, m: &Model) {
        let src = self.dir.join("src");
        let _ = std::fs::remove_dir_all(&src);
        for (p, text) in m.files() {
            let f = src.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        let base = self.dir.join("base");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("a.sql"), m.base_catalog(Dialect::Tsql)).unwrap();
        std::fs::write(base.join("b.sql"), m.base_catalog(self.to)).unwrap();
    }

    fn job(&self, out: &str, fresh: bool) -> Job {
        Job {
            tree: tree::Args {
                rules: Default::default(),
                src: self.dir.join("src"),
                from: Dialect::Tsql,
                to: self.to,
                jobs: Some(2),
                no_catalog: false,
                recheck: Default::default(),
                template: None,
                extensions: tree::extensions(Vec::new()),
                helpers: Vec::new(),
                deployment: Default::default(),
                catalogs: vec![self.dir.join("base/a.sql")],
                catalogs_b: vec![self.dir.join("base/b.sql")],
            },
            out: self.dir.join(out),
            overwrite: true,
            discard_edits: false,
            fresh,
            select: Select::default(),
            instances: None,
            queue_all: false,
            proof: Default::default(),
            row_counts: false,
        }
    }

    fn run(&self, out: &str, fresh: bool) {
        convert_into(&self.job(out, fresh), &Hooks::default())
            .unwrap_or_else(|e| panic!("convert into {out}: {e}"));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A JSON value without what differs between runs by design.
fn plain(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(m) => {
            for k in ["run", "checked", "progress", "files_not_checked", "out"] {
                m.remove(k);
            }
            m.values_mut().for_each(plain);
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(plain),
        _ => {}
    }
}

/// Every file under `dir` except the store and the markdown reports.
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
            if rel.starts_with(".sqldiff") || rel.ends_with(".md") {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap();
            let text = if rel.ends_with(".json") {
                let mut j: serde_json::Value = serde_json::from_str(&text).unwrap();
                plain(&mut j);
                j.to_string()
            } else {
                text
            };
            v.push((rel, text.replace("\r\n", "\n")));
        }
    }
    v.sort();
    v
}

/// `(files reused, files in the report)` of a run.
fn reuse(out: &Path) -> (usize, usize) {
    let j: serde_json::Value =
        serde_json::from_slice(&std::fs::read(out.join("conversion-report.json")).unwrap())
            .unwrap();
    let files = j["run"]["files"]
        .as_array()
        .expect("the run lists its files");
    let reused = files.iter().filter(|f| f["reused"] == true).count();
    (reused, files.len())
}

fn compare(a: &[(String, String)], b: &[(String, String)], what: &str) {
    // A source file that was dropped leaves its old output in a reused output
    // folder, with or without reuse (the folder is not cleaned); only the
    // files of the fresh run are compared, and each must be in the resumed one.
    let a: Vec<&(String, String)> = a.iter().filter(|x| b.iter().any(|y| y.0 == x.0)).collect();
    let names = |v: Vec<&(String, String)>| v.iter().map(|x| x.0.clone()).collect::<Vec<_>>();
    assert_eq!(
        names(a.clone()),
        names(b.iter().collect()),
        "{what}: the files differ"
    );
    for (x, y) in a.into_iter().zip(b) {
        if x.1 != y.1 {
            let at =
                x.1.bytes()
                    .zip(y.1.bytes())
                    .position(|(p, q)| p != q)
                    .unwrap_or(0);
            let (lo, hi) = (at.saturating_sub(150), at + 250);
            panic!(
                "{what}: {} differs at {at}\n resumed: {}\n fresh:   {}",
                x.0,
                &x.1[lo..hi.min(x.1.len())],
                &y.1[lo..hi.min(y.1.len())]
            );
        }
    }
}

/// One edit: store a run, edit, resume, run fresh, compare. Returns the
/// reuse of the resumed run.
fn one(to: Dialect, seed: u64, edit: &Edit) -> (usize, usize) {
    let tag = format!("{:?}-{seed}-{edit:?}", to);
    let f = Fixture::new(&tag.replace([' ', '"', '{', '}', ':', ','], ""), to);
    let mut model = Model::generate(seed);
    f.write(&model);
    f.run("res", false);
    let mut r = Rng::new(seed ^ 0xA5A5);
    let what = edit.apply(&mut model, &mut r);
    f.write(&model);
    f.run("res", false);
    f.run("fre", true);
    let label = format!("{to:?} seed {seed}, edit {edit:?}: {what}");
    compare(
        &tree_of(&f.dir.join("res")),
        &tree_of(&f.dir.join("fre")),
        &label,
    );
    reuse(&f.dir.join("res"))
}

fn every_edit(to: Dialect) {
    let mut lines = Vec::new();
    for &seed in SEEDS {
        for e in ALL_EDITS {
            let (reused, total) = one(to, seed, e);
            assert!(
                reused <= total,
                "{to:?} seed {seed} {e:?}: {reused} > {total}"
            );
            lines.push(format!(
                "{to:?} seed {seed} {e:?}: reused {reused} of {total}"
            ));
        }
    }
    println!("{}", lines.join("\n"));
}

#[test]
fn a_resume_after_any_edit_equals_a_fresh_run_to_postgres() {
    every_edit(Dialect::Postgres);
}

#[test]
fn a_resume_after_any_edit_equals_a_fresh_run_to_db2i() {
    every_edit(Dialect::Db2i);
}

/// The generator itself: every edit changes the tree or the base catalogs,
/// except the ones that are no-ops by design, and it is deterministic.
#[test]
fn every_edit_changes_the_input_unless_it_is_a_no_op() {
    for &seed in SEEDS {
        for e in ALL_EDITS {
            let before = Model::generate(seed);
            let mut m = before.clone();
            let mut r = Rng::new(seed ^ 0xA5A5);
            let what = e.apply(&mut m, &mut r);
            let mut m2 = before.clone();
            let mut r2 = Rng::new(seed ^ 0xA5A5);
            assert_eq!(
                what,
                e.apply(&mut m2, &mut r2),
                "{e:?} is not deterministic"
            );
            let inputs = |m: &Model| {
                (
                    m.files(),
                    m.base_catalog(Dialect::Tsql),
                    m.base_catalog(Dialect::Postgres),
                )
            };
            let noop = matches!(e, Edit::NoOpReorderTables) || what.contains("no change");
            if noop {
                continue;
            }
            assert!(
                inputs(&before) != inputs(&m),
                "seed {seed}: {e:?} ({what}) changed nothing"
            );
        }
    }
}
