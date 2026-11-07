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

use std::collections::BTreeMap;
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
        // Tests run in parallel: a counter keeps their folders apart.
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("sqldiff-t1-{name}-{}-{n}", std::process::id()));
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

/// A JSON value without what differs between runs by design. At the top: the
/// `run` object and the two folder names; nothing below the top is masked, so
/// every unit's engine status and span is compared.
fn plain(v: &mut serde_json::Value, top: bool) {
    match v {
        serde_json::Value::Object(m) => {
            if top {
                for k in ["run", "src", "out"] {
                    m.remove(k);
                }
                if let Some(l) = m.get_mut("ledger").and_then(|l| l.as_object_mut()) {
                    l.remove("path");
                }
            }
            m.values_mut().for_each(|x| plain(x, false));
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(|x| plain(x, false)),
        _ => {}
    }
}

/// Every file under `dir` except the store and `conversion-report.md` (it
/// carries the run line); the other reports are compared whole.
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
            if rel.starts_with(".sqldiff") || rel == "conversion-report.md" {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap();
            let text = if rel.ends_with(".json") {
                let mut j: serde_json::Value = serde_json::from_str(&text).unwrap();
                plain(&mut j, true);
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

fn report(out: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(out.join("conversion-report.json")).unwrap()).unwrap()
}

/// `(files reused, files in the report)` of a run.
fn reuse(out: &Path) -> (usize, usize) {
    let j = report(out);
    let files = j["run"]["files"]
        .as_array()
        .expect("the run lists its files");
    let reused = files.iter().filter(|f| f["reused"] == true).count();
    (reused, files.len())
}

/// What a file's check gave: its report entry and its converted text.
fn per_file(out: &Path) -> BTreeMap<String, String> {
    let mut j = report(out);
    plain(&mut j, true);
    j["files"]
        .as_array()
        .expect("the report lists its files")
        .iter()
        .map(|f| {
            let path = f["path"].as_str().unwrap().to_string();
            let text = std::fs::read_to_string(out.join(&path)).unwrap_or_default();
            (path, format!("{f}\n{}", text.replace("\r\n", "\n")))
        })
        .collect()
}

/// A window of `s` around byte `at`, on character boundaries.
fn around(s: &str, at: usize) -> String {
    let b = s.as_bytes();
    let (lo, hi) = (at.saturating_sub(150), (at + 250).min(b.len()));
    String::from_utf8_lossy(&b[lo..hi]).into_owned()
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
            panic!(
                "{what}: {} differs at {at}\n resumed: {}\n fresh:   {}",
                x.0,
                around(&x.1, at),
                around(&y.1, at)
            );
        }
    }
}

/// What one edit showed.
struct Outcome {
    /// Files the resumed run took from the store, and files it listed.
    reused: (usize, usize),
    /// Files whose own text is the same after the edit but whose fresh
    /// output is not: the files that could be reused stale.
    dependents_changed: Vec<String>,
}

/// One edit: store a run, edit, resume, run fresh, compare.
fn one(to: Dialect, seed: u64, edit: &Edit) -> Outcome {
    let tag = format!("{to:?}-{seed}-{edit:?}");
    let f = Fixture::new(&tag, to);
    let mut model = Model::generate(seed);
    f.write(&model);
    f.run("res", false);
    let before = per_file(&f.dir.join("res"));
    let old_files = model.files();
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
    let after = per_file(&f.dir.join("fre"));
    let new_files = model.files();
    let same_text = |path: &String| {
        let text = |v: &[(String, String)]| v.iter().find(|x| &x.0 == path).map(|x| x.1.clone());
        text(&old_files).is_some() && text(&old_files) == text(&new_files)
    };
    let dependents_changed = before
        .iter()
        .filter(|(path, old)| same_text(path) && after.get(*path).is_some_and(|n| n != *old))
        .map(|(path, _)| path.clone())
        .collect();
    Outcome {
        reused: reuse(&f.dir.join("res")),
        dependents_changed,
    }
}

