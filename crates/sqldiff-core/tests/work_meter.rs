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
