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

use serde::{Deserialize, Serialize};
use sqldiff_core::{Artifact, Dialect, Options, Report};
use sqldiff_syntax::{parse, LineIndex};

pub mod base;
pub mod callees;
pub mod cert;
pub mod drift;
pub mod gate;
pub mod instances;
pub mod job;
pub mod names;
mod out;
pub mod queue;
mod refused;
pub mod review;
mod template;
mod tok;
pub mod tree;
mod tsql;

pub use template::{
    check_template, convert_template, rewrite_template, rewrite_template_as, rewrite_template_with,
    split_pair, template_fragment, FragmentKind, StandIns, TemplateDraft, TemplateInfo, ValueKind,
};

/// How far a converted unit can be trusted, from best to worst.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub status: Status,
    /// Stable rule id, e.g. `ASM-CI-COLLATION` or `tsql.merge`.
    pub rule: String,
    pub text: String,
}

/// One rewrite the rules made: which rule, on which input bytes. Unlike a
/// [`Note`], a step never decides a unit's status; it records how the output
/// came from the input, for the certificate's trace (docs/verified-conversion.md),
/// and is an untrusted hint to a checker. Every rewrite leaves one, including
/// those that need no assumption; a token copied unchanged leaves none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// Stable rule id, `tsql.<area>.<name>` (e.g. `tsql.func.len`), or the
    /// id of the note the rule also writes where it has one (`tsql.merge`).
    pub rule: String,
    /// Byte offsets in the input of the text the rule rewrote.
    pub start: usize,
    pub end: usize,
    /// What the rule was applied with, when its id alone does not say
    /// (the target type, the variable's new name, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inst: Option<String>,
}

/// A unit's text as the trace's completeness rule compares it: without
/// whitespace and statement terminators, in lower case. A unit whose input
/// and output differ by more than that was rewritten, and its trace names at
/// least one rule (checked in debug builds and by the corpus test).
pub fn trace_text(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != ';')
        .flat_map(char::to_lowercase)
        .collect()
}

/// A span in the input or the output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Unit {
    pub status: Status,
    pub notes: Vec<Note>,
    /// The rewrites the rules made in this unit, in the order they made
    /// them. Empty for a unit written by hand.
    pub trace: Vec<Step>,
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
    /// Set when the output was written by hand (a `written` record with
    /// text in the review ledger), not by the rules.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub written: Option<Written>,
}

