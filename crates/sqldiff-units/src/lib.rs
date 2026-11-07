//! The data a conversion's first pass hands to the second: a [`Draft`] (the
//! rules' output with its units), the [`Note`]s and [`Step`]s on them, their
//! [`Status`], and who wrote a unit by hand ([`Written`]).
//!
//! Both stages of a conversion read these types, so they live in a crate of
//! their own that every stage's fingerprint covers (`docs/crate-split.md`).
//! The result key of a stored check is built from the `Debug` text of a
//! [`Draft`] (`Draft::canonical`): no type reachable from it may have a
//! `Debug` that leaves a field out (a test scans for hand-written ones).

// Determinism guard (perf plan W0): iterating a hash map or set in a `for` loop is
// as order-dependent as `.iter()`; each site needs a reasoned `#[allow]`.
#![warn(clippy::iter_over_hash_type)]

use serde::{Deserialize, Serialize};
use sqldiff_syntax::Dialect;

pub mod ids;
pub mod out;
pub mod shared;

pub use shared::Shared;

/// Who made a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum By {
    Human,
    Agent,
}

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
    /// Stable rule id, e.g. `ASM-CI-COLLATION` or `tsql.merge`. Both are
    /// shared with every equal note of the tree (`shared::Pool`).
    pub rule: Shared,
    pub text: Shared,
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

/// Who wrote a unit's output by hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Written {
    pub by: By,
    pub author: String,
    /// The unit's id as the rules wrote it, which the ledger record names.
    pub origin: String,
}

/// Choices that change what the rules write, every one off by default and
/// set by the person running the conversion. They are part of a [`Draft`],
/// so the key of a stored result (`store::CheckInput`) differs when one is
/// switched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleOptions {
    /// T-SQL → PostgreSQL: a system-versioned table whose period columns
    /// are both HIDDEN becomes a plain table without the period columns,
    /// history table and triggers, under `ASM-TEMPORAL-HISTORYLESS`. History
    /// support for it is future work. Tables with visible period columns are
    /// converted with their history as ever.
    pub temporal_historyless: bool,
}

/// A dialect as its stable id, for the stored tree state (`store::TreeState`).
mod dialect_id {
    use serde::{Deserialize, Deserializer, Serializer};
    use sqldiff_syntax::Dialect;

    pub fn serialize<S: Serializer>(d: &Dialect, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(d.id())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Dialect, D::Error> {
        let id = String::deserialize(d)?;
        id.parse()
            .map_err(|_| serde::de::Error::custom(format!("unknown dialect {id}")))
    }
}

/// The rules' output before it is checked. Lets a caller convert many files
/// first and then check each one against catalogs built from all of them.
#[derive(Debug, Serialize, Deserialize)]
pub struct Draft {
    #[serde(with = "dialect_id")]
    pub from: Dialect,
    #[serde(with = "dialect_id")]
    pub to: Dialect,
    /// The options the rules ran with.
    pub options: RuleOptions,
    pub raw: out::Out,
    /// The conversion without the per-statement exception blocks
    /// (`tsql.txn.continue`), when the draft has them: [`check`] keeps the
    /// one the engine proves better, so the blocks never make a unit worse.
    pub plain: Option<Box<Draft>>,
}

impl Draft {
    pub fn output(&self) -> &str {
        &self.raw.text
    }

    /// Whether every unit's source range lies in `input` and every output
    /// range in the draft's text, on character boundaries, in the draft and
    /// in its variant without exception blocks. A draft read back from a
    /// store that fails this was damaged: using it would index outside its
    /// text.
    pub fn well_formed(&self, input: &str) -> bool {
        let ok = |t: &str, (s, e): (usize, usize)| {
            s <= e && e <= t.len() && t.is_char_boundary(s) && t.is_char_boundary(e)
        };
        self.raw
            .units
            .iter()
            .all(|u| ok(input, u.src) && ok(&self.raw.text, u.out))
            && self.plain.as_ref().map_or(true, |p| p.well_formed(input))
    }

    /// The options the rules ran with.
    pub fn options(&self) -> RuleOptions {
        self.options
    }

    /// Every field, as text, for the key of a stored result
    /// (`store::CheckInput`): its text, its units with their notes, traces
    /// and hand-written marks, and the variant without exception blocks.
    pub fn canonical(&self) -> String {
        format!("{self:?}")
    }

    /// Unit ids as `review::unit_ids` gives them for the checked
    /// conversion: the same while no unit was replaced.
    pub fn unit_ids(&self, path: &str, input: &str) -> Vec<String> {
        ids::ids_of(
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
    pub fn worst_note(&self, at_least: Status) -> Option<String> {
        self.raw
            .units
            .iter()
            .flat_map(|u| &u.notes)
            .filter(|n| n.status >= at_least)
            .max_by_key(|n| n.status)
            .map(|n| n.text.to_string())
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

/// A hand-written unit's notes: `note`, then the rules' assumptions and
/// decisions from `old`.
pub fn kept_notes(note: &Note, old: &[Note]) -> Vec<Note> {
    let mut v = vec![note.clone()];
    v.extend(
        old.iter()
            .filter(|n| matches!(n.status, Status::Assumptions | Status::Decision))
            .cloned(),
    );
    v
}
