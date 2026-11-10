//! The fixture, the comparisons and the one-edit run shared by the replay
//! tests (`replay_equals_fresh`, `site_switch`).
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sqldiff_convert::job::{convert_into, Job};
use sqldiff_convert::plan::Select;
use sqldiff_convert::tree::{self, Hooks};
use sqldiff_core::Dialect;

use super::edits::{Edit, Model, Rng};

pub struct Fixture {
    pub dir: PathBuf,
    pub to: Dialect,
    /// Set by `write` from the model: the job checks without a catalog.
    pub no_catalog: std::cell::Cell<bool>,
    /// Set by `write` from the model: the target table T0 is hand-written
    /// (a ledger record) in every output folder a run writes.
    pub hand: std::cell::Cell<bool>,
}

impl Fixture {
    pub fn new(name: &str, to: Dialect) -> Fixture {
        // Tests run in parallel: a counter keeps their folders apart.
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("sqldiff-t1-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Fixture {
            dir,
            to,
            no_catalog: std::cell::Cell::new(false),
            hand: std::cell::Cell::new(false),
        }
    }

    /// Writes the model's tree and base catalogs, replacing what was there.
    pub fn write(&self, m: &Model) {
        self.no_catalog.set(m.no_catalog);
        self.hand.set(m.hand);
        let src = self.dir.join("src");
        let _ = std::fs::remove_dir_all(&src);
        for (p, text) in m.files() {
            let f = src.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        let base = self.dir.join("base");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        for (i, t) in m.base_files(Dialect::Tsql).iter().enumerate() {
            std::fs::write(base.join(format!("a{i}.sql")), t).unwrap();
        }
        for (i, t) in m.base_files(self.to).iter().enumerate() {
            std::fs::write(base.join(format!("b{i}.sql")), t).unwrap();
        }
    }

    /// The base catalog files of one side that `write` made, in order.
    fn catalogs(&self, side: &str) -> Vec<PathBuf> {
        (0..)
            .map(|i| self.dir.join(format!("base/{side}{i}.sql")))
            .take_while(|p| p.is_file())
            .collect()
    }

    pub fn job(&self, out: &str, fresh: bool) -> Job {
        Job {
            tree: tree::Args {
                rules: Default::default(),
                src: self.dir.join("src"),
                from: Dialect::Tsql,
                to: self.to,
                jobs: Some(2),
                no_catalog: self.no_catalog.get(),
                // No sampled re-check: it would catch some stale reuse by
                // failing the run, depending on the tree's key, and these
                // tests prove the keys by comparing outputs.
                recheck: tree::Recheck::None,
                template: None,
                extensions: tree::extensions(Vec::new()),
                helpers: Vec::new(),
                deployment: Default::default(),
                catalogs: self.catalogs("a"),
                catalogs_b: self.catalogs("b"),
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

    pub fn run(&self, out: &str, fresh: bool) {
        self.try_run(out, fresh)
            .unwrap_or_else(|e| panic!("convert into {out}: {e}"));
    }

    /// [`run`](Self::run) that returns the run's error (a store mismatch
    /// among them).
    pub fn try_run(&self, out: &str, fresh: bool) -> Result<(), String> {
        if self.hand.get() {
            self.write_hand(out)?;
        }
        convert_into(&self.job(out, fresh), &Hooks::default()).map(|_| ())
    }
}

impl Fixture {
    /// Records, in the output folder, a hand-written text for the table T0
    /// of the tree: the rules' output with one more column. The source
    /// table does not change, the target table does: what a dependent reads
    /// of the target table is no function of what it reads of the source's.
    /// (Once per folder; a folder that has the record keeps it.)
    fn write_hand(&self, out: &str) -> Result<(), String> {
        use sqldiff_convert::review::{append, By, LEDGER_FILE};
        let dir = self.dir.join(out);
        if dir.join(LEDGER_FILE).exists() {
            return Ok(());
        }
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let a = self.job(out, false).tree;
        let (inputs, unreadable) = tree::read(&a)?;
        let t = tree::convert(&a, &inputs, unreadable, None)?;
        let path = "tables/dbo.T0.sql";
        let k = t
            .report
            .files
            .iter()
            .position(|f| f.path == path)
            .ok_or("no tables/dbo.T0.sql")?;
        let c = &t.conversions[k];
        let u = c
            .units
            .iter()
            .position(|u| c.output[u.out.start..u.out.end].contains("CREATE TABLE"))
            .ok_or("no CREATE TABLE unit")?;
        let unit = t.report.files[k].units[u].id.clone();
        let old = &c.output[c.units[u].out.start..c.units[u].out.end];
        let at = old.rfind(')').ok_or("no closing parenthesis")?;
        let text = format!("{},\n  C9 integer NULL{}", old[..at].trim_end(), &old[at..]);
        let r = tree::written_record(
            &t,
            &inputs,
            path,
            &unit,
            &text,
            By::Human,
            "tester",
            "one more column",
            "2026-11-08T12:00:00Z",
        )?;
        append(&dir, &r)
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
pub fn plain(v: &mut serde_json::Value, top: bool) {
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
pub fn tree_of(dir: &Path) -> Vec<(String, String)> {
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

pub fn report(out: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(out.join("conversion-report.json")).unwrap()).unwrap()
}

/// `(files reused, files in the report)` of a run, and the files it checked
/// again.
pub fn reuse(out: &Path) -> ((usize, usize), Vec<String>) {
    let j = report(out);
    let files = j["run"]["files"]
        .as_array()
        .expect("the run lists its files");
    let reused = files.iter().filter(|f| f["reused"] == true).count();
    let again = files
        .iter()
        .filter(|f| f["reused"] != true)
        .map(|f| f["path"].as_str().unwrap().to_string())
        .collect();
    ((reused, files.len()), again)
}

/// What a file's check gave: its report entry and its converted text.
pub fn per_file(out: &Path) -> BTreeMap<String, String> {
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
pub fn around(s: &str, at: usize) -> String {
    let b = s.as_bytes();
    let (lo, hi) = (at.saturating_sub(150), (at + 250).min(b.len()));
    String::from_utf8_lossy(&b[lo..hi]).into_owned()
}

pub fn compare(a: &[(String, String)], b: &[(String, String)], what: &str) {
    if let Err(e) = try_compare(a, b, what) {
        panic!("{e}");
    }
}

/// [`compare`] that says what differs instead of panicking.
pub fn try_compare(
    a: &[(String, String)],
    b: &[(String, String)],
    what: &str,
) -> Result<(), String> {
    // A source file that was dropped leaves its old output in a reused output
    // folder, with or without reuse (the folder is not cleaned); only the
    // files of the fresh run are compared, and each must be in the resumed one.
    let a: Vec<&(String, String)> = a.iter().filter(|x| b.iter().any(|y| y.0 == x.0)).collect();
    let names = |v: Vec<&(String, String)>| v.iter().map(|x| x.0.clone()).collect::<Vec<_>>();
    if names(a.clone()) != names(b.iter().collect()) {
        return Err(format!("{what}: the files differ"));
    }
    for (x, y) in a.into_iter().zip(b) {
        if x.1 != y.1 {
            let at =
                x.1.bytes()
                    .zip(y.1.bytes())
                    .position(|(p, q)| p != q)
                    .unwrap_or(0);
            return Err(format!(
                "{what}: {} differs at {at}\n resumed: {}\n fresh:   {}",
                x.0,
                around(&x.1, at),
                around(&y.1, at)
            ));
        }
    }
    Ok(())
}

/// The draft entries of a run's store, by path below the output folder.
pub fn draft_entries(out: &Path) -> std::collections::BTreeSet<String> {
    let root = out.join(".sqldiff/draft-entries");
    let mut found = std::collections::BTreeSet::new();
    for k in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        for f in std::fs::read_dir(k.path()).into_iter().flatten().flatten() {
            if f.path().extension().is_some_and(|e| e == "json") {
                found.insert(format!(
                    "{}/{}",
                    k.file_name().to_string_lossy(),
                    f.file_name().to_string_lossy()
                ));
            }
        }
    }
    found
}

/// What one edit showed.
pub struct Outcome {
    /// Drafts the resumed run built (stored entries it did not have), and
    /// the files in the tree: a draft taken from a stored entry writes none.
    pub drafts_built: (usize, usize),
    /// Files the resumed run took from the store, and files it listed.
    pub reused: (usize, usize),
    /// The files it checked again.
    pub again: Vec<String>,
    /// Files whose own text is the same after the edit but whose fresh
    /// output is not: the files that could be reused stale.
    pub dependents_changed: Vec<String>,
    /// Whether the resumed tree and reports equal the fresh run's.
    pub verdict: Result<(), String>,
}

/// One edit: store a run, edit, resume, run fresh, compare.
pub fn one(to: Dialect, seed: u64, edit: &Edit) -> Outcome {
    let o = try_one(to, seed, edit);
    if let Err(e) = &o.verdict {
        panic!("{e}");
    }
    o
}

/// [`one`] that reports a resume that differs from a fresh run in `verdict`
/// instead of panicking.
pub fn try_one(to: Dialect, seed: u64, edit: &Edit) -> Outcome {
    try_one_in(to, seed, edit, false)
}

/// [`try_one`] on a tree that has a system-versioned table from the start
/// when `hidden`.
pub fn try_one_in(to: Dialect, seed: u64, edit: &Edit, hidden: bool) -> Outcome {
    let tag = format!("{to:?}-{seed}-{edit:?}-{hidden}");
    let f = Fixture::new(&tag, to);
    let mut model = Model::generate_with(seed, hidden);
    f.write(&model);
    f.run("res", false);
    let before = per_file(&f.dir.join("res"));
    let drafts_before = draft_entries(&f.dir.join("res"));
    let old_files = model.files();
    let mut r = Rng::new(seed ^ 0xA5A5);
    let what = edit.apply(&mut model, &mut r);
    f.write(&model);
    if let Err(e) = f.try_run("res", false) {
        return Outcome {
            drafts_built: (0, 0),
            reused: (0, 0),
            again: Vec::new(),
            dependents_changed: Vec::new(),
            verdict: Err(format!("{tag}: the resumed run failed: {e}")),
        };
    }
    f.run("fre", true);
    let label = format!("{to:?} seed {seed}, edit {edit:?}: {what}");
    let verdict = try_compare(
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
    let (reused, again) = reuse(&f.dir.join("res"));
    let drafts_built = (
        draft_entries(&f.dir.join("res"))
            .difference(&drafts_before)
            .count(),
        reused.1,
    );
    Outcome {
        drafts_built,
        reused,
        again,
        dependents_changed,
        verdict,
    }
}

/// Two edits one after the other, each resumed on the store the one before
/// left, then a fresh run: the stored entries of a resumed run (made after
/// the first edit) are what the second resume takes. Whether the second
/// resume equals the fresh run is the verdict.
pub fn try_two_in(
    to: Dialect,
    seed: u64,
    first: &Edit,
    second: &Edit,
    hidden: bool,
) -> Result<(), String> {
    let tag = format!("{to:?}-{seed}-{first:?}-{second:?}-{hidden}");
    let f = Fixture::new(&tag, to);
    let mut model = Model::generate_with(seed, hidden);
    f.write(&model);
    f.run("res", false);
    let mut what = String::new();
    for e in [first, second] {
        let mut r = Rng::new(seed ^ 0xA5A5);
        what.push_str(&format!("{e:?}: {}; ", e.apply(&mut model, &mut r)));
        f.write(&model);
        f.try_run("res", false)
            .map_err(|e| format!("{tag}: the resumed run failed: {e}"))?;
    }
    f.run("fre", true);
    try_compare(
        &tree_of(&f.dir.join("res")),
        &tree_of(&f.dir.join("fre")),
        &format!("{to:?} seed {seed}, edits {what}"),
    )
}
