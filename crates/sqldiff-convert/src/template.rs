//! Converting a template: SQL with a generator's placeholders and special
//! forms (`sqldiff_syntax::template`).
//!
//! The template becomes a fragment in which every placeholder is a stand-in
//! identifier and every special form a stand-in comment; the fragment is
//! converted and checked like any input; then the stand-ins in the output
//! are put back as the template writes them, so the result is a template for
//! the target dialect. Stand-ins are built from a stem that does not occur in
//! the template (in any letter case) and end in an index, so each names one
//! placeholder or form and no text of the user's can be mistaken for one.
//! Spans and the report point into the template (side A) and the restored
//! output (side B). A stand-in the rules dropped or changed is reported,
//! never guessed back.

use regex::Regex;
use serde::Serialize;
use sqldiff_core::artifact::location_in;
use sqldiff_core::composition::{remap_to_template, template_notes, PlaceholderBasis};
use sqldiff_core::model::Side;
use sqldiff_core::Dialect;
use sqldiff_syntax::template::{Derived, MapSpan, OffsetMap, SegmentKind, Template};
use sqldiff_syntax::LineIndex;

use crate::{check, recount, rewrite_in, span, Conversion, Draft, Note, Status, U16Index};
use sqldiff_core::catalog::Catalog;
use sqldiff_core::Options;

/// Excerpt lines in remapped findings.
const EXCERPT: usize = 3;

/// What the template held, for the viewer.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TemplateInfo {
    /// Placeholder names, in order of first use.
    pub parameters: Vec<String>,
    /// Candidates that are not placeholders (kept as SQL), as written.
    pub unresolved: Vec<String>,
    /// Special forms (generator directives), by form name.
    pub special_forms: Vec<String>,
}

/// What a parameter's values are (`[template] values` in the pack).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueKind {
    /// A name (the default): converted as one, so every value must be a
    /// name the target accepts there (`ASM-TEMPLATE-PLACEHOLDER`).
    Name,
    /// Text inside a string, or a number: the same in every dialect, no
    /// name rules apply.
    Text,
    Number,
    /// Dialect code the rules cannot convert: its value must be given per
    /// dialect (`template.value-per-dialect`).
    Type,
    Expression,
    Sql,
}

impl ValueKind {
    pub fn parse(s: &str) -> Option<ValueKind> {
        Some(match s {
            "name" => ValueKind::Name,
            "text" => ValueKind::Text,
            "number" => ValueKind::Number,
            "type" => ValueKind::Type,
            "expression" => ValueKind::Expression,
            "sql" => ValueKind::Sql,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            ValueKind::Name => "name",
            ValueKind::Text => "text",
            ValueKind::Number => "number",
            ValueKind::Type => "type",
            ValueKind::Expression => "expression",
            ValueKind::Sql => "sql",
        }
    }

    /// Dialect code: the same value cannot be assumed to work in another
    /// dialect.
    pub fn per_dialect(self) -> bool {
        matches!(
            self,
            ValueKind::Type | ValueKind::Expression | ValueKind::Sql
        )
    }
}

/// What a template holds when it is not whole statements
/// (`[[template.fragments]]` in the pack). Such a fragment is converted
/// inside a fixed stand-in statement; the converted stand-in must come out
/// exactly as it does around a neutral fragment, or the conversion reached
/// across the fragment's edge (hoisted a declaration, added a parameter)
/// and the fragment cannot be converted on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FragmentKind {
    Statements,
    /// A search condition, as after WHERE.
    Predicate,
    /// An expression or a select list.
    Expression,
    /// Assignments, as after UPDATE … SET.
    SetList,
    /// Statements inside a procedure body.
    RoutineBody,
}

