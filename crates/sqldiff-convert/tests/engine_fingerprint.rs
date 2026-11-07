//! The engine fingerprint (`build.rs`) covers the whole dependency closure
//! of the converter among the workspace crates, so a change in any crate
//! that can change a result changes every key.

use std::collections::BTreeSet;
use std::path::PathBuf;

fn crates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// The workspace crates `name` depends on (normal and build dependencies),
/// itself included.
fn closure(name: &str, seen: &mut BTreeSet<String>) {
    if !seen.insert(name.to_string()) {
        return;
    }
    let text = std::fs::read_to_string(crates_dir().join(name).join("Cargo.toml")).unwrap();
    let doc: toml::Table = text.parse().unwrap();
    for section in ["dependencies", "build-dependencies"] {
        let Some(deps) = doc.get(section).and_then(|d| d.as_table()) else {
            continue;
        };
        for dep in deps.keys().filter(|k| k.starts_with("sqldiff-")) {
            closure(dep, seen);
        }
    }
}

#[test]
fn the_fingerprint_covers_every_workspace_crate_the_converter_uses() {
    let mut used = BTreeSet::new();
    closure("sqldiff-convert", &mut used);
    let covered: BTreeSet<String> = sqldiff_convert::FINGERPRINTED_CRATES
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(covered, used);
}

#[test]
fn the_fingerprint_is_a_sha256_hex_digest() {
    let f = sqldiff_convert::ENGINE_FINGERPRINT;
    assert_eq!(f.len(), 64);
    assert!(f.bytes().all(|b| b.is_ascii_hexdigit()));
}
