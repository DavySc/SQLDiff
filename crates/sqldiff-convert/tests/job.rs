//! `job::convert_into` (what `sqldiff convert` and the GUI's runs write):
//! progress, cancel, and that a cancelled job writes nothing.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use sqldiff_convert::job::{convert_into, Job};
use sqldiff_convert::tree::{self, Hooks, CANCELLED};
use sqldiff_core::Dialect;

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("sqldiff-job-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (p, text) in [
            ("a.sql", "CREATE TABLE t (a int NOT NULL);\n"),
            ("b.sql", "SELECT TOP 1 a FROM t;\n"),
            (
                "sub/p.sql",
                "CREATE PROCEDURE dbo.p AS\nBEGIN\n  UPDATE dbo.t SET a = 1;\nEND\n",
            ),
        ] {
            let f = dir.join("src").join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        Fixture { dir }
    }

    fn job(&self, out: &str) -> Job {
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
                catalogs: Vec::new(),
                catalogs_b: Vec::new(),
            },
            out: self.dir.join(out),
            overwrite: false,
            discard_edits: false,
            instances: None,
            queue_all: false,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Every file under `dir`, relative, with its bytes.
fn files(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    let mut v = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                v.push((rel, std::fs::read(&p).unwrap()));
            }
        }
    }
    v.sort();
    v
}

/// `text` without the timings the reports carry.
fn untimed(text: &str) -> String {
    text.lines()
        .map(|l| {
            let t = l.trim_start();
            if t.starts_with("\"seconds\":") || (t.contains(" files, ") && t.ends_with(" s")) {
                "(time)"
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn progress_counts_every_check_and_the_output_is_what_the_plain_call_writes() {
    let f = Fixture::new("progress");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = Arc::clone(&seen);
    let hooks = Hooks {
        progress: Some(Box::new(move |n, total| s.lock().unwrap().push((n, total)))),
        cancel: Some(Arc::new(AtomicBool::new(false))),
    };
    let watched = convert_into(&f.job("watched"), &hooks).unwrap();
    let plain = convert_into(&f.job("plain"), &Hooks::default()).unwrap();
    let mut seen = seen.lock().unwrap().clone();
    seen.sort();
    assert_eq!(seen, [(1, 3), (2, 3), (3, 3)]);
    assert_eq!(watched.inputs.len(), 3);
    assert_eq!(watched.queue.items.len(), plain.queue.items.len());
    // The same files, byte for byte, apart from the folder named in the reports.
    let strip = |dir: &str, v: Vec<(String, Vec<u8>)>| -> Vec<(String, String)> {
        let name = f.dir.join(dir).display().to_string();
        let json = serde_json::to_string(&name).unwrap();
        let json = &json[1..json.len() - 1];
        v.into_iter()
            .map(|(p, b)| {
                let t = String::from_utf8(b).unwrap();
                let t = t.replace(json, "OUT").replace(&name, "OUT");
                (p, untimed(&t))
            })
            .collect()
    };
    assert_eq!(
        strip("watched", files(&f.dir.join("watched"))),
        strip("plain", files(&f.dir.join("plain")))
    );
}

#[test]
fn a_cancelled_job_fails_and_writes_nothing() {
    let f = Fixture::new("cancel");
    // Cancelled before the checks start.
    let hooks = Hooks {
        progress: None,
        cancel: Some(Arc::new(AtomicBool::new(true))),
    };
    let e = convert_into(&f.job("out"), &hooks).err().unwrap();
    assert_eq!(e, CANCELLED);
    assert!(files(&f.dir.join("out")).is_empty());

    // Cancelled during the checks, after the first one.
    let cancel = Arc::new(AtomicBool::new(false));
    let c = Arc::clone(&cancel);
    let calls = Arc::new(AtomicUsize::new(0));
    let n = Arc::clone(&calls);
    let mut job = f.job("out2");
    job.tree.jobs = Some(1);
    let hooks = Hooks {
        progress: Some(Box::new(move |_, _| {
            n.fetch_add(1, Ordering::Relaxed);
            c.store(true, Ordering::Relaxed);
        })),
        cancel: Some(cancel),
    };
    let e = convert_into(&job, &hooks).err().unwrap();
    assert_eq!(e, CANCELLED);
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "the remaining checks are skipped"
    );
    assert!(files(&f.dir.join("out2")).is_empty());
}
