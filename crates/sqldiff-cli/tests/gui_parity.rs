//! A tree converted from the app writes what the command on its run sheet
//! writes, byte for byte: the command is run as shown, through a shell.

use std::path::{Path, PathBuf};
use std::process::Command;

use sqldiff_gui::runs::{self, Command as Sheet, ConvertSpec, InstancesSpec, Run, Runs, State};

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
            State::Running => std::thread::sleep(std::time::Duration::from_millis(50)),
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

    let runs = Runs::default();
    let mut sheets = Vec::new();
    for (name, src, isolated, overwrite) in [
        ("project", &src, false, false),
        ("isolated", &src, true, false),
        ("no-project", &bare, false, false),
        ("overwrite", &src, false, true),
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
        let c = &run.command;
        match name {
            "project" | "overwrite" => {
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
    }
    // The instance check changes what the review queue says.
    assert_ne!(
        masked(&d.join("gui plain")),
        masked(&d.join("gui instances")),
        "the instance check was not read"
    );
    let _ = std::fs::remove_dir_all(&d);
}
