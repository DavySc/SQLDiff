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
    // Plain tables and the same tables under `[target.<cfg>.…]`.
    let mut tables: Vec<&toml::Table> = vec![&doc];
    if let Some(t) = doc.get("target").and_then(|t| t.as_table()) {
        tables.extend(t.values().filter_map(|v| v.as_table()));
    }
    for table in tables {
        for section in ["dependencies", "build-dependencies"] {
            let Some(deps) = table.get(section).and_then(|d| d.as_table()) else {
                continue;
            };
            for dep in deps.keys().filter(|k| k.starts_with("sqldiff-")) {
                closure(dep, seen);
            }
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

/// The check stage's fingerprint covers the converter's closure except the
/// draft-only crates: the crates only the draft stage reaches. A crate the
/// check also uses (syntax, catalog, model, equiv, units, core, source)
/// must stay in it.
#[test]
fn the_check_fingerprint_leaves_out_exactly_the_draft_only_crates() {
    let mut used = BTreeSet::new();
    closure("sqldiff-convert", &mut used);
    let check: BTreeSet<String> = sqldiff_convert::CHECK_FINGERPRINTED_CRATES
        .iter()
        .map(|s| s.to_string())
        .collect();
    let left_out: BTreeSet<String> = used.difference(&check).cloned().collect();
    assert_eq!(left_out, BTreeSet::from(["sqldiff-draft".to_string()]));
    assert!(check.is_subset(&used));
}

/// What the draft crate may depend on, and so what a change outside the
/// check fingerprint can reach: never the engine above it. A dependency on
/// core, convert, source or review would make draft code run inside code the
/// check fingerprint covers.
#[test]
fn the_draft_crate_does_not_reach_the_check_stage() {
    let mut used = BTreeSet::new();
    closure("sqldiff-draft", &mut used);
    for above in ["sqldiff-core", "sqldiff-convert", "sqldiff-source"] {
        assert!(!used.contains(above), "sqldiff-draft depends on {above}");
    }
}

/// The check stage (everything the converter links except the draft crate)
/// must not depend on the draft crate through a workspace crate other than
/// the converter itself: only `sqldiff-convert` links it.
#[test]
fn only_the_converter_links_the_draft_crate() {
    let mut users = Vec::new();
    for e in std::fs::read_dir(crates_dir()).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(text) = std::fs::read_to_string(e.path().join("Cargo.toml")) else {
            continue;
        };
        let doc: toml::Table = text.parse().unwrap();
        let uses = ["dependencies", "dev-dependencies", "build-dependencies"]
            .iter()
            .any(|s| {
                doc.get(*s)
                    .and_then(|d| d.as_table())
                    .is_some_and(|d| d.contains_key("sqldiff-draft"))
            });
        if uses && name != "sqldiff-draft" {
            users.push(name);
        }
    }
    users.sort();
    assert_eq!(users, ["sqldiff-convert"], "who links sqldiff-draft");
}

#[test]
fn the_check_fingerprint_differs_from_the_engine_fingerprint() {
    let f = sqldiff_convert::CHECK_FINGERPRINT;
    assert_eq!(f.len(), 64);
    assert!(f.bytes().all(|b| b.is_ascii_hexdigit()));
    // The draft crate's sources are in one and not the other.
    assert_ne!(f, sqldiff_convert::ENGINE_FINGERPRINT);
}
