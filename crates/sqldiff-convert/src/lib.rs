//! Rule-based dialect conversion, checked by the SQLDiff engine.
//!
//! `convert` rewrites an artifact from one dialect into another, statement by
//! statement, and records for each converted unit what the rules assumed or
//! could not do. The output is then diffed against the input with the normal
//! engine, and every finding lands on the unit it points at. Nothing is
//! guessed silently: a construct without a rule is kept as a commented-out
//! block and marked "not converted".
//!
//! Supported today: T-SQL → PostgreSQL (token-level rules over the lossless
//! CST; see `tsql_pg`). Expression-level and behavioural conversion follow
//! once expression trees and the semantic IR land.

use serde::Serialize;
use sqldiff_core::{Artifact, Dialect, Options, Report};
use sqldiff_syntax::{parse, LineIndex};

mod out;
mod template;
mod tok;
mod tsql_pg;

pub use template::{convert_template, TemplateInfo};

/// How far a converted unit can be trusted, from best to worst.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Converted by rules that need no assumption, and the engine found nothing.
    Converted,
    /// Converted; equal only if the listed assumptions hold.
    Assumptions,
    /// Converted to one of several faithful targets; someone should choose.
    Decision,
    /// The engine found a difference or leftover syntax in the output.
    Differs,
    /// No rule applies; the original is kept as a comment.
    NotConverted,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Converted => "converted",
            Status::Assumptions => "under assumptions",
            Status::Decision => "needs a decision",
            Status::Differs => "differs",
            Status::NotConverted => "not converted",
        }
    }
}

/// Why a unit has the status it has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Note {
    pub status: Status,
    /// Stable rule id, e.g. `ASM-CI-COLLATION` or `tsql.merge`.
    pub rule: String,
    pub text: String,
}

/// A span in the input or the output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Span {
    /// Byte offsets.
    pub start: usize,
    pub end: usize,
    /// UTF-16 offsets, for browser text controls.
    pub start_u16: usize,
    pub end_u16: usize,
    /// 1-based first and last line.
    pub line: usize,
    pub end_line: usize,
}

