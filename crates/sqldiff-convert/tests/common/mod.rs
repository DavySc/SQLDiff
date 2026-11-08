//! Helpers the source-scanning tests share: test modules and comments cut
//! from a Rust file.

#![allow(dead_code)]

use regex::Regex;

pub fn without_test_modules(text: &str) -> String {
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
pub fn block_end(s: &str) -> usize {
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

pub fn without_comments(text: &str) -> String {
    text.lines()
        .map(|l| match l.find("//") {
            Some(i) if !l[..i].contains('"') => &l[..i],
            _ => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
