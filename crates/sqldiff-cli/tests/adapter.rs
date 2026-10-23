//! The adapter protocol end to end: `sqldiff serve` as an out-of-process
//! adapter, read back through a pack-declared adapter and small chunks.

use std::path::PathBuf;
use std::process::Command;

use sqldiff_source::config::Sources;
use sqldiff_source::protocol::{ObjectFilter, PairScope};
use sqldiff_source::{FsSource, Resolved, Source, SourceSpec};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

#[test]
fn process_adapter_matches_in_process_source() {
    let dir = std::env::temp_dir().join(format!("sqldiff-adapter-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pack = dir.join("pack.toml");
    let cmd = serde_json::to_string(&[
        env!("CARGO_BIN_EXE_sqldiff"),
        "serve",
        fixtures().to_str().unwrap(),
        "--isolated",
        "--max-chunk",
        "64",
    ])
    .unwrap();
    std::fs::write(
        &pack,
        format!("[pack]\nname = \"t\"\nversion = \"1\"\n\n[[adapters]]\nname = \"fixtures\"\ncommand = {cmd}\ncredential = \"env:UNUSED\"\n"),
    )
    .unwrap();
    let cfg = Resolved::load(&Sources {
        isolated: true,
        packs: vec![pack],
        ..Sources::default()
    })
    .unwrap();
    let remote =
        sqldiff_source::open(&SourceSpec::parse("adapter:fixtures").unwrap(), &cfg).unwrap();
    let local = FsSource::new(&fixtures(), &Resolved::builtin()).unwrap();

    let d = remote.describe().unwrap();
    assert_eq!(d.name, "fs");
    let objects = remote.list_objects(&ObjectFilter::default()).unwrap();
    assert_eq!(
        objects,
        local.list_objects(&ObjectFilter::default()).unwrap()
    );
    for o in &objects {
        // Every artifact is far larger than 64 bytes, so this exercises reassembly.
        assert_eq!(
            remote.get_artifact(&o.id).unwrap().text,
            local.get_artifact(&o.id).unwrap().text
        );
    }
    assert_eq!(remote.get_pairs(&PairScope::default()).unwrap().len(), 3);
    assert!(remote.get_artifact("../etc/passwd").is_err());
}

#[test]
fn pairs_command_diffs_every_pair() {
    let out = Command::new(env!("CARGO_BIN_EXE_sqldiff"))
        .args([
            "pairs",
            fixtures().to_str().unwrap(),
            "--isolated",
            "--diff",
            "-f",
            "json",
        ])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["pairs"].as_array().unwrap().len(), 3);
    assert_eq!(v["provenance"][0]["kind"], "builtin");
    assert_eq!(out.status.code(), Some(1));
}
