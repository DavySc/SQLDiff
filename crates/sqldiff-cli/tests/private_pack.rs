//! A private pack (`--pack`, `private_packs`) is named by its file only: its
//! name and version reach no output, file or error message.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const NAME: &str = "zz-private-pack-name";
const VERSION: &str = "9.9.9-zz-private-version";

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch() -> Scratch {
    let d = std::env::temp_dir().join(format!("sqldiff-private-pack-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    Scratch(d)
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn sqldiff(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sqldiff"))
        .args(args)
        .env_remove("SQLDIFF_CONFIG")
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("home/cfg"))
        .env("APPDATA", root.join("home/cfg"))
        .env("SQLDIFF_DATA_DIR", root.join("data"))
        .current_dir(root)
        .output()
        .unwrap()
}

/// Everything the command wrote, on both streams.
fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn assert_hidden(what: &str, out: &str) {
    assert!(
        !out.contains(NAME) && !out.contains(VERSION),
        "{what} names the private pack:\n{out}"
    );
}

#[test]
fn a_private_pack_is_named_by_its_file_in_every_output() {
    let s = scratch();
    let root = s.0.as_path();
    let pack = root.join("pp");
    write(
        &pack.join("pack.toml"),
        &format!(
            "[pack]\nname = \"{NAME}\"\nversion = \"{VERSION}\"\ndescription = \"synthetic\"\n"
        ),
    );
    let tsql = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  SELECT 1 AS x;\nEND\n";
    let pg = "CREATE PROCEDURE dbo.p()\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  PERFORM 1;\nEND;\n$$;\n";
    write(&root.join("a/p.sql"), tsql);
    write(&root.join("b/p.sql"), pg);
    write(&root.join("src/p.tsql.sql"), tsql);
    write(&root.join("src/p.pg.sql"), pg);
    let pack_arg = pack.display().to_string();
    let file = pack.join("pack.toml").display().to_string();
    let with_pack = |args: &[&str]| -> Output {
        let mut all = vec!["--isolated", "--pack", pack_arg.as_str()];
        all.splice(0..0, args.iter().copied());
        sqldiff(root, &all)
    };
    let tree = ["tree", "--dialect-a", "tsql", "--dialect-b", "postgres"];

    let runs: Vec<(&str, Vec<&str>)> = vec![
        ("tree json", [&tree[..], &["-f", "json", "a", "b"]].concat()),
        (
            "tree json --redact",
            [&tree[..], &["-f", "json", "--redact", "a", "b"]].concat(),
        ),
        ("tree text", [&tree[..], &["a", "b"]].concat()),
        ("pairs json", vec!["pairs", "-f", "json", "src"]),
        (
            "pairs --diff json",
            vec!["pairs", "--diff", "-f", "json", "src"],
        ),
        ("pairs --diff text", vec!["pairs", "--diff", "src"]),
        ("queue json", vec!["queue", "-f", "json", "src"]),
        ("config", vec!["config"]),
        ("snapshot", vec!["snapshot", "--output", "snap.json", "src"]),
    ];
    for (what, args) in &runs {
        let o = with_pack(args);
        let out = text(&o);
        assert!(o.status.code().is_some_and(|c| c < 2), "{what}: {out}");
        assert_hidden(what, &out);
    }
    let snap = std::fs::read_to_string(root.join("snap.json")).unwrap();
    assert_hidden("the snapshot file", &snap);

    // The pack is still confirmed, by its file.
    let o = with_pack(&["config"]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let layer = v["layers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "private_pack")
        .unwrap();
    assert_eq!(layer["name"], file.as_str());
    assert!(layer.get("version").is_none(), "{layer}");
    let snap: serde_json::Value = serde_json::from_str(&snap).unwrap();
    assert!(
        snap["provenance"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["name"] == file.as_str()),
        "{snap}"
    );

    // Two packs of one name: the error names their files.
    let copy = root.join("pp2");
    write(
        &copy.join("pack.toml"),
        &std::fs::read_to_string(pack.join("pack.toml")).unwrap(),
    );
    let o = with_pack(&["config", "--pack", copy.to_str().unwrap()]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(2), "{out}");
    assert!(
        out.contains(&file) && out.contains(&copy.join("pack.toml").display().to_string()),
        "{out}"
    );
    assert_hidden("the duplicate error", &out);
}
