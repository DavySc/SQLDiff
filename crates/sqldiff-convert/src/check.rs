//! Pass 2 of a conversion: checking a draft against its source.
//!
//! Nothing here writes a draft. The only way this code gets a draft other
//! than the one it is handed is the `rebuild` callback of [`check_with`]
//! (a mix of the two variants of a draft), which the tree driver supplies;
//! `tests/stage_boundary.rs` asserts that no file of this crate that the
//! check runs through names the draft crate.

use super::*;

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
    check_with(input, draft, opts, None)
}

/// Builds the draft again with the routines starting at the given source
/// offsets (sorted) converted without exception blocks.
pub(crate) type Rebuild<'a> = &'a (dyn Fn(&[usize]) -> Draft + Sync);

/// The scores [`check`] ranks a conversion by: units with a difference
/// found, then units still open.
pub(crate) fn score_of<'a>(units: impl Iterator<Item = &'a Unit>) -> (usize, usize) {
    let (mut differs, mut open) = (0, 0);
    for u in units {
        differs += usize::from(u.status == Status::Differs);
        open += usize::from(!matches!(u.status, Status::Converted | Status::Assumptions));
    }
    (differs, open)
}

/// A marker that makes a test input build a mix (`cfg(test)` only).
#[cfg(test)]
pub(crate) const FORCE_MIX: &str = "-- sqldiff-test: force-mix";

/// The mix [`check_with`] built a draft for: the routines (by the source
/// offset where each starts) written without exception blocks, and the
/// draft that came out (`Draft::canonical`). Stored with the result whether or
/// not the mix won, because the draft code is not part of a result's
/// fingerprint: on reuse the mix is built again and compared, and a mix
/// that lost before can win after an edit to the rules, or the other way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mix {
    pub plain_at: Vec<usize>,
    pub draft: String,
}

/// [`check`], and when the draft has a variant without exception blocks the
/// variant is chosen for each routine on its own: a routine is its own unit
/// of behaviour. With `rebuild`, a file whose routines do best in different
/// variants is converted again with that mix and checked; the mix is kept
/// only when its score beats the better of the two whole-file variants.
pub(crate) fn check_with(
    input: &str,
    draft: Draft,
    opts: &Options,
    rebuild: Option<Rebuild>,
) -> Conversion {
    check_with_mix(input, draft, opts, rebuild).0
}

/// [`check_with`], and the mix that was built, if one was (whether or not it
/// was kept).
pub(crate) fn check_with_mix(
    input: &str,
    draft: Draft,
    opts: &Options,
    rebuild: Option<Rebuild>,
) -> (Conversion, Option<Mix>) {
    let mut draft = draft;
    let Some(plain) = draft.plain.take() else {
        return (check_one(input, draft, opts), None);
    };
    let from = draft.from;
    let blocks = check_one(input, draft, opts);
    let without = check_one(input, *plain, opts);
    // Never the version the engine has shown wrong over the one it cannot
    // yet prove: fewer units with a difference found first, then fewer units
    // open. (Both versions are the same code to run; only what the engine
    // can tie down differs, and a difference it found in the plain version,
    // T-SQL going on where PostgreSQL stops, is gone from the blocks.)
    let score = |c: &Conversion| score_of(c.units.iter());
    let (sb, sw) = (score(&blocks), score(&without));
    let mut built: Option<Mix> = None;
    let mix = rebuild.and_then(|rebuild| {
        // Each routine's score in each variant, by the units whose input
        // statement lies in it.
        let root = sqldiff_syntax::parse(input, from).syntax();
        let mut plain_at = Vec::new();
        let (mut blocks_better, mut plain_better) = (false, false);
        for r in root
            .descendants()
            .filter(|n| n.kind() == sqldiff_syntax::SyntaxKind::ROUTINE)
        {
            let range = r.text_range();
            let (rs, re) = (usize::from(range.start()), usize::from(range.end()));
            let inside = |c: &Conversion| {
                score_of(
                    c.units
                        .iter()
                        .filter(|u| rs <= u.src.start && u.src.start < re),
                )
            };
            let (rb, rw) = (inside(&blocks), inside(&without));
            if rw <= rb {
                plain_at.push(rs);
            }
            blocks_better |= rb < rw;
            plain_better |= rw < rb;
        }
        // Only a file where one routine does better with the blocks and
        // another does better without them can beat both whole-file variants.
        let fresh = blocks_better && plain_better;
        // Tests only: no input of the corpora builds a mix, so an input
        // naming `FORCE_MIX` has the engine try one (it is still kept only
        // when it scores better). Compiled out of every other build.
        #[cfg(test)]
        let fresh = fresh || input.contains(FORCE_MIX);
        fresh.then(|| {
            let mixed = rebuild(&plain_at);
            built = Some(Mix {
                plain_at: plain_at.clone(),
                draft: mixed.canonical(),
            });
            check_one(input, mixed, opts)
        })
    });
    let best = if sb < sw { blocks } else { without };
    let chosen = match mix {
        Some(m) if score(&m) < score(&best) => m,
        _ => best,
    };
    (chosen, built)
}

