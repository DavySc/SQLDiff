//! Batches: several sources converted into several targets as one batch of
//! ordinary runs. A batch writes what its runs started one by one write, and
//! keeps their guards.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sqldiff_gui::runs::{BatchSpec, ConvertSpec, Run, Runs, State};

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let d =
            std::env::temp_dir().join(format!("sqldiff-gui-batch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.0.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Every file under `dir`, relative, with its text; the output folder's
/// name and the timings masked.
fn masked(dir: &Path) -> Vec<(String, String)> {
    let name = dir.display().to_string();
    let json = serde_json::to_string(&name).unwrap();
    let json = &json[1..json.len() - 1];
    let mut v = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let rel = p
                .strip_prefix(dir)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let text = std::fs::read_to_string(&p)
                .unwrap()
                .replace(json, "OUT")
                .replace(&name, "OUT");
            let text = text
                .lines()
                .map(|l| {
                    let s = l.trim_start();
                    match s.starts_with("\"seconds\":")
                        || (s.contains(" files, ") && s.ends_with(" s"))
                    {
                        true => "(time)",
                        false => l,
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            v.push((rel, text));
        }
    }
    v.sort();
    v
}

fn finish(run: &Run) -> State {
    for _ in 0..1200 {
        match run.state() {
            State::Running => std::thread::sleep(Duration::from_millis(50)),
            s => return s,
        }
    }
    panic!("run still running after a minute");
}

fn sources(d: &Dir) {
    d.write("one/a.sql", "CREATE TABLE t (a int NOT NULL);\n");
    d.write(
        "one/sub/p.sql",
        "CREATE PROCEDURE dbo.p AS\nBEGIN\n  UPDATE dbo.t SET a = 1;\nEND\n",
    );
    d.write("two/q.sql", "CREATE TABLE u (b varchar(10) NULL);\n");
}

fn batch(d: &Dir, out: &str, sources: &[&str], targets: &[&str]) -> BatchSpec {
    BatchSpec {
        sources: sources
            .iter()
            .map(|s| d.0.join(s).display().to_string())
            .collect(),
        from: "tsql".into(),
        targets: targets.iter().map(|t| t.to_string()).collect(),
        out: d.0.join(out).display().to_string(),
        overwrite: false,
        template: false,
        parameters: Vec::new(),
        extensions: Vec::new(),
        packs: Vec::new(),
    }
}

#[test]
fn a_batch_writes_what_its_runs_started_one_by_one_write() {
    let d = Dir::new("same");
    sources(&d);
    let spec = batch(&d, "out", &["one", "two"], &["postgres", "db2i"]);
    let runs = Runs::default();

    // One by one: each run alone, into the folder the batch names for it.
    let mut alone = Vec::new();
    for s in ["one", "two"] {
        for t in ["postgres", "db2i"] {
            let out = d.0.join("out").join(s).join(t);
            let run = runs
                .start(
                    ConvertSpec {
                        src: d.0.join(s).display().to_string(),
                        out: out.display().to_string(),
                        from: "tsql".into(),
                        to: t.into(),
                        overwrite: false,
                        template: false,
                        parameters: Vec::new(),
                        extensions: Vec::new(),
                        packs: Vec::new(),
                        instances: None,
                    },
                    true,
                    |_| {},
                )
                .unwrap();
            assert!(matches!(finish(&run), State::Done { .. }), "{s} → {t}");
            let tree = masked(&out);
            assert!(
                tree.iter().any(|(p, _)| p.ends_with(".sql")),
                "{s} → {t} wrote no SQL"
            );
            alone.push((out.clone(), tree, run.view().command.args));
        }
    }
    std::fs::remove_dir_all(d.0.join("out")).unwrap();

    // As a batch, into the same folders.
    let view = runs.start_batch(&spec, true).unwrap();
    assert_eq!((view.cells.len(), view.cells[0].len()), (2, 2));
    for row in &view.cells {
        for cell in row {
            let id = cell.as_ref().expect("listed").id;
            let run = runs.get(id).unwrap();
            assert!(matches!(finish(&run), State::Done { .. }), "run {id}");
        }
    }
    let after = runs.batch(view.id).unwrap();
    let mut it = alone.iter();
    for row in &after.cells {
        for cell in row {
            let run = cell.as_ref().unwrap();
            let (out, tree, args) = it.next().unwrap();
            assert_eq!(&masked(out), tree, "run {}", run.id);
            assert_eq!(&run.command.args, args, "run {}", run.id);
            assert!(
                run.gate.is_some(),
                "the cell carries the run's own done gate"
            );
        }
    }
    // The sheet is the runs' commands, in matrix order.
    let lines: Vec<&str> = after
        .sheet
        .sh
        .lines()
        .filter(|l| !l.starts_with('#'))
        .collect();
    assert_eq!(lines.len(), 4);
    for (line, cell) in lines.iter().zip(after.cells.iter().flatten()) {
        assert_eq!(*line, cell.as_ref().unwrap().command.sh);
    }
}

#[test]
fn a_batch_keeps_the_guards_and_starts_all_its_runs_or_none() {
    let d = Dir::new("guards");
    sources(&d);
    d.write("other/x.sql", "CREATE TABLE t (a int);\n");
    let runs = Runs::default();
    let refused = |spec: BatchSpec, says: &str| {
        let e = runs.start_batch(&spec, true).unwrap_err();
        assert!(e.contains(says), "{e}");
        assert!(runs.list().is_empty(), "a refused batch starts no run");
    };
    // Two sources of one name would share an output folder.
    d.write("nested/one/a.sql", "CREATE TABLE t (a int);\n");
    refused(
        batch(&d, "out", &["one", "nested/one"], &["postgres"]),
        "both called `one`",
    );
    // A target twice is one folder twice.
    refused(
        batch(&d, "out", &["one"], &["postgres", "postgres"]),
        "listed twice",
    );
    // Writing into a source: one run would read where another writes.
    refused(
        batch(&d, "one/out", &["one", "two"], &["postgres"]),
        "reads where",
    );
    // A cell that cannot run refuses the whole batch.
    refused(
        batch(&d, "out", &["one", "missing"], &["postgres"]),
        "not found",
    );
    refused(
        batch(&d, "out", &["one"], &["postgres", "nowhere"]),
        "unknown dialect",
    );
    assert!(!d.0.join("out").exists(), "nothing was written");

    // Beside a running run: one writer per folder, nobody reads half a tree.
    for k in 0..300 {
        d.write(
            &format!("big/p{k}.sql"),
            &format!("CREATE PROCEDURE dbo.p{k} AS\nBEGIN\n  UPDATE dbo.t SET a = {k};\nEND\n"),
        );
    }
    let big = runs
        .start(
            ConvertSpec {
                src: d.0.join("big").display().to_string(),
                out: d.0.join("out/one/postgres").display().to_string(),
                from: "tsql".into(),
                to: "postgres".into(),
                overwrite: false,
                template: false,
                parameters: Vec::new(),
                extensions: Vec::new(),
                packs: Vec::new(),
                instances: None,
            },
            true,
            |_| {},
        )
        .unwrap();
    let before = runs.list().len();
    let e = runs
        .start_batch(&batch(&d, "out", &["one", "two"], &["postgres"]), true)
        .unwrap_err();
    assert!(
        matches!(big.state(), State::Running),
        "finished too soon for this test"
    );
    assert!(e.contains("another run is writing there"), "{e}");
    assert_eq!(runs.list().len(), before, "none of the batch started");
    big.cancel();
    assert!(matches!(finish(&big), State::Cancelled { .. }));
}

#[test]
fn cancel_stops_every_run_of_a_batch_that_has_not_finished() {
    let d = Dir::new("cancel");
    for s in ["a", "b"] {
        for k in 0..300 {
            d.write(
                &format!("{s}/p{k}.sql"),
                &format!("CREATE PROCEDURE dbo.p{k} AS\nBEGIN\n  UPDATE dbo.t SET a = {k};\nEND\n"),
            );
        }
    }
    let runs = Runs::default();
    let view = runs
        .start_batch(&batch(&d, "out", &["a", "b"], &["postgres", "db2i"]), true)
        .unwrap();
    assert_eq!(view.running, 4);
    let view = runs.cancel_batch(view.id).unwrap();
    assert_eq!(view.id, runs.batches()[0].id);
    for cell in view.cells.iter().flatten() {
        let run = runs.get(cell.as_ref().unwrap().id).unwrap();
        // Cancelled before writing, or past the point it can stop: done.
        assert!(
            matches!(finish(&run), State::Cancelled { .. } | State::Done { .. }),
            "run {}",
            run.id
        );
    }
    assert_eq!(runs.batch(view.id).unwrap().running, 0);
    assert!(runs.cancel_batch(999).is_none());
}

#[cfg(unix)]
#[test]
fn a_source_name_cannot_add_a_line_to_the_sheet() {
    let d = Dir::new("sheet");
    d.write("x\necho pwned/a.sql", "CREATE TABLE t (a int);\n");
    let runs = Runs::default();
    let v = runs
        .start_batch(&batch(&d, "out", &["x\necho pwned"], &["postgres"]), true)
        .unwrap();
    for sheet in [&v.sheet.sh, &v.sheet.powershell] {
        // One comment line, then the command, whose path stays quoted.
        let lines: Vec<&str> = sheet.lines().collect();
        assert!(
            lines[0].starts_with("# ") && !lines[0].contains("pwned/a"),
            "{sheet}"
        );
        assert!(lines[1].starts_with("sqldiff convert "), "{sheet}");
    }
    finish(&runs.get(v.cells[0][0].as_ref().unwrap().id).unwrap());
}

#[test]
fn concurrent_batches_get_ids_of_their_own() {
    let d = Dir::new("ids");
    sources(&d);
    let runs = std::sync::Arc::new(Runs::default());
    let ids: Vec<u64> = (0..2)
        .map(|n| {
            let (runs, spec) = (
                runs.clone(),
                batch(&d, &format!("out{n}"), &["one", "two"], &["postgres"]),
            );
            std::thread::spawn(move || runs.start_batch(&spec, true).unwrap().id)
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect();
    assert_ne!(ids[0], ids[1]);
    for b in runs.batches() {
        assert_eq!(b.cells.iter().flatten().flatten().count(), 2);
        for c in b.cells.iter().flatten().flatten() {
            finish(&runs.get(c.id).unwrap());
        }
    }
}
