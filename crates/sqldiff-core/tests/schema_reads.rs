//! What a check reads of its catalogs is recorded under the right role
//! (performance plan W4.3, PR 2b). Nothing replays yet: these tests look at
//! the read set a recorded `diff` leaves.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use sqldiff_core::catalog::record::{Outcome, Query, ReadSink, Side};
use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{diff_recorded, Artifact, Dialect, Facts, Options};

const T: Dialect = Dialect::Tsql;

#[derive(Default)]
struct Collect(Mutex<BTreeMap<(Side, Query), Outcome>>);

impl ReadSink for Collect {
    fn record(&self, side: Side, query: &Query, outcome: Outcome) {
        self.0
            .lock()
            .unwrap()
            .insert((side, query.clone()), outcome);
    }
}

fn catalog(dialect: Dialect, files: &[&str]) -> Arc<Catalog> {
    let mut cat = Catalog::default();
    for f in files {
        let root = sqldiff_syntax::parse(f, dialect).syntax();
        ddl::load_tree(&mut cat, &root, dialect);
    }
    Arc::new(cat)
}

fn proc_t(body: &str) -> String {
    format!("CREATE PROCEDURE dbo.p AS\nBEGIN\n  {body}\nEND")
}

/// The reads of `diff` of two T-SQL procedures against the given catalogs.
fn reads(
    cat_a: &Arc<Catalog>,
    cat_b: &Arc<Catalog>,
    body_a: &str,
    body_b: &str,
) -> BTreeMap<(Side, Query), Outcome> {
    let opts = Options {
        facts_a: vec![Facts::new(Arc::clone(cat_a))],
        facts_b: vec![Facts::new(Arc::clone(cat_b))],
        ..Options::default()
    };
    let sink = Arc::new(Collect::default());
    let _ = diff_recorded(
        &Artifact::new("a.sql", T, proc_t(body_a)),
        &Artifact::new("b.sql", T, proc_t(body_b)),
        &opts,
        sink.clone(),
    );
    let m = sink.0.lock().unwrap().clone();
    m
}

const TABLE: &str = "CREATE TABLE dbo.orders (id INT NOT NULL PRIMARY KEY, note VARCHAR(10) NULL);";
const TABLE_EDITED: &str =
    "CREATE TABLE dbo.orders (id INT NOT NULL PRIMARY KEY, note VARCHAR(10) NULL, extra INT NULL);";
const BODY_B: &str = "INSERT INTO dbo.orders (note, id) VALUES ('a', 1);";
const BODY: &str = "INSERT INTO dbo.orders (id, note) VALUES (1, 'a');";

