//! The behaviour model's work caps (perf plan R12, step A): a unit (one
//! routine pair) and a file's analysis may spend a counted amount of metered
//! work; past it the statements not compared are "not proven: work budget"
//! (`equiv.budget`), never equal. The cap sweep runs a few fixtures with the
//! cap at every value from 0 up to the total and checks that nothing moves
//! toward equal.

use sqldiff_core::ir::behaviour::{
    file_work_cap, set_caches_off_for_tests, unit_work_cap, CAP_OVERSHOOT,
};
use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};
use sqldiff_model::work::scope;

struct Fixture {
    name: &'static str,
    a: (Dialect, String),
    b: (Dialect, String),
}

fn pair_file(dir: &str, a: (&str, Dialect), b: (&str, Dialect)) -> Fixture {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../corpus/pairs")
        .join(dir);
    let read = |f: &str| {
        std::fs::read_to_string(root.join(f))
            .unwrap()
            .replace("\r\n", "\n")
    };
    Fixture {
        name: Box::leak(dir.to_string().into_boxed_str()),
        a: (a.1, read(a.0)),
        b: (b.1, read(b.0)),
    }
}

/// A routine file with branches and writes, a routine file whose two sides
/// have alignment and edit-checker work, and a script with one flat
/// transaction.
fn fixtures() -> Vec<Fixture> {
    let inserts = |sep: &str, end: &str| {
        let rows: String = (0..6)
            .map(|k| format!("INSERT INTO t (id, qty) VALUES ({k}, {});\n", k % 3))
            .collect();
        format!("CREATE TABLE t (id int, qty int);\n{sep}{rows}{end}")
    };
    vec![
        pair_file(
            "p01-move-stock",
            ("tsql.sql", Dialect::Tsql),
            ("db2i.sql", Dialect::Db2i),
        ),
        pair_file(
            "p02-upsert-price",
            ("tsql.sql", Dialect::Tsql),
            ("pg.sql", Dialect::Postgres),
        ),
        Fixture {
            name: "flat transaction",
            a: (Dialect::Tsql, inserts("BEGIN TRANSACTION;\n", "COMMIT;\n")),
            b: (Dialect::Postgres, inserts("BEGIN;\n", "COMMIT;\n")),
        },
    ]
}

fn run(f: &Fixture, caps: Option<(u64, u64)>) -> (Report, u64) {
    let opts = Options {
        work_caps: caps,
        ..Options::default()
    };
    let (r, w) = scope("cap", || {
        diff(
            &Artifact::new("a.sql", f.a.0, f.a.1.as_str()),
            &Artifact::new("b.sql", f.b.0, f.b.1.as_str()),
            &opts,
        )
    });
    (r, w.total())
}

fn json(r: &Report) -> String {
    serde_json::to_string(r).unwrap()
}

fn is_cut(f: &sqldiff_core::Finding) -> bool {
    f.rule == "equiv.budget" && f.verdict == Verdict::Unknown && f.witness.contains("work budget")
}

/// Caps to try for a total: every value up to 64, then a deterministic
/// sample (every `total / 200`th value), the total itself and above.
fn caps(total: u64) -> Vec<u64> {
    let mut v: Vec<u64> = (0..=total.min(64)).collect();
    let step = (total / 200).max(1);
    v.extend((65..=total).step_by(step as usize));
    v.extend([total, total + 1, total * 2]);
    v.sort_unstable();
    v.dedup();
    v
}

#[test]
fn no_cap_moves_a_verdict_toward_equal() {
    for f in fixtures() {
        let (full, total) = run(&f, None);
        assert!(total > 0, "{}", f.name);
        let (_, pre) = run(&f, Some((0, 0)));
        let mut cut_seen = false;
        for cap in caps(total) {
            let (r, spent) = run(&f, Some((cap, cap)));
            let ctx = format!("{} at cap {cap} of {total}", f.name);
            let cut = r.findings.iter().any(is_cut);
            cut_seen |= cut;
            if json(&r) != json(&full) {
                assert!(cut, "{ctx}: a changed report must say why");
            }
            // Every finding that is not a cut was there before, and no
            // verdict is stronger toward equal than it was.
            for g in r.findings.iter().filter(|g| !is_cut(g)) {
                let old = full
                    .findings
                    .iter()
                    .find(|o| o.fingerprint == g.fingerprint)
                    .unwrap_or_else(|| panic!("{ctx}: new finding {g:?}"));
                assert!(
                    g.verdict == old.verdict || g.verdict == Verdict::Unknown,
                    "{ctx}: {:?} -> {:?}",
                    old.verdict,
                    g.verdict
                );
            }
            // A finding that is gone is covered by a cut.
            for o in &full.findings {
                if !r.findings.iter().any(|g| g.fingerprint == o.fingerprint) {
                    assert!(cut, "{ctx}: {} vanished without a cut", o.fingerprint);
                }
            }
            // The overshoot past a check is bounded (statement-level work
            // before the behaviour model is `pre`).
            assert!(
                spent <= pre + cap + CAP_OVERSHOOT,
                "{ctx}: spent {spent}, pre {pre}"
            );
            if cap >= total {
                assert_eq!(json(&r), json(&full), "{ctx}: at the total nothing is cut");
            }
        }
        assert!(cut_seen, "{}: no cap in the sweep cut anything", f.name);
    }
}

