//! The check stage does not run draft code. A stored result is keyed by
//! `CHECK_FINGERPRINT`, which leaves the draft crate out (see `build.rs` and
//! `check::Mix`), so any file the check runs through that calls the draft
//! crate would let an edit of draft code leave a stale result in place.
//!
//! This test lists the files of `sqldiff-convert/src` that may name the
//! draft crate or call one of its entry points (test modules left out) and
//! fails for any other file, and for a listed file that no longer does (so
//! the list shrinks with the code).

use std::path::PathBuf;

use regex::Regex;

mod common;
use common::{without_comments, without_test_modules};

/// Files that build drafts or run draft code, and why they may.
const MAY_DRAFT: &[(&str, &str)] = &[
    (
        "base.rs",
        "pass 1: the base catalog is drafted (full fingerprint)",
    ),
    ("lib.rs", "re-exports and the single-text entry points"),
    ("refused.rs", "pass 1: stand-ins for refused statements"),
    (
        "template.rs",
        "templates are drafted per fragment; never stored",
    ),
    (
        "tree.rs",
        "the driver: drafts, claims and the mix `rebuild`",
    ),
];

#[test]
fn only_the_listed_files_run_draft_code() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    // The crate's name, or any draft entry point (the crate root re-exports
    // them, so a file reaches them as `crate::rewrite_in` or by a `use`).
    let draft = Regex::new(
        r"sqldiff_draft|\brewrite(_in|_with|_in_tree|_in_tree_plain)?\b|\bwithout_continuation\b|\bwith_plain_routines\b|\bClaims::of\b",
    )
    .unwrap();
    let mut using = Vec::new();
    let mut seen = 0;
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    files.sort();
    for p in files {
        if p.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        seen += 1;
        let text = without_comments(&without_test_modules(&std::fs::read_to_string(&p).unwrap()));
        if draft.is_match(&text) {
            using.push(p.file_name().unwrap().to_string_lossy().into_owned());
        }
    }
    assert!(seen > 10, "no sources under {}", dir.display());
    let want: Vec<String> = MAY_DRAFT.iter().map(|(f, _)| f.to_string()).collect();
    assert_eq!(
        using, want,
        "files that run draft code differ from the list in tests/stage_boundary.rs: \
         a file the check runs through must not call the draft crate (CHECK_FINGERPRINT leaves it out)"
    );
}
