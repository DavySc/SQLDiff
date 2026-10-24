//! Converting a template: SQL with a generator's placeholders and special
//! forms (`sqldiff_syntax::template`).
//!
//! The template's fragment (placeholders as `TPL_<NAME>` identifiers,
//! special forms as block comments) is converted and checked like any
//! input; then the placeholders and special forms are put back into the
//! output as the template spells them, so the result is a template for the
//! target dialect. Spans map back to the template. A placeholder the rules
//! renamed beyond recognition is reported on its unit, never guessed back.

use regex::Regex;
use serde::Serialize;
use sqldiff_core::Dialect;
use sqldiff_syntax::template::{stand_in, SegmentKind, Template};
use sqldiff_syntax::LineIndex;

use crate::tok::snake;
use crate::{convert, recount, span, Conversion, Note, Status};

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

/// Convert a template from one dialect to another; the output keeps the
/// template's placeholders and special forms.
pub fn convert_template(t: &Template, from: Dialect, to: Dialect) -> Result<Conversion, String> {
    let frag = t.fragment();
    let mut c = convert(&frag.text, from, to)?;
    let info = TemplateInfo {
        parameters: t.parameters(),
        unresolved: t.unresolved().map(|s| t.slice(s).to_string()).collect(),
        special_forms: t
            .segments
            .iter()
            .filter_map(|s| match &s.kind {
                SegmentKind::Special { form, .. } => Some(form.clone()),
                _ => None,
            })
            .collect(),
    };

    // Placeholders: the stand-in as written, or as the rules spelled a name
    // built from it (`p_tpl_name` for `@[NAME]`), back to the template text.
    let mut written: Vec<(String, String)> = Vec::new();
    for s in &t.segments {
        if let SegmentKind::Placeholder { name } = &s.kind {
            if !written.iter().any(|(n, _)| n == name) {
                written.push((name.clone(), t.slice(s).to_string()));
            }
        }
    }
    let mut swaps: Vec<Swap> = Vec::new();
    if !written.is_empty() {
        let mut alts: Vec<(String, usize)> = Vec::new();
        for (i, (name, _)) in written.iter().enumerate() {
            let a = stand_in(name);
            let b = snake(&a);
            alts.push((a.to_ascii_lowercase(), i));
            if b != a.to_ascii_lowercase() {
                alts.push((b, i));
            }
        }
        // Longest first, so TPL_AB is not read as TPL_A followed by B.
        alts.sort_by_key(|(a, _)| std::cmp::Reverse(a.len()));
        let re = Regex::new(&format!(
            "(?i){}",
            alts.iter()
                .map(|(a, _)| regex::escape(a))
                .collect::<Vec<_>>()
                .join("|")
        ))
        .expect("escaped alternatives");
        for m in re.find_iter(&c.output) {
            let low = m.as_str().to_ascii_lowercase();
            let i = alts.iter().find(|(a, _)| *a == low).map_or(0, |x| x.1);
            swaps.push(Swap {
                old: (m.start(), m.end()),
                new: written[i].1.clone(),
            });
        }
    }
    // Special forms: the k-th `/*form*/` of the output is the k-th of the template.
    let mut forms: Vec<(&str, Vec<&str>)> = Vec::new();
    for s in &t.segments {
        if let SegmentKind::Special { form, .. } = &s.kind {
            match forms.iter_mut().find(|(f, _)| f == form) {
                Some((_, v)) => v.push(t.slice(s)),
                None => forms.push((form, vec![t.slice(s)])),
            }
        }
    }
    let mut lost_forms = Vec::new();
    for (form, texts) in &forms {
        let comment = format!("/*{}*/", form.replace("*/", ""));
        let found: Vec<usize> = c.output.match_indices(&comment).map(|(i, _)| i).collect();
        if found.len() != texts.len() {
            lost_forms.push(form.to_string());
            continue;
        }
        for (at, text) in found.into_iter().zip(texts) {
            swaps.push(Swap {
                old: (at, at + comment.len()),
                new: text.to_string(),
            });
        }
    }
    swaps.sort_by_key(|s| s.old.0);

    let old = std::mem::take(&mut c.output);
    let mut out = String::with_capacity(old.len());
    let mut at = 0;
    for s in &swaps {
        out.push_str(&old[at..s.old.0]);
        out.push_str(&s.new);
        at = s.old.1;
    }
    out.push_str(&old[at..]);
    // Old output offset → new: inside a swap its start (end at its end).
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
    let leftover = Regex::new("(?i)tpl_").expect("literal");
    let placeholder_spans: Vec<(usize, usize, &str)> = t
        .segments
        .iter()
        .filter_map(|s| match &s.kind {
            SegmentKind::Placeholder { .. } => Some((s.start, s.end, t.slice(s))),
            _ => None,
        })
        .collect();
    for u in &mut c.units {
        let (ss, se) = frag.map.src_range(u.src.start, u.src.end);
        u.src = span(&t.text, &li, (ss, se));
        let (os, oe) = (shift(u.out.start), shift(u.out.end));
        u.out = span(&out, &lo, (os, oe.max(os)));
        let used: Vec<&str> = placeholder_spans
            .iter()
            .filter(|(s, e, _)| *s < se && ss < *e)
            .map(|x| x.2)
            .collect();
        if !used.is_empty() && u.status != Status::NotConverted {
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
        if u.status != Status::NotConverted && leftover.is_match(&out[u.out.start..u.out.end]) {
            u.notes.push(Note {
                status: Status::Differs,
                rule: "template.placeholder-lost".into(),
                text: "A placeholder was renamed by the conversion and could not be put back; check this unit by hand.".into(),
            });
        }
        if let Some(n) = u.notes.iter().map(|n| n.status).max() {
            u.status = u.status.max(n);
        }
        u.notes.sort_by_key(|n| std::cmp::Reverse(n.status));
    }
    if !lost_forms.is_empty() {
        if let Some(u) = c.units.first_mut() {
            u.status = u.status.max(Status::Differs);
            u.notes.insert(
                0,
                Note {
                    status: Status::Differs,
                    rule: "template.special-form-lost".into(),
                    text: format!(
                        "Generator directives ({}) did not survive the conversion in order; put them back by hand.",
                        lost_forms.join(", ")
                    ),
                },
            );
        }
    }
    c.output = out;
    c.template = Some(info);
    recount(&mut c);
    Ok(c)
}
