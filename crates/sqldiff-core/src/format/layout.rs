//! Layout: which gaps between tokens get a line break, and how deep each
//! line is indented.
//!
//! The formatter keeps every non-whitespace token (comments included) in
//! order and rewrites only the whitespace between them, plus the letter case
//! of keywords. [`collect`] walks the CST once and records for each token its
//! indent level, its parenthesis depth inside its statement and whether a
//! line break goes before it; [`render`] turns that into text.

use std::collections::BTreeSet;

use sqldiff_syntax::{Diagnostic, Dialect, SyntaxElement, SyntaxKind, SyntaxKind::*, SyntaxNode};

use super::keywords::recasable;
use super::{FormatOptions, IndentStyle, KeywordCase};

/// Why a line break goes before a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Brk {
    None,
    /// First token of a statement or compound statement.
    Unit,
    /// A keyword of a compound statement after its body: `ELSE`, `END`, `END TRY`.
    Footer,
    /// A clause keyword of a query or DML statement: `FROM`, `WHERE`, `JOIN`.
    Clause,
    /// An item after a top-level comma, with `one_item_per_line`.
    Item,
}

#[derive(Debug, Clone)]
pub(super) struct Tok {
    pub kind: SyntaxKind,
    pub start: usize,
    pub end: usize,
    pub level: usize,
    /// Parenthesis depth relative to the statement that owns the token.
    pub depth: usize,
    pub brk: Brk,
    /// The whitespace before the token and its text are kept as written.
    pub verbatim: bool,
    /// Only the whitespace before the token is kept as written.
    pub gap_kept: bool,
    /// Index of the top-level statement the token belongs to.
    pub top: Option<usize>,
}

impl Tok {
    fn is_comment(&self) -> bool {
        matches!(self.kind, LINE_COMMENT | BLOCK_COMMENT)
    }
}

pub(super) struct Layout {
    pub toks: Vec<Tok>,
    /// Byte range of each top-level statement in the input.
    pub tops: Vec<(usize, usize)>,
}

struct Collector<'a> {
    opts: &'a FormatOptions,
    text: &'a str,
    toks: Vec<Tok>,
    tops: Vec<(usize, usize)>,
    top: Option<usize>,
    depth: usize,
    bases: Vec<usize>,
}

pub(super) fn collect(
    text: &str,
    root: &SyntaxNode,
    diagnostics: &[Diagnostic],
    dialect: Dialect,
    opts: &FormatOptions,
) -> Layout {
    let mut c = Collector {
        opts,
        text,
        toks: Vec::new(),
        tops: Vec::new(),
        top: None,
        depth: 0,
        bases: vec![0],
    };
    c.file(root);
    protect(text, &mut c.toks, &c.tops, diagnostics, dialect);
    Layout {
        toks: c.toks,
        tops: c.tops,
    }
}