impl FragmentKind {
    pub fn parse(s: &str) -> Option<FragmentKind> {
        Some(match s {
            "statements" => FragmentKind::Statements,
            "predicate" => FragmentKind::Predicate,
            "expression" | "select-list" => FragmentKind::Expression,
            "set-list" => FragmentKind::SetList,
            "routine-body" => FragmentKind::RoutineBody,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            FragmentKind::Statements => "statements",
            FragmentKind::Predicate => "predicate",
            FragmentKind::Expression => "expression",
            FragmentKind::SetList => "set-list",
            FragmentKind::RoutineBody => "routine-body",
        }
    }

    /// The stand-in statement in T-SQL: text before the fragment (ending in
    /// the start marker, on one line so the fragment keeps its line numbers),
    /// a neutral fragment that converts as written, and the text after. The
    /// table is in a schema nothing defines, so names in the fragment are
    /// not bound against anything.
    fn harness(self, stem: &str) -> Option<(String, &'static str, String)> {
        let (t, m) = (format!("{stem}hs.{stem}h"), format!("/*{stem}hb*/"));
        Some(match self {
            FragmentKind::Statements => return None,
            FragmentKind::Predicate => (
                format!("SELECT 1 FROM {t} WHERE {m} "),
                "1 = 1",
                ";\n".into(),
            ),
            FragmentKind::Expression => (format!("SELECT {m} "), "1", format!(" FROM {t};\n")),
            FragmentKind::SetList => (format!("UPDATE {t} SET {m} "), "x = 1", ";\n".into()),
            FragmentKind::RoutineBody => (
                format!("CREATE PROCEDURE {t} AS BEGIN {m} "),
                "UPDATE x SET x = 1;",
                "\nEND\n".into(),
            ),
        })
    }
}

/// The stand-in statement around a fragment, and what the same statement
/// converts to around the neutral fragment.
#[derive(Debug, Clone)]
struct Harness {
    kind: FragmentKind,
    /// The fragment's byte range in the harnessed fragment text.
    frag: (usize, usize),
    /// The converted stand-in before and after the neutral fragment; `None`
    /// when the stand-in itself did not convert as expected.
    around: Option<(String, String)>,
}

/// One replacement in the output: old byte range, new text.
struct Swap {
    old: (usize, usize),
    new: String,
}

/// The stand-in names for a set of templates converted together. One stem
/// that none of them contains, in any letter case, and one index per
/// placeholder name across all of them, so the same placeholder reads the
/// same in every fragment and a catalog built from one fragment binds in
/// another.
#[derive(Debug, Clone)]
pub struct StandIns {
    stem: String,
    names: Vec<String>,
    /// The catalogs bind stand-in names across templates.
    across: bool,
    /// Value kinds by placeholder name; a name not here is a name.
    kinds: std::collections::BTreeMap<String, ValueKind>,
}

impl StandIns {
    pub fn new<'a>(templates: impl IntoIterator<Item = &'a Template>) -> StandIns {
        let mut stem = String::from("sqldifftpl");
        let mut names: Vec<String> = Vec::new();
        let mut texts: Vec<String> = Vec::new();
        for t in templates {
            texts.push(t.text.to_ascii_lowercase());
            for n in t.parameters() {
                if !names.contains(&n) {
                    names.push(n);
                }
            }
        }
        while texts.iter().any(|t| t.contains(&stem)) {
            stem.push('x');
        }
        StandIns {
            stem,
            names,
            across: false,
            kinds: Default::default(),
        }
    }

    /// The pack's value kinds, by placeholder name as the templates' parse
    /// keeps it (upper case under `ignore_case`).
    pub fn with_kinds(mut self, kinds: impl IntoIterator<Item = (String, ValueKind)>) -> StandIns {
        self.kinds.extend(kinds);
        self
    }

    fn kind(&self, name: &str) -> ValueKind {
        self.kinds.get(name).copied().unwrap_or(ValueKind::Name)
    }

    /// Says that the catalogs given to [`check_template`] were built from
    /// several templates under these stand-ins, so a name with a placeholder
    /// binds to an object another template defines. That holds only when
    /// one weave fills each placeholder with the same value everywhere, and
    /// every unit with a placeholder then says so
    /// (`ASM-TEMPLATE-SHARED-VALUES`).
    pub fn bound_across(mut self) -> StandIns {
        self.across = true;
        self
    }

    /// The stem, in lower case: any output still holding it has a stand-in
    /// that was not put back.
    pub fn stem(&self) -> &str {
        &self.stem
    }
}

/// A template's rules output before it is checked; see [`rewrite_template`].
#[derive(Debug)]
pub struct TemplateDraft {
    frag: Derived,
    draft: Draft,
    /// Placeholder spelling in this template, by stand-in index.
    written: Vec<String>,
    /// Special forms: template byte range and form name.
    specials: Vec<(usize, usize, String)>,
    harness: Option<Harness>,
}

