//! Rule-based dialect conversion, checked by the SQLDiff engine.
//!
//! `convert` rewrites an artifact from one dialect into another, statement by
//! statement, and records for each converted unit what the rules assumed or
//! could not do. The output is then diffed against the input with the normal
//! engine, and every finding lands on the unit it points at. Nothing is
//! guessed silently: a construct without a rule is kept as a commented-out
//! block and marked "not converted".
//!
//! Supported today: T-SQL → PostgreSQL and T-SQL → Db2 for i (token-level
//! rules over the lossless CST; see `tsql`). Expression-level and behavioural conversion follow
//! once expression trees and the semantic IR land.

use serde::Serialize;
use sqldiff_core::{Artifact, Dialect, Options, Report};
use sqldiff_syntax::{parse, LineIndex};

pub mod names;
mod out;
mod template;
mod tok;
mod tsql;

pub use template::{
    check_template, convert_template, rewrite_template, rewrite_template_as, split_pair,
    template_fragment, FragmentKind, StandIns, TemplateDraft, TemplateInfo, ValueKind,
};

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
    /// The engine could neither show the output equivalent to the input nor
    /// find a difference: the rules' claim is not confirmed, so someone must
    /// check it (or the engine learn the construct).
    Unproven,
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
            Status::Unproven => "not proven",
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
    /// Indices into `output_check.findings` (the output checked on its own:
    /// binding, residue, limits) that point into this unit.
    pub output_findings: Vec<usize>,
    /// The engine's part of the status: `differs` when a finding or a parse
    /// problem lands here, `unproven` when the engine's diff could not decide
    /// it (see [`undecided`]), else `converted`. Lets a viewer accept assumptions
    /// (dropping their notes) without hiding what the engine found.
    pub checked: Status,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Counts {
    pub converted: usize,
    pub assumptions: usize,
    pub decision: usize,
    pub unproven: usize,
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
    /// The output checked on its own as the target dialect (`sqldiff check`):
    /// a unit is only `converted` when nothing here points into it.
    pub output_check: Report,
    /// Indices into `output_check.findings` that gate units but point into
    /// no unit (text the rules wrote around units). Any entry here means the
    /// file as a whole cannot count as converted.
    pub unplaced: Vec<usize>,
    /// Set when the input was a template.
    pub template: Option<TemplateInfo>,
}

/// Dialect pairs with rules.
pub fn supported(from: Dialect, to: Dialect) -> bool {
    matches!(
        (from, to),
        (Dialect::Tsql, Dialect::Postgres) | (Dialect::Tsql, Dialect::Db2i)
    )
}

/// Convert `text` from one dialect to another and check the result. A pair
/// without rules is not an error: every statement comes back not converted
/// (kept as a comment), so a viewer can switch targets freely.
pub fn convert(text: &str, from: Dialect, to: Dialect) -> Result<Conversion, String> {
    Ok(check(text, rewrite(text, from, to), &Options::default()))
}

/// Like `convert`, with engine options for the check: catalogs for both
/// sides (`catalog_a` in the source dialect, `catalog_b` in the target),
/// helpers and declared facts.
pub fn convert_with(
    text: &str,
    from: Dialect,
    to: Dialect,
    opts: &Options,
) -> Result<Conversion, String> {
    Ok(check(text, rewrite(text, from, to), opts))
}

/// The rules' output before it is checked. Lets a caller convert many files
/// first and then check each one against catalogs built from all of them.
#[derive(Debug)]
pub struct Draft {
    from: Dialect,
    to: Dialect,
    raw: out::Out,
}

impl Draft {
    pub fn output(&self) -> &str {
        &self.raw.text
    }
}

/// Apply the rules only. Nothing in a draft has been checked yet.
pub fn rewrite(text: &str, from: Dialect, to: Dialect) -> Draft {
    rewrite_in(text, from, to, None)
}

/// Apply the rules with the source catalog of the whole tree being
/// converted (`sqldiff_core::catalog::ddl` over every input file), so a
/// rule can rely on what other files define: a call with OUTPUT arguments
/// converts only when the called procedure's parameters are known.
pub fn rewrite_in(
    text: &str,
    from: Dialect,
    to: Dialect,
    source: Option<&sqldiff_core::catalog::Catalog>,
) -> Draft {
    let root = parse(text, from).syntax();
    let raw = match (from, to) {
        (Dialect::Tsql, Dialect::Postgres | Dialect::Db2i) => {
            tsql::convert(&root, text, to, source)
        }
        _ => no_rules(&root, text, from, to),
    };
    Draft { from, to, raw }
}