impl Collector<'_> {
    fn file(&mut self, node: &SyntaxNode) {
        for el in node.children_with_tokens() {
            match el {
                SyntaxElement::Token(t) => self.token(t.kind(), t.text_range(), 0),
                SyntaxElement::Node(n) if n.kind() == BATCH => self.file(&n),
                SyntaxElement::Node(n) if n.kind().is_unit() => {
                    let r = n.text_range();
                    self.top = Some(self.tops.len());
                    self.tops.push((r.start().into(), r.end().into()));
                    self.unit(&n, 0, true);
                    self.top = None;
                }
                SyntaxElement::Node(n) => self.inline(&n, 0),
            }
        }
    }

    fn token(&mut self, kind: SyntaxKind, range: rowan::TextRange, level: usize) {
        if kind == WHITESPACE || kind == INFERRED_TERMINATOR || range.is_empty() {
            return;
        }
        if kind == R_PAREN {
            self.depth = self.depth.saturating_sub(1);
        }
        let base = *self.bases.last().unwrap_or(&0);
        self.toks.push(Tok {
            kind,
            start: range.start().into(),
            end: range.end().into(),
            level,
            depth: self.depth.saturating_sub(base),
            brk: Brk::None,
            verbatim: false,
            gap_kept: false,
            top: self.top,
        });
        if kind == L_PAREN {
            self.depth += 1;
        }
    }

    /// Index of the last non-comment token pushed so far.
    fn last_sig(&self) -> Option<usize> {
        self.toks.iter().rposition(|t| !t.is_comment())
    }

    fn first_sig_from(&self, from: usize) -> Option<usize> {
        (from..self.toks.len()).find(|&i| !self.toks[i].is_comment())
    }

    /// A token directly inside a compound node: after a body it starts a line.
    fn own_token(&mut self, t: &sqldiff_syntax::SyntaxToken, level: usize, after_body: &mut bool) {
        let n = self.toks.len();
        self.token(t.kind(), t.text_range(), level);
        if self.toks.len() > n && !self.toks[n].is_comment() {
            if *after_body && !matches!(t.kind(), SEMICOLON | COMMA) {
                self.toks[n].brk = Brk::Footer;
            }
            *after_body = false;
        }
    }

    fn unit(&mut self, node: &SyntaxNode, level: usize, brk: bool) {
        let first = self.toks.len();
        self.bases.push(self.depth);
        let mut after_body = false;
        for el in node.children_with_tokens() {
            match el {
                SyntaxElement::Token(t) => self.own_token(&t, level, &mut after_body),
                SyntaxElement::Node(n) => {
                    let k = n.kind();
                    if k == CONDITION || k == LABEL {
                        self.inline(&n, level);
                    } else if k.is_container() {
                        self.container(&n, level);
                        after_body = true;
                    } else if k.is_unit() {
                        self.unit(&n, level + 1, true);
                        after_body = true;
                    } else {
                        self.inline(&n, level);
                    }
                }
            }
        }
        self.bases.pop();
        if brk {
            if let Some(i) = self.first_sig_from(first) {
                self.toks[i].brk = Brk::Unit;
            }
        }
        if node.kind() == STATEMENT {
            self.clauses(first);
        }
        if node.kind() == ERROR {
            for t in &mut self.toks[first..] {
                t.verbatim = true;
            }
        }
    }

    fn container(&mut self, node: &SyntaxNode, level: usize) {
        let k = node.kind();
        let units: Vec<SyntaxNode> = node.children().filter(|c| c.kind().is_unit()).collect();
        // T-SQL `ELSE IF`: the nested IF stays on the ELSE line.
        if k == BRANCH
            && units.len() == 1
            && units[0].kind() == IF_STMT
            && node.children().count() == 1
            && self
                .last_sig()
                .is_some_and(|i| self.toks[i].kind == IDENT && self.text_is(i, "ELSE"))
        {
            for el in node.children_with_tokens() {
                match el {
                    SyntaxElement::Token(t) => self.token(t.kind(), t.text_range(), level),
                    SyntaxElement::Node(n) => self.unit(&n, level, false),
                }
            }
            return;
        }
        // A body that is one BEGIN ... END block lines up with its header.
        let inner = if matches!(k, BRANCH | ROUTINE_BODY)
            && units.len() == 1
            && units[0].kind() == BLOCK
            && node.children().count() == 1
        {
            level
        } else {
            level + 1
        };
        let first = self.toks.len();
        let mut after_body = false;
        for el in node.children_with_tokens() {
            match el {
                SyntaxElement::Token(t) => self.own_token(&t, level, &mut after_body),
                SyntaxElement::Node(n) => {
                    let nk = n.kind();
                    if nk.is_unit() && nk != LABEL {
                        self.unit(&n, inner, true);
                        after_body = true;
                    } else if nk == BRANCH {
                        // `WHEN ... THEN` of a handler: the body is one level in.
                        self.container(&n, level);
                        after_body = true;
                    } else if nk.is_container() {
                        self.container(&n, inner);
                        after_body = true;
                    } else {
                        self.inline(&n, level);
                    }
                }
            }
        }
        if matches!(
            k,
            TRY_BLOCK | CATCH_BLOCK | DECLARE_SECTION | EXCEPTION_SECTION | EXCEPTION_HANDLER
        ) {
            if let Some(i) = self.first_sig_from(first) {
                if self.toks[i].brk == Brk::None {
                    self.toks[i].brk = Brk::Footer;
                }
            }
        }
        // A PostgreSQL body between dollar quotes is kept as written unless
        // `postgres_bodies` is on: the server stores it verbatim.
        if k == ROUTINE_BODY && !self.opts.postgres_bodies {
            let quotes: Vec<usize> = (first..self.toks.len())
                .filter(|&i| self.toks[i].kind == DOLLAR_QUOTE)
                .collect();
            if let (Some(&open), Some(&close)) = (quotes.first(), quotes.last()) {
                if close > open {
                    for t in &mut self.toks[open + 1..=close] {
                        t.verbatim = true;
                    }
                }
            }
        }
    }

    fn inline(&mut self, node: &SyntaxNode, level: usize) {
        for el in node.children_with_tokens() {
            match el {
                SyntaxElement::Token(t) => self.token(t.kind(), t.text_range(), level),
                SyntaxElement::Node(n) if n.kind().is_container() => self.container(&n, level),
                SyntaxElement::Node(n) if n.kind().is_unit() && n.kind() != LABEL => {
                    self.unit(&n, level + 1, true)
                }
                SyntaxElement::Node(n) => self.inline(&n, level),
            }
        }
    }

    fn text_is(&self, i: usize, upper: &str) -> bool {
        self.word(i).is_some_and(|w| w == upper)
    }

    fn word(&self, i: usize) -> Option<String> {
        let t = &self.toks[i];
        (t.kind == IDENT).then(|| self.src(t).to_ascii_uppercase())
    }

    fn src(&self, t: &Tok) -> &str {
        &self.text[t.start..t.end]
    }

    /// Line breaks before the clauses of a query or DML statement.
    fn clauses(&mut self, first: usize) {
        if !self.opts.clause_breaks && !self.opts.one_item_per_line {
            return;
        }
        let sig: Vec<usize> = (first..self.toks.len())
            .filter(|&i| !self.toks[i].is_comment() && !self.toks[i].verbatim)
            .collect();
        if sig.len()
            != (first..self.toks.len())
                .filter(|&i| !self.toks[i].is_comment())
                .count()
        {
            return;
        }
        let words: Vec<String> = sig
            .iter()
            .map(|&i| self.word(i).unwrap_or_default())
            .collect();
        let Some(head) = words.first() else { return };
        let view = (words[0] == "CREATE" || words[0] == "ALTER")
            && words.iter().skip(1).take(4).any(|w| w == "VIEW")
            && words.iter().skip(1).take_while(|w| *w != "VIEW").all(|w| {
                matches!(
                    w.as_str(),
                    "OR" | "REPLACE"
                        | "ALTER"
                        | "MATERIALIZED"
                        | "RECURSIVE"
                        | "TEMP"
                        | "TEMPORARY"
                )
            });
        let dml = matches!(
            head.as_str(),
            "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "WITH"
        );
        if !dml && !view {
            return;
        }
        const JOIN_PREFIX: &[&str] = &[
            "LEFT", "RIGHT", "FULL", "INNER", "CROSS", "OUTER", "NATURAL", "LOOP", "HASH", "MERGE",
            "REMOTE",
        ];
        let mut case_open = 0usize;
        for k in 1..sig.len() {
            let i = sig[k];
            let w = words[k].as_str();
            let prev = words[k - 1].as_str();
            let next = words.get(k + 1).map(String::as_str).unwrap_or("");
            let depth0 = self.toks[i].depth == 0;
            if depth0 {
                match w {
                    "CASE" => case_open += 1,
                    "END" if case_open > 0 => case_open -= 1,
                    _ => {}
                }
            }
            if !depth0 || case_open > 0 || self.toks[i].brk != Brk::None {
                continue;
            }
            if self.opts.one_item_per_line && self.toks[sig[k - 1]].kind == COMMA {
                if self.toks[sig[k - 1]].depth == 0 {
                    self.toks[i].brk = Brk::Item;
                }
                continue;
            }
            // `t.select` is a name.
            if !self.opts.clause_breaks
                || self.toks[i].kind != IDENT
                || self.toks[sig[k - 1]].kind == DOT
            {
                continue;
            }
            let clause = match w {
                "FROM" => prev != "DELETE" && prev != "DISTINCT",
                "WHERE" | "HAVING" | "UNION" | "INTERSECT" | "EXCEPT" | "RETURNING" | "LIMIT"
                | "OFFSET" | "SELECT" => true,
                "GROUP" | "ORDER" => next == "BY",
                "VALUES" => prev != "DEFAULT",
                "SET" => matches!(head.as_str(), "UPDATE" | "MERGE"),
                "OUTPUT" => matches!(head.as_str(), "INSERT" | "UPDATE" | "DELETE" | "MERGE"),
                "USING" | "WHEN" => head == "MERGE",
                "JOIN" => !JOIN_PREFIX.contains(&prev),
                "LEFT" | "RIGHT" | "FULL" | "INNER" | "CROSS" | "NATURAL" => {
                    matches!(
                        next,
                        "JOIN" | "OUTER" | "APPLY" | "HASH" | "LOOP" | "MERGE" | "REMOTE"
                    ) && !JOIN_PREFIX.contains(&prev)
                }
                "OUTER" => next == "APPLY" && !JOIN_PREFIX.contains(&prev),
                _ => false,
            };
            if clause {
                self.toks[i].brk = Brk::Clause;
            }
        }
    }
}

