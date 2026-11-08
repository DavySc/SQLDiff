//! The work meter counts deterministic events: the same input gives the same
//! totals every time, and the sites that bound a file's check are reached.

use sqldiff_core::{diff, Artifact, Dialect, Options};
use sqldiff_model::work::{scope, Site};

fn pair() -> (String, String) {
    let mk = |c: u32| {
        let body: String = (0..8)
            .map(|k| {
                format!("  UPDATE dbo.t SET a = a + {k} WHERE a = {};\n  IF @@ROWCOUNT = 0 INSERT INTO dbo.t (a) VALUES ({k});\n", if k == 7 { c } else { 1 })
            })
            .collect();
        format!("CREATE PROCEDURE dbo.p AS\nBEGIN\n{body}END\n")
    };
    (mk(1), mk(2))
}

fn run() -> sqldiff_model::work::Work {
    let (a, b) = pair();
    let ((), w) = scope("test", || {
        let _ = diff(
            &Artifact::new("a.sql", Dialect::Tsql, a.as_str()),
            &Artifact::new("b.sql", Dialect::Tsql, b.as_str()),
            &Options::default(),
        );
    });
    w
}

#[test]
fn the_same_input_gives_the_same_work() {
    let first = run();
    assert!(first.get(Site::DiffCalls) >= 1, "{first}");
    assert!(first.get(Site::AlignCells) > 0, "{first}");
    assert!(first.get(Site::ExplorerSteps) > 0, "{first}");
    assert_eq!(first, run());
    // Another thread, same totals.
    let other = std::thread::spawn(run).join().unwrap();
    assert_eq!(first, other);
}

// What is counted does not depend on what answered. The caches (explorer,
// flipped scenarios) are speed-ups: their work is counted at the first request
// and not again, hit or recomputed. A path dropped by joining is one event;
// the exponential work it avoids is not counted. A replay restored from a
// checkpoint counts what it runs from the fault on, not the restore.
mod fixed {
    use sqldiff_core::artifact::Analyzed;
    use sqldiff_core::ir::behaviour::set_caches_off_for_tests;
    use sqldiff_core::ir::{self, ErrorKind, Explorer, Fault, KeyUniverse, Scenario};
    use sqldiff_core::model::Side;
    use sqldiff_core::{diff, Artifact, Dialect, Options};
    use sqldiff_model::work::{scope, Site, Work};

    fn cfg_of(body: &str) -> ir::Cfg {
        let sql = format!("CREATE PROCEDURE dbo.p @p int AS\nBEGIN\n{body}END\nGO\n");
        let art = Artifact::new("a.sql", Dialect::Tsql, sql.as_str());
        ir::lower(&Analyzed::new(&art, Side::A, 3))
    }

    fn steps(w: &Work) -> u64 {
        w.get(Site::ExplorerSteps)
    }

    /// Two procedures with branches, loops and writes, so that the behaviour
    /// model builds explorers, replays faults and flips scenarios.
    fn pair() -> (String, String) {
        let mk = |c: u32| {
            let body: String = (0..6)
                .map(|k| {
                    format!(
                        "  IF @p > {k} UPDATE dbo.t SET a = a + {k} WHERE a = {};\n  IF @@ROWCOUNT = 0 INSERT INTO dbo.t (a) VALUES ({k});\n",
                        if k == 5 { c } else { 1 }
                    )
                })
                .collect();
            format!("CREATE PROCEDURE dbo.p @p int AS\nBEGIN\n  SET XACT_ABORT ON;\n  BEGIN TRY\n{body}  END TRY\n  BEGIN CATCH\n    THROW;\n  END CATCH\nEND\n")
        };
        (mk(1), mk(2))
    }

    fn run() -> Work {
        let (a, b) = pair();
        scope("t", || {
            let _ = diff(
                &Artifact::new("a.sql", Dialect::Tsql, a.as_str()),
                &Artifact::new("b.sql", Dialect::Tsql, b.as_str()),
                &Options::default(),
            );
        })
        .1
    }

    /// With the explorer and flipped caches off the total is the same.
    #[test]
    fn the_total_is_the_same_with_the_caches_off() {
        let on = run();
        assert!(steps(&on) > 1000, "{on}");
        set_caches_off_for_tests(true);
        let off = run();
        set_caches_off_for_tests(false);
        assert_eq!(on, off);
        // Other thread: same again (nothing carries over between files).
        assert_eq!(on, std::thread::spawn(run).join().unwrap());
    }

    /// A joined run drops paths at one event each: it counts far fewer steps
    /// than the plain run, which does the exponential work.
    #[test]
    fn a_joined_run_does_not_count_the_work_it_avoided() {
        let body: String = (0..6)
            .map(|i| format!("  IF @p > {i}\n    PRINT 'x{i}';\n"))
            .collect::<String>()
            + "  UPDATE dbo.t SET a = 1;\n";
        let cfg = cfg_of(&body);
        let keys = KeyUniverse::of(&[&cfg]);
        let base = Scenario::default();
        let (plain, wp) = scope("plain", || {
            Explorer::in_universe_joined(&cfg, &base, &keys, false)
        });
        let (joined, wj) = scope("joined", || {
            Explorer::in_universe_joined(&cfg, &base, &keys, true)
        });
        assert_eq!(plain.joined(), 0);
        assert!(joined.joined() > 0);
        assert!(steps(&wp) > 100, "{wp}");
        assert!(steps(&wj) < steps(&wp) / 4, "plain {wp}\njoined {wj}");
    }

