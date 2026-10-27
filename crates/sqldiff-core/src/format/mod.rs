//! SQL formatter for T-SQL, PostgreSQL and Db2 for i.
//!
//! The formatter rewrites only the whitespace between tokens and the letter
//! case of keywords (see [`keywords`]); every other byte, comments included,
//! is kept. It never trusts its own layout: the output is parsed again and
//! must give the same tokens, the same tree and the same diagnostics as the
//! input, and the engine must find it equal to the input. A top-level
//! statement that fails any of these is kept exactly as written and listed
//! in [`Formatted::kept`]; if the failure cannot be pinned to a statement
//! the whole input is returned unchanged.
//!
//! Layout is configurable ([`FormatOptions`]); config files declare it in a
//! `[format]` section with per-dialect overrides ([`FormatConfig`]).

mod keywords;
mod layout;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sqldiff_syntax::{parse, Dialect, LineIndex, SyntaxElement, SyntaxKind, SyntaxNode};

use crate::{Artifact, Options, Verdict};

pub use keywords::recasable;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeywordCase {
    Upper,
    Lower,
    /// Keywords keep the case they are written in.
    Preserve,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndentStyle {
    Spaces,
    Tabs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineEnding {
    /// The line ending most of the input uses.
    Keep,
    Lf,
    Crlf,
}

/// How to lay out SQL. Every choice only moves whitespace or changes the
/// case of keywords, so none of them can change what the SQL means.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatOptions {
    /// Spaces per indent level (with `indent_style = "spaces"`).
    pub indent_width: usize,
    pub indent_style: IndentStyle,
    pub keyword_case: KeywordCase,
    /// Start `FROM`, `WHERE`, `JOIN`, `GROUP BY`, ... of a query or DML
    /// statement on their own line.
    pub clause_breaks: bool,
    /// Put each item after a top-level comma of a query or DML statement on
    /// its own line (select list, `SET` list, `ORDER BY` list).
    pub one_item_per_line: bool,
    /// Insert a space after a comma written without one.
    pub space_after_comma: bool,
    /// Insert spaces around `=`, `<>`, `<`, `>`, `<=`, `>=`, `!=` written
    /// without them.
    pub space_around_comparisons: bool,
    /// Blank lines kept in a row; more are removed.
    pub max_blank_lines: usize,
    pub line_ending: LineEnding,
    /// End the output with exactly one line ending.
    pub final_newline: bool,
    /// Join lines the input breaks inside a statement. Off: line breaks in
    /// the input are kept and only re-indented.
    pub join_lines: bool,
    /// Also lay out PostgreSQL routine bodies between dollar quotes. Off by
    /// default: the server stores the body text exactly as written.
    pub postgres_bodies: bool,
}

impl Default for FormatOptions {
    fn default() -> Self {
        FormatOptions {
            indent_width: 4,
            indent_style: IndentStyle::Spaces,
            keyword_case: KeywordCase::Upper,
            clause_breaks: true,
            one_item_per_line: false,
            space_after_comma: true,
            space_around_comparisons: true,
            max_blank_lines: 1,
            line_ending: LineEnding::Keep,
            final_newline: true,
            join_lines: false,
            postgres_bodies: false,
        }
    }
}

/// Format keys of one config layer; unset keys fall through to lower layers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormatKeys {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indent: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indent_style: Option<IndentStyle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyword_case: Option<KeywordCase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clause_breaks: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub one_item_per_line: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_after_comma: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_around_comparisons: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_blank_lines: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_ending: Option<LineEnding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_newline: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join_lines: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postgres_bodies: Option<bool>,
}

impl FormatKeys {
    /// Keys set in `hi` replace the ones here.
    pub fn merge(&mut self, hi: &FormatKeys) {
        macro_rules! take {
            ($($f:ident),*) => {$( if hi.$f.is_some() { self.$f = hi.$f; } )*};
        }
        take!(
            indent,
            indent_style,
            keyword_case,
            clause_breaks,
            one_item_per_line,
            space_after_comma,
            space_around_comparisons,
            max_blank_lines,
            line_ending,
            final_newline,
            join_lines,
            postgres_bodies
        );
    }

    pub fn apply(&self, o: &mut FormatOptions) {
        macro_rules! set {
            ($($f:ident => $g:ident),*) => {$( if let Some(v) = self.$f { o.$g = v; } )*};
        }
        set!(
            indent => indent_width,
            indent_style => indent_style,
            keyword_case => keyword_case,
            clause_breaks => clause_breaks,
            one_item_per_line => one_item_per_line,
            space_after_comma => space_after_comma,
            space_around_comparisons => space_around_comparisons,
            max_blank_lines => max_blank_lines,
            line_ending => line_ending,
            final_newline => final_newline,
            join_lines => join_lines,
            postgres_bodies => postgres_bodies
        );
    }

