//! The rule proof registry (`profiles/rule-proofs.toml`) is complete by
//! construction: this test scans the converter's sources and the rule files
//! for rule ids and fails when an id is used but not registered, registered
//! but not used, or used in a file its entry does not list. It also keeps
//! `docs/proof-coverage.md` generated from the registry:
//! `SQLDIFF_BLESS=1 cargo test -p sqldiff-core --test rule_proofs`.
//!
//! What counts as a use:
//! - in `crates/sqldiff-convert/src` (test modules left out), a string
//!   literal `"tsql.x"`, `"pg.x"`, `"idiom.x"` or `"db2.x"`; a literal the
//!   code completes (`"tsql.func.{}"`) is a use of the family `tsql.func.*`,
//!   and a member registered on its own (`tsql.func.len`) is used when its
//!   family is and its name (`"LEN"`) is a literal in the family's sites;
//! - in `crates/sqldiff-core/profiles/rules/*.rules`, the head of a
//!   `(translate ...)`, `(idiom ...)` or `(lint ...)` form, whose kind must
//!   match.
//!
//! Finding and reporting ids (`equiv.*`, `review.*`, `template.*`, ...) are
//! not conversion rules and are not scanned.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use regex::Regex;
use sqldiff_core::rule_proofs::{self, registry, Kind};
use sqldiff_core::rules::rule::{self, RuleSet};

#[derive(Default)]
struct Scan {
    /// Registry key (an id, or a family `tsql.func.*`) to the files using it.
    uses: BTreeMap<String, BTreeSet<String>>,
    /// Ids and families the registry does not know, with their files.
    unknown: BTreeMap<String, BTreeSet<String>>,
    /// The kind each rule-file head has.
    kinds: BTreeMap<String, Kind>,
    /// The text of each scanned Rust file, by relative path.
    texts: BTreeMap<String, String>,
}

fn crates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            files(&p, ext, out);
        } else if p.extension().is_some_and(|e| e == ext) {
            out.push(p);
        }
    }
}

/// The text without `#[cfg(test)] mod name { ... }` blocks: test modules
/// use made-up ids on purpose.
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
/// closes it, given the text right after its `{`. Braces in strings, raw
/// strings, character literals and comments do not count.
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
            // A character literal ('{', '\'', '\u{7b}'); a lifetime has no
            // closing quote within a few bytes.
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

/// A quoted literal and a rule-file head must be registered as is; a
/// family literal (`"tsql.func.{"`, recorded as `tsql.func.*`) names its
/// family. Neither falls back to a wider family: a literal the registry
/// lost must be reported, not swallowed by `tsql.*`.
fn record(scan: &mut Scan, key: &str, file: &str) {
    let known = registry()
        .binary_search_by(|e| e.id.as_str().cmp(key))
        .is_ok();
    let bucket = if known {
        &mut scan.uses
    } else {
        &mut scan.unknown
    };
    bucket
        .entry(key.to_string())
        .or_default()
        .insert(file.into());
}