#[test]
fn the_sweep_gives_the_same_reports_with_the_caches_off() {
    for f in fixtures() {
        let (_, total) = run(&f, None);
        for cap in caps(total) {
            let (on, _) = run(&f, Some((cap, cap)));
            set_caches_off_for_tests(true);
            let (off, _) = run(&f, Some((cap, cap)));
            set_caches_off_for_tests(false);
            assert_eq!(json(&on), json(&off), "{} at cap {cap}", f.name);
        }
    }
}

fn witnesses(r: &Report) -> Vec<&str> {
    r.findings
        .iter()
        .filter(|f| f.rule == "equiv.budget")
        .map(|f| f.witness.as_str())
        .collect()
}

#[test]
fn the_witness_names_the_scope_and_both_numbers() {
    let f = &fixtures()[0];
    let (unit, _) = run(f, Some((5, u64::MAX)));
    let w = witnesses(&unit);
    assert!(!w.is_empty());
    assert!(
        w.iter()
            .all(|w| w.contains("unit work budget (") && w.contains(" of 5 units of work)")),
        "{w:?}"
    );
    let (file, _) = run(f, Some((u64::MAX, 5)));
    let w = witnesses(&file);
    assert!(!w.is_empty());
    assert!(
        w.iter()
            .all(|w| w.contains("file work budget (") && w.contains(" of 5 units of work")),
        "{w:?}"
    );
    // Both used up: the unit's is named first.
    let (both, _) = run(f, Some((5, 5)));
    assert!(witnesses(&both)
        .iter()
        .all(|w| w.contains("unit work budget")));
}

#[test]
fn the_comparison_budgets_keep_their_own_names() {
    let f = &fixtures()[0];
    let opts = |b| Options {
        work_budgets: Some(b),
        ..Options::default()
    };
    let go = |b| {
        diff(
            &Artifact::new("a.sql", f.a.0, f.a.1.as_str()),
            &Artifact::new("b.sql", f.b.0, f.b.1.as_str()),
            &opts(b),
        )
    };
    let r = go((2, u64::MAX));
    assert!(witnesses(&r)
        .iter()
        .all(|w| w.contains("routine comparison budget (")));
    let r = go((u64::MAX, 2));
    assert!(witnesses(&r)
        .iter()
        .all(|w| w.contains("file comparison budget (")));
}

#[test]
fn the_caps_follow_the_unit_and_the_file() {
    // The floor holds for a small unit; a long unit's cap grows with its
    // statements; the file's is never below its largest unit's.
    assert_eq!(unit_work_cap(10), 1 << 24);
    assert_eq!(unit_work_cap(4000), 4000 << 15);
    assert_eq!(file_work_cap(1, 0), 1 << 26);
    assert_eq!(file_work_cap(100, 0), 100 << 20);
    assert_eq!(file_work_cap(1, 4000 << 15), 4000 << 15);
}

/// A lowered cap is part of the options: the fixtures give different
/// reports at different caps, so a stored result could never be reused
/// across them (the key test in sqldiff-convert covers the key itself).
#[test]
fn a_lowered_cap_changes_the_report() {
    let f = &fixtures()[0];
    let (a, _) = run(f, Some((5, 5)));
    let (b, _) = run(f, None);
    assert_ne!(json(&a), json(&b));
}

/// The file cap counts from the start of this comparison, not from the
/// outermost meter: `convert` checks one file in several comparisons, and a
/// later one must give what a direct `diff` gives.
#[test]
fn a_comparison_inside_a_busy_scope_gives_the_report_of_a_direct_one() {
    for f in fixtures() {
        let (_, total) = run(&f, None);
        for cap in [total / 4, total / 2, total] {
            let (direct, _) = run(&f, Some((u64::MAX, cap)));
            let ((), _) = scope("outer", || {
                // Earlier comparisons of the same file spend a lot.
                let _ = run(&f, None);
                let _ = run(&f, None);
                let (nested, _) = run(&f, Some((u64::MAX, cap)));
                assert_eq!(json(&nested), json(&direct), "{} at file cap {cap}", f.name);
            });
        }
    }
}

/// A flipped-scenario recompute that starts with the cap already used up
/// (cap 0, so every replay begins over it) is muted: it neither tests the
/// cap nor changes the report, whether the scenario came from the cache or
/// was recomputed.
#[test]
fn a_replay_that_starts_over_the_cap_gives_the_same_report_cached_or_not() {
    for f in fixtures() {
        let (on, _) = run(&f, Some((0, 0)));
        set_caches_off_for_tests(true);
        let (off, _) = run(&f, Some((0, 0)));
        set_caches_off_for_tests(false);
        assert_eq!(json(&on), json(&off), "{}", f.name);
        assert!(
            on.findings.iter().any(is_cut) || on.verdict() != Verdict::Equal,
            "{}: a zero cap moved nothing toward equal",
            f.name
        );
    }
}