/// Who wrote a unit's output by hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Written {
    pub by: review::By,
    pub author: String,
    /// The unit's id as the rules wrote it, which the ledger record names.
    pub origin: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Counts {
    pub converted: usize,
    pub assumptions: usize,
    pub decision: usize,
    pub unproven: usize,
    pub differs: usize,
    pub not_converted: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// The engine's "not proven" reasons (`report.unknown_reasons`) that name
    /// no line inside a unit. Any entry keeps the file from counting as
    /// converted: something in it was not compared.
    pub unproven: Vec<String>,
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

    /// Unit ids as [`review::unit_ids`] gives them for the checked
    /// conversion: the same while no unit was replaced.
    pub fn unit_ids(&self, path: &str, input: &str) -> Vec<String> {
        review::ids_of(
            path,
            self.raw.units.iter().map(|u| {
                (
                    input.get(u.src.0..u.src.1).unwrap_or(""),
                    self.raw.text.get(u.out.0..u.out.1).unwrap_or(""),
                )
            }),
        )
    }

    /// The text of the worst rule note at `at_least` or worse, if any.
    pub(crate) fn worst_note(&self, at_least: Status) -> Option<String> {
        self.raw
            .units
            .iter()
            .flat_map(|u| &u.notes)
            .filter(|n| n.status >= at_least)
            .max_by_key(|n| n.status)
            .map(|n| n.text.clone())
    }

    /// Adds a note to unit `i` (a hand-written text that could not be applied).
    pub fn note_unit(&mut self, i: usize, note: Note) {
        if let Some(u) = self.raw.units.get_mut(i) {
            u.notes.push(note);
        }
    }

    /// Replaces unit `i`'s output with `text`, written by hand; `note` says
    /// who wrote it. The rules' assumptions and decisions on the unit are
    /// kept (`kept_notes`): the check compares under the same dialect
    /// mappings they name, so a hand-written text never claims fewer. Their
    /// other notes were about the rules' output and go; the check that
    /// follows judges the new text like any other. Fails when another
    /// unit's output overlaps unit `i`'s.
    pub fn replace_unit(
        &mut self,
        i: usize,
        text: &str,
        written: Written,
        note: Note,
    ) -> Result<(), String> {
        let (s, e) = self.raw.units.get(i).ok_or("no such unit")?.out;
        if self
            .raw
            .units
            .iter()
            .enumerate()
            .any(|(k, u)| k != i && u.out.0 < e.max(s + 1) && s < u.out.1.max(u.out.0 + 1))
        {
            return Err("another unit's output overlaps this one".into());
        }
        let text = text.trim_end_matches('\n');
        self.raw.text.replace_range(s..e, text);
        let shift = |x: usize| if x >= e { x + text.len() - (e - s) } else { x };
        for (k, u) in self.raw.units.iter_mut().enumerate() {
            if k == i {
                u.out = (s, s + text.len());
                u.notes = kept_notes(&note, &u.notes);
                u.trace.clear();
                u.written = Some(written.clone());
            } else {
                u.out = (shift(u.out.0), shift(u.out.1));
            }
        }
        Ok(())
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
    let raw = guarded(&root, text, || match (from, to) {
        (Dialect::Tsql, Dialect::Postgres | Dialect::Db2i) => {
            tsql::convert(&root, text, to, source)
        }
        _ => no_rules(&root, text, from, to),
    });
    Draft { from, to, raw }
}

/// Runs the rules over one file. A panic in them must not take the other files
/// of a tree with it: the file becomes "not converted", every unit with the
/// reason, and the run goes on.
fn guarded(
    root: &sqldiff_syntax::SyntaxNode,
    text: &str,
    rules: impl FnOnce() -> out::Out,
) -> out::Out {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(rules)) {
        Ok(o) => o,
        Err(e) => {
            let what = e
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| e.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown".into());
            refuse_all(
                root,
                text,
                &format!(
                    "the conversion rules stopped on this file with an internal error ({what}); nothing in it was converted. This is a bug: report it with a reduced example"
                ),
                "internal-error",
            )
        }
    }
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
    // The engine pairs statements through the units' trace: an input
    // statement only with output written from it.
    let traced = Options {
        anchors: raw
            .units
            .iter()
            .map(|u| sqldiff_core::equiv::Anchor { a: u.src, b: u.out })
            .collect(),
        ..opts.clone()
    };
    // Davy, 2026-10-29: the output keeps the target's text types, and each
    // statement whose result T-SQL's case-insensitive collation can change
    // is flagged with ASM-CI-COLLATION for the reviewer.
    let report = {
        let _ci = sqldiff_core::equiv::compare::ci_collation_scope(true);
        sqldiff_core::diff(&a, &b, &traced)
    };
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
    let (ui, uo) = (U16Index::new(input), U16Index::new(&raw.text));
    let mut units: Vec<Unit> = raw
        .units
        .into_iter()
        .map(|u| {
            let mut notes = u.notes;
            notes.sort_by_key(|n| std::cmp::Reverse(n.status));
            Unit {
                status: notes.first().map_or(Status::Converted, |n| n.status),
                notes,
                trace: u.trace,
                src: span(input, &li, &ui, u.src),
                out: span(&raw.text, &lo, &uo, u.out),
                findings: Vec::new(),
                output_findings: Vec::new(),
                checked: Status::Converted,
                written: u.written,
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
    // What the engine says it did not compare: a unit is converted only
    // when its input and output were compared, so every such statement
    // (DDL, calls, statements with no counterpart, ...) is not proven. A
    // reason that names no unit's line keeps the whole file from counting.
    let mut unproven = Vec::new();
    for reason in &report.unknown_reasons {
        let hits: Vec<usize> = match reason_line(reason) {
            Some((out_side, line)) => units
                .iter()
                .enumerate()
                .filter(|(_, u)| {
                    let s = if out_side { u.out } else { u.src };
                    s.line <= line && line <= s.end_line
                })
                .map(|(i, _)| i)
                .collect(),
            None => Vec::new(),
        };
        if hits.is_empty() {
            unproven.push(reason.clone());
            continue;
        }
        // The innermost units: a routine's header unit spans its body, and
        // the reason is about the statement, not the routine.
        let span = |i: usize| {
            let u = &units[i];
            let s = if reason_line(reason).is_some_and(|(o, _)| o) {
                u.out
            } else {
                u.src
            };
            (s.start, s.end)
        };
        let inner: Vec<usize> = hits
            .iter()
            .copied()
            .filter(|&i| {
                let (a, b) = span(i);
                !hits.iter().any(|&j| {
                    let (c, d) = span(j);
                    j != i && a <= c && d <= b && (c, d) != (a, b)
                })
            })
            .collect();
        let hits = inner;
        let text = format!(
            "The engine did not compare this with its conversion: {}",
            reason_text(reason)
        );
        for i in hits {
            let u = &mut units[i];
            u.status = u.status.max(Status::Unproven);
            u.checked = u.checked.max(Status::Unproven);
            if !u.notes.iter().any(|n| n.text == text) {
                let at = u.notes.partition_point(|n| n.status >= Status::Unproven);
                u.notes.insert(
                    at,
                    Note {
                        status: Status::Unproven,
                        rule: NOT_COMPARED.into(),
                        text: text.clone(),
                    },
                );
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

    // The engine's assumptions about the pair as a whole name no line, so
    // each one keeps every unit of the file from counting as converted:
    // "converted" means no assumption at all. The few every conversion
    // makes (`EVERY_PAIR`) are stated once for the whole tree instead.
    for x in report
        .assumptions
        .iter()
        .filter(|x| !EVERY_PAIR.contains(&x.id.as_str()))
    {
        for u in &mut units {
            if u.notes.iter().any(|n| n.rule == x.id) {
                continue;
            }
            u.status = u.status.max(Status::Assumptions);
            let at = u.notes.partition_point(|n| n.status >= Status::Assumptions);
            u.notes.insert(
                at,
                Note {
                    status: Status::Assumptions,
                    rule: x.id.clone(),
                    text: x.text.clone(),
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
        unproven,
        template: None,
    };
    recount(&mut c);
    c
}

/// Assumptions the engine's diff makes about every conversion pair (names
/// fold, resolve in the default schema, and so on): stated once for the
/// whole tree, not on each unit.
pub(crate) const EVERY_PAIR: [&str; 4] = [
    "ASM-IDENT-FOLD",
    "ASM-DEFAULT-SCHEMA",
    "ASM-DBO-SCHEMA",
    "ASM-TSQL-OWNERSHIP-CHAIN",
];

/// Rule of the note on a unit the engine did not compare.
pub const NOT_COMPARED: &str = "equiv.not-compared";

/// `(output side, line)` of an engine reason `side A (T-SQL) line 12: ...`.
fn reason_line(reason: &str) -> Option<(bool, usize)> {
    let rest = reason.strip_prefix("side ")?;
    let out_side = match rest.as_bytes().first()? {
        b'A' => false,
        b'B' => true,
        _ => return None,
    };
    let (_, after) = rest.split_once(") line ")?;
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    after[digits.len()..].starts_with(':').then_some(())?;
    Some((out_side, digits.parse().ok()?))
}

/// A reason without its `side A (T-SQL) line 12: ` prefix.
fn reason_text(reason: &str) -> &str {
    match reason_line(reason) {
        Some(_) => reason.split_once(": ").map_or(reason, |(_, rest)| rest),
        None => reason,
    }
}

/// A hand-written unit's notes: `note`, then the rules' assumptions and
/// decisions from `old`.
fn kept_notes(note: &Note, old: &[Note]) -> Vec<Note> {
    let mut v = vec![note.clone()];
    v.extend(
        old.iter()
            .filter(|n| matches!(n.status, Status::Assumptions | Status::Decision))
            .cloned(),
    );
    v
}

/// Replaces unit `i`'s output in a checked conversion with `text`, written
/// by hand, without checking it again: the unit becomes `unproven` with
/// `note` and the rules' assumptions and decisions (for templates, whose
/// hand-written units the instance check judges). Fails when another
/// unit's output overlaps unit `i`'s.
pub fn splice_unit(
    c: &mut Conversion,
    i: usize,
    text: &str,
    written: Written,
    note: Note,
) -> Result<(), String> {
    let (s, e) = {
        let u = c.units.get(i).ok_or("no such unit")?;
        (u.out.start, u.out.end)
    };
    if c.units
        .iter()
        .enumerate()
        .any(|(k, u)| k != i && u.out.start < e.max(s + 1) && s < u.out.end.max(u.out.start + 1))
    {
        return Err("another unit's output overlaps this one".into());
    }
    let text = text.trim_end_matches('\n');
    c.output.replace_range(s..e, text);
    let shift = |x: usize| if x >= e { x + text.len() - (e - s) } else { x };
    let lo = LineIndex::new(&c.output);
    let uo = U16Index::new(&c.output);
    for (k, u) in c.units.iter_mut().enumerate() {
        let (a, b) = if k == i {
            (s, s + text.len())
        } else {
            (shift(u.out.start), shift(u.out.end))
        };
        u.out = span(&c.output, &lo, &uo, (a, b));
        if k == i {
            u.notes = kept_notes(&note, &u.notes);
            u.trace.clear();
            u.findings.clear();
            u.output_findings.clear();
            u.checked = Status::Unproven;
            u.status = note.status.max(Status::Unproven);
            u.written = Some(written.clone());
        }
    }
    recount(c);
    Ok(())
}

pub(crate) fn recount(c: &mut Conversion) {
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
    refuse_all(root, text, &reason, "no-rules")
}

/// Every unit of the file kept as a comment, "not converted" for `reason`.
fn refuse_all(root: &sqldiff_syntax::SyntaxNode, text: &str, reason: &str, rule: &str) -> out::Out {
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
                rule: rule.into(),
                text: reason.to_string(),
            }],
            vec![Step {
                rule: rule.into(),
                start: s,
                end: e,
                inst: None,
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

/// Byte offset to UTF-16 offset in `text` without re-encoding the text from
/// its start for every span: the counts at char boundaries every
/// [`U16Index::STEP`] bytes, then the rest of the way encoded.
pub(crate) struct U16Index {
    /// (byte offset at a char boundary, UTF-16 units before it), ascending.
    marks: Vec<(usize, usize)>,
}

impl U16Index {
    const STEP: usize = 4096;

    pub(crate) fn new(text: &str) -> Self {
        let mut marks = vec![(0, 0)];
        let mut units = 0;
        for (i, c) in text.char_indices() {
            if i >= marks.last().unwrap().0 + Self::STEP {
                marks.push((i, units));
            }
            units += c.len_utf16();
        }
        U16Index { marks }
    }

    /// The same as `text[..b].encode_utf16().count()`.
    fn at(&self, text: &str, b: usize) -> usize {
        let (at, units) = self.marks[self.marks.partition_point(|m| m.0 <= b) - 1];
        units + text[at..b].encode_utf16().count()
    }
}

pub(crate) fn span(
    text: &str,
    li: &LineIndex,
    ui: &U16Index,
    (start, end): (usize, usize),
) -> Span {
    let u16 = |b: usize| ui.at(text, b);
    Span {
        start,
        end,
        start_u16: u16(start),
        end_u16: u16(end),
        line: li.line_col(start).0,
        end_line: li.line_col(end.saturating_sub(1).max(start)).0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_in_the_rules_makes_the_file_not_converted_not_a_crash() {
        let text = "CREATE PROCEDURE a AS SELECT 1\nGO\nCREATE PROCEDURE b AS SELECT 2\nGO\n";
        let root = parse(text, Dialect::Tsql).syntax();
        let o = guarded(&root, text, || panic!("the rules lost their place"));
        assert!(o.text.contains("[not converted]"), "{}", o.text);
        assert!(o.text.contains("the rules lost their place"), "{}", o.text);
        assert_eq!(o.units.len(), 2, "every unit is accounted for");
        assert!(o.units.iter().all(|u| u
            .notes
            .iter()
            .any(|n| n.rule == "internal-error" && n.status == Status::NotConverted)));
    }

    #[test]
    fn rules_that_do_not_panic_are_untouched() {
        let text = "CREATE PROCEDURE a AS SELECT 1\nGO\n";
        let root = parse(text, Dialect::Tsql).syntax();
        let o = guarded(&root, text, out::Out::default);
        assert!(o.text.is_empty());
    }
}

#[cfg(test)]
mod u16_index {
    use super::U16Index;

    #[test]
    fn matches_encoding_from_the_start() {
        let mut text = String::new();
        for i in 0..3000 {
            text.push_str(["a", "é", "€", "𝄞", "\n", "SELECT 1;"][i % 6]);
        }
        let ui = U16Index::new(&text);
        for b in (0..=text.len()).filter(|&b| text.is_char_boundary(b)) {
            assert_eq!(ui.at(&text, b), text[..b].encode_utf16().count(), "at {b}");
        }
        let empty = U16Index::new("");
        assert_eq!(empty.at("", 0), 0);
    }
}
