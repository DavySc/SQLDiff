//! Input the binder cannot read faithfully: template placeholders and client
//! directives that a tool replaces before the SQL reaches the server, routine
//! headers that did not parse, and statements with parse problems. A binding
//! error found there is not proven, because the names may bind differently
//! once the input is what the server sees; it becomes an assumption instead
//! (a `different` verdict must never rest on text SQLDiff could not read).

use sqldiff_syntax::{Dialect, SyntaxKind, SyntaxNode};

use crate::artifact::Analyzed;
use crate::catalog::ddl;

/// A region whose binding errors are not proven.
#[derive(Debug, Clone)]
pub(crate) struct Doubt {
    pub start: usize,
    pub end: usize,
    pub id: String,
    pub text: String,
    /// Appended to the message of each issue it covers.
    pub note: String,
}

/// Prefix of the assumption ids doubts attach.
pub(crate) const PREFIX: &str = "ASM-BIND-UNREAD";

pub(crate) fn is_doubt(id: &str) -> bool {
    id.starts_with(PREFIX)
}

/// Doubts of one artifact.
pub(crate) fn doubts(a: &Analyzed) -> Vec<Doubt> {
    let mut out = Vec::new();
    let text = a.artifact.text.as_str();
    let found = placeholders(&a.root, text, a.dialect());
    if let Some(first) = found.first() {
        let shown: Vec<String> = found.iter().take(3).map(|s| format!("`{s}`")).collect();
        out.push(Doubt {
            start: 0,
            end: text.len(),
            id: format!("{PREFIX}:placeholders"),
            text: format!(
                "The file's placeholders or client directives ({}) do not change which names its statements bind to",
                shown.join(", ")
            ),
            note: format!(
                " Not proven: the file contains `{first}`, which a tool replaces before the SQL runs, so names may bind differently."
            ),
        });
    }
    for r in a
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ROUTINE)
    {
        if ddl::routine_header(&r, a.dialect()).is_none() {
            let range = r.text_range();
            let (line, _) = a.lines.line_col(range.start().into());
            out.push(Doubt {
                start: range.start().into(),
                end: range.end().into(),
                id: format!("{PREFIX}:header-{line}"),
                text: format!(
                    "The routine header on line {line}, which SQLDiff could not read, declares no parameter the body uses"
                ),
                note: format!(
                    " Not proven: the routine header on line {line} could not be read, so its parameters are unknown."
                ),
            });
        }
    }
    for dg in &a.parse.diagnostics {
        let Some((s, e)) = unit_at(&a.root, dg.offset) else {
            continue;
        };
        if out.iter().any(|x| x.start <= s && e <= x.end) {
            continue;
        }
        let (line, _) = a.lines.line_col(dg.offset);
        out.push(Doubt {
            start: s,
            end: e,
            id: format!("{PREFIX}:parse-{line}"),
            text: format!(
                "The parse problem on line {line} does not change which names the statement binds to"
            ),
            note: format!(
                " Not proven: line {line} has a parse problem ({}), so the statement may have been read wrongly.",
                dg.message
            ),
        });
    }
    out
}

/// Range of the outermost routine or statement around `offset`.
fn unit_at(root: &SyntaxNode, offset: usize) -> Option<(usize, usize)> {
    let len: usize = root.text_range().end().into();
    let at = offset.min(len.saturating_sub(1));
    let tok = root
        .token_at_offset(rowan::TextSize::from(at as u32))
        .right_biased()?;
    let unit = tok
        .parent_ancestors()
        .filter(|n| matches!(n.kind(), SyntaxKind::ROUTINE | SyntaxKind::STATEMENT))
        .last()?;
    let r = unit.text_range();
    Some((r.start().into(), r.end().into()))
}