impl TemplateDraft {
    /// The converted fragment, stand-ins still in place.
    pub fn output(&self) -> &str {
        self.draft.output()
    }
}

/// The fragment of `t` under `stand`: placeholders as stand-in names,
/// special forms as stand-in comments, with its offset map to the template.
/// What a source catalog for templates converted together is built from.
pub fn template_fragment(t: &Template, stand: &StandIns) -> String {
    fragment(t, stand).0.text
}

fn fragment(t: &Template, stand: &StandIns) -> (Derived, Vec<String>, Vec<(usize, usize, String)>) {
    let stem = &stand.stem;
    let mut written: Vec<String> = vec![String::new(); stand.names.len()];
    let mut specials: Vec<(usize, usize, String)> = Vec::new();
    let mut text = String::with_capacity(t.text.len());
    let mut map = OffsetMap::default();
    for s in &t.segments {
        let start = text.len();
        let (piece, exact) = match &s.kind {
            SegmentKind::Placeholder { name } => {
                let i = stand
                    .names
                    .iter()
                    .position(|n| n == name)
                    .expect("stand-ins are built from these templates");
                if written[i].is_empty() {
                    written[i] = t.slice(s).to_string();
                }
                (format!("{stem}{i}z"), false)
            }
            SegmentKind::Special { form, .. } => {
                specials.push((s.start, s.end, form.clone()));
                (format!("/*{stem}s{}z*/", specials.len() - 1), false)
            }
            _ => (t.slice(s).to_string(), true),
        };
        text.push_str(&piece);
        map.spans.push(MapSpan {
            src: (s.start, s.end),
            dst: (start, text.len()),
            exact,
        });
    }
    (Derived { text, map }, written, specials)
}

/// Convert a template from one dialect to another; the output keeps the
/// template's placeholders and special forms. `basis` and `delimiters` say
/// how the placeholders were decided (see `compose::Placeholders::parse`),
/// for the report's notes on them.
pub fn convert_template(
    t: &Template,
    from: Dialect,
    to: Dialect,
    basis: PlaceholderBasis,
    delimiters: (&str, &str),
) -> Result<Conversion, String> {
    let stand = StandIns::new([t]);
    let d = rewrite_template(t, from, to, &stand, None);
    Ok(check_template(
        t,
        d,
        &stand,
        &Options::default(),
        basis,
        delimiters,
    ))
}

/// Apply the rules to a template's fragment, with the source catalog of
/// every template converted together (built from [`template_fragment`]s
/// under the same `stand`). Nothing in a draft has been checked yet.
pub fn rewrite_template(
    t: &Template,
    from: Dialect,
    to: Dialect,
    stand: &StandIns,
    source: Option<&Catalog>,
) -> TemplateDraft {
    rewrite_template_as(t, FragmentKind::Statements, from, to, stand, source)
}

/// [`rewrite_template`] for a template that holds a `kind` of fragment: it
/// is converted inside a stand-in statement (T-SQL sources only, the only
/// ones with rules).
pub fn rewrite_template_as(
    t: &Template,
    kind: FragmentKind,
    from: Dialect,
    to: Dialect,
    stand: &StandIns,
    source: Option<&Catalog>,
) -> TemplateDraft {
    let (mut frag, written, specials) = fragment(t, stand);
    let harness = match kind.harness(&stand.stem) {
        Some((pre, neutral, post)) if from == Dialect::Tsql => {
            let marker = format!("/*{}hb*/", stand.stem);
            let base = rewrite_in(&format!("{pre}{neutral}{post}"), from, to, source);
            let base = base.output();
            let around = base.find(&marker).and_then(|m| {
                let m = m + marker.len();
                let n = m + base[m..].find(neutral)?;
                Some((base[..m].to_string(), base[n + neutral.len()..].to_string()))
            });
            let (p, f) = (pre.len(), frag.text.len());
            let mut spans = vec![MapSpan {
                src: (0, 0),
                dst: (0, p),
                exact: false,
            }];
            spans.extend(frag.map.spans.iter().map(|m| MapSpan {
                src: m.src,
                dst: (m.dst.0 + p, m.dst.1 + p),
                exact: m.exact,
            }));
            spans.push(MapSpan {
                src: (t.text.len(), t.text.len()),
                dst: (p + f, p + f + post.len()),
                exact: false,
            });
            frag = Derived {
                text: format!("{pre}{}{post}", frag.text),
                map: OffsetMap { spans },
            };
            Some(Harness {
                kind,
                frag: (p, p + f),
                around,
            })
        }
        _ => None,
    };
    let draft = rewrite_in(&frag.text, from, to, source);
    TemplateDraft {
        frag,
        draft,
        written,
        specials,
        harness,
    }
}

