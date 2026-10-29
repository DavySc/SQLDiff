//! A batch (mass conversion) is ordinary conversion runs: it writes the trees
//! its runs write when started one by one, its matrix points at those runs,
//! it starts all or none, and cancel stops every run not finished.

use std::path::{Path, PathBuf};

use sqldiff_gui::runs::{BatchSource, BatchSpec, ConvertSpec, Run, Runs, Spec, State};

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sqldiff-gui-batch-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Every file under `dir`, relative, with its text; the folder's own path
/// and the timings masked.
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
            let t = std::fs::read_to_string(&p).unwrap();
            let t = t.replace(json, "OUT").replace(&name, "OUT");
            let t = t
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
            v.push((rel, t));
        }
    }
    v.sort();
    v
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

/// Two T-SQL trees, `orders` and `billing`, under `d`.
fn sources(d: &Path) -> (PathBuf, PathBuf) {
    let orders = d.join("in").join("orders");
    write(
        &orders.join("t.sql"),
        "CREATE TABLE dbo.orders (id int NOT NULL, status varchar(10) NOT NULL);\n",
    );
    write(
        &orders.join("sub").join("q.sql"),
        "SELECT TOP 5 id FROM dbo.orders WHERE status = 'open' ORDER BY id;\n",
    );
    let billing = d.join("in").join("billing");
    write(
        &billing.join("b.sql"),
        "CREATE TABLE dbo.bills (id int NOT NULL, amount decimal(10,2) NULL);\nSELECT ISNULL(amount, 0) FROM dbo.bills;\n",
    );
    (orders, billing)
}

fn spec(srcs: &[&Path], targets: &[&str], out: &Path) -> BatchSpec {
    BatchSpec {
        sources: srcs
            .iter()
            .map(|s| BatchSource {
                src: s.display().to_string(),
                from: "tsql".into(),
            })
            .collect(),
        targets: targets.iter().map(|t| t.to_string()).collect(),
        out: out.display().to_string(),
        overwrite: false,
        template: false,
        parameters: Vec::new(),
        extensions: Vec::new(),
        packs: Vec::new(),
    }
}

