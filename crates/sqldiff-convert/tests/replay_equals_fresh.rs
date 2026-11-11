//! T1 and T2 of the recorded-reads design (W4.3): after a catalog edit, a
//! resume on the old store gives byte-for-byte the tree and report that
//! `--fresh` gives, and the report says how many files it reused.
//!
//! Today a result's key holds the digest of both whole catalogs, so an edit
//! that changes a catalog reuses nothing and this test passes without
//! reusing; it is the gate the replay of recorded reads (PR 3b) has to pass
//! while reusing. The edits come from `support::edits`, a generator that
//! knows nothing about any corpus; every failure prints the seed and the
//! edit.

mod support;

use std::collections::BTreeMap;

use sqldiff_core::Dialect;
use support::edits::{Edit, Model, Rng, ALL_EDITS};
use support::harness::{one, Fixture};

/// The seeds the test runs; a failure names the one that broke.
const SEEDS: &[u64] = &[7, 21];

/// The most files an edit may make a resume check again: the files that read
/// what it changed, as measured (the harness runs without the sampled
/// re-check, so nothing is added for it). A file that reads nothing the edit touched is taken from the
/// store, so a bound that is too low means a read is recorded too widely, and
/// the test failing on a bound too high is not what it is for: lowering a
/// bound is how a better recording shows.
///
/// The anchor tables are read by about twenty dependents each (a dependent
/// names a table, or a column of it, directly), hence the wide table edits;
/// `VersioningOn` makes a HIDDEN column appear, which every file that
/// expands a star reads; the narrow edits (a view, a routine, a trigger,
/// a schema, the base catalog) check a handful.
fn recheck_at_most(e: &Edit) -> usize {
    match e {
        Edit::NoOpComment | Edit::NoOpReorderTables => 0,
        Edit::AddTable => 6,
        Edit::DropTable => 27,
        Edit::RenameTable
        | Edit::AddColumn
        | Edit::AddMixedCaseColumn
        | Edit::DropColumn
        | Edit::RetypeColumn
        | Edit::ReorderColumns
        | Edit::FlipNullability
        | Edit::ChangeDefault
        | Edit::ToggleIndex => 28,
        Edit::AddRoutine | Edit::DropRoutine | Edit::AddTrigger => 6,
        Edit::ViewBody => 8,
        Edit::ChangeParameterType | Edit::AddSchemaTable => 7,
        Edit::BaseDrop => 8,
        Edit::DropTrigger => 4,
        Edit::VersioningOn
        | Edit::DropAllTables
        | Edit::NoProcedures
        | Edit::DropFunctions
        | Edit::CaseClash
        | Edit::CaseClashIndex
        | Edit::CaseClashColumn
        | Edit::SourceLookalikes
        | Edit::SourceOverrides
        | Edit::TargetOverrides
        | Edit::TargetLookalikes
        | Edit::ViewLookalike
        | Edit::NoBaseA
        | Edit::NoBaseB
        | Edit::RetypeSide
        | Edit::NoCatalog
        | Edit::VersioningToggle
        | Edit::HandWritten => usize::MAX / 2,
        Edit::AddBitColumn | Edit::RetypeSink | Edit::AddStatusRoutine => usize::MAX / 2,
        Edit::BaseTriggerEvent | Edit::TargetBaseTriggerEvent => 5,
        Edit::BaseRetype => 6,
        Edit::BaseTable => 5,
        Edit::SplitBase => 13,
    }
}

/// The most drafts an edit may make a resume build again (P7 of the Phase B
/// review): a draft is taken from its stored entry unless its text changed
/// or something it asked of the source catalog or the claims has another
/// answer. As measured on both dialects and both seeds.
///
/// Pass-1 reads are recorded narrow for the table lookups (`Table(parts)`),
/// the callee lookup (`ProcedureNamed`) and the type set (`ColumnTypes`);
/// the sites with no killing generated edit yet (`table_wide`,
/// `column_owners`, the `temporal.rs` lookups, `tables`) still record `WholeCatalog` and rebuild
/// on any edit, but this generator's trees do not reach them. Lowering a
/// bound is how a better recording shows.
fn drafts_at_most(e: &Edit) -> usize {
    match e {
        Edit::NoOpReorderTables
        | Edit::CaseClash
        | Edit::CaseClashIndex
        | Edit::CaseClashColumn
        | Edit::TargetOverrides
        | Edit::TargetLookalikes
        | Edit::NoBaseB
        | Edit::TargetBaseTriggerEvent
        | Edit::HandWritten => 0,
        Edit::NoOpComment => 1,
        // A column edit that keeps the set of column types changes the
        // read of the table it edits and of the drafts that name it. As
        // measured: 19 to 22 on PostgreSQL, 8 to 9 on Db2 for i, of about 93
        // files.
        Edit::AddColumn
        | Edit::AddMixedCaseColumn
        | Edit::ReorderColumns
        | Edit::FlipNullability
        | Edit::ChangeDefault
        | Edit::ToggleIndex => 24,
        _ => usize::MAX / 2,
    }
}