fn scan() -> Scan {
    let root = crates_dir();
    let mut scan = Scan::default();
    let rel = |p: &Path| {
        p.strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/")
    };
    // `"tsql.ctl.if"` is an id; `"tsql.func.{"` the family `tsql.func.*`.
    let literal = Regex::new(r#""((?:tsql|pg|idiom|db2)\.[a-z0-9._-]*)(["{])"#).unwrap();
    let mut rs = Vec::new();
    files(&root.join("sqldiff-convert/src"), "rs", &mut rs);
    assert!(
        rs.len() > 3,
        "no converter sources under {}",
        root.display()
    );
    for p in rs {
        let text = without_test_modules(&std::fs::read_to_string(&p).unwrap());
        let file = rel(&p);
        for m in literal.captures_iter(&text) {
            let (body, end) = (&m[1], &m[2]);
            if end == "{" {
                record(&mut scan, &format!("{body}*"), &file);
            } else if body.len() > body.find('.').unwrap() + 1 {
                record(&mut scan, body, &file);
            }
        }
        scan.texts.insert(file, text);
    }
    let head = Regex::new(r"(?m)^\((translate|idiom|lint)\s+(\S+)").unwrap();
    let mut rules = Vec::new();
    files(
        &root.join("sqldiff-core/profiles/rules"),
        "rules",
        &mut rules,
    );
    assert_eq!(rules.len(), rule::BUILTIN.len(), "the built-in rule files");
    for p in rules {
        let text = std::fs::read_to_string(&p).unwrap();
        let file = rel(&p);
        for m in head.captures_iter(&text) {
            let kind = match &m[1] {
                "translate" => Kind::Translate,
                "idiom" => Kind::Idiom,
                _ => Kind::Lint,
            };
            let id = m[2].trim_end_matches(')');
            scan.kinds.insert(id.to_string(), kind);
            record(&mut scan, id, &file);
        }
    }
    scan
}

#[test]
fn every_rule_id_in_the_sources_is_registered_and_every_registered_id_is_used() {
    let scan = scan();
    let mut problems = Vec::new();
    for (id, files) in &scan.unknown {
        problems.push(format!(
            "{id} is used but not registered ({}); add it to crates/sqldiff-core/profiles/rule-proofs.toml",
            files.iter().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    for e in registry() {
        let want: BTreeSet<String> = e.sites.iter().cloned().collect();
        let used = match &e.family {
            // A member of a family: the family is used, and the member's
            // name is a literal the family's code can build the id from.
            Some(f) => {
                let name = e.id[f.len() - 1..].to_ascii_uppercase();
                let lit = format!("\"{name}\"");
                let named = e
                    .sites
                    .iter()
                    .any(|s| scan.texts.get(s).is_some_and(|t| t.contains(&lit)));
                if !named {
                    problems.push(format!(
                        "{}: no literal {lit} in {:?}, so the family {f} cannot build it",
                        e.id, e.sites
                    ));
                }
                scan.uses.get(f)
            }
            None => scan.uses.get(&e.id),
        };
        match used {
            None => problems.push(format!(
                "{} is registered but no source uses it; remove it from the registry",
                e.id
            )),
            Some(got) if *got != want => problems.push(format!(
                "{}: the registry lists sites {:?}, the sources use it in {:?}",
                e.id, want, got
            )),
            Some(_) => {}
        }
        match scan.kinds.get(&e.id) {
            Some(k) if *k != e.kind => problems.push(format!(
                "{}: the registry says kind `{}`, the rule file `{}`",
                e.id,
                e.kind.label(),
                k.label()
            )),
            None if !e.kind.is_converter() => problems.push(format!(
                "{}: kind `{}` but no rule file has that head",
                e.id,
                e.kind.label()
            )),
            _ => {}
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn the_rule_loader_sees_the_same_rule_file_ids() {
    let set = RuleSet::builtin();
    let loaded: BTreeSet<&str> = set.rules.iter().map(|r| r.id.as_str()).collect();
    let registered: BTreeSet<&str> = registry()
        .iter()
        .filter(|e| !e.kind.is_converter())
        .map(|e| e.id.as_str())
        .collect();
    assert_eq!(loaded, registered);
    for r in &set.rules {
        let e = rule_proofs::lookup(&r.id).unwrap();
        let want = match r.kind {
            rule::Kind::Idiom => Kind::Idiom,
            rule::Kind::Translate => Kind::Translate,
            rule::Kind::Lint => Kind::Lint,
        };
        assert_eq!(e.kind, want, "{}", r.id);
    }
}

#[test]
fn the_proof_coverage_table_is_generated_from_the_registry() {
    let path = crates_dir().join("../docs/proof-coverage.md");
    let want = rule_proofs::markdown();
    if std::env::var_os("SQLDIFF_BLESS").is_some() {
        std::fs::write(&path, &want).unwrap();
    }
    // A CRLF checkout (Windows) is the same file.
    let got = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        got == want,
        "{} is stale: run SQLDIFF_BLESS=1 cargo test -p sqldiff-core --test rule_proofs",
        path.display()
    );
}