fn check_one(input: &str, draft: Draft, opts: &Options) -> Conversion {
    let Draft {
        from,
        to,
        raw,
        options,
        ..
    } = draft;
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
        historyless: options.temporal_historyless,
        // Nothing here reads the witnesses (see `trim_reports`).
        skip_witnesses: true,
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
                            rule: f.rule.clone().into(),
                            text: format!(
                                "The engine could not show the output equivalent to the input: {}",
                                f.witness
                            )
                            .into(),
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
            // A dropped `:r` include: there is nothing to compare, and the
            // unit's outcome is the decision to run the included file's
            // conversion there (`tsql.sqlcmd-include`).
            if reason_text(reason).starts_with("a SQLCMD directive `:r`")
                && u.notes.iter().any(|n| n.rule == "tsql.sqlcmd-include")
            {
                continue;
            }
            u.status = u.status.max(Status::Unproven);
            u.checked = u.checked.max(Status::Unproven);
            if !u.notes.iter().any(|n| n.text == text) {
                let at = u.notes.partition_point(|n| n.status >= Status::Unproven);
                u.notes.insert(
                    at,
                    Note {
                        status: Status::Unproven,
                        rule: NOT_COMPARED.into(),
                        text: text.clone().into(),
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
                    rule: f.rule.clone().into(),
                    text: format!(
                        "The output does not check as {}: {}",
                        to.display_name(),
                        without_side(&f.witness)
                    )
                    .into(),
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
                    )
                    .into(),
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
                    rule: x.id.clone().into(),
                    text: x.text.clone().into(),
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

/// Rule of the note on a unit of a file that was not checked in this run
/// (an increment that left it out, with no stored result that matches).
pub const NOT_CHECKED: &str = "plan.not-checked";

/// What a file that was not checked comes out as: its draft with every unit
/// not converted, so no total, gate or queue can read it as converted. The
/// engine never ran on it. Reports are empty.
pub(crate) fn not_checked(input: &str, draft: Draft) -> Conversion {
    let Draft { from, to, raw, .. } = draft;
    let (li, lo) = (LineIndex::new(input), LineIndex::new(&raw.text));
    let (ui, uo) = (U16Index::new(input), U16Index::new(&raw.text));
    let units: Vec<Unit> = raw
        .units
        .into_iter()
        .map(|u| Unit {
            status: Status::NotConverted,
            notes: vec![Note {
                status: Status::NotConverted,
                rule: NOT_CHECKED.into(),
                text: "This file was not checked in this run, and no stored result matches it."
                    .into(),
            }],
            trace: u.trace,
            src: span(input, &li, &ui, u.src),
            out: span(&raw.text, &lo, &uo, u.out),
            findings: Vec::new(),
            output_findings: Vec::new(),
            checked: Status::NotConverted,
            written: u.written,
        })
        .collect();
    let empty = |d: Dialect| {
        sqldiff_core::check(&Artifact::new("converted.sql", d, ""), &Options::default())
    };
    let mut c = Conversion {
        from: from.id().to_string(),
        to: to.id().to_string(),
        output: raw.text,
        units,
        counts: Counts::default(),
        report: empty(to),
        output_check: empty(to),
        unplaced: Vec::new(),
        unproven: vec!["the file was not checked in this run".to_string()],
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

pub(crate) fn recount<C: Cored>(c: &mut C) {
    let mut counts = Counts::default();
    for u in c.units() {
        *match u.status {
            Status::Converted => &mut counts.converted,
            Status::Assumptions => &mut counts.assumptions,
            Status::Decision => &mut counts.decision,
            Status::Unproven => &mut counts.unproven,
            Status::Differs => &mut counts.differs,
            Status::NotConverted => &mut counts.not_converted,
        } += 1;
        if u.status == Status::NotConverted && u.impossible().is_some() {
            counts.impossible += 1;
        }
    }
    c.set_counts(counts);
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
mod u16_index {
    use super::*;

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