/// Check a template draft like [`crate::check`] (the catalogs in `opts` are
/// in stand-in terms, built from fragments and drafts under `stand`), then
/// put the placeholders and special forms back and point every span into
/// the template and the restored output.
pub fn check_template(
    t: &Template,
    d: TemplateDraft,
    stand: &StandIns,
    opts: &Options,
    basis: PlaceholderBasis,
    delimiters: (&str, &str),
) -> Conversion {
    let TemplateDraft {
        frag,
        draft,
        written,
        specials,
        harness,
    } = d;
    let (from, to) = (draft.from, draft.to);
    let stem = &stand.stem;
    let names = t.parameters();
    // The stand-in statement's table exists, with columns nobody knows, on
    // a side that binds against a catalog: it is not the fragment's problem.
    let mut with_harness;
    let opts = if harness.is_some() {
        with_harness = opts.clone();
        let table = |d: Dialect| {
            let mut c = Catalog::default();
            let ident = |n: String| sqldiff_core::catalog::Ident::plain(n).stored(d);
            let mut tb = sqldiff_core::catalog::Table::new(
                Some(ident(format!("{stem}hs"))),
                ident(format!("{stem}h")),
                sqldiff_core::catalog::TableKind::Table,
            );
            tb.columns_known = false;
            c.put_table(d, tb);
            std::sync::Arc::new(c)
        };
        if !opts.facts_a.is_empty() || !opts.catalog_a.is_empty() {
            with_harness.facts_a.push(table(from));
        }
        if !opts.facts_b.is_empty() || !opts.catalog_b.is_empty() {
            with_harness.facts_b.push(table(to));
        }
        &with_harness
    } else {
        opts
    };
    let mut c = check(&frag.text, draft, opts);
    if let Some(h) = &harness {
        let mid = h.around.as_ref().and_then(|(pre, post)| {
            let o = &c.output;
            (o.len() >= pre.len() + post.len()
                && o.starts_with(pre.as_str())
                && o.ends_with(post.as_str()))
            .then(|| (pre.len(), o.len() - post.len()))
        });
        match mid {
            Some(mid) => {
                // The converted statement's layout around the fragment is
                // not the fragment's: keep the template's own edges.
                let body = t.text.trim();
                let lead = &t.text[..t.text.len() - t.text.trim_start().len()];
                let trail = &t.text[lead.len() + body.len()..];
                strip(&mut c, mid, h.frag, (lead, trail))
            }
            None => return leaked(t, c, h.kind, stem, &written, &specials),
        }
    }

    // Put the placeholders and special forms back.
    let re = Regex::new(&format!(r"(?i-u)/\*{stem}s(\d+)z\*/|{stem}(\d+)z"))
        .expect("stem is alphanumeric");
    let mut swaps: Vec<Swap> = Vec::new();
    let mut seen = vec![0usize; specials.len()];
    for m in re.captures_iter(&c.output) {
        let all = m.get(0).expect("group 0");
        let new = match (m.get(1), m.get(2)) {
            (Some(k), _) => k.as_str().parse::<usize>().ok().and_then(|k| {
                let (s, e, _) = specials.get(k)?;
                seen[k] += 1;
                Some(t.text[*s..*e].to_string())
            }),
            (_, Some(i)) => i
                .as_str()
                .parse::<usize>()
                .ok()
                .and_then(|i| written.get(i).filter(|w| !w.is_empty()).cloned()),
            _ => None,
        };
        // An index that names nothing stays as it is and is reported below.
        if let Some(new) = new {
            swaps.push(Swap {
                old: (all.start(), all.end()),
                new,
            });
        }
    }
    let old = std::mem::take(&mut c.output);
    let mut out = String::with_capacity(old.len());
    let mut at = 0;
    for s in &swaps {
        out.push_str(&old[at..s.old.0]);
        out.push_str(&s.new);
        at = s.old.1;
    }
    out.push_str(&old[at..]);
    // Old output offset → new: inside a swap its start.
    let shift = |off: usize| {
        let mut delta: isize = 0;
        for s in &swaps {
            if off <= s.old.0 {
                break;
            }
            if off < s.old.1 {
                return (s.old.0 as isize + delta) as usize;
            }
            delta += s.new.len() as isize - (s.old.1 - s.old.0) as isize;
        }
        (off as isize + delta) as usize
    };

    let (li, lo) = (LineIndex::new(&t.text), LineIndex::new(&out));
    let (ui, uo) = (U16Index::new(&t.text), U16Index::new(&out));
    let out_lower = out.to_ascii_lowercase();
    let placeholder_spans: Vec<(usize, usize, &str, &str)> = t
        .segments
        .iter()
        .filter_map(|s| match &s.kind {
            SegmentKind::Placeholder { name } => Some((s.start, s.end, t.slice(s), name.as_str())),
            _ => None,
        })
        .collect();
    let mut problems: Vec<String> = Vec::new();
    let mut directives: Vec<&str> = Vec::new();
    for (_, _, form) in &specials {
        if !directives.contains(&form.as_str()) {
            directives.push(form);
        }
    }
    for u in &mut c.units {
        let (ss, se) = frag.map.src_range(u.src.start, u.src.end);
        u.src = span(&t.text, &li, &ui, (ss, se));
        let (os, oe) = (shift(u.out.start), shift(u.out.end));
        u.out = span(&out, &lo, &uo, (os, oe.max(os)));
        if u.status == Status::NotConverted {
            continue;
        }
        if !directives.is_empty() {
            u.notes.push(Note {
                status: Status::Assumptions,
                rule: "ASM-TEMPLATE-DIRECTIVE".into(),
                text: format!(
                    "The template's generator directives ({}) were converted as comments, without the text they add; this unit holds only if the woven output does, so check the woven instances.",
                    directives.join(", ")
                ),
            });
        }
        if let Some(h) = &harness {
            u.notes.push(Note {
                status: Status::Assumptions,
                rule: "ASM-TEMPLATE-FRAGMENT".into(),
                text: format!(
                    "This {} was converted inside a stand-in statement: its names were not bound and its meaning in the woven statement was not checked, so check the woven instances.",
                    h.kind.label()
                ),
            });
        }
        let mut used: Vec<&str> = Vec::new();
        let mut per_dialect: Vec<(&str, ValueKind)> = Vec::new();
        for (s, e, p, name) in &placeholder_spans {
            if !(*s < se && ss < *e) {
                continue;
            }
            match stand.kind(name) {
                ValueKind::Name if !used.contains(p) => used.push(p),
                k if k.per_dialect() && !per_dialect.iter().any(|(q, _)| q == p) => {
                    per_dialect.push((p, k))
                }
                _ => {}
            }
        }
        for (p, k) in &per_dialect {
            u.notes.push(Note {
                status: Status::Decision,
                rule: "template.value-per-dialect".into(),
                text: format!(
                    "{p} holds {} {}, which the rules cannot convert: give its {} value per dialect in the instance manifest (params_for) and check the woven instances.",
                    if k.label().starts_with('e') { "an" } else { "a" },
                    k.label(),
                    to.display_name()
                ),
            });
        }
        if !used.is_empty() {
            u.notes.push(Note {
                status: Status::Assumptions,
                rule: "ASM-TEMPLATE-PLACEHOLDER".into(),
                text: format!(
                    "{} converted as names: every value the generator fills in must be a name {} accepts as written here.",
                    used.join(", "),
                    to.display_name()
                ),
            });
            if stand.across {
                u.notes.push(Note {
                    status: Status::Assumptions,
                    rule: "ASM-TEMPLATE-SHARED-VALUES".into(),
                    text: format!(
                        "Names with {} were bound against what other templates define, as if one weave fills each placeholder with the same value in every template; an instance woven with other values binds elsewhere, so check the woven instances.",
                        used.join(", ")
                    ),
                });
            }
        }
        if out_lower[u.out.start..u.out.end].contains(stem.as_str()) {
            u.notes.push(Note {
                status: Status::Differs,
                rule: "template.placeholder-lost".into(),
                text: "A placeholder was changed by the conversion and could not be put back; check this unit by hand.".into(),
            });
            problems.push(format!(
                "output line {}: a placeholder was changed by the conversion and could not be put back",
                u.out.line
            ));
        }
    }
    // A special form that did not come through once: on the unit it stands in
    // (or the next one), and in the report.
    for (k, (s, _, form)) in specials.iter().enumerate() {
        if seen[k] == 1 {
            continue;
        }
        let line = li.line_col(*s).0;
        let what = if seen[k] == 0 {
            "is missing from"
        } else {
            "is repeated in"
        };
        let text = format!(
            "The generator directive `{form}` at template line {line} {what} the converted template; put it back by hand."
        );
        problems.push(format!(
            "template line {line}: directive `{form}` {what} the output"
        ));
        let target = c
            .units
            .iter_mut()
            .filter(|u| u.src.end > *s)
            .min_by_key(|u| u.src.start.max(*s) - *s);
        if let Some(u) = target {
            u.notes.push(Note {
                status: Status::Differs,
                rule: "template.special-form-lost".into(),
                text,
            });
        }
    }
    for u in &mut c.units {
        u.notes.sort_by_key(|n| std::cmp::Reverse(n.status));
        if let Some(n) = u.notes.first() {
            u.status = u.status.max(n.status);
        }
    }

    // Stand-ins in finding and note text, as the template writes them.
    let respell = |text: &str| respell(text, t, stem, &written, &specials);
    for u in &mut c.units {
        for n in &mut u.notes {
            n.text = respell(&n.text);
        }
    }
    // The output's own check: its side is the restored output.
    for f in &mut c.output_check.findings {
        if let Some(loc) = f.a.as_mut() {
            let span = (shift(loc.byte_start), shift(loc.byte_end));
            *loc = location_in(Side::A, &loc.path, &out, &lo, span, EXCERPT);
        }
        f.witness = respell(&f.witness);
    }
    for w in &mut c.output_check.unknown_reasons {
        *w = respell(w);
    }
    // The report: side A in template terms, side B in the restored output.
    let r = &mut c.report;
    remap_to_template(r, Side::A, t, &frag, from, EXCERPT);
    for f in &mut r.findings {
        if let Some(loc) = f.b.as_mut() {
            let span = (shift(loc.byte_start), shift(loc.byte_end));
            *loc = location_in(Side::B, &loc.path, &out, &lo, span, EXCERPT);
        }
        f.witness = respell(&f.witness);
    }
    for w in &mut r.unknown_reasons {
        *w = respell(w);
    }
    template_notes(r, Side::A, t, "input.sql", from, basis, delimiters, EXCERPT);
    for p in problems {
        r.unknown_reasons.push(format!("converted template, {p}"));
    }
    r.verdict = sqldiff_core::session::derive_verdict(r);

    c.output = out;
    c.template = Some(TemplateInfo {
        parameters: names,
        unresolved: t.unresolved().map(|s| t.slice(s).to_string()).collect(),
        special_forms: specials.into_iter().map(|(_, _, f)| f).collect(),
    });
    recount(&mut c);
    c
}

