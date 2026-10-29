//! A tree converted from the app writes what the command on its run sheet
//! writes, byte for byte: the command is run as shown, through a shell.

use std::path::{Path, PathBuf};
use std::process::Command;

use sqldiff_gui::runs::{
    self, BatchSource, BatchSpec, ChangesSpec, Command as Sheet, ConvertSpec, DriftSpec, DriftTree,
    FormatSpec, InstancesSpec, Run, Runs, State,
};

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// Every file under `dir`, relative, with its text.
fn files(dir: &Path) -> Vec<(String, String)> {
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
                v.push((rel, std::fs::read_to_string(&p).unwrap()));
            }
        }
    }
    v.sort();
    v
}

/// `files`, with the output folder's name, the timings and the build masked
/// (an instance report's run fingerprint hashes the running program).
fn masked(dir: &Path) -> Vec<(String, String)> {
    let name = dir.display().to_string();
    let json = serde_json::to_string(&name).unwrap();
    let json = &json[1..json.len() - 1];
    files(dir)
        .into_iter()
        .map(|(p, t)| {
            let t = t.replace(json, "OUT").replace(&name, "OUT");
            let t = t
                .lines()
                .map(|l| {
                    let s = l.trim_start();
                    if s.starts_with("\"seconds\":")
                        || (s.contains(" files, ") && s.ends_with(" s"))
                        || l.starts_with("  \"fingerprint\":")
                    {
                        "(time)"
                    } else {
                        l
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            (p, t)
        })
        .collect()
}

fn finish(run: &Run) -> State {
    for _ in 0..1200 {
        match run.state() {
            s if s.busy() => std::thread::sleep(std::time::Duration::from_millis(50)),
            s => return s,
        }
    }
    panic!("run still running after a minute");
}

/// Runs the sheet's command for this platform's shell from `elsewhere`,
/// with `sqldiff` on the PATH and `$SQLDIFF_CONFIG` naming another file.
fn run_sheet(sheet: &Sheet, elsewhere: &Path, other_user: &Path) -> std::process::Output {
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_sqldiff"));
    let mut path = vec![bin.parent().unwrap().to_path_buf()];
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let mut c = if cfg!(windows) {
        let mut c = Command::new("powershell");
        c.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            // `-Command` reports only 0 or 1; pass on sqldiff's own code.
            // (Not part of the sheet: pasted into a console, `exit` would
            // close it.)
            &format!("{}; exit $LASTEXITCODE", sheet.powershell),
        ]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", &sheet.sh]);
        c
    };
    c.current_dir(elsewhere)
        .env("PATH", std::env::join_paths(path).unwrap())
        .env("SQLDIFF_CONFIG", other_user)
        .output()
        .unwrap()
}

#[test]
fn the_run_sheet_command_writes_what_the_run_wrote() {
    // One test: the app's user config is named by the environment (of this
    // process); the commands run with another, broken one in theirs.
    let d = std::env::temp_dir().join(format!("sqldiff-gui-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let user = d.join("user config.toml");
    write(&user, "");
    std::env::set_var("SQLDIFF_CONFIG", &user);
    // The commands run from a folder and an environment whose config would
    // break them, so a command that found config where it is run fails.
    let elsewhere = d.join("elsewhere");
    write(&elsewhere.join("sqldiff.toml"), "not a config [\n");
    let other_user = d.join("other user.toml");
    write(&other_user, "not a config [\n");

    // A tree with a project config that changes the conversion, in a folder
    // whose name needs quoting in either shell.
    let root = d.join("it's a [tree] $HOME");
    write(
        &root.join("sqldiff.toml"),
        "[[helpers]]\nname = \"site_name\"\ndialect = \"postgres\"\nreturns = \"text\"\n",
    );
    let src = root.join("src");
    write(&src.join("a.sql"), "CREATE TABLE t (a int NOT NULL);\n");
    // A call to a routine only the project's helper defines.
    write(
        &src.join("sub/p.sql"),
        "CREATE PROCEDURE dbo.p AS\nBEGIN\n  DECLARE @n nvarchar(50) = dbo.site_name();\n  UPDATE dbo.t SET a = @n;\nEND\n",
    );
    // The same tree without a project config.
    let bare = d.join("bare");
    write(&bare.join("a.sql"), "CREATE TABLE t (a int NOT NULL);\n");
    // Base catalogs, in a folder whose name needs quoting too, and a query
    // on a table only they define.
    let base = root.join("base catalogs");
    write(
        &base.join("orders.sql"),
        "CREATE TABLE orders (order_id int NOT NULL, status varchar(10) NOT NULL);\n",
    );
    write(
        &base.join("items.sql"),
        "CREATE TABLE order_items (order_id int NOT NULL);\n",
    );
    let with_base = root.join("upgrade");
    write(
        &with_base.join("a.sql"),
        "CREATE TABLE t (a int NOT NULL);\n",
    );
    write(&with_base.join("q.sql"), "SELECT status FROM orders;\n");
    let catalogs: Vec<String> = ["orders.sql", "items.sql"]
        .iter()
        .map(|f| base.join(f).display().to_string())
        .collect();
    let base_pg = root.join("base catalogs pg").join("orders.sql");
    write(
        &base_pg,
        "CREATE TABLE orders (order_id integer NOT NULL, status varchar(10) NOT NULL);\n",
    );
    let catalogs_b = vec![base_pg.display().to_string()];

    let runs = Runs::default();
    let mut sheets = Vec::new();
    let none = Vec::new();
    for (name, src, isolated, overwrite, cats, cats_b) in [
        ("project", &src, false, false, &none, &none),
        ("isolated", &src, true, false, &none, &none),
        ("no-project", &bare, false, false, &none, &none),
        ("overwrite", &src, false, true, &none, &none),
        ("catalogs", &with_base, false, false, &catalogs, &none),
        (
            "catalogs-b",
            &with_base,
            false,
            false,
            &catalogs,
            &catalogs_b,
        ),
    ] {
        let gui = d.join(format!("gui {name}"));
        let cli = d.join(format!("cli {name}"));
        if overwrite {
            write(&gui.join("stale.txt"), "x");
            write(&cli.join("stale.txt"), "x");
        }
        let spec = |out: &Path| ConvertSpec {
            src: src.display().to_string(),
            out: out.display().to_string(),
            from: "tsql".into(),
            to: "postgres".into(),
            overwrite,
            template: false,
            parameters: Vec::new(),
            extensions: Vec::new(),
            packs: Vec::new(),
            catalogs: cats.clone(),
            catalogs_b: cats_b.clone(),
            instances: None,
        };
        let run = runs.start(spec(&gui), isolated, |_| {}).unwrap();
        assert!(
            matches!(finish(&run), State::Done { .. }),
            "{name}: {:?}",
            run.state()
        );
        // The run's command, for the CLI's folder.
        let sheet = runs::command(&spec(&cli), isolated).unwrap();
        let mut args = run.command.args.clone();
        let o = args.iter().position(|a| a == "-o").unwrap() + 1;
        args[o] = cli.display().to_string();
        assert_eq!(sheet.args, args);
        let o = run_sheet(&sheet, &elsewhere, &other_user);
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(
            o.status.code().is_some_and(|c| c <= 1),
            "{name}: {}\n{err}",
            sheet.sh
        );
        assert_eq!(masked(&gui), masked(&cli), "{name}: {}", sheet.sh);
        assert_gate_is_review_status(&run);
        let c = &run.command;
        // The catalogs are on the sheet, in order.
        let given = |flag: &str| -> Vec<&String> {
            c.args
                .iter()
                .zip(c.args.iter().skip(1))
                .filter(|(f, _)| *f == flag)
                .map(|(_, p)| p)
                .collect()
        };
        assert_eq!(
            given("--catalog"),
            cats.iter().collect::<Vec<_>>(),
            "{name}"
        );
        assert_eq!(
            given("--catalog-b"),
            cats_b.iter().collect::<Vec<_>>(),
            "{name}"
        );
        if name.starts_with("catalogs") {
            let report = std::fs::read_to_string(gui.join("conversion-report.json")).unwrap();
            assert!(report.contains("\"base_catalogs\""), "{report}");
            assert!(!report.contains("bind.unknown-table"), "{report}");
        }
        match name {
            "project" | "overwrite" | "catalogs" | "catalogs-b" => {
                assert_eq!(
                    c.project_config,
                    Some(root.join("sqldiff.toml").display().to_string())
                );
                assert!(c.args.contains(&"--config".to_string()));
            }
            "isolated" => {
                assert_eq!((&c.project_config, &c.user_config), (&None, &None));
                assert!(!c.args.contains(&"--user-config".to_string()));
            }
            _ => {
                assert_eq!(c.project_config, None);
                assert!(!c.args.contains(&"--config".to_string()));
            }
        }
        if name != "isolated" {
            assert_eq!(c.user_config, Some(user.display().to_string()));
            assert!(c.args.contains(&"--user-config".to_string()));
        }
        assert_eq!(c.args.contains(&"--overwrite".to_string()), overwrite);
        assert!(c.args.contains(&"--isolated".to_string()));
        sheets.push(sheet);
    }
    // The project config the sheet names is the one the command reads.
    write(&root.join("sqldiff.toml"), "not a config [\n");
    std::fs::remove_dir_all(d.join("cli project")).unwrap();
    let o = run_sheet(&sheets[0], &elsewhere, &other_user);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(2), "{err}");
    assert!(err.contains("sqldiff.toml"), "{err}");
    // So is the user config, though the environment names another.
    write(&user, "not a config [\n");
    std::fs::remove_dir_all(d.join("cli no-project")).unwrap();
    let o = run_sheet(&sheets[2], &elsewhere, &d.join("absent.toml"));
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(2), "{err}");
    assert!(err.contains("user config.toml"), "{err}");
    std::env::remove_var("SQLDIFF_CONFIG");
    let _ = std::fs::remove_dir_all(&d);
}

/// The example pack, with the `<%NAME%>` template syntax.
fn pack() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/examples/pack")
        .display()
        .to_string()
}

#[test]
fn a_template_run_and_its_command_write_the_same() {
    let d = std::env::temp_dir().join(format!("sqldiff-gui-parity-tpl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let elsewhere = d.join("elsewhere");
    write(&elsewhere.join("sqldiff.toml"), "not a config [\n");
    let src = d.join("src");
    write(
        &src.join("tables/entity.tpl"),
        "CREATE TABLE <%SCHEMA%>.<%TABLE%> (Id int NOT NULL, Price decimal(12, 2) NOT NULL);\n",
    );
    write(
        &src.join("procs/touch.tpl"),
        "<%@include header.tpl %>CREATE PROCEDURE <%SCHEMA%>.Touch<%TABLE%> @Id int AS\nBEGIN\n  UPDATE <%SCHEMA%>.<%TABLE%> SET Price = 0 WHERE Id = @Id;\nEND\n",
    );
    write(&src.join("procs/header.tpl"), "-- generated\n");
    write(&src.join("plain.sql"), "not read: wrong extension\n");
    let manifest = d.join("instances.toml");
    write(
        &manifest,
        "[params]\nSCHEMA = \"dbo\"\n[params_for.postgres]\nSCHEMA = \"public\"\n\n[[instance]]\nname = \"tables/Customer\"\ntemplate = \"tables/entity.tpl\"\nparams = { TABLE = \"Customer\" }\n\n[[instance]]\nname = \"procs/TouchCustomer\"\ntemplate = \"procs/touch.tpl\"\nparams = { TABLE = \"Customer\" }\n",
    );
    let spec = |out: &Path, instances: Option<&Path>| ConvertSpec {
        src: src.display().to_string(),
        out: out.display().to_string(),
        from: "tsql".into(),
        to: "postgres".into(),
        overwrite: false,
        template: true,
        parameters: vec!["SCHEMA".into(), "TABLE".into()],
        extensions: vec![".tpl".into()],
        packs: vec![pack()],
        catalogs: Vec::new(),
        catalogs_b: Vec::new(),
        instances: instances.map(|p| p.display().to_string()),
    };
    let runs = Runs::default();
    // Converted templates, then their instance check by the command line.
    let first = runs
        .start(spec(&d.join("conv"), None), true, |_| {})
        .unwrap();
    assert!(
        matches!(finish(&first), State::Done { .. }),
        "{:?}",
        first.state()
    );
    let c = &first.command;
    for flag in [
        "--template",
        "--parameter",
        "--extension",
        "--pack",
        "--isolated",
    ] {
        assert!(c.args.contains(&flag.to_string()), "{flag}: {}", c.sh);
    }
    assert!(!d.join("conv/plain.sql").exists());
    assert!(d.join("conv/procs/touch.tpl").is_file());
    // The instance check, from the app and by its command.
    let check = |out: &Path| InstancesSpec {
        manifest: manifest.display().to_string(),
        source: Some(src.display().to_string()),
        target: Some(d.join("conv").display().to_string()),
        from: "tsql".into(),
        to: "postgres".into(),
        out: out.display().to_string(),
        woven: None,
        woven_target: None,
        generator_only: false,
        parameters: vec!["TABLE".into()],
        accept: vec!["ASM-DBO-SCHEMA".into()],
        packs: vec![pack()],
        overwrite: false,
    };
    let run = runs
        .start_instances(check(&d.join("inst gui")), true, |_| {})
        .unwrap();
    assert!(
        matches!(finish(&run), State::Done { .. }),
        "{:?}",
        run.state()
    );
    assert!(d.join("inst gui/instance-report.json").is_file());
    let sheet = runs::instances_command(&check(&d.join("inst cli")), true).unwrap();
    for flag in [
        "--source",
        "--target",
        "--parameter",
        "--accept",
        "--pack",
        "--isolated",
    ] {
        assert!(
            sheet.args.contains(&flag.to_string()),
            "{flag}: {}",
            sheet.sh
        );
    }
    let o = run_sheet(&sheet, &elsewhere, &d.join("absent.toml"));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        o.status.code().is_some_and(|c| c <= 1),
        "{}\n{err}",
        sheet.sh
    );
    assert_eq!(
        masked(&d.join("inst gui")),
        masked(&d.join("inst cli")),
        "{}",
        sheet.sh
    );

    for (name, instances) in [("plain", None), ("instances", Some(d.join("inst gui")))] {
        let gui = d.join(format!("gui {name}"));
        let cli = d.join(format!("cli {name}"));
        let run = runs
            .start(spec(&gui, instances.as_deref()), true, |_| {})
            .unwrap();
        assert!(
            matches!(finish(&run), State::Done { .. }),
            "{name}: {:?}",
            run.state()
        );
        let sheet = runs::command(&spec(&cli, instances.as_deref()), true).unwrap();
        let o = run_sheet(&sheet, &elsewhere, &d.join("absent.toml"));
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(
            o.status.code().is_some_and(|c| c <= 1),
            "{name}: {}\n{err}",
            sheet.sh
        );
        assert_eq!(masked(&gui), masked(&cli), "{name}: {}", sheet.sh);
        assert_eq!(
            sheet.args.contains(&"--instances".to_string()),
            instances.is_some()
        );
        // The done gate the run shows is `sqldiff review-status`'s.
        assert_gate_is_review_status(&run);
    }
    // The instance check changes what the review queue says.
    assert_ne!(
        masked(&d.join("gui plain")),
        masked(&d.join("gui instances")),
        "the instance check was not read"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// The run's done gate is what `sqldiff review-status` says of its output,
/// given the run's own arguments.
fn assert_gate_is_review_status(run: &Run) {
    let mut args = run.command.args.clone();
    assert_eq!((args[0].as_str(), args[2].as_str()), ("convert", "-o"));
    args[0] = "review-status".into();
    args.remove(2);
    args.retain(|a| a != "--overwrite");
    args.push("--json".into());
    let o = Command::new(env!("CARGO_BIN_EXE_sqldiff"))
        .args(&args)
        .output()
        .unwrap();
    let cli: serde_json::Value = serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&o.stderr)));
    let gui = run.view().gate.expect("a done run shows its gate");
    assert_eq!(gui.error, None);
    assert_eq!(serde_json::json!(gui.complete), cli["complete"]);
    assert_eq!(serde_json::json!(gui.counts), cli["counts"]);
    assert_eq!(serde_json::json!(gui.accepted), cli["accepted"]);
    assert_eq!(
        serde_json::json!(gui.blockers),
        serde_json::json!(cli["blockers"].as_array().unwrap().len())
    );
}

/// Runs `git` in `dir`.
fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

#[test]
fn a_drift_run_and_its_command_write_the_same() {
    let d = std::env::temp_dir().join(format!("sqldiff-gui-parity-drift-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let elsewhere = d.join("elsewhere");
    write(&elsewhere.join("sqldiff.toml"), "not a config [\n");
    // Two trees of one repository: the templates the generator reads for
    // T-SQL and for PostgreSQL. `touch` is not in the PostgreSQL tree and
    // `entity` has other placeholders there.
    let repo = d.join("repo");
    git(
        &{
            std::fs::create_dir_all(&repo).unwrap();
            repo.clone()
        },
        &["init", "-q"],
    );
    let (ms, pg) = (repo.join("mssql tpl"), repo.join("pg"));
    write(
        &ms.join("tables/entity.tpl"),
        "CREATE TABLE <%SCHEMA%>.<%TABLE%> (Id int NOT NULL);\n",
    );
    write(
        &ms.join("procs/touch.tpl"),
        "CREATE PROCEDURE <%SCHEMA%>.Touch<%TABLE%> @Id int AS\nBEGIN\n  UPDATE <%SCHEMA%>.<%TABLE%> SET Id = 0 WHERE Id = @Id;\nEND\n",
    );
    write(
        &pg.join("tables/entity.tpl"),
        "CREATE TABLE <%SCHEMA%>.<%TABLE%> (Id integer NOT NULL, Extra <%KIND%>);\n",
    );
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(&repo, &["checkout", "-q", "-b", "topic"]);
    // Changed in one tree only, since the base.
    write(
        &ms.join("tables/entity.tpl"),
        "CREATE TABLE <%SCHEMA%>.<%TABLE%> (Id int NOT NULL, Price int);\n",
    );
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "change"]);
    let manifest = d.join("instances.toml");
    write(
        &manifest,
        "[params]\nSCHEMA = \"dbo\"\nKIND = \"text\"\n[params_for.postgres]\nSCHEMA = \"public\"\n\n[[instance]]\nname = \"tables/Customer\"\ntemplate = \"tables/entity.tpl\"\nparams = { TABLE = \"Customer\" }\n",
    );
    let spec = |out: &Path, base: Option<&str>, manifest: bool| DriftSpec {
        trees: vec![
            DriftTree {
                dialect: "tsql".into(),
                dir: ms.display().to_string(),
            },
            DriftTree {
                dialect: "postgres".into(),
                dir: pg.display().to_string(),
            },
        ],
        reference: None,
        out: out.display().to_string(),
        extensions: vec![".tpl".into()],
        parameters: vec!["TABLE".into()],
        accept: vec!["ASM-DBO-SCHEMA".into()],
        base: base.map(str::to_string),
        manifest: manifest.then(|| manifest_path(&d)),
        packs: vec![pack()],
        overwrite: false,
    };
    let runs = Runs::default();
    for (name, base, with_manifest) in [
        ("trees", None, false),
        ("base", Some("main"), false),
        ("instances", None, true),
    ] {
        let (gui, cli) = (d.join(format!("gui {name}")), d.join(format!("cli {name}")));
        let run = runs
            .start_drift(spec(&gui, base, with_manifest), true, |_| {})
            .unwrap();
        let state = finish(&run);
        let State::Done {
            complete, totals, ..
        } = &state
        else {
            panic!("{name}: {state:?}");
        };
        let sheet = runs::drift_command(&spec(&cli, base, with_manifest), true).unwrap();
        for flag in [
            "--tree",
            "--extension",
            "--parameter",
            "--pack",
            "--isolated",
        ] {
            assert!(
                sheet.args.contains(&flag.to_string()),
                "{flag}: {}",
                sheet.sh
            );
        }
        assert_eq!(sheet.args.contains(&"--base".to_string()), base.is_some());
        assert_eq!(
            sheet.args.contains(&"--manifest".to_string()),
            with_manifest
        );
        let o = run_sheet(&sheet, &elsewhere, &d.join("absent.toml"));
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(
            o.status.code().is_some_and(|c| c <= 1),
            "{name}: {}\n{err}",
            sheet.sh
        );
        // The run's verdict is the command's exit code, and it is not a
        // review gate: there is nothing here to review or to call done.
        assert_eq!(*complete, o.status.code() == Some(0), "{name}");
        assert!(totals["drifting"].as_u64().unwrap() > 0, "{name}: {totals}");
        assert_eq!(masked(&gui), masked(&cli), "{name}: {}", sheet.sh);
        let report = std::fs::read_to_string(gui.join("drift-report.json")).unwrap();
        assert!(report.contains("drift.missing"), "{name}");
        assert_eq!(
            report.contains("drift.changed-alone"),
            base.is_some(),
            "{name}"
        );
        assert_eq!(
            gui.join("instances/postgres/instance-report.json")
                .is_file(),
            with_manifest
        );
    }
    let _ = std::fs::remove_dir_all(&d);
}

fn manifest_path(d: &Path) -> String {
    d.join("instances.toml").display().to_string()
}

#[test]
fn a_branch_review_run_and_its_command_write_the_same() {
    let d = std::env::temp_dir().join(format!("sqldiff-gui-parity-chg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let elsewhere = d.join("elsewhere");
    write(&elsewhere.join("sqldiff.toml"), "not a config [\n");
    // A repository with a branch that changes, reformats and adds SQL; the
    // folder has a space in its name, as the pathspec case needs.
    let repo = d.join("my repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    write(&repo.join("a.tsql.sql"), "SELECT a FROM t WHERE b = 1;\n");
    write(
        &repo.join("sub dir/b.tsql.sql"),
        "SELECT a FROM t WHERE b = 1;\n",
    );
    write(
        &repo.join("sub dir/c.tsql.sql"),
        "SELECT a FROM t WHERE b = 1;\n",
    );
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(&repo, &["checkout", "-q", "-b", "topic"]);
    write(&repo.join("a.tsql.sql"), "SELECT a FROM t WHERE b = 2;\n");
    write(
        &repo.join("sub dir/b.tsql.sql"),
        "SELECT   a   FROM t WHERE b = 1;\n",
    );
    write(&repo.join("sub dir/new.tsql.sql"), "SELECT 1;\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "change"]);
    let spec = |out: &Path, dialect: Option<&str>, paths: &[&str]| ChangesSpec {
        repo: repo.display().to_string(),
        base: "main".into(),
        head: None,
        dialect: dialect.map(str::to_string),
        paths: paths.iter().map(|p| p.to_string()).collect(),
        out: out.display().to_string(),
        packs: vec![pack()],
        overwrite: false,
    };
    let runs = Runs::default();
    // The second keeps only `sub dir`: a reformatting (equal) and an added
    // file, which was compared with nothing, so at best unknown.
    for (name, dialect, paths, verdict) in [
        ("all", None, &[][..], "different"),
        (
            "one dialect and a path",
            Some("tsql"),
            &["sub dir"][..],
            "unknown",
        ),
    ] {
        let (gui, cli) = (d.join(format!("gui {name}")), d.join(format!("cli {name}")));
        let run = runs
            .start_changes(spec(&gui, dialect, paths), true, |_| {})
            .unwrap();
        let state = finish(&run);
        let State::Done {
            complete, totals, ..
        } = &state
        else {
            panic!("{name}: {state:?}");
        };
        let sheet = runs::changes_command(&spec(&cli, dialect, paths), true).unwrap();
        for flag in [
            "--repo",
            "--base",
            "--head",
            "--json",
            "--no-cache",
            "--pack",
            "--isolated",
        ] {
            assert!(
                sheet.args.contains(&flag.to_string()),
                "{flag}: {}",
                sheet.sh
            );
        }
        assert_eq!(sheet.args.contains(&"--".to_string()), !paths.is_empty());
        let o = run_sheet(&sheet, &elsewhere, &d.join("absent.toml"));
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(
            o.status.code().is_some_and(|c| c <= 1),
            "{name}: {}\n{err}",
            sheet.sh
        );
        // The verdict is the command's exit code; there is no gate here.
        assert_eq!(*complete, o.status.code() == Some(0), "{name}");
        assert_eq!(totals["verdict"], verdict, "{name}: {totals}");
        let (g, c) = (masked(&gui), masked(&cli));
        assert_eq!(
            g.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(),
            ["changes.json", "changes.md", "changes.sarif"],
            "{name}"
        );
        assert_eq!(g, c, "{name}: {}", sheet.sh);
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// `files`, with the timings masked.
fn timeless(dir: &Path) -> Vec<(String, String)> {
    files(dir)
        .into_iter()
        .map(|(p, t)| {
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
            (p, t)
        })
        .collect()
}

#[test]
fn a_batch_and_its_script_write_the_same() {
    let d = std::env::temp_dir().join(format!("sqldiff-gui-parity-batch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let elsewhere = d.join("elsewhere");
    write(&elsewhere.join("sqldiff.toml"), "not a config [\n");
    // Two trees, one in a folder whose name needs quoting in either shell.
    let orders = d.join("in").join("it's [orders] $HOME");
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
        "CREATE TABLE dbo.bills (id int NOT NULL, amount decimal(10,2) NULL);\n",
    );
    let out = d.join("out");
    let runs = Runs::default();
    // Isolated: the commands load no config, as the runs did.
    let b = runs
        .start_batch(
            BatchSpec {
                sources: [&orders, &billing]
                    .iter()
                    .map(|s| BatchSource {
                        src: s.display().to_string(),
                        from: "tsql".into(),
                    })
                    .collect(),
                targets: vec!["postgres".into(), "db2i".into()],
                out: out.display().to_string(),
                overwrite: false,
                template: false,
                parameters: Vec::new(),
                extensions: Vec::new(),
                packs: Vec::new(),
            },
            true,
        )
        .unwrap();
    for c in &b.cells {
        let run = runs.get(c.run.unwrap()).unwrap();
        assert!(
            matches!(finish(&run), State::Done { .. }),
            "{:?}",
            run.state()
        );
    }
    let script = runs.batch(b.id).unwrap().script;
    // The app's trees moved aside, the script writes them again where the
    // runs wrote them.
    let app = d.join("app");
    std::fs::rename(&out, &app).unwrap();
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_sqldiff"));
    let mut path = vec![bin.parent().unwrap().to_path_buf()];
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let mut c = if cfg!(windows) {
        let mut c = Command::new("powershell");
        c.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &script.powershell,
        ]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", &script.sh]);
        c
    };
    let o = c
        .current_dir(&elsewhere)
        .env("PATH", std::env::join_paths(path).unwrap())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.code().is_some_and(|c| c <= 1), "{err}");
    assert_eq!(timeless(&app), timeless(&out), "{}", script.sh);
    // Four trees, two per source.
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 2);
    // The worst exit code comes through: a run that fails fails the script.
    std::fs::remove_dir_all(&out).unwrap();
    std::fs::remove_dir_all(&billing).unwrap();
    let o = c.output().unwrap();
    assert_eq!(
        o.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_format_run_and_its_command_lay_out_the_same() {
    let d = std::env::temp_dir().join(format!("sqldiff-gui-parity-fmt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let elsewhere = d.join("elsewhere");
    write(&elsewhere.join("sqldiff.toml"), "not a config [\n");
    // The same folder twice: one laid out by the run, one by its command.
    for side in ["gui ws", "cli ws"] {
        let ws = d.join(side);
        write(
            &ws.join("sqldiff.toml"),
            "[format]\nkeyword_case = \"lower\"\n\n[format.tsql]\none_item_per_line = true\n",
        );
        write(&ws.join("a.pg.sql"), "select a,b from t where x=1\n");
        write(
            &ws.join("sub/b.tsql.sql"),
            "SELECT a,b FROM t WHERE x=1;\r\nGO\r\n",
        );
        write(&ws.join("sub/c.pg.sql"), "select 1\n");
        write(&ws.join("notes.txt"), "select a,b\n");
    }
    let spec = |side: &str, write: bool| FormatSpec {
        dir: d.join(side).display().to_string(),
        write,
        config: Some(d.join(side).join("sqldiff.toml").display().to_string()),
    };
    let runs = Runs::default();
    for write in [false, true] {
        let run = runs
            .start_format(spec("gui ws", write), true, |_| {})
            .unwrap();
        let state = finish(&run);
        let State::Done { complete, .. } = &state else {
            panic!("{write}: {state:?}");
        };
        let sheet = runs::format_command(&spec("cli ws", write), true).unwrap();
        assert!(
            sheet.args.contains(&"--isolated".to_string()),
            "{}",
            sheet.sh
        );
        assert!(sheet.args.contains(&"--config".to_string()), "{}", sheet.sh);
        let o = run_sheet(&sheet, &elsewhere, &d.join("absent.toml"));
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(
            o.status.code().is_some_and(|c| c <= 1),
            "{write}: {}\n{err}",
            sheet.sh
        );
        assert_eq!(*complete, o.status.code() == Some(0), "{write}: {err}");
        assert_eq!(
            files(&d.join("gui ws")),
            files(&d.join("cli ws")),
            "{write}: {}",
            sheet.sh
        );
    }
    // Laid out with the folder's settings, CRLF kept.
    let b = std::fs::read_to_string(d.join("gui ws/sub/b.tsql.sql")).unwrap();
    assert!(b.starts_with("select a,\r\n"), "{b:?}");
    assert_eq!(
        std::fs::read_to_string(d.join("gui ws/notes.txt")).unwrap(),
        "select a,b\n"
    );
    let _ = std::fs::remove_dir_all(&d);
}
