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
    // A table refused for its computed column stands in for binding; what
    // uses it is flagged, and the flags are stored with the result.
    (
        "tables/c.sql",
        "CREATE TABLE dbo.c (a int NOT NULL, custom nvarchar(max) NULL, other AS (json_query(custom, N'strict $.Other')));\n",
    ),
    (
        "procs/usec.sql",
        "CREATE PROCEDURE dbo.usec AS\nBEGIN\n  UPDATE dbo.c SET a = 1;\nEND\n",
    ),
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
        std::fs::write(
            dir.join("base.sql"),
            "CREATE TABLE dbo.ext (k int NOT NULL PRIMARY KEY, v varchar(10) NULL);\n",
        )
        .unwrap();
        Fixture { dir }
    }

    fn job(&self, out: &str, jobs: usize, select: Select, to: Dialect, catalogs: &[&str]) -> Job {
        Job {
            tree: tree::Args {
                rules: Default::default(),
                src: self.dir.join("src"),
                from: Dialect::Tsql,
                to,
                jobs: Some(jobs),
                no_catalog: false,
                recheck: Default::default(),
                template: None,
                extensions: tree::extensions(Vec::new()),
                helpers: Vec::new(),
                deployment: Default::default(),
                catalogs: catalogs.iter().map(|c| self.dir.join(c)).collect(),
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

/// One run of the matrix.
struct Case<'a> {
    to: Dialect,
    catalogs: &'a [&'a str],
}

/// A JSON value without what differs between runs by design. At the top: the
/// `run` object and the two folder names. In a report of an increment, also
/// what says which files it checked.
fn plain(v: &mut serde_json::Value, top: bool, increments: bool) {
    match v {
        serde_json::Value::Object(m) => {
            if top {
                for k in ["run", "src", "out"] {
                    m.remove(k);
                }
                // The ledger file is named by its path in the output folder.
                if let Some(l) = m.get_mut("ledger").and_then(|l| l.as_object_mut()) {
                    l.remove("path");
                }
            }
            if increments {
                for k in ["checked", "progress", "files_not_checked"] {
                    m.remove(k);
                }
            }
            m.values_mut().for_each(|x| plain(x, false, increments));
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(|x| plain(x, false, increments)),
        _ => {}
    }
}

/// Every file under `dir` except the store, relative, with its text; the
/// JSON reports without what may differ. Nothing else is masked: the
/// markdown reports and every reason compare whole.
fn tree_of(dir: &Path, increments: bool) -> Vec<(String, String)> {
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
            let text = if rel.ends_with(".json") && rel.starts_with("conversion-report") {
                let mut j: serde_json::Value = serde_json::from_str(&text).unwrap();
                plain(&mut j, true, increments);
                j.to_string()
            } else if rel.ends_with(".md") && rel.starts_with("conversion-report") {
                if increments {
                    // The increment line of the summary; the tables are
                    // compared through the JSON report.
                    continue;
                }
                // The report names its output folder once, in its heading.
                text.replace(&dir.display().to_string(), "<out>")
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
            let (lo, hi) = (at.saturating_sub(150), at + 650);
            panic!(
                "{what}: {} differs at {at}\n left:  {}\n right: {}",
                x.0,
                &x.1[lo..hi.min(x.1.len())],
                &y.1[lo..hi.min(y.1.len())]
            );
        }
    }
}

fn run(f: &Fixture, c: &Case, out: &str, jobs: usize, select: Select, fresh: bool) {
    let mut job = f.job(out, jobs, select, c.to, c.catalogs);
    job.fresh = fresh;
    convert_into(&job, &Hooks::default()).unwrap();
}

/// Every way of running the same tree gives the same files and report.
fn matrix(name: &str, c: Case, ledger: Option<&str>) {
    let f = Fixture::new(name);
    let start = |out: &str| {
        if let Some(text) = ledger {
            let o = f.out(out);
            std::fs::create_dir_all(&o).unwrap();
            std::fs::write(o.join("review-decisions.jsonl"), text).unwrap();
        }
    };
    start("o-a");
    run(&f, &c, "o-a", 1, Select::default(), true);
    let base = tree_of(&f.out("o-a"), false);
    // Fresh with more threads, into another folder.
    start("o-b");
    run(&f, &c, "o-b", 4, Select::default(), true);
    same(
        &base,
        &tree_of(&f.out("o-b"), false),
        "-j4 fresh, other folder",
    );
    // Resumed from the store another thread count made, both ways.
    run(&f, &c, "o-a", 4, Select::default(), false);
    same(
        &base,
        &tree_of(&f.out("o-a"), false),
        "-j4 resumed on a -j1 store",
    );
    run(&f, &c, "o-b", 1, Select::default(), false);
    same(
        &base,
        &tree_of(&f.out("o-b"), false),
        "-j1 resumed on a -j4 store",
    );
    // --fresh over a store that exists.
    run(&f, &c, "o-a", 3, Select::default(), true);
    same(
        &base,
        &tree_of(&f.out("o-a"), false),
        "--fresh over a store",
    );
    // Resumed again and again: the re-check samples differ per run, the
    // output does not.
    for n in 0..4 {
        run(&f, &c, "o-a", 1 + n % 3, Select::default(), false);
        same(&base, &tree_of(&f.out("o-a"), false), "repeated resume");
    }
    // Increments with alternating thread counts end where the full run does.
    start("o-inc");
    for step in 0..FILES.len() {
        run(
            &f,
            &c,
            "o-inc",
            if step % 2 == 0 { 1 } else { 3 },
            next(1),
            false,
        );
    }
    run(&f, &c, "o-inc", 2, Select::default(), false);
    same(
        &tree_of(&f.out("o-a"), true),
        &tree_of(&f.out("o-inc"), true),
        "increments",
    );
}

#[test]
fn thread_count_store_and_folder_never_change_the_output() {
    matrix(
        "pg",
        Case {
            to: Dialect::Postgres,
            catalogs: &[],
        },
        None,
    );
}

#[test]
fn the_same_holds_for_db2_for_i() {
    matrix(
        "db2",
        Case {
            to: Dialect::Db2i,
            catalogs: &[],
        },
        None,
    );
}

#[test]
fn the_same_holds_with_a_base_catalog() {
    matrix(
        "base",
        Case {
            to: Dialect::Postgres,
            catalogs: &["base.sql"],
        },
        None,
    );
}

#[test]
fn the_same_holds_with_a_hand_written_unit_in_the_ledger() {
    // The id of the function's unit comes from a first run.
    let f = Fixture::new("ledger-id");
    run(
        &f,
        &Case {
            to: Dialect::Postgres,
            catalogs: &[],
        },
        "x",
        1,
        Select::default(),
        true,
    );
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(f.out("x").join("conversion-report.json")).unwrap())
            .unwrap();
    let file = report["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["path"] == "funcs/f.sql")
        .unwrap();
    let unit = file["units"]
        .as_array()
        .unwrap()
        .iter()
        .chain(file["converted_units"].as_array().unwrap())
        .next()
        .unwrap();
    let record = serde_json::json!({
        "unit": unit["id"],
        "path": "funcs/f.sql",
        "state": "written",
        "by": "human",
        "author": "tester",
        "at": "2026-01-01T00:00:00Z",
        "input_hash": "",
        "output_hash": "",
        "text": "CREATE OR REPLACE FUNCTION f(p_x integer) RETURNS integer LANGUAGE sql AS $$ SELECT p_x + 2 $$;\n",
    });
    matrix(
        "ledger",
        Case {
            to: Dialect::Postgres,
            catalogs: &[],
        },
        Some(&format!("{record}\n")),
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
    let _ = convert_into(
        &f.job("c", 1, Select::default(), Dialect::Postgres, &[]),
        &hooks,
    );
    let o = f.out("c");
    assert!(
        !o.join(".sqldiff").join("run-counter").exists(),
        "a cancelled run recorded itself"
    );
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
fn stand_in_flags_come_from_the_store_without_changing_the_output() {
    let f = Fixture::new("flags");
    let c = Case {
        to: Dialect::Postgres,
        catalogs: &[],
    };
    run(&f, &c, "a", 2, Select::default(), true);
    let base = tree_of(&f.out("a"), false);
    let flags = f.out("a").join(".sqldiff/flags");
    let stored = std::fs::read_dir(&flags).map(|d| d.count()).unwrap_or(0);
    assert!(
        stored > 0,
        "the flags of the file using the stand-in are stored"
    );
    assert!(
        base.iter().any(|(_, t)| t.contains("ASM-TABLE-STAND-IN")),
        "the fixture does flag a unit"
    );
    // Resumed: the flags are read, not computed; same files.
    for jobs in [1, 3] {
        run(&f, &c, "a", jobs, Select::default(), false);
        same(
            &base,
            &tree_of(&f.out("a"), false),
            "resumed with stored flags",
        );
    }
    // A stored flag set that does not read back is computed again.
    for e in std::fs::read_dir(&flags).unwrap().flatten() {
        for g in std::fs::read_dir(e.path()).unwrap().flatten() {
            std::fs::write(g.path(), b"not json").unwrap();
        }
    }
    run(&f, &c, "a", 2, Select::default(), false);
    same(&base, &tree_of(&f.out("a"), false), "unreadable flags");
}

#[test]
fn a_checked_again_file_must_give_the_stored_stand_in_flags() {
    let f = Fixture::new("flagsmismatch");
    let c = Case {
        to: Dialect::Postgres,
        catalogs: &[],
    };
    run(&f, &c, "a", 2, Select::default(), true);
    let flags = f.out("a").join(".sqldiff/flags");
    // A flag set that reads back but is not what the file gives now.
    let mut changed = 0;
    for e in std::fs::read_dir(&flags).unwrap().flatten() {
        for g in std::fs::read_dir(e.path()).unwrap().flatten() {
            std::fs::write(g.path(), b"[]").unwrap();
            changed += 1;
        }
    }
    assert!(changed > 0);
    // Every file is checked again (`--fresh`): the flags are compared.
    let mut job = f.job("a", 2, Select::default(), c.to, c.catalogs);
    job.fresh = true;
    let Err(err) = convert_into(&job, &Hooks::default()) else {
        panic!("a flag set that differs from the fresh one must fail the run");
    };
    assert!(err.contains("stand-in flags"), "{err}");
    assert!(err.contains("store is emptied"), "{err}");
}
