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

use crate::{check, recount, rewrite_in, span, Conversion, Draft, Note, Status};
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
        StandIns { stem, names }
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
    let (frag, written, specials) = fragment(t, stand);
    let draft = rewrite_in(&frag.text, from, to, source);
    TemplateDraft {
        frag,
        draft,
        written,
        specials,
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
    } = d;
    let (from, to) = (draft.from, draft.to);
    let stem = &stand.stem;
    let names = t.parameters();
    let mut c = check(&frag.text, draft, opts);

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
    let out_lower = out.to_ascii_lowercase();
    let placeholder_spans: Vec<(usize, usize, &str)> = t
        .segments
        .iter()
        .filter_map(|s| match &s.kind {
            SegmentKind::Placeholder { .. } => Some((s.start, s.end, t.slice(s))),
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
        u.src = span(&t.text, &li, (ss, se));
        let (os, oe) = (shift(u.out.start), shift(u.out.end));
        u.out = span(&out, &lo, (os, oe.max(os)));
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
        let mut used: Vec<&str> = Vec::new();
        for (s, e, p) in &placeholder_spans {
            if *s < se && ss < *e && !used.contains(p) {
                used.push(p);
            }
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
    let respell = |text: &str| -> String {
        re.replace_all(text, |m: &regex::Captures| {
            let pick = |g: usize| m.get(g).and_then(|x| x.as_str().parse::<usize>().ok());
            match (pick(1), pick(2)) {
                (Some(k), _) => specials.get(k).map(|(s, e, _)| t.text[*s..*e].to_string()),
                (_, Some(i)) => written.get(i).filter(|w| !w.is_empty()).cloned(),
                _ => None,
            }
            .unwrap_or_else(|| m[0].to_string())
        })
        .into_owned()
    };
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
