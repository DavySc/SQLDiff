//! Pair view for SQLDiff: statement-aligned rows over a diff report.
//!
//! `build` runs the engine and returns the report (exactly what
//! `sqldiff diff -f json` prints) plus a `view`: both sides split into
//! blocks of whole lines around statements, aligned by statement content
//! rather than by line, with per-token styling classes and change marks.
//! The browser UI in `assets/` renders that payload; it does no analysis.
//!
//! A [`session::Session`] holds one pair or two paired trees, the
//! session's assumption flips and the review store, and feeds the review
//! queue, assumption panel and provenance bar around the pair view.

use serde::Serialize;
use sqldiff_core::{Artifact, Options, Report};
use sqldiff_syntax::template::Derived;
use sqldiff_syntax::{ast, parse, LineIndex, SyntaxElement, SyntaxKind, SyntaxNode};

pub mod app;
pub mod convert;
pub mod convert_review;
pub mod report;
pub mod review;
pub mod server;
pub mod session;

/// The UI page; the payload is injected by the server or by `export`.
pub const INDEX_HTML: &str = include_str!("../assets/index.html");

#[derive(Debug, Clone, Serialize)]
pub struct Payload {
    pub report: Report,
    pub view: View,
    /// Source texts, so the scratchpad can edit them.
    pub sources: [String; 2],
    /// Whether a server is behind the page (edits re-diff) or it is a static export.
    pub live: bool,
    /// Position of the pair in the session queue.
    pub index: usize,
    /// Review snapshot key (`sqldiff_review::snapshot`): both content hashes.
    pub snapshot: String,
    /// Review status of each finding, resolved by the store (exact snapshot,
    /// then `*`, expiry, outdated). Empty where no store is behind the page.
    pub review: Vec<sqldiff_review::Status>,
}

#[derive(Debug, Clone, Serialize)]
pub struct View {
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RowKind {
    /// Paired and equal after normalization.
    Same,
    /// Paired but different.
    Changed,
    OnlyA,
    OnlyB,
}

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub kind: RowKind,
    pub a: Option<Block>,
    pub b: Option<Block>,
}