fn every_edit(to: Dialect) {
    let mut lines = Vec::new();
    // Per edit, whether some seed showed it on a dependent (the probe).
    let mut shown: BTreeMap<String, bool> = BTreeMap::new();
    for e in ALL_EDITS.iter().filter(|e| only(e)) {
        for &seed in SEEDS {
            let o = one(to, seed, e);
            let (reused, total) = o.reused;
            if e.is_no_op() {
                // The edited file is checked again, and one stored file in
                // fifty is checked again as a sample.
                assert!(
                    reused + 1 + total.div_ceil(50) >= total,
                    "{to:?} seed {seed} {e:?}: only {reused} of {total} reused"
                );
            } else {
                // Today the result key holds both whole catalog digests, so
                // an edit that changes a catalog reuses nothing. PR 3b
                // replaces this line with the files each edit leaves valid.
                assert_eq!(
                    reused, 0,
                    "{to:?} seed {seed} {e:?}: {reused} of {total} reused"
                );
                *shown.entry(format!("{e:?}")).or_default() |= !o.dependents_changed.is_empty();
            }
            lines.push(format!(
                "{to:?} seed {seed} {e:?}: reused {reused} of {total}, {} dependents change",
                o.dependents_changed.len()
            ));
        }
    }
    println!("{}", lines.join("\n"));
    // The probe that gives the test its meaning: stale reuse is only visible
    // when some file whose own text did not change comes out differently
    // after the edit. For every real edit, on some seed, one must. The edits
    // none can show are listed in `EXEMPT` with the reason, and the list is
    // compared both ways: an edit that starts to show must leave it, so it
    // cannot rot into a blanket excuse.
    let mut weak: Vec<String> = shown
        .into_iter()
        .filter(|(_, ok)| !ok)
        .map(|(e, _)| format!("{to:?} {e}"))
        .collect();
    let mut exempt: Vec<String> = EXEMPT
        .iter()
        .filter(|(d, _, _)| *d == to)
        .map(|(d, edit, _)| format!("{d:?} {edit}"))
        .collect();
    weak.sort();
    exempt.sort();
    if std::env::var("SQLDIFF_T1_ONLY").is_err() {
        assert_eq!(
            weak, exempt,
            "edits that change no dependent's output (stale reuse of them would pass T1) \
             against the documented exemptions"
        );
    }
}

/// `SQLDIFF_T1_ONLY=FlipNullability,ChangeDefault` limits the run to the
/// named edits (a comma list), for working on the generator.
fn only(e: &Edit) -> bool {
    std::env::var("SQLDIFF_T1_ONLY")
        .map(|v| v.split(',').any(|n| n == format!("{e:?}")))
        .unwrap_or(true)
}

/// Edits the probe cannot show on a dependent, with why.
const EXEMPT: &[(Dialect, &str, &str)] = &[
    (
        Dialect::Postgres,
        "ChangeDefault",
        "no rule reads a column default of another file's table (the default is read only in a table's own file, by the table diff); the recorded `Table` digest still holds it",
    ),
    (Dialect::Db2i, "ChangeDefault", "same"),
    (
        Dialect::Db2i,
        "ToggleIndex",
        "UPDATE ... FROM is never converted to Db2 for i, with or without a unique key",
    ),
    (
        Dialect::Db2i,
        "ChangeParameterType",
        "calls are compared only from T-SQL to PostgreSQL (the Db2 for i call claim is paused)",
    ),
    (
        Dialect::Db2i,
        "AddTrigger",
        "DROP TRIGGER is compared only from T-SQL to PostgreSQL",
    ),
    (Dialect::Db2i, "DropTrigger", "same"),
];

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
                    [Dialect::Tsql, Dialect::Postgres, Dialect::Db2i].map(|d| m.base_catalog(d)),
                )
            };
            if matches!(e, Edit::NoOpReorderTables) {
                continue;
            }
            assert!(
                inputs(&before) != inputs(&m),
                "seed {seed}: {e:?} ({what}) changed nothing"
            );
        }
    }
}

/// Writes seed `SQLDIFF_T1_DUMP_SEED`'s tree and base catalogs to
/// `SQLDIFF_T1_DUMP` for a look with the command line:
/// `SQLDIFF_T1_DUMP=/tmp/t1 cargo test -p sqldiff-convert --test replay_equals_fresh dump -- --ignored`
#[test]
#[ignore]
fn dump() {
    let Ok(dir) = std::env::var("SQLDIFF_T1_DUMP") else {
        return;
    };
    let seed = std::env::var("SQLDIFF_T1_DUMP_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(SEEDS[0]);
    let f = Fixture::new("dump", Dialect::Postgres);
    f.write(&Model::generate(seed));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::rename(f.dir.join("src"), format!("{dir}/src"))
        .or_else(|_| {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::rename(f.dir.join("src"), format!("{dir}/src"))
        })
        .unwrap();
    std::fs::rename(f.dir.join("base"), format!("{dir}/base")).unwrap();
}