#[test]
fn a_batch_writes_what_its_runs_write_one_by_one() {
    let d = dir("same");
    let (orders, billing) = sources(&d);
    let runs = Runs::default();
    let out = d.join("batch");
    let b = runs
        .start_batch(
            spec(&[&orders, &billing], &["postgres", "db2i"], &out),
            true,
        )
        .unwrap();
    assert_eq!(b.cells.len(), 4);
    assert_eq!(b.names, ["orders", "billing"]);
    let cells: Vec<(usize, &str)> = b.cells.iter().map(|c| (c.source, c.to.as_str())).collect();
    assert_eq!(
        cells,
        [(0, "postgres"), (0, "db2i"), (1, "postgres"), (1, "db2i")]
    );
    let batch_runs: Vec<_> = b
        .cells
        .iter()
        .map(|c| runs.get(c.run.unwrap()).unwrap())
        .collect();
    for r in &batch_runs {
        assert!(matches!(finish(r), State::Done { .. }), "{:?}", r.state());
    }
    let b = runs.batch(b.id).unwrap();
    assert!(!b.busy);
    for (c, r) in b.cells.iter().zip(&batch_runs) {
        let cell = r.batch.as_ref().unwrap();
        assert_eq!((cell.batch, cell.source, &cell.to), (b.id, c.source, &c.to));
        // The matrix shows the run's own gate, which its review judges.
        assert!(r.view().gate.is_some(), "run {}", r.id);
    }
    // The same runs, started one by one, write the same trees.
    let names = ["orders", "billing"];
    for (c, r) in b.cells.iter().zip(&batch_runs) {
        let (src, name) = ([&orders, &billing][c.source], names[c.source]);
        let batch_out = out.join(name).join(&c.to);
        let one_out = d.join("one").join(name).join(&c.to);
        let one = runs
            .start(
                ConvertSpec {
                    src: src.display().to_string(),
                    out: one_out.display().to_string(),
                    from: "tsql".into(),
                    to: c.to.clone(),
                    overwrite: false,
                    template: false,
                    parameters: Vec::new(),
                    extensions: Vec::new(),
                    packs: Vec::new(),
                    catalogs: Vec::new(),
                    catalogs_b: Vec::new(),
                    instances: None,
                },
                true,
                |_| {},
            )
            .unwrap();
        assert!(matches!(finish(&one), State::Done { .. }));
        assert!(one.batch.is_none());
        let files = masked(&batch_out);
        assert!(files.iter().any(|(p, _)| p == "sub/q.sql") || name == "billing");
        assert_eq!(files, masked(&one_out), "{name} → {}", c.to);
        // So does its command, but for the output folder.
        let mut args = r.command.args.clone();
        let o = args.iter().position(|a| a == "-o").unwrap() + 1;
        assert_eq!(args[o], batch_out.display().to_string());
        args[o] = one_out.display().to_string();
        assert_eq!(args, one.command.args);
    }
    // The run sheet is the runs' commands, in the batch's order.
    let mut at = 0;
    for r in &batch_runs {
        let i = b.script.sh[at..]
            .find(&r.command.sh)
            .expect("command in the sh script");
        at += i + r.command.sh.len();
        assert!(b.script.powershell.contains(&r.command.powershell));
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_batch_starts_all_or_none() {
    let d = dir("refused");
    let (orders, billing) = sources(&d);
    let runs = Runs::default();
    let out = d.join("out");
    let refused = |s: BatchSpec, why: &str| {
        let e = runs.start_batch(s, true).unwrap_err();
        assert!(e.contains(why), "{e}");
        assert!(runs.list().is_empty(), "{e}");
        assert!(!out.exists(), "{e}");
    };
    // Two sources of one name would write into one folder.
    let other = d.join("elsewhere").join("orders");
    write(&other.join("x.sql"), "SELECT 1;\n");
    refused(
        spec(&[&orders, &other], &["postgres"], &out),
        "two sources are named `orders`",
    );
    // A pair without rules refuses the batch, not just its run.
    let mut s = spec(&[&orders, &billing], &["postgres"], &out);
    s.sources[1].from = "postgres".into();
    s.targets.push("db2i".into());
    refused(s, "no conversion rules");
    // A missing source.
    refused(
        spec(&[&orders, &d.join("absent")], &["postgres"], &out),
        "not found",
    );
    // Writing where a run of the batch reads.
    refused(
        spec(&[&orders, &billing], &["postgres"], &d.join("in")),
        "would write where",
    );
    refused(spec(&[&orders], &["tsql"], &out), "nothing to convert");
    refused(
        spec(&[&orders], &["postgres", "postgres"], &out),
        "given twice",
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn cancel_stops_every_run_not_finished() {
    let d = dir("cancel");
    let (orders, billing) = sources(&d);
    let third = d.join("in").join("third");
    write(&third.join("c.sql"), "SELECT 1 AS one;\n");
    let runs = Runs::default();
    let out = d.join("out");
    let b = runs
        .start_batch(
            spec(&[&orders, &billing, &third], &["postgres", "db2i"], &out),
            true,
        )
        .unwrap();
    let b = runs.cancel_batch(b.id).unwrap();
    let batch_runs: Vec<_> = b
        .cells
        .iter()
        .map(|c| runs.get(c.run.unwrap()).unwrap())
        .collect();
    for r in &batch_runs {
        match finish(r) {
            State::Done { .. } => {}
            State::Cancelled { .. } => {
                let Spec::Convert(c) = &r.spec else {
                    panic!("a batch's run converts")
                };
                let o = PathBuf::from(&c.out);
                // A run stopped while running may have made its folder;
                // nothing is written into it.
                let empty = !std::fs::read_dir(&o).is_ok_and(|mut d| d.next().is_some());
                assert!(empty, "{}: written after cancel", o.display());
            }
            s => panic!("run {}: {s:?}", r.id),
        }
    }
    // Queued runs never start once cancelled.
    assert!(matches!(
        batch_runs.last().unwrap().state(),
        State::Cancelled { .. }
    ));
    assert!(!runs.batch(b.id).unwrap().busy);
    // Their folders are free again: the same batch starts.
    let again = runs
        .start_batch(
            spec(&[&orders, &billing, &third], &["postgres", "db2i"], &out),
            true,
        )
        .unwrap();
    for c in &again.cells {
        finish(&runs.get(c.run.unwrap()).unwrap());
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn batches_are_kept_in_the_data_dir() {
    let d = dir("kept");
    let (orders, _) = sources(&d);
    let data = d.join("data");
    let runs = Runs::with_history(Some(&data));
    let out = d.join("out");
    let b = runs
        .start_batch(spec(&[&orders], &["postgres", "db2i"], &out), true)
        .unwrap();
    for c in &b.cells {
        finish(&runs.get(c.run.unwrap()).unwrap());
    }
    let script = runs.batch(b.id).unwrap().script;
    let again = Runs::with_history(Some(&data));
    let kept = again.batches();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].id, b.id);
    assert!(kept[0].cells.iter().all(|c| c.run.is_some()));
    assert_eq!(kept[0].script, script);
    let _ = std::fs::remove_dir_all(&d);
}