/// Engine findings on the output alone that gate a unit: it does not bind
/// (unknown built-in, column, variable, type, ...), still holds source-dialect
/// syntax, breaks a target limit, or is a known hazard. Findings about the
/// code's own quality (analysis hits) are not about the conversion and do not.
pub fn gates(f: &sqldiff_core::Finding) -> bool {
    use sqldiff_core::ChangeClass as C;
    matches!(f.class, C::Binding | C::Residue | C::Limit) || f.rule.starts_with("hazard.")
}

/// Engine findings of the input-against-output diff that leave the
/// conversion undecided: the engine compared the two sides and could neither
/// show them equivalent nor find a witness (`equiv.unproven`, statements it
/// could not check, transaction or error behaviour it could not prove). A
/// unit holding one is not `converted` or `under assumptions`, whatever its
/// rules assumed. Not counted: findings that describe one side's code on its
/// own (analysis hits, lints and hazards, which `gates` handles for the
/// output), and binding, residue and limit findings, which the output's own
/// check decides.
pub fn undecided(f: &sqldiff_core::Finding) -> bool {
    use sqldiff_core::ChangeClass as C;
    f.verdict == sqldiff_core::Verdict::Unknown
        && !matches!(f.class, C::Binding | C::Residue | C::Limit | C::Cosmetic)
        && !f.rule.starts_with("rules.")
        && !f.rule.starts_with("hazard.")
}

/// Check a draft: diff the input against the output, check the output on
/// its own, and give every unit the worst of its rule notes and findings.
pub fn check(input: &str, draft: Draft, opts: &Options) -> Conversion {
    let Draft { from, to, raw } = draft;
    let a = Artifact::new("input.sql", from, input);
    let b = Artifact::new("converted.sql", to, raw.text.as_str());
    let report = sqldiff_core::diff(&a, &b, opts);
    // The output on its own, bound against the target catalog.
    let own = Options {
        catalog_a: opts.catalog_b.clone(),
        facts_a: opts.facts_b.clone(),
        catalog_b: Vec::new(),
        facts_b: Vec::new(),
        ..opts.clone()
    };
    let output_check = sqldiff_core::check(&b, &own);
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
                output_findings: Vec::new(),
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
            } else if undecided(f) {
                u.status = u.status.max(Status::Unproven);
                u.checked = u.checked.max(Status::Unproven);
                if !u.notes.iter().any(|n| n.rule == f.rule) {
                    // Notes stay worst first.
                    let at = u.notes.partition_point(|n| n.status >= Status::Unproven);
                    u.notes.insert(
                        at,
                        Note {
                            status: Status::Unproven,
                            rule: f.rule.clone(),
                            text: format!(
                                "The engine could not show the output equivalent to the input: {}",
                                f.witness
                            ),
                        },
                    );
                }
            }
        }
    }
    // The output's own check: anything that does not bind, or still reads as
    // the source dialect, keeps its unit from counting as converted, whether
    // the finding is proven or rests on an assumption.
    let mut unplaced = Vec::new();
    for (fi, f) in output_check.findings.iter().enumerate() {
        if !gates(f) {
            continue;
        }
        let Some(loc) = f.a.as_ref() else {
            unplaced.push(fi);
            continue;
        };
        let Some(u) = units
            .iter_mut()
            .find(|u| overlaps((u.out.start, u.out.end + 1), (loc.byte_start, loc.byte_end)))
        else {
            unplaced.push(fi);
            continue;
        };
        u.output_findings.push(fi);
        u.status = u.status.max(Status::Differs);
        u.checked = Status::Differs;
        if !u.notes.iter().any(|n| n.rule == f.rule) {
            u.notes.insert(
                0,
                Note {
                    status: Status::Differs,
                    rule: f.rule.clone(),
                    text: format!(
                        "The output does not check as {}: {}",
                        to.display_name(),
                        without_side(&f.witness)
                    ),
                },
            );
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
        output_check,
        unplaced,
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
            Status::Unproven => &mut counts.unproven,
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

/// A one-sided check's witness without its `Side A line N: ` prefix (the
/// unit already says where).
fn without_side(w: &str) -> &str {
    w.strip_prefix("Side A line ")
        .and_then(|r| r.split_once(": "))
        .map_or(w, |(_, rest)| rest)
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
