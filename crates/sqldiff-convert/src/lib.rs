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

// Determinism guard (perf plan W0): iterating a hash map or set in a `for` loop is
// as order-dependent as `.iter()`; each site needs a reasoned `#[allow]`.
#![warn(clippy::iter_over_hash_type)]

use serde::{Deserialize, Serialize};
pub use shared::Shared;
use sqldiff_core::{Artifact, Dialect, Options, Report};
use sqldiff_syntax::{parse, LineIndex};
use sqldiff_units::kept_notes;
pub use sqldiff_units::{trace_text, Draft, Note, RuleOptions, Status, Step, Written};

include!(concat!(env!("OUT_DIR"), "/fingerprint.rs"));

pub mod check;
pub use check::{check, gates, splice_unit, undecided, NOT_CHECKED, NOT_COMPARED};
pub(crate) use check::{
    check_with_reads, not_checked, recount, span, Checked, Rebuild, U16Index, EVERY_PAIR,
};

pub mod base;
pub mod callees;
pub mod cert;
pub mod compare;
pub mod drift;
pub mod gate;
use sqldiff_draft::rewrite_in_tree;
use sqldiff_draft::rewrite_in_tree_plain;
pub use sqldiff_draft::{rewrite, rewrite_in, rewrite_with};
pub use sqldiff_units::impossible;
pub mod instances;
pub mod job;
pub mod mem;
pub mod names;
pub mod plan;
pub mod queue;
mod refused;
pub mod review;
pub use sqldiff_units::shared;
pub mod store;

mod template;

pub mod tree;

pub use template::{
    check_template, convert_template, job_material, not_checked_template, rewrite_template,
    rewrite_template_as, rewrite_template_with, split_pair, template_fragment, FragmentKind,
    StandIns, TemplateDraft, TemplateInfo, ValueKind,
};