/// Cuts a harnessed conversion down to the fragment: the output to `mid`,
/// the units and findings to those about the fragment (`frag` in the
/// harnessed input). A finding attached to a unit that stays, stays, even
/// if it points into the stand-in, so nothing that gated a unit is lost.
/// The white space at the output's edges becomes `edges`, the template's.
fn strip(c: &mut Conversion, mid: (usize, usize), frag: (usize, usize), edges: (&str, &str)) {
    let inner = &c.output[mid.0..mid.1];
    let b = mid.0 + (inner.len() - inner.trim_start().len());
    let e = (mid.1 - (inner.len() - inner.trim_end().len())).max(b);
    let moved = |off: usize| off.clamp(b, e) - b + edges.0.len();
    let hits = |s: usize, t: usize, r: (usize, usize)| s < r.1 && r.0 < t.max(s + 1);
    let keep: Vec<bool> = c
        .units
        .iter()
        .map(|u| hits(u.out.start, u.out.end, mid) || hits(u.src.start, u.src.end, frag))
        .collect();
    let mut attached = std::collections::BTreeSet::new();
    let mut attached_own = std::collections::BTreeSet::new();
    for (u, k) in c.units.iter().zip(&keep) {
        if *k {
            attached.extend(u.findings.iter().copied());
            attached_own.extend(u.output_findings.iter().copied());
        }
    }
    let clamp = |loc: &mut sqldiff_core::model::Location| {
        loc.byte_start = moved(loc.byte_start);
        loc.byte_end = moved(loc.byte_end);
    };
    // The engine's diff.
    let mut new_index = vec![None; c.report.findings.len()];
    let mut findings = Vec::new();
    for (i, mut f) in std::mem::take(&mut c.report.findings)
        .into_iter()
        .enumerate()
    {
        let in_frag =
            f.a.as_ref()
                .is_some_and(|l| hits(l.byte_start, l.byte_end, frag));
        let in_mid =
            f.b.as_ref()
                .is_some_and(|l| hits(l.byte_start, l.byte_end, mid));
        if !(attached.contains(&i) || in_frag || in_mid) {
            continue;
        }
        if let Some(l) = f.b.as_mut() {
            clamp(l);
        }
        new_index[i] = Some(findings.len());
        findings.push(f);
    }
    c.report.findings = findings;
    // The output's own check.
    let mut new_own = vec![None; c.output_check.findings.len()];
    let mut own = Vec::new();
    for (i, mut f) in std::mem::take(&mut c.output_check.findings)
        .into_iter()
        .enumerate()
    {
        let located = f.a.as_ref().map(|l| hits(l.byte_start, l.byte_end, mid));
        if !(attached_own.contains(&i) || located != Some(false)) {
            continue;
        }
        if let Some(l) = f.a.as_mut() {
            clamp(l);
        }
        new_own[i] = Some(own.len());
        own.push(f);
    }
    c.output_check.findings = own;
    c.unplaced = c.unplaced.iter().filter_map(|&i| new_own[i]).collect();
    let units = std::mem::take(&mut c.units);
    c.units = units
        .into_iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(mut u, _)| {
            u.out.start = moved(u.out.start);
            u.out.end = moved(u.out.end);
            u.findings = u.findings.iter().filter_map(|&i| new_index[i]).collect();
            u.output_findings = u
                .output_findings
                .iter()
                .filter_map(|&i| new_own[i])
                .collect();
            u
        })
        .collect();
    c.output = format!("{}{}{}", edges.0, &c.output[b..e], edges.1);
}