/// Text the lexer reads as SQL but that is not: script-client lines and
/// inline data. These regions, and the line break that ends them, stay
/// exactly as written; so does every top-level statement the parser
/// reported a problem in.
///
/// - A line starting with `\` is a psql meta-command (`\set`, `\if`,
///   `\echo ... \quit`); psql reads it to the end of the line, case
///   sensitively.
/// - In T-SQL a line starting with `:` is a sqlcmd command (`:setvar`, `:r`).
/// - After `COPY ... FROM STDIN;` the lines up to one holding only `\.` are
///   data rows, with tabs between the columns.
fn protect(
    text: &str,
    toks: &mut [Tok],
    tops: &[(usize, usize)],
    diagnostics: &[Diagnostic],
    dialect: Dialect,
) {
    let line_start = |off: usize| text[..off].rfind('\n').map_or(0, |i| i + 1);
    let line_end = |off: usize| text[off..].find('\n').map_or(text.len(), |i| off + i);
    let mut regions: Vec<(usize, usize)> = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        let at_line_start = text[line_start(t.start)..t.start]
            .chars()
            .all(|c| c == ' ' || c == '\t');
        let rest = &text[t.start..];
        if at_line_start
            && (rest.starts_with('\\') || (dialect == Dialect::Tsql && rest.starts_with(':')))
        {
            regions.push((t.start, line_end(t.start)));
        }
        if t.kind == IDENT && text[t.start..t.end].eq_ignore_ascii_case("COPY") {
            if let Some(end) = copy_data(text, toks, i) {
                regions.push(end);
            }
        }
    }
    for d in diagnostics {
        if let Some(&(s, e)) = tops.iter().find(|(s, e)| *s <= d.offset && d.offset < *e) {
            regions.push((s, e));
        }
    }
    for (s, e) in regions {
        for t in toks.iter_mut() {
            if t.start >= s && t.start < e {
                t.verbatim = true;
            }
        }
        // The line break that ends the region.
        if let Some(t) = toks.iter_mut().find(|t| t.start >= e) {
            t.gap_kept = true;
        }
    }
}

