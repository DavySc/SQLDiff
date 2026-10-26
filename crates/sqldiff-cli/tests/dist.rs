//! `sqldiff dist`: colleague folders, with and without a bundled config.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A scratch folder, removed when the test ends, also when it fails: each
/// one holds copies of the executables (hundreds of MB in a debug build).
struct Scratch(PathBuf);

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Scratch {
    let d = std::env::temp_dir().join(format!("sqldiff-dist-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    Scratch(d)
}

/// Runs `bin` with no user config of its own: an empty home, no $SQLDIFF_CONFIG.
fn run(bin: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .env_remove("SQLDIFF_CONFIG")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("cfg"))
        .env("APPDATA", home.join("cfg"))
        .env("SQLDIFF_DATA_DIR", home.join("data"))
        .current_dir(home)
        .output()
        .unwrap()
}

fn sqldiff(home: &Path, args: &[&str]) -> Output {
    run(Path::new(env!("CARGO_BIN_EXE_sqldiff")), home, args)
}

fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

const PACK: &str = r#"deny_list = ["examplecorp-internal"]

[pack]
name = "org conventions"
version = "2.1"

[[pairing.rules]]
name = "strip-usp"
strip_prefix = ["usp_"]
"#;

/// A user config the way a developer keeps it: relative pack path, machine
/// paths for data and source, an adapter source that travels.
fn private_setup(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir.join("packs/org")).unwrap();
    std::fs::write(dir.join("packs/org/pack.toml"), PACK).unwrap();
    let cfg = dir.join("config.toml");
    std::fs::write(
        &cfg,
        r#"private_packs = ["packs/org"]
data_dir = "data"
excerpt_lines = 5

[dialect_versions]
db2i = "7.5"
"#,
    )
    .unwrap();
    cfg
}

#[test]
fn generic_distribution_has_no_config() {
    let home = scratch("generic");
    let out = home.join("out");
    let o = sqldiff(&home, &["dist", "--no-config", "-o", out.to_str().unwrap()]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(out.join(exe("sqldiff")).is_file());
    assert!(out.join("README.md").is_file());
    assert!(!out.join("config").exists());
}

#[test]
fn bundled_config_is_loaded_beside_the_executable() {
    let home = scratch("bundled");
    let cfg = private_setup(&home.join("private"));
    let out = home.join("colleague");
    let o = sqldiff(
        &home,
        &[
            "dist",
            "-o",
            out.to_str().unwrap(),
            "--user-config",
            cfg.to_str().unwrap(),
        ],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).contains("data_dir left out"), "{}", stderr(&o));

    let bundled = std::fs::read_to_string(out.join("config/config.toml")).unwrap();
    assert!(bundled.contains("packs/org_conventions"), "{bundled}");
    assert!(!bundled.contains("data_dir"), "{bundled}");
    assert_eq!(
        std::fs::read_to_string(out.join("config/packs/org_conventions/pack.toml")).unwrap(),
        PACK
    );
    assert!(out.join("config/PRIVATE.txt").is_file());

    // The copied executable finds config/ by itself, from any working directory.
    let o = run(&out.join(exe("sqldiff")), &home, &["config"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let layers = v["layers"].as_array().unwrap();
    assert!(
        layers
            .iter()
            .any(|l| l["kind"] == "private_pack" && l["name"] == "org conventions"),
        "{layers:?}"
    );
    assert!(
        layers
            .iter()
            .any(|l| l["kind"] == "user" && l["name"].as_str().unwrap().ends_with("config.toml")),
        "{layers:?}"
    );
    assert_eq!(v["excerpt_lines"], 5);
    assert_eq!(v["dialect_versions"]["db2i"], "7.5");

    // $SQLDIFF_CONFIG still wins, and --isolated still ignores it.
    let o = run(&out.join(exe("sqldiff")), &home, &["config", "--isolated"]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["layers"].as_array().unwrap().len(), 1, "{v}");
}

#[test]
fn private_bundle_is_never_written_into_a_work_tree() {
    let home = scratch("worktree");
    let cfg = private_setup(&home.join("private"));
    let repo = home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    let out = repo.join("dist/colleague");
    let o = sqldiff(
        &home,
        &[
            "dist",
            "-o",
            out.to_str().unwrap(),
            "--user-config",
            cfg.to_str().unwrap(),
        ],
    );
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert!(stderr(&o).contains("git work tree"), "{}", stderr(&o));
    assert!(!repo.join("dist").exists());
}

#[test]
fn refuses_a_config_that_does_not_load() {
    let home = scratch("secret");
    let dir = home.join("private");
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("config.toml");
    std::fs::write(
        &cfg,
        "[[adapters]]\nname = \"a\"\ncommand = [\"a\"]\nenv = { DB_PASSWORD = \"x\" }\n",
    )
    .unwrap();
    let out = home.join("out");
    let o = sqldiff(
        &home,
        &[
            "dist",
            "-o",
            out.to_str().unwrap(),
            "--user-config",
            cfg.to_str().unwrap(),
        ],
    );
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert!(stderr(&o).contains("looks like a secret"), "{}", stderr(&o));
    assert!(!out.exists());
}

#[test]
fn needs_a_config_or_no_config() {
    let home = scratch("none");
    let out = home.join("out");
    let o = sqldiff(&home, &["dist", "-o", out.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert!(stderr(&o).contains("--no-config"), "{}", stderr(&o));
    assert!(!out.exists());
}

#[test]
fn refuses_a_folder_that_is_not_empty() {
    let home = scratch("nonempty");
    let out = home.join("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("x"), "x").unwrap();
    let o = sqldiff(&home, &["dist", "--no-config", "-o", out.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
}

#[test]
fn bundles_another_platforms_build() {
    let home = scratch("cross");
    let win = home.join("win");
    std::fs::create_dir_all(&win).unwrap();
    for p in ["sqldiff.exe", "sqldiff-gui.exe", "sqldiff-mcp.exe"] {
        std::fs::write(win.join(p), p).unwrap();
    }
    let out = home.join("out");
    let o = sqldiff(
        &home,
        &[
            "dist",
            "--no-config",
            "--from",
            win.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    for p in [
        "sqldiff.exe",
        "sqldiff-gui.exe",
        "sqldiff-mcp.exe",
        "README.md",
    ] {
        assert!(out.join(p).is_file(), "{p}");
    }
}