    /// Every key set to what `o` holds: the inverse of [`FormatKeys::apply`].
    pub fn from_options(o: &FormatOptions) -> FormatKeys {
        FormatKeys {
            indent: Some(o.indent_width),
            indent_style: Some(o.indent_style),
            keyword_case: Some(o.keyword_case),
            clause_breaks: Some(o.clause_breaks),
            one_item_per_line: Some(o.one_item_per_line),
            space_after_comma: Some(o.space_after_comma),
            space_around_comparisons: Some(o.space_around_comparisons),
            max_blank_lines: Some(o.max_blank_lines),
            line_ending: Some(o.line_ending),
            final_newline: Some(o.final_newline),
            join_lines: Some(o.join_lines),
            postgres_bodies: Some(o.postgres_bodies),
        }
    }

    /// The names of the keys set here, as the config file spells them.
    pub fn names(&self) -> Vec<String> {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::Object(m)) => m.keys().cloned().collect(),
            _ => Vec::new(),
        }
    }

    pub fn validate(&self, section: &str) -> Result<(), String> {
        if let Some(i) = self.indent {
            if !(1..=16).contains(&i) {
                return Err(format!("{section}.indent: expected 1 to 16, got {i}"));
            }
        }
        if let Some(n) = self.max_blank_lines {
            if n > 10 {
                return Err(format!(
                    "{section}.max_blank_lines: expected 0 to 10, got {n}"
                ));
            }
        }
        Ok(())
    }
}

/// The `[format]` section: keys for every dialect plus `[format.tsql]`,
/// `[format.postgres]` and `[format.db2i]` overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "toml::Table", into = "toml::Table")]
pub struct FormatConfig {
    pub all: FormatKeys,
    pub tsql: FormatKeys,
    pub postgres: FormatKeys,
    pub db2i: FormatKeys,
}

const DIALECT_TABLES: [&str; 3] = ["tsql", "postgres", "db2i"];

impl TryFrom<toml::Table> for FormatConfig {
    type Error = String;

    fn try_from(mut t: toml::Table) -> Result<Self, String> {
        let mut take = |name: &str| -> Result<FormatKeys, String> {
            match t.remove(name) {
                None => Ok(FormatKeys::default()),
                Some(v) => v.try_into().map_err(|e| format!("format.{name}: {e}")),
            }
        };
        let (tsql, postgres, db2i) = (take("tsql")?, take("postgres")?, take("db2i")?);
        let all: FormatKeys = toml::Value::Table(t)
            .try_into()
            .map_err(|e| format!("format: {e}"))?;
        let c = FormatConfig {
            all,
            tsql,
            postgres,
            db2i,
        };
        c.validate()?;
        Ok(c)
    }
}

impl From<FormatConfig> for toml::Table {
    fn from(c: FormatConfig) -> toml::Table {
        let table = |k: &FormatKeys| match toml::Value::try_from(k) {
            Ok(toml::Value::Table(t)) => t,
            _ => toml::Table::new(),
        };
        let mut t = table(&c.all);
        for (name, k) in DIALECT_TABLES.iter().zip([&c.tsql, &c.postgres, &c.db2i]) {
            if *k != FormatKeys::default() {
                t.insert(name.to_string(), toml::Value::Table(table(k)));
            }
        }
        t
    }
}

impl FormatConfig {
    pub fn merge(&mut self, hi: &FormatConfig) {
        self.all.merge(&hi.all);
        self.tsql.merge(&hi.tsql);
        self.postgres.merge(&hi.postgres);
        self.db2i.merge(&hi.db2i);
    }

    pub fn validate(&self) -> Result<(), String> {
        self.all.validate("format")?;
        for (name, k) in DIALECT_TABLES
            .iter()
            .zip([&self.tsql, &self.postgres, &self.db2i])
        {
            k.validate(&format!("format.{name}"))?;
        }
        Ok(())
    }

    /// The options for one dialect: defaults, then `[format]`, then the
    /// dialect's own table.
    pub fn options(&self, dialect: Dialect) -> FormatOptions {
        let mut o = FormatOptions::default();
        self.all.apply(&mut o);
        match dialect {
            Dialect::Tsql => self.tsql.apply(&mut o),
            Dialect::Postgres => self.postgres.apply(&mut o),
            Dialect::Db2i => self.db2i.apply(&mut o),
        }
        o
    }
}

/// A top-level statement the formatter kept exactly as written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Kept {
    /// 1-based lines in the input.
    pub start_line: usize,
    pub end_line: usize,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Formatted {
    pub text: String,
    /// Statements left as written because the check failed for them.
    pub kept: Vec<Kept>,
}

impl Formatted {
    pub fn changed(&self, input: &str) -> bool {
        self.text != input
    }
}