/// The data rows after `COPY ... FROM STDIN;` starting at token `i`: from
/// the end of the statement through the `\.` line, or to the end of the
/// input without one.
fn copy_data(text: &str, toks: &[Tok], i: usize) -> Option<(usize, usize)> {
    let mut stdin = false;
    let mut prev_from = false;
    for t in &toks[i + 1..] {
        if t.kind == SEMICOLON {
            if !stdin {
                return None;
            }
            let start = t.end;
            let mut off = text[start..].find('\n').map(|k| start + k + 1)?;
            while off < text.len() {
                let end = text[off..].find('\n').map_or(text.len(), |k| off + k);
                if text[off..end].trim_end_matches('\r') == "\\." {
                    return Some((start, end));
                }
                off = end + 1;
            }
            return Some((start, text.len()));
        }
        let w = &text[t.start..t.end];
        if t.kind == IDENT {
            stdin |= prev_from && w.eq_ignore_ascii_case("STDIN");
            prev_from = w.eq_ignore_ascii_case("FROM");
        } else if !t.is_comment() {
            prev_from = false;
        }
    }
    None
}

/// Tokens whose case the formatter changes.
pub(super) fn keyword_set(text: &str, toks: &[Tok], dialect: Dialect) -> BTreeSet<usize> {
    let sig: Vec<usize> = (0..toks.len()).filter(|&i| !toks[i].is_comment()).collect();
    let mut out = BTreeSet::new();
    for (k, &i) in sig.iter().enumerate() {
        let t = &toks[i];
        if t.kind != IDENT || t.verbatim {
            continue;
        }
        let near_dot = |j: Option<&usize>| j.is_some_and(|&j| toks[j].kind == DOT);
        let next_is = |kind| sig.get(k + 1).is_some_and(|&j| toks[j].kind == kind);
        if near_dot(k.checked_sub(1).and_then(|p| sig.get(p))) || near_dot(sig.get(k + 1)) {
            continue;
        }
        // `name:` is a label.
        if next_is(COLON) {
            continue;
        }
        let w = &text[t.start..t.end];
        if w.is_ascii() && recasable(&w.to_ascii_uppercase(), dialect) {
            out.insert(i);
        }
    }
    out
}