fn every_edit(to: Dialect) {
    let mut lines = Vec::new();
    // Per edit, whether some seed showed it on a dependent (the probe).
    let mut shown: BTreeMap<String, bool> = BTreeMap::new();
    for e in ALL_EDITS.iter().filter(|e| only(e)) {
        for &seed in SEEDS {
            let o = one(to, seed, e);
            let (reused, total) = o.reused;
            if e.is_no_op() {
                // Only the edited file is checked again.
                assert!(
                    reused + 1 >= total,
                    "{to:?} seed {seed} {e:?}: only {reused} of {total} reused"
                );
            } else {
                // (Soundness is the comparison with the fresh run in `one`. A
                // file can be reused and still change: the cross-file passes,
                // such as a call's status following the callee's, run on
                // reused results too.)
                // T2, reuse side: only the files that read what the edit
                // changed are checked again (the bound is what a run
                // measured).
                let at_most = recheck_at_most(e);
                assert!(
                    o.again.len() <= at_most,
                    "{to:?} seed {seed} {e:?}: {} files checked again, at most {at_most} expected: {:?}",
                    o.again.len(),
                    o.again
                );
                let drafts_max = drafts_at_most(e);
                assert!(
                    o.drafts_built.0 <= drafts_max,
                    "{to:?} seed {seed} {e:?}: {} drafts built again, at most {drafts_max} expected",
                    o.drafts_built.0
                );
                *shown.entry(format!("{e:?}")).or_default() |= !o.dependents_changed.is_empty();
            }
            lines.push(format!(
                "{to:?} seed {seed} {e:?}: reused {reused} of {total}, {} dependents change, \
                 {} of {} drafts built",
                o.dependents_changed.len(),
                o.drafts_built.0,
                o.drafts_built.1
            ));
        }
    }
    println!("{}", lines.join("\n"));
    // The probe that gives the test its meaning: stale reuse is only visible
    // when some file whose own text did not change comes out differently
    // after the edit. For every real edit, on some seed, one must. The edits
    // none can show are listed in `EXEMPT` with the reason, and the list is
    // compared both ways: an edit that starts to show must leave it, so it
    // cannot rot into a blanket excuse.
    let mut weak: Vec<String> = shown
        .into_iter()
        .filter(|(_, ok)| !ok)
        .map(|(e, _)| format!("{to:?} {e}"))
        .collect();
    let mut exempt: Vec<String> = EXEMPT
        .iter()
        .filter(|(d, _, _)| *d == to)
        .map(|(d, edit, _)| format!("{d:?} {edit}"))
        .collect();
    weak.sort();
    exempt.sort();
    if std::env::var("SQLDIFF_T1_ONLY").is_err() {
        assert_eq!(
            weak, exempt,
            "edits that change no dependent's output (stale reuse of them would pass T1) \
             against the documented exemptions"
        );
    }
}

/// `SQLDIFF_T1_ONLY=FlipNullability,ChangeDefault` limits the run to the
/// named edits (a comma list), for working on the generator.
fn only(e: &Edit) -> bool {
    std::env::var("SQLDIFF_T1_ONLY")
        .map(|v| v.split(',').any(|n| n == format!("{e:?}")))
        .unwrap_or(true)
}