fn newline(text: &str, ending: LineEnding) -> &'static str {
    match ending {
        LineEnding::Lf => "\n",
        LineEnding::Crlf => "\r\n",
        LineEnding::Keep => {
            if text.matches("\r\n").count() * 2 > text.matches('\n').count() {
                "\r\n"
            } else {
                "\n"
            }
        }
    }
}

/// How many times a failed check may narrow the reverted statements before
/// the formatter gives up and returns the input unchanged.
const ROUNDS: usize = 8;

/// Formats `text`. The result is checked; see the module documentation.
pub fn format(text: &str, dialect: Dialect, opts: &FormatOptions) -> Formatted {
    let parsed = parse(text, dialect);
    let root = parsed.syntax();
    let lay = layout::collect(text, &root, &parsed.diagnostics, dialect, opts);
    let keywords = layout::keyword_set(text, &lay.toks, dialect);
    let lines = LineIndex::new(text);
    let mut reverted: BTreeMap<usize, String> = BTreeMap::new();
    let mut checked: BTreeSet<usize> = BTreeSet::new();
    for _ in 0..ROUNDS {
        let set: BTreeSet<usize> = reverted.keys().copied().collect();
        let (out, starts) = layout::render(text, &lay, opts, &keywords, &set);
        let failure = verify(
            text,
            &out,
            dialect,
            &root,
            &parsed.diagnostics,
            &lay,
            &starts,
            &mut checked,
        );
        match failure {
            None => {
                let kept = reverted
                    .into_iter()
                    .map(|(u, reason)| {
                        let (s, e) = lay.tops[u];
                        Kept {
                            start_line: lines.line_col(s).0,
                            end_line: lines.line_col(e.saturating_sub(1).max(s)).0,
                            reason,
                        }
                    })
                    .collect();
                return Formatted { text: out, kept };
            }
            Some(Failure { tops, reason }) => {
                let fresh: Vec<usize> = tops
                    .into_iter()
                    .filter(|u| !reverted.contains_key(u))
                    .collect();
                if fresh.is_empty() {
                    return unchanged(text, &lines, reason);
                }
                for u in fresh {
                    reverted.insert(u, reason.clone());
                }
            }
        }
    }
    unchanged(text, &lines, "the check kept failing".to_string())
}

fn unchanged(text: &str, lines: &LineIndex, reason: String) -> Formatted {
    Formatted {
        text: text.to_string(),
        kept: vec![Kept {
            start_line: 1,
            end_line: lines.line_col(text.len().saturating_sub(1)).0,
            reason: format!("the whole file, because {reason}"),
        }],
    }
}

struct Failure {
    /// Top-level statements to keep as written; empty: cannot be pinned.
    tops: Vec<usize>,
    reason: String,
}

fn fail(tops: Vec<usize>, reason: impl Into<String>) -> Option<Failure> {
    Some(Failure {
        tops,
        reason: reason.into(),
    })
}

/// Non-whitespace tokens with text, as the layout sees them.
fn tokens(root: &SyntaxNode) -> Vec<(SyntaxKind, String)> {
    root.descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(|t| {
            t.kind() != SyntaxKind::WHITESPACE
                && t.kind() != SyntaxKind::INFERRED_TERMINATOR
                && !t.text().is_empty()
        })
        .map(|t| (t.kind(), t.text().to_string()))
        .collect()
}

/// The tree shape of each top-level statement (node kinds and inferred
/// terminators in preorder), and the shape around them.
fn shapes(root: &SyntaxNode) -> (Vec<SyntaxKind>, Vec<Vec<SyntaxKind>>) {
    fn shape(n: &SyntaxNode) -> Vec<SyntaxKind> {
        n.descendants_with_tokens()
            .filter_map(|e| match e {
                SyntaxElement::Node(n) => Some(n.kind()),
                SyntaxElement::Token(t) if t.kind() == SyntaxKind::INFERRED_TERMINATOR => {
                    Some(t.kind())
                }
                SyntaxElement::Token(_) => None,
            })
            .collect()
    }
    let mut outer = Vec::new();
    let mut tops = Vec::new();
    fn walk(n: &SyntaxNode, outer: &mut Vec<SyntaxKind>, tops: &mut Vec<Vec<SyntaxKind>>) {
        outer.push(n.kind());
        for c in n.children() {
            if c.kind() == SyntaxKind::BATCH {
                walk(&c, outer, tops);
            } else if c.kind().is_unit() {
                outer.push(SyntaxKind::STATEMENT);
                tops.push(shape(&c));
            } else {
                outer.extend(shape(&c));
            }
        }
    }
    walk(root, &mut outer, &mut tops);
    (outer, tops)
}