/// Drops what is read of neither engine report of `c` once its file is
/// checked and its units have their statuses: of each report only the
/// findings (units point into them) and the assumptions (the deployment
/// report reads them) stay. For a review (`tree::convert_for_review`).
pub fn trim_reports(c: &mut Conversion) {
    for r in [&mut c.report, &mut c.output_check] {
        r.witnesses = Vec::new();
        r.diagnostics = Vec::new();
        r.normalizations = Vec::new();
        r.unknown_reasons = Vec::new();
        r.discharged = Vec::new();
        r.suppressed = Vec::new();
        r.refinements = Vec::new();
        r.proof = None;
    }
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

impl Unit {
    /// The SQL Server-only feature the unit's refusal names, if it is one:
    /// the unit is still not converted ([`impossible`]).
    pub fn impossible(&self) -> Option<impossible::Feature> {
        if self.status == Status::NotConverted {
            impossible::of_note(&self.notes)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Counts {
    pub converted: usize,
    pub assumptions: usize,
    pub decision: usize,
    pub unproven: usize,
    pub differs: usize,
    /// Every not converted unit. Of them, `impossible` are marked as
    /// impossible to convert automatically (`impossible`): a count within
    /// `not_converted`, never beside it, so no file or total reads
    /// cleaner for it.
    pub not_converted: usize,
    #[serde(default)]
    pub impossible: usize,
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

/// What the cross-file passes (`callees::resolve`, `base::Base::flag`) read
/// and change of a checked file: its units, their counts, and whether
/// something in it names no unit (`unplaced`, `unproven`), which keeps the
/// file from counting as converted. A pass that works on this alone does
/// not need the file's output or engine reports, so those can stay in the
/// result store while the passes run.
#[derive(Debug, Clone, Default)]
pub struct Core {
    pub units: Vec<Unit>,
    pub counts: Counts,
    /// `unplaced` or `unproven` of the conversion is not empty.
    pub blocked: bool,
}

impl Core {
    /// Moves the units of `c` out into a core; [`Core::restore`] puts them back.
    pub fn take(c: &mut Conversion) -> Core {
        Core {
            units: std::mem::take(&mut c.units),
            counts: c.counts.clone(),
            blocked: !c.unproven.is_empty() || !c.unplaced.is_empty(),
        }
    }

    /// Puts the units and counts back into the conversion they came from.
    pub fn restore(self, c: &mut Conversion) {
        c.units = self.units;
        c.counts = self.counts;
    }
}

/// A checked file as the cross-file passes see it: a [`Conversion`], or
/// just its [`Core`].
pub trait Cored {
    fn units(&self) -> &[Unit];
    fn units_mut(&mut self) -> &mut Vec<Unit>;
    /// Something in the file names no unit; see [`Core::blocked`].
    fn blocked(&self) -> bool;
    fn set_counts(&mut self, counts: Counts);
}

impl Cored for Conversion {
    fn units(&self) -> &[Unit] {
        &self.units
    }
    fn units_mut(&mut self) -> &mut Vec<Unit> {
        &mut self.units
    }
    fn blocked(&self) -> bool {
        !self.unproven.is_empty() || !self.unplaced.is_empty()
    }
    fn set_counts(&mut self, counts: Counts) {
        self.counts = counts;
    }
}

impl Cored for Core {
    fn units(&self) -> &[Unit] {
        &self.units
    }
    fn units_mut(&mut self) -> &mut Vec<Unit> {
        &mut self.units
    }
    fn blocked(&self) -> bool {
        self.blocked
    }
    fn set_counts(&mut self, counts: Counts) {
        self.counts = counts;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_core_carries_the_units_out_and_back_and_recounts_like_the_conversion() {
        let text = "CREATE TABLE t (a int NOT NULL)\nGO\nSELECT TOP 1 a FROM t\nGO\n";
        let mut c = convert(text, Dialect::Tsql, Dialect::Postgres).unwrap();
        let before = serde_json::to_string(&c).unwrap();
        let mut core = Core::take(&mut c);
        assert!(c.units.is_empty() && !core.units.is_empty());
        assert_eq!(
            core.blocked,
            c.blocked(),
            "blocked is read before the units leave"
        );
        // A change through the core is counted like one on the conversion.
        core.units[0].status = Status::NotConverted;
        recount(&mut core);
        let mut direct = convert(text, Dialect::Tsql, Dialect::Postgres).unwrap();
        direct.units[0].status = Status::NotConverted;
        recount(&mut direct);
        core.restore(&mut c);
        assert_eq!(
            serde_json::to_string(&c).unwrap(),
            serde_json::to_string(&direct).unwrap()
        );
        assert_ne!(serde_json::to_string(&c).unwrap(), before);
    }
}

#[cfg(test)]
mod per_routine_variant {
    use super::*;
    use crate::check::{check_with, score_of};

    const TABLES: &str = "CREATE TABLE dbo.A (Id int NOT NULL PRIMARY KEY, Qty int NOT NULL);
CREATE TABLE dbo.B (Id int NOT NULL PRIMARY KEY, Qty int NOT NULL);
";
    const TWO: &str = "CREATE PROCEDURE dbo.First @Id int AS
BEGIN
    INSERT dbo.A (Id, Qty) VALUES (@Id, 1);
    UPDATE dbo.B SET Qty = Qty + 1 WHERE Id = @Id;
END;
GO
CREATE PROCEDURE dbo.Second @Id int AS
BEGIN
    INSERT dbo.B (Id, Qty) VALUES (@Id, 1);
    UPDATE dbo.A SET Qty = Qty + 1 WHERE Id = @Id;
END;
";

    fn draft(plain_at: &[usize]) -> Draft {
        let mut cat = sqldiff_core::catalog::Catalog::default();
        for f in [TABLES, TWO] {
            let root = parse(f, Dialect::Tsql).syntax();
            sqldiff_core::catalog::ddl::load_tree(&mut cat, &root, Dialect::Tsql);
        }
        rewrite_in_tree_plain(
            TWO,
            Dialect::Tsql,
            Dialect::Postgres,
            Some(&cat),
            None,
            RuleOptions::default(),
            plain_at,
        )
    }

    fn blocks(d: &Draft) -> usize {
        d.output().matches("EXCEPTION WHEN").count()
    }

    #[test]
    fn a_routine_can_be_written_without_blocks_on_its_own() {
        let both = draft(&[]);
        let second = TWO.find("CREATE PROCEDURE dbo.Second").unwrap();
        let only_first = draft(&[second]);
        let neither = draft(&[0, second]);
        assert_eq!(blocks(&both), 4);
        assert_eq!(blocks(&only_first), 2);
        assert_eq!(blocks(&neither), 0);
        assert_eq!(both.plain.as_ref().map(|p| blocks(p)), Some(0));
        // The mixed draft keeps no second variant.
        assert!(only_first.plain.is_none());
    }

    #[test]
    fn a_mix_is_kept_only_when_it_scores_better() {
        let opts = Options {
            catalog_a: vec![Artifact::new("t.sql", Dialect::Tsql, TABLES)],
            catalog_b: vec![Artifact::new(
                "t.sql",
                Dialect::Postgres,
                rewrite_in(TABLES, Dialect::Tsql, Dialect::Postgres, None).output(),
            )],
            ..Options::default()
        };
        let second = TWO.find("CREATE PROCEDURE dbo.Second").unwrap();
        let rebuild = |at: &[usize]| draft(at);
        let with = check_with(TWO, draft(&[]), &opts, Some(&rebuild));
        let without = check(TWO, draft(&[]), &opts);
        // Whatever the engine says of the mix, the result is never worse
        // than the whole-file choice, and equal to it when not better.
        let score = |c: &Conversion| score_of(c.units.iter());
        assert!(score(&with) <= score(&without));
        if score(&with) == score(&without) {
            assert_eq!(with.output, without.output);
        }
        let _ = second;
    }
}
