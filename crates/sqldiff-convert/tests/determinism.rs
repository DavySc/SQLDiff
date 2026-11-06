//! Determinism: the same tree converted with any thread count, fresh or
//! resumed from a store another run made, in any split into increments and
//! into any output folder gives byte-identical files and a report that
//! differs only in `run` (timings and reuse marks) and the folder names.

use std::path::{Path, PathBuf};

use sqldiff_convert::job::{convert_into, Job};
use sqldiff_convert::plan::Select;
use sqldiff_convert::tree::{self, Hooks};
use sqldiff_core::Dialect;

const FILES: &[(&str, &str)] = &[
    ("tables/t.sql", "CREATE TABLE dbo.t (a int NOT NULL, b varchar(20) NULL);\n"),
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
    (
        "procs/branchy.sql",
        "CREATE PROCEDURE dbo.branchy @x int AS\nBEGIN\n  UPDATE dbo.t SET a = 1 WHERE a = @x;\n  IF @@ROWCOUNT = 0\n    INSERT INTO dbo.t (a) VALUES (@x);\n  IF @x > 5\n    UPDATE dbo.t SET b = 'big' WHERE a = @x;\n  ELSE\n    UPDATE dbo.t SET b = 'small' WHERE a = @x;\nEND\n",
    ),
    (
        "procs/cursor.sql",
        "CREATE PROCEDURE dbo.cur AS\nBEGIN\n  DECLARE @v int;\n  DECLARE c CURSOR FOR SELECT a FROM dbo.t;\n  OPEN c;\n  FETCH NEXT FROM c INTO @v;\n  WHILE @@FETCH_STATUS = 0\n  BEGIN\n    UPDATE dbo.t SET b = 'x' WHERE a = @v;\n    FETCH NEXT FROM c INTO @v;\n  END\n  CLOSE c;\n  DEALLOCATE c;\nEND\n",
    ),
    ("load.sql", "INSERT INTO dbo.t (a) VALUES (1);\n"),
];

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("sqldiff-det-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (p, text) in FILES {
            let f = dir.join("src").join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        Fixture { dir }
    }

    fn job(&self, out: &str, jobs: usize, select: Select) -> Job {
        Job {
            tree: tree::Args {
                rules: Default::default(),
                src: self.dir.join("src"),
                from: Dialect::Tsql,
                to: Dialect::Postgres,
                jobs: Some(jobs),
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

/// A JSON value without what differs between runs by design: the `run`
/// object, and what only describes an increment or a folder.
fn plain(v: &mut serde_json::Value, dir: &str) {
    match v {
        serde_json::Value::Object(m) => {
            for k in [
                "run",
                "checked",
                "progress",
                "files_not_checked",
                "out",
                "src",
            ] {
                m.remove(k);
            }
            m.values_mut().for_each(|x| plain(x, dir));
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(|x| plain(x, dir)),
        serde_json::Value::String(s) => *s = s.replace(dir, "<dir>"),
        _ => {}
    }
}

/// Every file under `dir` except the store, relative, with its text; JSON
/// reports without what may differ (markdown reports compared whole, the
/// folder spelt `<dir>`).
fn tree_of(dir: &Path, root: &Path) -> Vec<(String, String)> {
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
            let text = if rel.ends_with(".json") {
                let mut j: serde_json::Value = serde_json::from_str(&text).unwrap();
                plain(&mut j, &dir.display().to_string());
                plain(&mut j, &root.display().to_string());
                j.to_string()
            } else {
                text.replace(&dir.display().to_string(), "<dir>")
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
                "{what}: {} differs at {at}\n left:  {}\n right: {}",
                x.0,
                &x.1[lo..hi.min(x.1.len())],
                &y.1[lo..hi.min(y.1.len())]
            );
        }
    }
}

fn run(f: &Fixture, out: &str, jobs: usize, select: Select, fresh: bool) {
    let mut job = f.job(out, jobs, select);
    job.fresh = fresh;
    convert_into(&job, &Hooks::default()).unwrap();
}

#[test]
fn thread_count_store_and_folder_never_change_the_output() {
    let f = Fixture::new("matrix");
    run(&f, "a", 1, Select::default(), true);
    let base = tree_of(&f.out("a"), &f.dir);
    // Fresh with more threads, into another folder.
    run(&f, "b", 4, Select::default(), true);
    same(
        &base,
        &tree_of(&f.out("b"), &f.dir),
        "-j4 fresh, other folder",
    );
    // Resumed from the store another thread count made, both ways.
    run(&f, "a", 4, Select::default(), false);
    same(
        &base,
        &tree_of(&f.out("a"), &f.dir),
        "-j4 resumed on a -j1 store",
    );
    run(&f, "b", 1, Select::default(), false);
    same(
        &base,
        &tree_of(&f.out("b"), &f.dir),
        "-j1 resumed on a -j4 store",
    );
    // Resumed again and again: the re-check samples differ per run, the
    // output does not.
    for n in 0..4 {
        run(&f, "a", 1 + n % 3, Select::default(), false);
        same(&base, &tree_of(&f.out("a"), &f.dir), "repeated resume");
    }
    // Increments with alternating thread counts end where the full run does.
    for step in 0..FILES.len() {
        run(&f, "inc", if step % 2 == 0 { 1 } else { 3 }, next(1), false);
    }
    run(&f, "inc", 2, Select::default(), false);
    same(&base, &tree_of(&f.out("inc"), &f.dir), "increments");
}

#[test]
fn the_re_check_seed_is_not_the_clock() {
    // The same store history gives the same seed, a different run count a
    // different one, whatever time it is.
    let key = {
        let mut k = sqldiff_convert::store::TreeKeyBuilder::new();
        k.field("x", b"1");
        k.finish()
    };
    assert_eq!(
        sqldiff_convert::store::seed_of(&key, 3),
        sqldiff_convert::store::seed_of(&key, 3)
    );
    assert_ne!(
        sqldiff_convert::store::seed_of(&key, 3),
        sqldiff_convert::store::seed_of(&key, 4)
    );
}

#[test]
fn a_run_cancelled_before_it_starts_leaves_the_folder_as_it_was() {
    use std::sync::{atomic::AtomicBool, Arc};
    let f = Fixture::new("cancel");
    let hooks = Hooks {
        progress: None,
        cancel: Some(Arc::new(AtomicBool::new(true))),
    };
    let _ = convert_into(&f.job("c", 1, Select::default()), &hooks);
    let o = f.out("c");
    assert!(
        !o.join(".sqldiff").join("run-counter").exists(),
        "a cancelled run recorded itself"
    );
}