/// Renders the layout. `reverted` holds top-level statements kept as written.
/// Returns the text and the output offset of each token.
pub(super) fn render(
    text: &str,
    lay: &Layout,
    opts: &FormatOptions,
    keywords: &BTreeSet<usize>,
    reverted: &BTreeSet<usize>,
) -> (String, Vec<usize>) {
    let nl = super::newline(text, opts.line_ending);
    let toks = &lay.toks;
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    let mut starts = Vec::with_capacity(toks.len());
    let kept = |i: usize| {
        let t = &toks[i];
        t.verbatim
            || t.top
                .is_some_and(|u| reverted.contains(&u) && i > 0 && toks[i - 1].top == Some(u))
    };
    let max_nl = opts.max_blank_lines + 1;
    for i in 0..toks.len() {
        let t = &toks[i];
        let prev_end = if i == 0 { 0 } else { toks[i - 1].end };
        let gap = &text[prev_end..t.start];
        let plain = gap.chars().all(|c| matches!(c, ' ' | '\t' | '\r' | '\n'));
        let nls = gap.matches('\n').count();
        let newline = |n: usize, out: &mut String| {
            for _ in 0..n {
                out.push_str(nl);
            }
            push_indent(out, indent_of(toks, i), opts);
        };
        if i == 0 {
            if !plain {
                out.push_str(gap);
            }
        } else if kept(i) || t.gap_kept || !plain || verbatim_pair(&toks[i - 1], t) {
            out.push_str(gap);
        } else {
            let a = &toks[i - 1];
            let n = nls.min(max_nl);
            if a.kind == LINE_COMMENT {
                newline(n.max(1), &mut out);
            } else if t.kind == BATCH_SEP {
                for _ in 0..n.max(1) {
                    out.push_str(nl);
                }
            } else if a.kind == BATCH_SEP {
                if t.is_comment() && nls == 0 {
                    out.push_str(gap);
                } else {
                    newline(n.max(1), &mut out);
                }
            } else if t.brk != Brk::None {
                newline(n.max(1), &mut out);
            } else if nls > 0 && (!opts.join_lines || t.is_comment() || a.is_comment()) {
                newline(n, &mut out);
            } else if gap.is_empty() {
                if (a.kind == COMMA && opts.space_after_comma)
                    || (opts.space_around_comparisons && comparison_gap(text, toks, i))
                {
                    out.push(' ');
                }
            } else {
                out.push(' ');
            }
        }
        starts.push(out.len());
        let word = &text[t.start..t.end];
        let recase = keywords.contains(&i) && !kept(i);
        match (recase, opts.keyword_case) {
            (true, KeywordCase::Upper) => out.push_str(&word.to_ascii_uppercase()),
            (true, KeywordCase::Lower) => out.push_str(&word.to_ascii_lowercase()),
            _ => out.push_str(word),
        }
    }
    let tail = &text[toks.last().map_or(0, |t| t.end)..];
    let plain = tail.chars().all(|c| matches!(c, ' ' | '\t' | '\r' | '\n'));
    // A token kept as written may run to the end of the input (an
    // unterminated string): a line ending after it would land inside it.
    if !plain || toks.is_empty() || !opts.final_newline || toks.last().is_some_and(|t| t.verbatim) {
        out.push_str(tail);
    } else {
        out.push_str(nl);
    }
    (out, starts)
}

