//! The assumption registry (`profiles/assumptions.toml`) is complete by
//! construction: this test scans every crate's sources and rule files for
//! `ASM-*` ids and fails when an id is used but not registered, registered
//! but not used, or used in a file its entry does not list. It also keeps
//! `docs/assumptions.md` generated from the registry:
//! `SQLDIFF_BLESS=1 cargo test -p sqldiff-core --test assumptions`.
//!
//! What counts as a use:
//! - in Rust sources (`crates/*/src`, test modules left out), a string
//!   literal that starts with an id: `"ASM-NAME"`, `"ASM-NAME:{key}"` (an
//!   instance of `ASM-NAME`), `"ASM-NAME-{rest}"` (the family `ASM-NAME-*`);
//!   a literal that is only a prefix (`"ASM-FACT-"`, read with
//!   `starts_with`) must match a registered id or family but is not a use;
//! - in rule files (`crates/*/profiles/**/*.rules`), an id outside a comment.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use regex::Regex;
use sqldiff_core::assumptions::{self, registry};

#[derive(Default)]
struct Scan {
    /// Registry key (an id, or a family `ASM-NAME-*`) to the files using it.
    uses: BTreeMap<String, BTreeSet<String>>,
    /// Literal ids that the registry does not know, with their files.
    unknown: BTreeMap<String, BTreeSet<String>>,
    /// Prefix-only literals, with their files.
    prefixes: BTreeMap<String, BTreeSet<String>>,
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

fn record(scan: &mut Scan, id: &str, next: Option<char>, file: &str) {
    if id.ends_with('-') {
        if next == Some('{') {
            let fam = format!("{id}*");
            if registry().iter().any(|e| e.id == fam) {
                scan.uses.entry(fam).or_default().insert(file.into());
            } else {
                scan.unknown.entry(fam).or_default().insert(file.into());
            }
        } else {
            scan.prefixes
                .entry(id.to_string())
                .or_default()
                .insert(file.into());
        }
        return;
    }
    match assumptions::lookup(id) {
        Some(e) => {
            scan.uses
                .entry(e.id.clone())
                .or_default()
                .insert(file.into());
        }
        None => {
            scan.unknown
                .entry(id.to_string())
                .or_default()
                .insert(file.into());
        }
    }
}

fn scan() -> Scan {
    let root = crates_dir();
    let mut scan = Scan::default();
    let literal = Regex::new(r#""(ASM-[A-Z0-9][A-Z0-9-]*)(.)?"#).unwrap();
    let bare = Regex::new(r"\bASM-[A-Z0-9][A-Z0-9-]*").unwrap();
    let mut crates: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("Cargo.toml").is_file())
        .collect();
    crates.sort();
    assert!(crates.len() > 3, "no crates under {}", root.display());
    for c in crates {
        let rel = |p: &Path| {
            p.strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        };
        let mut rs = Vec::new();
        files(&c.join("src"), "rs", &mut rs);
        for p in rs {
            let text = without_test_modules(&std::fs::read_to_string(&p).unwrap());
            let file = rel(&p);
            for m in literal.captures_iter(&text) {
                let next = m.get(2).and_then(|n| n.as_str().chars().next());
                record(&mut scan, &m[1], next, &file);
            }
        }
        let mut rules = Vec::new();
        files(&c.join("profiles"), "rules", &mut rules);
        for p in rules {
            let text: String = std::fs::read_to_string(&p)
                .unwrap()
                .lines()
                .filter(|l| !l.trim_start().starts_with(';'))
                .collect::<Vec<_>>()
                .join("\n");
            let file = rel(&p);
            for m in bare.find_iter(&text) {
                record(&mut scan, m.as_str(), None, &file);
            }
        }
    }
    scan
}

#[test]
fn every_assumption_id_in_the_sources_is_registered_and_every_registered_id_is_used() {
    let scan = scan();
    let mut problems = Vec::new();
    for (id, files) in &scan.unknown {
        problems.push(format!(
            "{id} is used but not registered ({}); add it to crates/sqldiff-core/profiles/assumptions.toml",
            files.iter().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    for e in registry() {
        let want: BTreeSet<String> = e.sites.iter().cloned().collect();
        match scan.uses.get(&e.id) {
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
    }
    for (p, files) in &scan.prefixes {
        if !registry().iter().any(|e| e.id.starts_with(p.as_str())) {
            problems.push(format!(
                "the prefix {p} ({}) matches no registered id",
                files.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn the_assumption_table_is_generated_from_the_registry() {
    let path = crates_dir().join("../docs/assumptions.md");
    let want = assumptions::markdown();
    if std::env::var_os("SQLDIFF_BLESS").is_some() {
        std::fs::write(&path, &want).unwrap();
    }
    let got = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        got == want,
        "{} is stale: run SQLDIFF_BLESS=1 cargo test -p sqldiff-core --test assumptions",
        path.display()
    );
}
