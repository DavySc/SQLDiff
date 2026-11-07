//! Every field of the engine's `Options` is accounted for in a conversion
//! check (performance plan W4.3, review B-5).
//!
//! The per-file result key holds the helpers, the draft (whose options carry
//! `anchors` and `historyless`) and, until the series' PR 3, the catalog
//! digests. The other fields are not in it, which is right only while a tree
//! run never sets them. The W5 live loop is where that could stop being true.
//! So:
//!
//! 1. `Options` is destructured without `..`: a new field breaks this build
//!    until it is classified here;
//! 2. the sources of the converter are scanned: every `Options { .. }` it
//!    builds names only the fields classified as "set by the converter", and
//!    each of them is in the key or recorded.

use std::path::PathBuf;

mod common;
use common::{block_end, without_comments, without_test_modules};

use sqldiff_core::Options;

/// How a field reaches a conversion check, and why that is sound.
const FIELDS: &[(&str, &str)] = &[
    ("excerpt_lines", "constant: the default, never set by the converter"),
    ("catalog_a", "recorded (PR 3); today in the key as a digest"),
    ("catalog_b", "recorded (PR 3); today in the key as a digest"),
    ("facts_a", "recorded (PR 3); today in the key as a digest"),
    ("facts_b", "recorded (PR 3); today in the key as a digest"),
    ("helpers", "in the key (`Debug` of all helpers)"),
    ("flips", "constant: empty in a conversion check"),
    ("declared", "constant: empty in a conversion check"),
    ("proof", "constant: off in a conversion check"),
    ("observer", "constant: no side channel in a conversion check"),
    ("anchors", "in the key: derived from the draft's units"),
    ("historyless", "in the key: the draft's options"),
    ("skip_witnesses", "in the key: fixed by the converter (true in every conversion check, by the check fingerprint's code)"),
    ("work_budgets", "constant: the built-in budgets"),
    ("work_caps", "constant: the built-in caps"),
    ("alignment_bound", "constant: the built-in bound"),
    ("statement_work_cap", "constant: no statement-level allowance"),
];

#[test]
fn every_option_is_classified() {
    let Options {
        excerpt_lines: _,
        catalog_a: _,
        catalog_b: _,
        facts_a: _,
        facts_b: _,
        helpers: _,
        flips: _,
        declared: _,
        proof: _,
        observer: _,
        anchors: _,
        historyless: _,
        skip_witnesses: _,
        work_budgets: _,
        work_caps: _,
        alignment_bound: _,
        statement_work_cap: _,
    } = Options::default();
    assert_eq!(FIELDS.len(), 17);
}

/// The field names assigned at the top level of the `Options { .. }` literals
/// of one file, in source order.
fn literals(text: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = text[at..].find("Options {") {
        let start = at + i;
        // Not `RuleOptions {` and not a type name ending in Options.
        let before = text[..start].chars().next_back();
        at = start + "Options {".len();
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let body = &text[at..at + block_end(&text[at..]) - 1];
        let mut fields = Vec::new();
        let (mut depth, mut word) = (0i32, String::new());
        let b = body.as_bytes();
        let mut k = 0;
        while k < b.len() {
            let c = b[k] as char;
            match c {
                '(' | '{' | '[' => depth += 1,
                ')' | '}' | ']' => depth -= 1,
                ':' if depth == 0 && b.get(k + 1) != Some(&b':') && !word.is_empty() => {
                    fields.push(std::mem::take(&mut word));
                }
                c if depth == 0 && (c.is_alphanumeric() || c == '_') => word.push(c),
                ':' => {
                    // `::` in a path
                    k += 1;
                    word.clear();
                }
                _ => {
                    if depth == 0 {
                        word.clear();
                    }
                }
            }
            k += 1;
        }
        out.push(fields);
    }
    out
}

#[test]
fn the_converter_sets_only_the_fields_that_are_in_the_key_or_recorded() {
    // Per file: the fields each `Options { .. }` literal assigns.
    let expected: &[(&str, &[&[&str]])] = &[
        // Round one of a base catalog: the check a tree file gets.
        ("base.rs", &[&["helpers", "facts_a", "facts_b"]]),
        // `check_one`: the draft's anchors, options and the witnesses switch;
        // then the output's own check against the target catalog.
        (
            "check.rs",
            &[
                &["anchors", "historyless", "skip_witnesses"],
                &["catalog_a", "facts_a", "catalog_b", "facts_b"],
            ],
        ),
        (
            "instances.rs",
            &[&["catalog_a", "facts_a", "catalog_b", "facts_b"]],
        ),
        // The two tree runs (files and templates).
        ("tree.rs", &[&["helpers"], &["helpers"]]),
    ];
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found: Vec<(String, Vec<Vec<String>>)> = Vec::new();
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
        let text = without_comments(&without_test_modules(&std::fs::read_to_string(&p).unwrap()));
        let lits = literals(&text);
        if !lits.is_empty() {
            found.push((p.file_name().unwrap().to_string_lossy().into_owned(), lits));
        }
    }
    let want: Vec<(String, Vec<Vec<String>>)> = expected
        .iter()
        .map(|(f, ls)| {
            (
                f.to_string(),
                ls.iter()
                    .map(|l| l.iter().map(|s| s.to_string()).collect())
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        found, want,
        "the converter builds `Options` differently: a field it sets must be in the result key or recorded, \
         and classified in tests/options_in_key.rs"
    );
    // Every field a literal assigns is classified as in the key or recorded.
    for (_, ls) in &found {
        for l in ls {
            for f in l {
                let why = FIELDS.iter().find(|(n, _)| n == f).unwrap().1;
                assert!(
                    why.starts_with("in the key") || why.starts_with("recorded"),
                    "{f}: {why}"
                );
            }
        }
    }
}

/// A constant field must not be set after the `Options` is built either:
/// `opts.alignment_bound = Some(1)` or `opts.flips.insert(..)` would change a
/// check without touching the key.
#[test]
fn the_constant_fields_are_never_assigned_later() {
    // `proof` is also a field of reports and units, so it counts only on a
    // receiver named like options.
    let re = regex::Regex::new(
        r"\.(excerpt_lines|flips|declared|observer|work_budgets|work_caps|alignment_bound|statement_work_cap)\s*(=[^=]|\.insert|\.push|\.extend)|\b\w*opt\w*\.proof\s*(=[^=]|\.insert|\.push|\.extend)",
    )
    .unwrap();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![dir.clone()];
    let mut seen = 0;
    while let Some(d) = files.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                files.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                seen += 1;
                let text =
                    without_comments(&without_test_modules(&std::fs::read_to_string(&p).unwrap()));
                if let Some(m) = re.find(&text) {
                    panic!(
                        "{}: `{}` sets a field the result key treats as constant; \
                         put it in the key (or record it) and reclassify it in tests/options_in_key.rs",
                        p.display(),
                        m.as_str()
                    );
                }
            }
        }
    }
    assert!(seen > 5, "no sources scanned");
}