#[allow(clippy::too_many_arguments)]
fn verify(
    text: &str,
    out: &str,
    dialect: Dialect,
    root: &SyntaxNode,
    diagnostics: &[sqldiff_syntax::Diagnostic],
    lay: &layout::Layout,
    starts: &[usize],
    checked: &mut BTreeSet<usize>,
) -> Option<Failure> {
    let reparsed = parse(out, dialect);
    let new_root = reparsed.syntax();
    let top_of = |i: usize| lay.toks.get(i).and_then(|t| t.top);

    // 1. The same tokens, in the same order, with the same text; only
    //    recasable keywords may differ in case.
    let (a, b) = (tokens(root), tokens(&new_root));
    let same = |x: &(SyntaxKind, String), y: &(SyntaxKind, String)| {
        x.0 == y.0
            && (x.1 == y.1
                || (x.0 == SyntaxKind::IDENT
                    && x.1.is_ascii()
                    && x.1.eq_ignore_ascii_case(&y.1)
                    && recasable(&x.1.to_ascii_uppercase(), dialect)))
    };
    if let Some(k) = (0..a.len().max(b.len())).find(|&k| match (a.get(k), b.get(k)) {
        (Some(x), Some(y)) => !same(x, y),
        _ => true,
    }) {
        let tops: BTreeSet<usize> = [k.checked_sub(1).and_then(top_of), top_of(k)]
            .into_iter()
            .flatten()
            .collect();
        let what = a
            .get(k)
            .map_or("end of input".to_string(), |t| format!("`{}`", t.1));
        return fail(
            tops.into_iter().collect(),
            format!("the output reads differently at {what}"),
        );
    }

    // 2. The same tree.
    let (outer_a, tops_a) = shapes(root);
    let (outer_b, tops_b) = shapes(&new_root);
    if outer_a != outer_b || tops_a.len() != tops_b.len() {
        return fail(Vec::new(), "the output parses into different statements");
    }
    let differ: Vec<usize> = (0..tops_a.len())
        .filter(|&u| tops_a[u] != tops_b[u])
        .collect();
    if !differ.is_empty() {
        return fail(differ, "the output parses differently");
    }

    // 3. The same parse problems.
    let msgs = |d: &[sqldiff_syntax::Diagnostic]| -> Vec<String> {
        d.iter().map(|d| d.message.clone()).collect()
    };
    if msgs(diagnostics) != msgs(&reparsed.diagnostics) {
        return fail(Vec::new(), "the output has different parse problems");
    }

    // 4. The engine finds each statement equal to what it was. Statements
    //    are compared one by one: the tree is already known to be the same,
    //    and the engine's cost grows faster than the size of its input.
    let mut spans: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
    for (i, t) in lay.toks.iter().enumerate() {
        if let Some(u) = t.top {
            spans.entry(u).or_insert((i, i)).1 = i;
        }
    }
    let opts = Options::default();
    let mut failed = Vec::new();
    let mut first_reason = None;
    for (&u, &(i0, i1)) in &spans {
        if checked.contains(&u) {
            continue;
        }
        let (t0, t1) = (&lay.toks[i0], &lay.toks[i1]);
        let before = &text[t0.start..t1.end];
        let after = &out[starts[i0]..starts[i1] + (t1.end - t1.start)];
        if before == after {
            checked.insert(u);
            continue;
        }
        match engine_check(before, after, dialect, &opts) {
            None => {
                checked.insert(u);
            }
            Some(reason) => {
                failed.push(u);
                first_reason.get_or_insert(reason);
            }
        }
    }
    match first_reason {
        None => None,
        Some(reason) => fail(failed, reason),
    }
}

/// `None` if the engine finds `after` equal to `before`. When the engine
/// cannot reach `equal` even for identical text (parse problems, constructs
/// it cannot analyse), `after` must give exactly what `before` gives
/// against itself.
fn engine_check(before: &str, after: &str, dialect: Dialect, opts: &Options) -> Option<String> {
    let a = Artifact::new("input", dialect, before);
    let report = crate::diff(&a, &Artifact::new("formatted", dialect, after), opts);
    if report.verdict == Verdict::Equal && report.findings.is_empty() {
        return None;
    }
    let rules = |r: &crate::Report| -> Vec<String> {
        let mut v: Vec<String> = r.findings.iter().map(|f| f.rule.clone()).collect();
        v.sort();
        v
    };
    let base = crate::diff(&a, &Artifact::new("formatted", dialect, before), opts);
    if base.verdict == report.verdict && rules(&base) == rules(&report) {
        return None;
    }
    let what = report
        .findings
        .first()
        .map_or(report.verdict.label().to_string(), |f| f.rule.clone());
    Some(format!(
        "the engine does not find the output equal ({what})"
    ))
}

#[cfg(test)]
mod tests;