/// `text` with stand-ins spelt as the template writes them.
fn respell(
    text: &str,
    t: &Template,
    stem: &str,
    written: &[String],
    specials: &[(usize, usize, String)],
) -> String {
    let re = Regex::new(&format!(r"(?i-u)/\*{stem}s(\d+)z\*/|{stem}(\d+)z"))
        .expect("stem is alphanumeric");
    re.replace_all(text, |m: &regex::Captures| {
        let pick = |g: usize| m.get(g).and_then(|x| x.as_str().parse::<usize>().ok());
        match (pick(1), pick(2)) {
            (Some(k), _) => specials.get(k).map(|(s, e, _)| t.text[*s..*e].to_string()),
            (_, Some(i)) => written.get(i).filter(|w| !w.is_empty()).cloned(),
            _ => None,
        }
        .unwrap_or_else(|| m[0].to_string())
    })
    .replace(&format!("{stem}hs.{stem}h."), "")
    .replace(&format!("{stem}hs.{stem}h"), "the woven statement's table")
}

/// A fragment whose conversion reached across its edges: the template is
/// kept as it is, one unit that differs, and nothing else is claimed.
fn leaked(
    t: &Template,
    mut c: Conversion,
    kind: FragmentKind,
    stem: &str,
    written: &[String],
    specials: &[(usize, usize, String)],
) -> Conversion {
    // The stand-in may not have converted at all; then say why.
    let mut inner: Vec<String> = Vec::new();
    for u in c.units.iter().filter(|u| u.status > Status::Assumptions) {
        for n in u.notes.iter().filter(|n| n.status > Status::Assumptions) {
            if !inner.contains(&n.text) {
                inner.push(n.text.clone());
            }
        }
    }
    let mut why = format!(
        "Converting this {} changed the stand-in statement around it (a declaration hoisted, a parameter added, code moved across its edge, or a rule that did not apply), so it cannot be converted on its own: convert it by hand and check the woven instances.",
        kind.label()
    );
    if !inner.is_empty() {
        why.push_str(&format!(" The conversion said: {}", inner.join(" ")));
    }
    let why = respell(&why, t, stem, written, specials);
    let li = LineIndex::new(&t.text);
    let all = span(&t.text, &li, &U16Index::new(&t.text), (0, t.text.len()));
    c.units = vec![crate::Unit {
        status: Status::Differs,
        notes: vec![Note {
            status: Status::Differs,
            rule: "template.context-leak".into(),
            text: why.clone(),
        }],
        src: all,
        out: all,
        findings: Vec::new(),
        output_findings: Vec::new(),
        checked: Status::Differs,
        written: None,
    }];
    c.report.findings.clear();
    c.report
        .unknown_reasons
        .push(format!("converted template: {why}"));
    c.report.verdict = sqldiff_core::session::derive_verdict(&c.report);
    c.output_check.findings.clear();
    c.unplaced.clear();
    c.output = t.text.clone();
    c.template = Some(TemplateInfo {
        parameters: t.parameters(),
        unresolved: t.unresolved().map(|s| t.slice(s).to_string()).collect(),
        special_forms: specials.iter().map(|(_, _, f)| f.clone()).collect(),
    });
    recount(&mut c);
    c
}