/// Contiguous whole lines of one side.
#[derive(Debug, Clone, Serialize)]
pub struct Block {
    /// 1-based first and last line.
    pub lines: (usize, usize),
    /// Segments covering the block text exactly: (text, style, mark).
    /// Mark: 0 unchanged, 1 changed within a paired row, 2 only on this side,
    /// 3 paired only through the cross-dialect key (`@Qty` ~ `p_Qty`).
    pub segs: Vec<(String, &'static str, u8)>,
    /// Indices into `report.findings` whose location on this side overlaps the block.
    pub findings: Vec<usize>,
    #[serde(skip)]
    range: (usize, usize),
    #[serde(skip)]
    key: Vec<String>,
    #[serde(skip)]
    tok_keys: Vec<(usize, String, String)>,
}

/// Diff two artifacts and build the pair view.
pub fn build(a: &Artifact, b: &Artifact, opts: &Options, live: bool) -> Payload {
    build_view(a, b, sqldiff_core::diff(a, b, opts), live)
}

/// The pair view over a report already made for these artifacts.
pub fn build_view(a: &Artifact, b: &Artifact, report: Report, live: bool) -> Payload {
    build_view_on(a, b, [None, None], report, live)
}

/// [`build_view`] where a side is a template: its blocks come from the SQL
/// fragment the engine diffed (`fragment`), shown in the template's text,
/// so findings (in template offsets) attach to the statement they are in.
pub fn build_view_on(
    a: &Artifact,
    b: &Artifact,
    fragment: [Option<&Derived>; 2],
    report: Report,
    live: bool,
) -> Payload {
    let side = |x: &Artifact, f: Option<&Derived>| match f {
        Some(f) => onto_template(blocks(&f.text, x.dialect), f, &x.text),
        None => blocks(&x.text, x.dialect),
    };
    let mut ba = side(a, fragment[0]);
    let mut bb = side(b, fragment[1]);
    attach_findings(&mut ba, &report, true);
    attach_findings(&mut bb, &report, false);
    let rows = align(ba, bb);
    Payload {
        snapshot: sqldiff_review::snapshot(&report),
        review: Vec::new(),
        report,
        view: View { rows },
        sources: [a.text.clone(), b.text.clone()],
        live,
        index: 0,
    }
}

/// The page with a payload embedded: a [`session::Page`], or a bare [`Payload`].
pub fn page<T: Serialize>(payload: &T) -> String {
    // `</` cannot appear inside the script element.
    let json = serde_json::to_string(payload)
        .unwrap()
        .replace("</", "<\\/");
    INDEX_HTML.replace(
        "/*__SQLDIFF_PAYLOAD__*/null",
        &format!("/*__SQLDIFF_PAYLOAD__*/{json}"),
    )
}

/// Each token of `text` as parsed (routine bodies lexed as code) with the
/// style the views give it: `(start, end, style)`, byte offsets. The page-side
/// highlighter in convert mode must agree with it (see tests/highlight.rs).
pub fn token_styles(
    text: &str,
    dialect: sqldiff_syntax::Dialect,
) -> Vec<(usize, usize, &'static str)> {
    let mut at = 0;
    parse(text, dialect)
        .syntax()
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .map(|t| {
            let s = at;
            at += t.text().len();
            (s, at, style(t.kind(), t.text()))
        })
        .collect()
}

fn style(kind: SyntaxKind, text: &str) -> &'static str {
    use SyntaxKind::*;
    match kind {
        WHITESPACE | INFERRED_TERMINATOR => "ws",
        LINE_COMMENT | BLOCK_COMMENT => "cmt",
        IDENT
            if sqldiff_core::normalize::is_keyword(&text.to_ascii_uppercase())
                || is_extra_keyword(text) =>
        {
            "kw"
        }
        IDENT | QUOTED_IDENT => "id",
        VARIABLE | PARAM => "var",
        NUMBER => "num",
        STRING | DOLLAR_STRING => "str",
        DOLLAR_QUOTE | BATCH_SEP => "kw",
        ERROR_TOKEN => "err",
        OP => "op",
        _ => "pun",
    }
}

/// Keywords the normalizer does not need (their case already folds) but that read better styled.
pub(crate) fn is_extra_keyword(text: &str) -> bool {
    EXTRA_KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(text))
}

/// Every word the views style as a keyword, upper case: the normalizer's
/// keywords and [`EXTRA_KEYWORDS`]. Convert mode's editors highlight with it.
pub fn styled_keywords() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = sqldiff_core::normalize::keywords()
        .iter()
        .chain(EXTRA_KEYWORDS)
        .copied()
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

const EXTRA_KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "INSERT",
    "INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "IF",
    "THEN",
    "RETURN",
    "RETURNS",
    "PROCEDURE",
    "FUNCTION",
    "LANGUAGE",
    "OPTION",
    "TRAN",
    "TRANSACTION",
    "ROLLBACK",
    "TRY",
    "THROW",
    "RAISE",
    "EXCEPTION",
    "WHEN",
    "OR",
    "REPLACE",
    "NOT",
    "NULL",
    "IS",
    "ON",
    "JOIN",
    "LEFT",
    "INNER",
    "OUTER",
    "GROUP",
    "ORDER",
    "HAVING",
    "UNION",
    "EXISTS",
    "WHILE",
    "LOOP",
    "FOR",
    "EXIT",
    "HANDLER",
    "SIGNAL",
    "RESIGNAL",
    "SQLSTATE",
    "SQLEXCEPTION",
    "MERGE",
    "USING",
    "MATCHED",
    "OUT",
    "OUTPUT",
    "NOCOUNT",
    "XACT_ABORT",
    "INT",
    "INTEGER",
    "VARCHAR",
    "NVARCHAR",
    "CHAR",
    "DECIMAL",
    "NUMERIC",
    "BIGINT",
    "SMALLINT",
    "DATE",
    "PERFORM",
    "CONFLICT",
    "NOTHING",
    "TOP",
    "LIMIT",
    "OFFSET",
    "FETCH",
    "WITH",
    "TABLE",
    "VIEW",
    "TRIGGER",
    "INDEX",
    "PRIMARY",
    "KEY",
    "FOREIGN",
    "REFERENCES",
    "UNIQUE",
    "PLPGSQL",
    "SQL",
    "GET",
    "DIAGNOSTICS",
    "STACKED",
    "FOUND",
    "SAVEPOINT",
    "RELEASE",
    "WORK",
    "SAVE",
    "ITERATE",
    "LEAVE",
    "REPEAT",
    "UNTIL",
    "ATOMIC",
    "GOTO",
    "PRINT",
    "EXEC",
    "EXECUTE",
    "OPEN",
];

