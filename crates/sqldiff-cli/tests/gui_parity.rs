//! A tree converted from the app writes what the command on its run sheet
//! writes, byte for byte: the command is run as shown, through a shell.

use std::path::{Path, PathBuf};
use std::process::Command;

use sqldiff_gui::runs::{self, Command as Sheet, ConvertSpec, Run, Runs, State};

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

/// `files`, with the output folder's name and the timings masked.
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
/// with `sqldiff` on the PATH.
fn run_sheet(sheet: &Sheet, elsewhere: &Path) -> std::process::Output {
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
            &sheet.powershell,
        ]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", &sheet.sh]);
        c
    };
    c.current_dir(elsewhere)
        .env("PATH", std::env::join_paths(path).unwrap())
        .output()
        .unwrap()
}

#[test]
fn the_run_sheet_command_writes_what_the_run_wrote() {
    // One test: the user config is named by the environment, for the app
    // (this process) and the command alike.
    let d = std::env::temp_dir().join(format!("sqldiff-gui-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let user = d.join("user config.toml");
    write(&user, "");
    std::env::set_var("SQLDIFF_CONFIG", &user);
    // The commands run from a folder whose config would break them, so a
    // command that read config from where it is run fails.
    let elsewhere = d.join("elsewhere");
    write(&elsewhere.join("sqldiff.toml"), "not a config [\n");

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
        let o = run_sheet(&sheet, &elsewhere);
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
                assert_eq!(c.cwd, None);
            }
            "isolated" => {
                assert_eq!((&c.project_config, &c.user_config), (&None, &None));
                assert!(c.args.contains(&"--isolated".to_string()));
            }
            _ => {
                assert_eq!(c.project_config, None);
                assert_eq!(c.cwd, Some(bare.display().to_string()));
            }
        }
        if name != "isolated" {
            assert_eq!(c.user_config, Some(user.display().to_string()));
        }
        assert_eq!(c.args.contains(&"--overwrite".to_string()), overwrite);
        sheets.push(sheet);
    }
    // The project config the sheet names is the one the command reads.
    write(&root.join("sqldiff.toml"), "not a config [\n");
    std::fs::remove_dir_all(d.join("cli project")).unwrap();
    let o = run_sheet(&sheets[0], &elsewhere);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(2), "{err}");
    assert!(err.contains("sqldiff.toml"), "{err}");
    std::env::remove_var("SQLDIFF_CONFIG");
    let _ = std::fs::remove_dir_all(&d);
}