/// Splits the conversion of a routine's opening and closing templates,
/// converted together as `head`, a line holding only `marker` (a comment
/// that occurs nowhere else), and `foot`, into one conversion per template.
/// A unit that reaches across the marker, or a marker that did not come
/// through exactly once, leaves both templates unconverted and differing.
pub fn split_pair(c: Conversion, head: &str, foot: &str, marker: &str) -> (Conversion, Conversion) {
    let foot_in = head.len() + 1 + marker.len() + 1;
    let found = (c.output.matches(marker).count() == 1)
        .then(|| c.output.find(marker))
        .flatten();
    let crosses = |s: usize, e: usize, cut: (usize, usize)| s < cut.0 && e > cut.1;
    let cut = found.map(|m| {
        let line = c.output[..m].rfind('\n').map_or(0, |i| i + 1);
        let next = c.output[m..]
            .find('\n')
            .map_or(c.output.len(), |i| m + i + 1);
        (line, next)
    });
    let ok = cut.is_some_and(|(line, next)| {
        !c.units.iter().any(|u| {
            crosses(u.src.start, u.src.end, (head.len(), foot_in))
                || crosses(u.out.start, u.out.end, (line, next))
        })
    });
    let part = |text: &str, out: String, units: Vec<crate::Unit>| {
        let mut p = c.clone();
        p.output = out;
        let (li, lo) = (LineIndex::new(text), LineIndex::new(&p.output));
        let (ui, uo) = (U16Index::new(text), U16Index::new(&p.output));
        p.units = units
            .into_iter()
            .map(|mut u| {
                u.src = span(text, &li, &ui, (u.src.start, u.src.end));
                u.out = span(&p.output, &lo, &uo, (u.out.start, u.out.end));
                u
            })
            .collect();
        recount(&mut p);
        p
    };
    let Some((line, next)) = cut.filter(|_| ok) else {
        let broken = |text: &str| {
            let li = LineIndex::new(text);
            let all = span(text, &li, &U16Index::new(text), (0, text.len()));
            let mut p = c.clone();
            p.output = text.to_string();
            p.units = vec![crate::Unit {
                status: Status::Differs,
                notes: vec![Note {
                    status: Status::Differs,
                    rule: "template.pair-split".into(),
                    text: "This routine's opening and closing templates were converted together, and the conversion moved code across the line between them, so neither can be converted on its own: convert them by hand and check the woven instances.".into(),
                }],
                src: all,
                out: all,
                findings: Vec::new(),
                output_findings: Vec::new(),
                checked: Status::Differs,
                written: None,
            }];
            recount(&mut p);
            p
        };
        return (broken(head), broken(foot));
    };
    let (mut h, mut f) = (Vec::new(), Vec::new());
    for mut u in c.units.clone() {
        if u.src.start < head.len() {
            u.src.end = u.src.end.min(head.len());
            u.out.end = u.out.end.min(line);
            h.push(u);
        } else {
            u.src.start = u.src.start.max(foot_in) - foot_in;
            u.src.end = u.src.end.max(foot_in) - foot_in;
            u.out.start = u.out.start.max(next) - next;
            u.out.end = u.out.end.max(next) - next;
            f.push(u);
        }
    }
    let head_out = c.output[..line].to_string();
    let foot_out = c.output[next..].to_string();
    (part(head, head_out, h), part(foot, foot_out, f))
}
