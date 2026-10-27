//! `drift::check`, the code `sqldiff drift` and the app's drift runs share:
//! what it finds, what it writes, and how it stops.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use sqldiff_convert::drift::{self, Args};
use sqldiff_convert::tree::{Hooks, CANCELLED};
use sqldiff_core::{Dialect, Options};
use sqldiff_source::config::Sources;
use sqldiff_source::Resolved;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sqldiff-drift-lib-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

const PACK: &str = r#"
[pack]
name = "drift"
version = "0.1.0"
schema = 1

[template]
open = "<%"
close = "%>"
name_pattern = "[A-Z_][A-Z0-9_]*"
"#;

fn args(d: &Path, out: &Path) -> Args {
    write(&d.join("pack.toml"), PACK);
    write(
        &d.join("mssql/a.sql"),
        "CREATE TABLE <%SCHEMA%>.<%TABLE%> (Id int);\n",
    );
    write(
        &d.join("pg/a.sql"),
        "CREATE TABLE <%SCHEMA%>.<%OTHER%> (Id integer);\n",
    );
    write(&d.join("mssql/only_here.sql"), "SELECT 1;\n");
    Args {
        trees: vec![
            (Dialect::Tsql, d.join("mssql")),
            (Dialect::Postgres, d.join("pg")),
        ],
        reference: None,
        out: out.to_path_buf(),
        extensions: Vec::new(),
        parameters: Vec::new(),
        base: None,
        manifest: None,
        accept: Vec::new(),
        jobs: Some(1),
        overwrite: false,
        options: Box::new(|_, _| Ok(Options::default())),
        cfg: Resolved::load(&Sources {
            isolated: true,
            packs: vec![d.join("pack.toml")],
            ..Sources::default()
        })
        .unwrap(),
    }
}

#[test]
fn finds_drift_and_writes_the_reports_last() {
    let d = scratch("finds");
    let out = d.join("out");
    let steps = Arc::new(Mutex::new(Vec::new()));
    let s = Arc::clone(&steps);
    let hooks = Hooks {
        progress: Some(Box::new(move |n, total| s.lock().unwrap().push((n, total)))),
        cancel: None,
    };
    let c = drift::check(&args(&d, &out), &hooks).unwrap();
    assert!(!c.ok);
    assert_eq!((c.templates, c.drifting), (2, 2));
    // `a.sql` has other placeholders in pg; `only_here.sql` is not in pg.
    assert_eq!(c.items, 2);
    assert!(c.summary.contains("drift.missing"), "{}", c.summary);
    let [json, md] = &c.reports;
    assert_eq!(json, &out.join("drift-report.json"));
    assert!(json.is_file() && md.is_file());
    // One step per tree read, then the report; the last step is the total.
    let steps = steps.lock().unwrap().clone();
    assert_eq!(steps, [(1, 3), (2, 3), (3, 3)]);
}

#[test]
fn a_cancelled_check_writes_nothing() {
    let d = scratch("cancel");
    let out = d.join("out");
    let hooks = Hooks {
        progress: None,
        cancel: Some(Arc::new(AtomicBool::new(true))),
    };
    let e = drift::check(&args(&d, &out), &hooks).err().unwrap();
    assert_eq!(e, CANCELLED);
    // The folder is made (as `convert` makes its own); nothing is in it.
    let left: Vec<_> = std::fs::read_dir(&out)
        .map(|r| r.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn a_cancel_after_the_trees_are_read_still_writes_nothing() {
    let d = scratch("late");
    let out = d.join("out");
    let cancel = Arc::new(AtomicBool::new(false));
    let c = Arc::clone(&cancel);
    // Cancel when the last tree has been read: the report is not written.
    let hooks = Hooks {
        progress: Some(Box::new(move |n, _| {
            if n == 2 {
                c.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        })),
        cancel: Some(cancel),
    };
    let e = drift::check(&args(&d, &out), &hooks).err().unwrap();
    assert_eq!(e, CANCELLED);
    assert!(!out.join("drift-report.json").exists());
    assert!(!out.join("drift-report.md").exists());
}

#[test]
fn needs_two_trees_and_a_template_syntax() {
    let d = scratch("refuse");
    let out = d.join("out");
    let mut a = args(&d, &out);
    a.trees.truncate(1);
    assert!(drift::check(&a, &Hooks::default())
        .err()
        .unwrap()
        .contains("at least two"));
    let mut a = args(&d, &out);
    a.cfg = Resolved::builtin();
    assert!(drift::check(&a, &Hooks::default())
        .err()
        .unwrap()
        .contains("[template]"));
}