/// One converted statement (or compound header) and where it came from.
#[derive(Debug, Clone, Serialize)]
pub struct Unit {
    pub status: Status,
    pub notes: Vec<Note>,
    pub src: Span,
    pub out: Span,
    /// Indices into `report.findings` that point into this unit.
    pub findings: Vec<usize>,
    /// The engine's part of the status: `differs` when a finding or a parse
    /// problem lands here, else `converted`. Lets a viewer accept assumptions
    /// (dropping their notes) without hiding what the engine found.
    pub checked: Status,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Counts {
    pub converted: usize,
    pub assumptions: usize,
    pub decision: usize,
    pub differs: usize,
    pub not_converted: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Conversion {
    /// Dialect ids, e.g. `tsql`.
    pub from: String,
    pub to: String,
    pub output: String,
    pub units: Vec<Unit>,
    pub counts: Counts,
    /// The engine's diff of input (side A) against output (side B). For a
    /// template, of its fragment against the converted fragment.
    pub report: Report,
    /// Set when the input was a template.
    pub template: Option<TemplateInfo>,
}

/// Dialect pairs with rules.
pub fn supported(from: Dialect, to: Dialect) -> bool {
    matches!((from, to), (Dialect::Tsql, Dialect::Postgres))
}

/// Convert `text` from one dialect to another and check the result. A pair
/// without rules is not an error: every statement comes back not converted
/// (kept as a comment), so a viewer can switch targets freely.
pub fn convert(text: &str, from: Dialect, to: Dialect) -> Result<Conversion, String> {
    let root = parse(text, from).syntax();
    let raw = match (from, to) {
        (Dialect::Tsql, Dialect::Postgres) => tsql_pg::convert(&root, text),
        _ => no_rules(&root, text, from, to),
    };
    Ok(verify(text, from, to, raw))
}

fn verify(input: &str, from: Dialect, to: Dialect, raw: out::Out) -> Conversion {
    let a = Artifact::new("input.sql", from, input);
    let b = Artifact::new("converted.sql", to, raw.text.as_str());
    let report = sqldiff_core::diff(&a, &b, &Options::default());
    let (li, lo) = (LineIndex::new(input), LineIndex::new(&raw.text));
    let mut units: Vec<Unit> = raw
        .units
        .into_iter()
        .map(|u| {
            let mut notes = u.notes;
            notes.sort_by_key(|n| std::cmp::Reverse(n.status));
            Unit {
                status: notes.first().map_or(Status::Converted, |n| n.status),
                notes,
                src: span(input, &li, u.src),
                out: span(&raw.text, &lo, u.out),
                findings: Vec::new(),
                checked: Status::Converted,
            }
        })
        .collect();

    // Findings land on the unit they point into: output side first, else input side.
    for (fi, f) in report.findings.iter().enumerate() {
        let hit = |loc: &Option<sqldiff_core::model::Location>, out_side: bool| {
            let loc = loc.as_ref()?;
            units.iter().position(|u| {
                let s = if out_side { u.out } else { u.src };
                overlaps((s.start, s.end), (loc.byte_start, loc.byte_end))
            })
        };
        if let Some(ui) = hit(&f.b, true).or_else(|| hit(&f.a, false)) {
            let u = &mut units[ui];
            u.findings.push(fi);
            // Residue in the output, or a behavioural difference, is a difference.
            if f.verdict == sqldiff_core::Verdict::Different
                || f.class == sqldiff_core::ChangeClass::Residue && f.b.is_some()
            {
                u.status = u.status.max(Status::Differs);
                u.checked = Status::Differs;
            }
        }
    }
    // The output must parse as the target dialect.
    for d in parse(&raw.text, to).diagnostics {
        if let Some(u) = units
            .iter_mut()
            .find(|u| overlaps((u.out.start, u.out.end + 1), (d.offset, d.offset + 1)))
        {
            u.status = u.status.max(Status::Differs);
            u.checked = Status::Differs;
            u.notes.insert(
                0,
                Note {
                    status: Status::Differs,
                    rule: "parse".into(),
                    text: format!(
                        "The output does not parse as {}: {}",
                        to.display_name(),
                        d.message
                    ),
                },
            );
        }
    }

    let mut c = Conversion {
        from: from.id().to_string(),
        to: to.id().to_string(),
        output: raw.text,
        units,
        counts: Counts::default(),
        report,
        template: None,
    };
    recount(&mut c);
    c
}

fn recount(c: &mut Conversion) {
    let mut counts = Counts::default();
    for u in &c.units {
        *match u.status {
            Status::Converted => &mut counts.converted,
            Status::Assumptions => &mut counts.assumptions,
            Status::Decision => &mut counts.decision,
            Status::Differs => &mut counts.differs,
            Status::NotConverted => &mut counts.not_converted,
        } += 1;
    }
    c.counts = counts;
}

/// Every top-level unit kept as a comment, for a pair without rules.
fn no_rules(root: &sqldiff_syntax::SyntaxNode, text: &str, from: Dialect, to: Dialect) -> out::Out {
    let reason = if from == to {
        format!("source and target are both {}", from.display_name())
    } else {
        format!(
            "no conversion rules from {} to {} yet",
            from.display_name(),
            to.display_name()
        )
    };
    let mut o = out::Out::default();
    let units = root.children().flat_map(|c| {
        if c.kind() == sqldiff_syntax::SyntaxKind::BATCH {
            c.children().filter(|u| u.kind().is_unit()).collect()
        } else if c.kind().is_unit() {
            vec![c]
        } else {
            Vec::new()
        }
    });
    for u in units {
        let (s, e) = sqldiff_syntax::ast::sig_range(&u);
        let mut t = format!("-- [not converted] {reason}\n");
        for line in text[s..e].lines() {
            t.push_str("-- ");
            t.push_str(line);
            t.push('\n');
        }
        o.unit(
            (s, e),
            0,
            &t,
            vec![Note {
                status: Status::NotConverted,
                rule: "no-rules".into(),
                text: reason.clone(),
            }],
        );
    }
    o
}

fn overlaps(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 < b.1.max(b.0 + 1) && b.0 < a.1
}

fn span(text: &str, li: &LineIndex, (start, end): (usize, usize)) -> Span {
    let u16 = |b: usize| text[..b].encode_utf16().count();
    Span {
        start,
        end,
        start_u16: u16(start),
        end_u16: u16(end),
        line: li.line_col(start).0,
        end_line: li.line_col(end.saturating_sub(1).max(start)).0,
    }
}