/// Comparison key of a token across dialects: case-folded, unquoted, with
/// variable sigils (`@x`), conventional parameter prefixes (`p_x`, `v_x`)
/// and underscores (`item_id` ~ `ItemId`) dropped, so a converted statement pairs with its original.
fn fuzzy_key(kind: SyntaxKind, text: &str) -> String {
    use SyntaxKind::*;
    match kind {
        IDENT | QUOTED_IDENT | VARIABLE => {
            let s = ast::unquote(text).trim_start_matches('@').to_uppercase();
            let s = ["P_", "V_"]
                .iter()
                .find_map(|p| s.strip_prefix(p).filter(|r| !r.is_empty()))
                .unwrap_or(&s);
            s.replace('_', "")
        }
        _ => text.to_string(),
    }
}

/// Compound statements up to this many lines are shown as one block.
const COMPACT_LINES: usize = 8;

/// The statements that become blocks: those containing no other statement,
/// and short compound statements (an IF with its branches), so that
/// reformatting a one-line IF over several lines still pairs as one row.
fn block_units(root: &SyntaxNode, li: &LineIndex) -> Vec<SyntaxNode> {
    use SyntaxKind::*;
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(n) = stack.pop() {
        let has_units = n.descendants().skip(1).any(|d| d.kind().is_unit());
        let compact = matches!(
            n.kind(),
            IF_STMT | LOOP_STMT | CASE_STMT | TRY_CATCH | HANDLER
        ) && {
            let (s, e) = ast::sig_range(&n);
            li.line_col(e).0 - li.line_col(s).0 < COMPACT_LINES
        };
        if n.kind().is_unit() && (!has_units || compact) {
            out.push(n);
        } else {
            let mut kids: Vec<SyntaxNode> = n.children().collect();
            kids.reverse();
            stack.extend(kids);
        }
    }
    out
}

