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

// Logical charging (R4): what a cache, a memo or a replay saves is charged
// again, so the total is what the algorithm does and not what its shortcuts
// happened to skip.
mod logical {
    use sqldiff_core::artifact::Analyzed;
    use sqldiff_core::ir::{self, ErrorKind, Explorer, Fault, KeyUniverse, Scenario};
    use sqldiff_core::model::Side;
    use sqldiff_core::{Artifact, Dialect};
    use sqldiff_model::work::{scope, Site};

    fn cfg_of(body: &str) -> ir::Cfg {
        let sql = format!("CREATE PROCEDURE dbo.p @p int AS\nBEGIN\n{body}END\nGO\n");
        let art = Artifact::new("a.sql", Dialect::Tsql, sql.as_str());
        ir::lower(&Analyzed::new(&art, Side::A, 3))
    }

    fn steps(w: &sqldiff_model::work::Work) -> u64 {
        w.get(Site::ExplorerSteps)
    }

    /// Joining drops paths, but each dropped path is charged the subtree it
    /// would have run: joined and plain runs count the same steps.
    #[test]
    fn a_joined_run_counts_what_the_plain_run_does() {
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
        assert_eq!(steps(&wp), steps(&wj), "plain {wp}\njoined {wj}");
        assert_eq!(plain.built(), wp);
        assert_eq!(joined.built(), wj);
    }

    /// A fault scenario restored from a checkpoint is charged the prefix it
    /// skips (the ticks before the fault), not the ticks the restore re-ran:
    /// a fault reached at tick 600 is charged about 600 steps.
    #[test]
    fn a_replay_is_charged_its_prefix_not_the_restore() {
        // Far more ticks than checkpoints, so restores re-run many ticks.
        let body: String = (0..700)
            .map(|i| format!("  UPDATE dbo.t SET a = {i};\n"))
            .collect();
        let cfg = cfg_of(&body);
        let keys = KeyUniverse::of(&[&cfg]);
        let base = Scenario::default();
        let (x, built) = scope("build", || Explorer::in_universe(&cfg, &base, &keys));
        // Nodes are numbered from the end of the routine: the 100th statement
        // node is reached at tick 600, so 600 ticks of prefix, then a short rest.
        let node = (0..cfg.nodes.len())
            .filter(|&n| cfg.node(n).stmt().is_some())
            .nth(100)
            .expect("a statement");
        let sc = Scenario {
            faults: vec![Fault {
                node,
                kind: ErrorKind::ALL[0],
            }],
            ..base.clone()
        };
        let (_, w) = scope("replay", || x.explore(&cfg, &sc));
        let (w_steps, built_steps) = (steps(&w), steps(&built));
        // Charged the prefix (about 600 ticks, whatever the checkpoint
        // spacing), not the few ticks the restore re-ran, and not the whole run.
        assert!(
            (590..=640).contains(&w_steps) && w_steps < built_steps,
            "replay {w} vs build {built}"
        );
        // And it is the same every time.
        let (_, again) = scope("replay", || x.explore(&cfg, &sc));
        assert_eq!(w, again);
    }
}