#[test]
fn reads_are_filed_under_the_role_not_the_cache() {
    // Both roles are served by one cached side: the same catalog object.
    let same = catalog(T, &[TABLE]);
    let before = reads(&same, &same, BODY, BODY_B);
    for q in ["HiddenTables", "Supplied"] {
        for side in [Side::Source, Side::Target] {
            assert!(
                before
                    .keys()
                    .any(|(s, k)| *s == side && format!("{k:?}").starts_with(q)),
                "{q} under {side:?} is not recorded"
            );
        }
    }
    assert!(
        before.keys().any(|(s, _)| *s == Side::Source)
            && before.keys().any(|(s, _)| *s == Side::Target),
        "a check reads both its catalogs: {:?}",
        before.keys().collect::<Vec<_>>()
    );
    assert!(
        before
            .keys()
            .any(|(_, k)| matches!(k, Query::SchemaTables(_))),
        "the equivalence schema's table reads are recorded"
    );
    // Edit only the target.
    let edited = catalog(T, &[TABLE_EDITED]);
    let after = reads(&same, &edited, BODY, BODY_B);
    let side = |m: &BTreeMap<(Side, Query), Outcome>, s: Side| {
        m.iter()
            .filter(|((x, _), _)| *x == s)
            .map(|((_, q), o)| (q.clone(), *o))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(
        side(&before, Side::Source),
        side(&after, Side::Source),
        "editing the target must not move a read filed under the source"
    );
    // A replay re-asks the recorded queries: it rejects only when a query
    // already in the read set answers differently (a new key alone would
    // not make it reject).
    assert!(
        moved(&before, &after, Side::Target),
        "an answer recorded under the target must change when the target is edited"
    );
    assert!(
        !moved(&before, &after, Side::Source),
        "no answer recorded under the source may change"
    );
}

/// Whether some read of `before` under `side` answers differently in `after`.
fn moved(
    before: &BTreeMap<(Side, Query), Outcome>,
    after: &BTreeMap<(Side, Query), Outcome>,
    side: Side,
) -> bool {
    before
        .iter()
        .any(|(k, o)| k.0 == side && after.get(k).is_some_and(|n| n != o))
}

#[test]
fn the_second_check_on_a_cached_side_records_the_same_reads() {
    // H5: the read set belongs to the check, not to the cached side. A
    // second check of the same pair on the same cached catalogs records
    // everything the first one did.
    let cat = catalog(T, &[TABLE]);
    let first = reads(&cat, &cat, BODY, BODY_B);
    let second = reads(&cat, &cat, BODY, BODY_B);
    assert!(!first.is_empty());
    assert_eq!(first, second);
}

const HIDDEN: &str = "CREATE TABLE dbo.t (k INT NOT NULL PRIMARY KEY, f DATETIME2 GENERATED ALWAYS AS ROW START HIDDEN NOT NULL, e DATETIME2 GENERATED ALWAYS AS ROW END HIDDEN NOT NULL, v INT NULL, PERIOD FOR SYSTEM_TIME (f, e)) WITH (SYSTEM_VERSIONING = ON (HISTORY_TABLE = dbo.t_h));";
const HIDDEN_PLUS: &str = "CREATE TABLE dbo.t (k INT NOT NULL PRIMARY KEY, f DATETIME2 GENERATED ALWAYS AS ROW START HIDDEN NOT NULL, e DATETIME2 GENERATED ALWAYS AS ROW END HIDDEN NOT NULL, v INT NULL, c INT NULL, PERIOD FOR SYSTEM_TIME (f, e)) WITH (SYSTEM_VERSIONING = ON (HISTORY_TABLE = dbo.t_h));";
const PG_PLAIN: &str = "CREATE TABLE t (k integer NOT NULL PRIMARY KEY, v integer NULL);";

#[test]
fn a_visible_column_added_to_a_hidden_table_moves_the_hidden_read() {
    // Whether the star over a view is mirrored depends on the visible
    // columns of each hidden table, so the `HiddenTables` answer carries the
    // whole table: adding a visible column to the source table must change
    // it even though no file names the table.
    let run = |source: &str| {
        let opts = Options {
            facts_a: vec![Facts::new(catalog(T, &[source]))],
            facts_b: vec![Facts::new(catalog(Dialect::Postgres, &[PG_PLAIN]))],
            ..Options::default()
        };
        let sink = Arc::new(Collect::default());
        let _ = diff_recorded(
            &Artifact::new("a.sql", T, proc_t("SELECT 1;")),
            &Artifact::new(
                "b.sql",
                Dialect::Postgres,
                "CREATE PROCEDURE p()\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  SELECT 1;\nEND;\n$$;",
            ),
            &opts,
            sink.clone(),
        );
        let m = sink.0.lock().unwrap().clone();
        m
    };
    let before = run(HIDDEN);
    let after = run(HIDDEN_PLUS);
    let hidden = |m: &BTreeMap<(Side, Query), Outcome>| m[&(Side::Source, Query::HiddenTables)];
    assert_ne!(hidden(&before), hidden(&after));
    // The counterpart on the other side is read too.
    assert!(before
        .keys()
        .any(|(s, q)| *s == Side::Target && *q == Query::SchemaTables("t".into())));
}

#[test]
fn a_hidden_table_added_to_the_target_alone_moves_a_recorded_read() {
    // The source has a hidden table, so a short-circuited `||` would never
    // ask the target's `HiddenTables`. Adding a hidden table to the target
    // alone must change an answer that was already recorded, or a replay
    // would take a stale result.
    const OTHER_HIDDEN: &str = "CREATE TABLE dbo.u (k INT NOT NULL PRIMARY KEY, f DATETIME2 GENERATED ALWAYS AS ROW START HIDDEN NOT NULL, e DATETIME2 GENERATED ALWAYS AS ROW END HIDDEN NOT NULL, v INT NULL, PERIOD FOR SYSTEM_TIME (f, e)) WITH (SYSTEM_VERSIONING = ON (HISTORY_TABLE = dbo.u_h));";
    const PLAIN_T: &str = "CREATE TABLE dbo.t (k INT NOT NULL PRIMARY KEY, v INT NULL);";
    let run = |target: &[&str]| {
        reads(
            &catalog(T, &[HIDDEN]),
            &catalog(T, target),
            "SELECT 1;",
            "SELECT 1;",
        )
    };
    let before = run(&[PLAIN_T]);
    let after = run(&[PLAIN_T, OTHER_HIDDEN]);
    assert!(
        before
            .keys()
            .any(|(s, q)| *s == Side::Target && *q == Query::HiddenTables),
        "the target's HiddenTables is recorded although the source hides columns"
    );
    assert!(moved(&before, &after, Side::Target));
    assert!(!moved(&before, &after, Side::Source));
}