/// Edits the probe cannot show on a dependent, with why.
const EXEMPT: &[(Dialect, &str, &str)] = &[
    (
        Dialect::Postgres,
        "ViewLookalike",
        "it shows only where a star is listed, i.e. in the tree with a HIDDEN column; this probe runs the plain tree (`site_switch.rs` kills the `visible_star` lookup with it on the hidden tree)",
    ),
    (
        Dialect::Db2i,
        "ViewLookalike",
        "same",
    ),
    (
        Dialect::Postgres,
        "ChangeDefault",
        "no rule reads a column default of another file's table (the default is read only in a table's own file, by the table diff); the recorded `Table` digest still holds it",
    ),
    (Dialect::Db2i, "ChangeDefault", "same"),
    (
        Dialect::Db2i,
        "AddBitColumn",
        "T-SQL procedure bodies are not converted to Db2 for i (the draft is the source text), so the bit rewrites that follow the catalog's types do not run; `site_switch.rs` kills the `ColumnTypes` read of pass 1 on the PostgreSQL tree",
    ),
    (
        Dialect::Db2i,
        "ToggleIndex",
        "UPDATE ... FROM is never converted to Db2 for i, with or without a unique key",
    ),
    (
        Dialect::Db2i,
        "ChangeParameterType",
        "calls are compared only from T-SQL to PostgreSQL (the Db2 for i call claim is paused)",
    ),
    (
        Dialect::Db2i,
        "AddTrigger",
        "DROP TRIGGER is compared only from T-SQL to PostgreSQL",
    ),
    (Dialect::Db2i, "DropTrigger", "same"),
    (
        Dialect::Postgres,
        "TargetOverrides",
        "a table of the tree shadows a base-catalog table of the same name on the target side as well: nothing a file reads changes (measured)",
    ),
    (Dialect::Db2i, "TargetOverrides", "same"),
    (Dialect::Db2i, "SourceOverrides", "same, and DROP TRIGGER (the one file it changes on PostgreSQL) is not compared for Db2 for i"),
    (
        Dialect::Db2i,
        "CaseClashColumn",
        "the column and index spelling checks guard conversions to PostgreSQL that fold case; no Db2 for i conversion reads them in another file (measured)",
    ),
    (Dialect::Db2i, "CaseClashIndex", "same"),
    (
        Dialect::Db2i,
        "BaseTriggerEvent",
        "DROP TRIGGER is compared only from T-SQL to PostgreSQL",
    ),
    (Dialect::Db2i, "TargetBaseTriggerEvent", "same"),
    (
        Dialect::Db2i,
        "RetypeSide",
        "the trigger that reads the pseudo table is not converted to Db2 for i (`inserted`/`deleted` need a rewrite)",
    ),
];

#[test]
fn a_resume_after_any_edit_equals_a_fresh_run_to_postgres() {
    every_edit(Dialect::Postgres);
}

#[test]
fn a_resume_after_any_edit_equals_a_fresh_run_to_db2i() {
    every_edit(Dialect::Db2i);
}

/// The generator itself: every edit changes the tree or the base catalogs,
/// except the ones that are no-ops by design, and it is deterministic.
#[test]
fn every_edit_changes_the_input_unless_it_is_a_no_op() {
    for &seed in SEEDS {
        for e in ALL_EDITS {
            let before = Model::generate(seed);
            let mut m = before.clone();
            let mut r = Rng::new(seed ^ 0xA5A5);
            let what = e.apply(&mut m, &mut r);
            let mut m2 = before.clone();
            let mut r2 = Rng::new(seed ^ 0xA5A5);
            assert_eq!(
                what,
                e.apply(&mut m2, &mut r2),
                "{e:?} is not deterministic"
            );
            let inputs = |m: &Model| {
                (
                    m.files(),
                    [Dialect::Tsql, Dialect::Postgres, Dialect::Db2i].map(|d| m.base_files(d)),
                    // Not a file of the tree: the ledger the fixture writes.
                    m.hand,
                )
            };
            if matches!(e, Edit::NoOpReorderTables) {
                continue;
            }
            assert!(
                inputs(&before) != inputs(&m),
                "seed {seed}: {e:?} ({what}) changed nothing"
            );
        }
    }
}

/// Writes seed `SQLDIFF_T1_DUMP_SEED`'s tree and base catalogs to
/// `SQLDIFF_T1_DUMP` for a look with the command line:
/// `SQLDIFF_T1_DUMP=/tmp/t1 cargo test -p sqldiff-convert --test replay_equals_fresh dump -- --ignored`
#[test]
#[ignore]
fn dump() {
    let Ok(dir) = std::env::var("SQLDIFF_T1_DUMP") else {
        return;
    };
    let seed = std::env::var("SQLDIFF_T1_DUMP_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(SEEDS[0]);
    let f = Fixture::new("dump", Dialect::Postgres);
    f.write(&Model::generate(seed));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::rename(f.dir.join("src"), format!("{dir}/src"))
        .or_else(|_| {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::rename(f.dir.join("src"), format!("{dir}/src"))
        })
        .unwrap();
    std::fs::rename(f.dir.join("base"), format!("{dir}/base")).unwrap();
}