    fn replay_cost(earlier: u64, nth: usize) -> u64 {
        let body: String = (0..700)
            .map(|i| format!("  UPDATE dbo.t SET a = {i};\n"))
            .collect();
        let cfg = cfg_of(&body);
        let keys = KeyUniverse::of(&[&cfg]);
        let base = Scenario::default();
        let node = (0..cfg.nodes.len())
            .filter(|&n| cfg.node(n).stmt().is_some())
            .nth(nth)
            .expect("a statement");
        let sc = Scenario {
            faults: vec![Fault {
                node,
                kind: ErrorKind::ALL[0],
            }],
            ..base.clone()
        };
        scope("file", || {
            sqldiff_model::work::spend(Site::ExplorerSteps, earlier);
            let x = Explorer::in_universe(&cfg, &base, &keys);
            steps(&scope("replay", || x.explore(&cfg, &sc)).1)
        })
        .0
    }

    /// A fault reached at tick 600 (nodes are numbered from the end of the
    /// routine, so the 100th statement node) counts what its replay runs, a
    /// few steps, whatever the checkpoint spacing and whatever the file did
    /// before. Its prefix was counted when the fault-free run was made.
    #[test]
    fn a_replay_counts_only_what_it_runs() {
        let late = replay_cost(0, 100);
        assert!(late < 20, "{late}");
        assert_eq!(late, replay_cost(1_000_000, 100));
    }

    /// Two faults make no resumable replay: a full run, counted once.
    #[test]
    fn two_faults_are_counted_as_one_full_run() {
        let body: String = (0..50)
            .map(|i| format!("  UPDATE dbo.t SET a = {i};\n"))
            .collect();
        let cfg = cfg_of(&body);
        let keys = KeyUniverse::of(&[&cfg]);
        let base = Scenario::default();
        let x = Explorer::in_universe(&cfg, &base, &keys);
        let nodes: Vec<usize> = (0..cfg.nodes.len())
            .filter(|&n| cfg.node(n).stmt().is_some())
            .collect();
        let sc = Scenario {
            faults: nodes[..2]
                .iter()
                .map(|&node| Fault {
                    node,
                    kind: ErrorKind::ALL[0],
                })
                .collect(),
            ..base.clone()
        };
        let (_, w) = scope("two", || x.explore(&cfg, &sc));
        assert!(steps(&w) > 0 && steps(&w) < 200, "{w}");
    }

    /// The recompute of a flipped scenario (caches off, or a cache that was
    /// cleared) spends no routine or file diff budget, so a budget set to
    /// exactly what the cached run spends gives the same report and the same
    /// work with the caches on or off. p01-move-stock reaches the flipped
    /// scenarios (`persists`) with first requests and repeats.
    #[test]
    fn a_flipped_scenario_recomputed_spends_no_budget() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/pairs/p01-move-stock");
        let read = |f: &str| {
            std::fs::read_to_string(dir.join(f))
                .unwrap()
                .replace("\r\n", "\n")
        };
        let (a, b) = (read("tsql.sql"), read("db2i.sql"));
        let run = |budget: Option<(u64, u64)>| {
            let opts = Options {
                work_budgets: budget,
                ..Options::default()
            };
            let (rep, w) = scope("t", || {
                diff(
                    &Artifact::new("a.sql", Dialect::Tsql, a.as_str()),
                    &Artifact::new("b.sql", Dialect::Db2i, b.as_str()),
                    &opts,
                )
            });
            (serde_json::to_string(&rep).unwrap(), w)
        };
        let (base_report, base) = run(None);
        let spent = base.get(Site::BehaviourDiffs);
        assert!(spent > 4, "{base}");
        // Exactly the cached run's spend: nothing is cut with the caches on.
        let (on_report, on) = run(Some((spent, u64::MAX)));
        assert_eq!(on_report, base_report);
        set_caches_off_for_tests(true);
        let (off_report, off) = run(Some((spent, u64::MAX)));
        set_caches_off_for_tests(false);
        assert_eq!(off_report, on_report);
        assert_eq!(off, on);
    }
}

fn counts(a: Artifact, b: Artifact) -> (u64, u64) {
    let ((), w) = scope("test", || {
        let _ = diff(&a, &b, &Options::default());
    });
    (w.get(Site::ItemsBuilt), w.get(Site::PairCompares))
}

// The statement-level sites count exactly: a cross-dialect routine of n
// statements builds 2n items (n per side) and compares n pairs.
#[test]
fn cross_dialect_items_and_pairs_are_counted_exactly() {
    for n in [1usize, 3, 6] {
        let ts: String = (0..n)
            .map(|k| format!("  INSERT INTO dbo.t (a) VALUES ({k});\n"))
            .collect();
        let pg: String = (0..n)
            .map(|k| format!("  INSERT INTO t (a) VALUES ({k});\n"))
            .collect();
        let a = format!("CREATE PROCEDURE dbo.p AS\nBEGIN\n{ts}END\n");
        let b = format!("CREATE PROCEDURE p() LANGUAGE plpgsql AS $$\nBEGIN\n{pg}END;\n$$;\n");
        let (items, pairs) = counts(
            Artifact::new("a.sql", Dialect::Tsql, a.as_str()),
            Artifact::new("b.sql", Dialect::Postgres, b.as_str()),
        );
        assert_eq!(items, 2 * n as u64);
        assert_eq!(pairs, n as u64);
    }
}

// A same-dialect diff with an Unknown stmt.modified finding refines it, which
// builds items too.
#[test]
fn the_refinement_of_a_modified_statement_is_counted() {
    let (a, b) = pair();
    let (items, pairs) = counts(
        Artifact::new("a.sql", Dialect::Tsql, a.as_str()),
        Artifact::new("b.sql", Dialect::Tsql, b.as_str()),
    );
    // Only the refinement builds items and compares pairs in a same-dialect
    // diff: 64 items over both sides, one modified statement compared.
    assert_eq!((items, pairs), (64, 1));
}