/// Splits a text into blocks of whole lines: each block holds the lines of
/// one or more statements that share lines, or a run of lines outside
/// statements (headers, BEGIN/END). Blank lines join the block before them.
fn blocks(text: &str, dialect: sqldiff_syntax::Dialect) -> Vec<Block> {
    let parse = parse(text, dialect);
    let root = parse.syntax();
    let li = LineIndex::new(text);
    let mut line_starts: Vec<usize> = vec![0];
    line_starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    if line_starts.last() == Some(&text.len()) && !text.is_empty() {
        line_starts.pop();
    }
    let nlines = line_starts.len();
    let line_end = |l: usize| -> usize {
        // 1-based line -> byte end including its newline
        if l < nlines {
            line_starts[l]
        } else {
            text.len()
        }
    };

    // Line spans of statements, merged where they share lines.
    let mut spans: Vec<(usize, usize)> = block_units(&root, &li)
        .iter()
        .map(|n| {
            let (s, e) = ast::sig_range(n);
            (li.line_col(s).0, li.line_col(e.max(s + 1) - 1).0)
        })
        .collect();
    spans.sort();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in spans {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    // Lines outside statements (routine headers, BEGIN/END) group into one block per run.
    let mut spans_by_line: Vec<(usize, usize)> = Vec::new();
    let mut l = 1;
    for &(s, e) in &merged {
        if l < s {
            spans_by_line.push((l, s - 1));
        }
        spans_by_line.push((s, e));
        l = e + 1;
    }
    if l <= nlines {
        spans_by_line.push((l, nlines));
    }
    // Blank lines join the previous block.
    let is_blank = |(s, e): (usize, usize)| text[line_starts[s - 1]..line_end(e)].trim().is_empty();
    let mut lines: Vec<(usize, usize)> = Vec::new();
    for sp in spans_by_line {
        match lines.last_mut() {
            Some(last) if is_blank(sp) => last.1 = sp.1,
            _ => lines.push(sp),
        }
    }

    let tokens: Vec<_> = root
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .collect();
    let mut out = Vec::new();
    let mut ti = 0;
    for (sl, el) in lines {
        let (bs, be) = (line_starts[sl - 1], line_end(el));
        let mut segs = Vec::new();
        let mut key = Vec::new();
        let mut tok_keys = Vec::new();
        while ti < tokens.len() {
            let t = &tokens[ti];
            let r = t.text_range();
            let (ts, te) = (usize::from(r.start()), usize::from(r.end()));
            if ts >= be {
                break;
            }
            let (cs, ce) = (ts.max(bs), te.min(be));
            if ce > cs {
                segs.push((text[cs..ce].to_string(), style(t.kind(), t.text()), 0));
                if ast::is_significant(t.kind()) && t.kind() != SyntaxKind::SEMICOLON && ts >= bs {
                    let k = fuzzy_key(t.kind(), t.text());
                    tok_keys.push((segs.len() - 1, k.clone(), t.text().to_uppercase()));
                    key.push(k);
                }
            }
            if te <= be {
                ti += 1;
            } else {
                break;
            }
        }
        out.push(Block {
            lines: (sl, el),
            segs,
            findings: Vec::new(),
            range: (bs, be),
            key,
            tok_keys,
        });
    }
    out
}

/// Blocks of a template's fragment, moved onto the template: block ranges,
/// lines and segment texts in the template (a stand-in shows as the
/// placeholder it stands for), comparison keys as the fragment has them.
fn onto_template(blocks: Vec<Block>, fragment: &Derived, template: &str) -> Vec<Block> {
    let li = LineIndex::new(template);
    let mut cursor = 0;
    let mut out = Vec::with_capacity(blocks.len());
    for b in blocks {
        let start = fragment.map.to_src(b.range.0).max(cursor);
        let end = fragment.map.to_src(b.range.1).max(start);
        let mut segs = Vec::with_capacity(b.segs.len());
        // Where each fragment segment went, for the token keys.
        let mut moved = Vec::with_capacity(b.segs.len());
        let mut at = start;
        let mut fs = b.range.0;
        for (text, style, mark) in b.segs {
            let fe = fs + text.len();
            let (s, e) = fragment.map.src_range(fs, fe);
            let (s, e) = (s.max(at), e.min(end).max(s.max(at)));
            if s > at {
                segs.push((template[at..s].to_string(), "ws", 0));
            }
            moved.push(segs.len());
            segs.push((template[s..e].to_string(), style, mark));
            at = e;
            fs = fe;
        }
        if end > at {
            segs.push((template[at..end].to_string(), "ws", 0));
        }
        let lines = (
            li.line_col(start).0,
            li.line_col(end.max(start + 1) - 1)
                .0
                .max(li.line_col(start).0),
        );
        out.push(Block {
            lines,
            segs,
            findings: Vec::new(),
            range: (start, end),
            key: b.key,
            tok_keys: b
                .tok_keys
                .into_iter()
                .map(|(i, k, u)| (moved[i], k, u))
                .collect(),
        });
        cursor = end;
    }
    out
}

fn attach_findings(blocks: &mut [Block], report: &Report, side_a: bool) {
    for (i, f) in report.findings.iter().enumerate() {
        let loc = if side_a { &f.a } else { &f.b };
        let Some(loc) = loc else { continue };
        for b in blocks.iter_mut() {
            if loc.byte_start < b.range.1 && loc.byte_end > b.range.0 {
                b.findings.push(i);
            }
        }
    }
}

fn similarity(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let n = sqldiff_core::normalize::lcs_len(a, b);
    2.0 * n as f64 / (a.len() + b.len()) as f64
}

/// Minimum similarity for two blocks to share a row.
const PAIR_THRESHOLD: f64 = 0.5;
/// Above this many cell comparisons, align on exact keys only.
const DP_BUDGET: usize = 400_000;

/// Global alignment of two block lists maximizing total similarity.
fn align(a: Vec<Block>, b: Vec<Block>) -> Vec<Row> {
    let (n, m) = (a.len(), b.len());
    let pairs: Vec<(usize, usize)> = if n * m <= DP_BUDGET {
        let sim = |i: usize, j: usize| similarity(&a[i].key, &b[j].key);
        let mut dp = vec![vec![0f64; m + 1]; n + 1];
        let mut take = vec![vec![false; m + 1]; n + 1];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                let s = sim(i, j);
                let diag = if s >= PAIR_THRESHOLD {
                    dp[i + 1][j + 1] + s
                } else {
                    f64::MIN
                };
                let best = dp[i + 1][j].max(dp[i][j + 1]);
                if diag >= best {
                    dp[i][j] = diag;
                    take[i][j] = true;
                } else {
                    dp[i][j] = best;
                }
            }
        }
        let (mut i, mut j) = (0, 0);
        let mut out = Vec::new();
        while i < n && j < m {
            if take[i][j] {
                out.push((i, j));
                i += 1;
                j += 1;
            } else if dp[i + 1][j] >= dp[i][j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
        out
    } else {
        let ka: Vec<&Vec<String>> = a.iter().map(|x| &x.key).collect();
        let kb: Vec<&Vec<String>> = b.iter().map(|x| &x.key).collect();
        sqldiff_core::normalize::lcs_pairs(&ka, &kb)
    };

    let mut a: Vec<Option<Block>> = a.into_iter().map(Some).collect();
    let mut b: Vec<Option<Block>> = b.into_iter().map(Some).collect();
    let mut rows = Vec::new();
    let (mut i, mut j) = (0, 0);
    for (pi, pj) in pairs.into_iter().chain(std::iter::once((n, m))) {
        while i < pi {
            rows.push(one_sided(a[i].take().unwrap(), true));
            i += 1;
        }
        while j < pj {
            rows.push(one_sided(b[j].take().unwrap(), false));
            j += 1;
        }
        if pi < n && pj < m {
            rows.push(paired(a[pi].take().unwrap(), b[pj].take().unwrap()));
            i = pi + 1;
            j = pj + 1;
        }
    }
    rows
}

fn one_sided(mut blk: Block, side_a: bool) -> Row {
    let is_tok: Vec<usize> = blk.tok_keys.iter().map(|t| t.0).collect();
    for k in is_tok {
        blk.segs[k].2 = 2;
    }
    Row {
        kind: if side_a {
            RowKind::OnlyA
        } else {
            RowKind::OnlyB
        },
        a: side_a.then(|| blk.clone()),
        b: (!side_a).then_some(blk),
    }
}

fn paired(mut x: Block, mut y: Block) -> Row {
    if x.key == y.key {
        for (tx, ty) in x.tok_keys.iter().zip(&y.tok_keys) {
            if tx.2 != ty.2 {
                x.segs[tx.0].2 = 3;
                y.segs[ty.0].2 = 3;
            }
        }
        return Row {
            kind: RowKind::Same,
            a: Some(x),
            b: Some(y),
        };
    }
    let kx: Vec<&str> = x.tok_keys.iter().map(|t| t.1.as_str()).collect();
    let ky: Vec<&str> = y.tok_keys.iter().map(|t| t.1.as_str()).collect();
    let common = sqldiff_core::normalize::lcs_pairs(&kx, &ky);
    let mut keep_x = vec![false; kx.len()];
    let mut keep_y = vec![false; ky.len()];
    for (i, j) in common {
        keep_x[i] = true;
        keep_y[j] = true;
    }
    for (t, keep) in x.tok_keys.iter().zip(keep_x) {
        if !keep {
            x.segs[t.0].2 = 1;
        }
    }
    for (t, keep) in y.tok_keys.iter().zip(keep_y) {
        if !keep {
            y.segs[t.0].2 = 1;
        }
    }
    Row {
        kind: RowKind::Changed,
        a: Some(x),
        b: Some(y),
    }
}