/// Placeholders and client directives in significant text, as spelled.
fn placeholders(root: &SyntaxNode, text: &str, d: Dialect) -> Vec<String> {
    // Text with strings and comments blanked, offsets kept.
    let mut masked: Vec<u8> = text.as_bytes().to_vec();
    let toks: Vec<_> = root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .collect();
    for t in &toks {
        if matches!(
            t.kind(),
            SyntaxKind::STRING
                | SyntaxKind::DOLLAR_STRING
                | SyntaxKind::LINE_COMMENT
                | SyntaxKind::BLOCK_COMMENT
                | SyntaxKind::QUOTED_IDENT
        ) {
            let r = t.text_range();
            let (s, e): (usize, usize) = (r.start().into(), r.end().into());
            for b in &mut masked[s..e.min(text.len())] {
                if *b != b'\n' {
                    *b = b' ';
                }
            }
        }
    }
    let m = String::from_utf8_lossy(&masked).into_owned();
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        if !out.iter().any(|x| x == s) {
            out.push(s.to_string());
        }
    };
    let b = m.as_bytes();
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        // `@name@` (PostgreSQL extension scripts: `@extschema@`).
        if c == b'@' && (i == 0 || !ident(b[i - 1]) && b[i - 1] != b'@') {
            let mut k = i + 1;
            while k < b.len() && ident(b[k]) {
                k += 1;
            }
            if k > i + 1 && k < b.len() && b[k] == b'@' && !b[i + 1].is_ascii_digit() {
                push(&m[i..=k]);
                i = k + 1;
                continue;
            }
        }
        // `${name}` (Flyway, Liquibase, shell), `$(name)` (SQLCMD), `{{ name }}` (Jinja, dbt).
        if c == b'$' && i + 1 < b.len() && (b[i + 1] == b'{' || b[i + 1] == b'(') {
            let close = if b[i + 1] == b'{' { b'}' } else { b')' };
            if let Some(k) = (i + 2..b.len().min(i + 130)).find(|&k| b[k] == close) {
                let inner = m[i + 2..k].trim();
                if !inner.is_empty() && inner.bytes().all(|x| ident(x) || x == b'.' || x == b'-') {
                    push(&m[i..=k]);
                    i = k + 1;
                    continue;
                }
            }
        }
        if c == b'{' && b.get(i + 1) == Some(&b'{') {
            if let Some(k) = m[i..].find("}}") {
                push(&m[i..i + k + 2]);
                i += k + 2;
                continue;
            }
        }
        i += 1;
    }
    // Line directives: SQLCMD `:r`, `:setvar`, ...; psql `\set`, `\gset`, `\i`, ...
    for line in m.lines() {
        let l = line.trim_start();
        if let Some(rest) = l.strip_prefix(':') {
            let word: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect::<String>()
                .to_ascii_lowercase();
            let sqlcmd = [
                "r",
                "setvar",
                "on",
                "connect",
                "out",
                "error",
                "exit",
                "quit",
                "listvar",
                "reset",
                "serverlist",
                "xml",
                "perftrace",
                "ed",
                "help",
            ];
            if d == Dialect::Tsql && sqlcmd.contains(&word.as_str()) {
                push(&format!(":{word}"));
            }
        }
        if d == Dialect::Postgres && l.starts_with('\\') {
            let word: String = l.chars().take_while(|c| !c.is_whitespace()).collect();
            push(&word);
        }
    }
    // psql `:name`, `:'name'`, `:"name"`; Db2 host variables `:name`.
    if d != Dialect::Tsql {
        let sig: Vec<_> = toks.iter().filter(|t| !t.kind().is_trivia()).collect();
        for (k, t) in sig.iter().enumerate() {
            if t.kind() != SyntaxKind::COLON {
                continue;
            }
            let Some(next) = sig.get(k + 1) else { continue };
            let adjacent = next.text_range().start() == t.text_range().end();
            if !adjacent
                || !matches!(
                    next.kind(),
                    SyntaxKind::IDENT | SyntaxKind::STRING | SyntaxKind::QUOTED_IDENT
                )
            {
                continue;
            }
            // Not an array slice `a[i:j]` / `a[:j]`, a Db2 label `l1:` or `name := v`.
            let operand = k
                .checked_sub(1)
                .map(|p| sig[p])
                .map_or(true, |p| match p.kind() {
                    SyntaxKind::IDENT => crate::catalog::builtins::of(d).is_keyword(p.text()),
                    SyntaxKind::QUOTED_IDENT
                    | SyntaxKind::NUMBER
                    | SyntaxKind::R_PAREN
                    | SyntaxKind::R_BRACKET
                    | SyntaxKind::L_BRACKET
                    | SyntaxKind::VARIABLE => false,
                    _ => true,
                });
            if operand {
                push(&format!(":{}", next.text()));
            }
        }
    }
    out
}
