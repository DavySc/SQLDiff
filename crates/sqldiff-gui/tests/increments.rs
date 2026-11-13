//! A conversion started from the page may check only part of a tree: the run
//! carries the increment as the CLI's flags, its totals say how much of the
//! plan is checked, and the plan can be read before any run.

use std::path::{Path, PathBuf};

use sqldiff_gui::runs::{command, plan_view, ConvertSpec, IncrementSpec, Run, Runs, State};

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sqldiff-gui-incr-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn finish(run: &Run) -> State {
    for _ in 0..2400 {
        match run.state() {
            s if s.busy() => std::thread::sleep(std::time::Duration::from_millis(50)),
            s => return s,
        }
    }
    panic!("run {} still busy after two minutes", run.id);
}

/// A table, and a query that uses it: two groups, the table first.
fn tree(d: &Path) -> PathBuf {
    let src = d.join("in");
    write(
        &src.join("t.sql"),
        "CREATE TABLE dbo.orders (id int NOT NULL, status varchar(10) NOT NULL);\n",
    );
    write(
        &src.join("q.sql"),
        "SELECT TOP 5 id FROM dbo.orders WHERE status = 'open' ORDER BY id;\n",
    );
    src
}

fn spec(src: &Path, out: &Path, increment: IncrementSpec, overwrite: bool) -> ConvertSpec {
    ConvertSpec {
        src: src.display().to_string(),
        out: out.display().to_string(),
        from: "tsql".into(),
        to: "postgres".into(),
        overwrite,
        template: false,
        parameters: Vec::new(),
        extensions: Vec::new(),
        packs: Vec::new(),
        catalogs: Vec::new(),
        catalogs_b: Vec::new(),
        instances: None,
        increment,
    }
}

fn next(n: usize) -> IncrementSpec {
    IncrementSpec {
        next: Some(n),
        ..Default::default()
    }
}

#[test]
fn the_command_names_the_increment_as_the_cli_does() {
    let d = dir("cmd");
    let src = tree(&d);
    let inc = IncrementSpec {
        next: Some(2),
        schemas: vec!["dbo".into(), " ".into()],
        objects: vec!["dbo.orders".into()],
        paths: vec!["sub/**".into()],
        changed: true,
    };
    let c = command(&spec(&src, &d.join("out"), inc, false), true).unwrap();
    let a = c.args.join(" ");
    for flag in [
        "--next 2",
        "--schema dbo",
        "--object dbo.orders",
        "--path sub/**",
        "--changed",
    ] {
        assert!(a.contains(flag), "{flag} missing from {a}");
    }
    let whole = command(&spec(&src, &d.join("out"), Default::default(), false), true).unwrap();
    for flag in ["--next", "--schema", "--object", "--path", "--changed"] {
        assert!(
            !whole.args.iter().any(|x| x == flag),
            "{flag} on a whole run"
        );
    }
}

#[test]
fn the_plan_lists_the_groups_in_the_order_they_are_taken() {
    let d = dir("plan");
    let src = tree(&d);
    let p = plan_view(&spec(&src, &d.join("out"), Default::default(), false), true).unwrap();
    assert_eq!((p.files, p.groups, p.cyclic), (2, 2, 0));
    assert_eq!(p.first[0].paths, ["t.sql"]);
    assert_eq!(p.first[1].paths, ["q.sql"]);
    // Nothing is written by planning.
    assert!(!d.join("out").exists());
}

#[test]
fn increments_into_one_folder_add_up_and_the_rest_is_not_checked() {
    let d = dir("run");
    let src = tree(&d);
    let out = d.join("out");
    let runs = Runs::default();
    let totals = |s: State| match s {
        State::Done { totals, .. } => totals,
        other => panic!("not done: {other:?}"),
    };
    let r = runs
        .start(spec(&src, &out, next(1), false), true, |_| {})
        .unwrap();
    let t = totals(finish(&r));
    assert_eq!(t["progress"]["files_checked"], 1, "{t}");
    assert_eq!(t["progress"]["files"], 2, "{t}");
    assert_eq!(t["files_not_checked"], 1, "{t}");
    let r = runs
        .start(spec(&src, &out, next(1), true), true, |_| {})
        .unwrap();
    let t = totals(finish(&r));
    assert_eq!(t["progress"]["files_checked"], 2, "{t}");
    assert!(
        t.get("files_not_checked").is_none() || t["files_not_checked"] == 0,
        "{t}"
    );
}