/// Whether the empty gap before token `i` touches a comparison operator and
/// may take a space. Next to another operator token it stays empty: T-SQL
/// reads `*=` as one operator, `* =` as two.
fn comparison_gap(text: &str, toks: &[Tok], i: usize) -> bool {
    let cmp = |t: &Tok| {
        t.kind == OP
            && matches!(
                &text[t.start..t.end],
                "=" | "<>" | "!=" | "<" | ">" | "<=" | ">="
            )
    };
    let (a, b) = (&toks[i - 1], &toks[i]);
    let sign = |t: &Tok| t.kind == OP && matches!(&text[t.start..t.end], "-" | "+");
    (cmp(b) && a.kind != OP) || (cmp(a) && (b.kind != OP || sign(b)))
}

/// Gaps that must keep their exact shape. Between two string literals a
/// line break matters: PostgreSQL joins `'a'` and `'b'` on separate lines
/// into one constant.
fn verbatim_pair(a: &Tok, b: &Tok) -> bool {
    let s = |k| matches!(k, STRING | DOLLAR_STRING);
    (s(a.kind) && s(b.kind)) || a.kind == ERROR_TOKEN || b.kind == ERROR_TOKEN
}

fn indent_of(toks: &[Tok], i: usize) -> usize {
    let t = &toks[i];
    if t.is_comment() {
        // A comment on its own line lines up with the code after it.
        return match toks[i + 1..].iter().find(|n| !n.is_comment()) {
            Some(n) if n.brk == Brk::Footer => n.level + 1,
            Some(n) => line_indent(n),
            None => t.level,
        };
    }
    line_indent(t)
}

fn line_indent(t: &Tok) -> usize {
    match t.brk {
        Brk::Unit | Brk::Footer | Brk::Clause => t.level,
        Brk::Item | Brk::None => t.level + 1 + t.depth,
    }
}

fn push_indent(out: &mut String, level: usize, opts: &FormatOptions) {
    match opts.indent_style {
        IndentStyle::Tabs => out.extend(std::iter::repeat('\t').take(level)),
        IndentStyle::Spaces => out.extend(std::iter::repeat(' ').take(level * opts.indent_width)),
    }
}
