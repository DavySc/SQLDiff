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

fn without_test_modules(text: &str) -> String {
    let start =
        Regex::new(r"(?m)^[ \t]*#\[cfg\(test\)\]\s*(pub(\(crate\))?\s+)?mod\s+\w+\s*\{").unwrap();
    let mut out = String::new();
    let mut at = 0;
    while let Some(m) = start.find_at(text, at) {
        out.push_str(&text[at..m.start()]);
        at = m.end() + block_end(&text[m.end()..]);
    }
    out.push_str(&text[at..]);
    out
}

/// The length of a Rust block's body up to and including the `}` that
/// closes it; braces in strings, character literals and comments do not count.
fn block_end(s: &str) -> usize {
    let b = s.as_bytes();
    let mut depth = 1usize;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            b'r' if matches!(b.get(i + 1), Some(b'#' | b'"'))
                && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) =>
            {
                let mut j = i + 1;
                while b.get(j) == Some(&b'#') {
                    j += 1;
                }
                if b.get(j) == Some(&b'"') {
                    let close = format!("\"{}", "#".repeat(j - i - 1));
                    let rest = &s[j + 1..];
                    i = j + 1 + rest.find(&close).expect("closed raw string") + close.len() - 1;
                }
            }
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'\'' => {
                if b.get(i + 1) == Some(&b'\\') {
                    if let Some(k) = s[i + 2..].find('\'') {
                        i += 2 + k;
                    }
                } else if b.get(i + 2) == Some(&b'\'') {
                    i += 2;
                } else if let Some(c) = s[i + 1..].chars().next() {
                    let n = c.len_utf8();
                    if b.get(i + 1 + n) == Some(&b'\'') {
                        i += 1 + n;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    panic!("unbalanced test module");
}

/// Comments cut from the text, so prose naming the crate does not count.
fn without_comments(text: &str) -> String {
    text.lines()
        .map(|l| match l.find("//") {
            Some(i) if !l[..i].contains('"') => &l[..i],
            _ => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

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
