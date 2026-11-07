//! The pair-state meter (perf plan W6 measurement): counts on generated
//! shapes whose answer is known, and determinism. The meter is not on the
//! diff path, so these tests only pin what it reports.
use sqldiff_core::{pair_states, Artifact, Dialect, Options};

fn proc_of(body: &str) -> String {
    format!("CREATE PROCEDURE dbo.p @p int AS\nBEGIN\n{body}END\nGO\n")
}

fn rows(src: &str) -> Vec<sqldiff_core::ir::behaviour::PairRow> {
    let a = Artifact::new("a.sql", Dialect::Tsql, src);
    let b = Artifact::new("b.sql", Dialect::Tsql, src);
    pair_states(&a, &b, &Options::default(), 100_000)
}

fn baseline(src: &str) -> sqldiff_core::ir::behaviour::PairRow {
    rows(src).into_iter().find(|r| r.env == "baseline").unwrap()
}

#[test]
fn sequential_ifs_with_writes_are_paths_but_few_agreeing_pair_states() {
    let n = 8;
    let body: String = (0..n)
        .map(|i| format!("  IF @p > {i}\n    INSERT INTO dbo.t{i}(a) VALUES (1);\n"))
        .collect();
    let r = baseline(&proc_of(&body));
    let [strict_steps, strict, agree_steps, agree, _, _] = &r.measures[..] else {
        panic!("six variants")
    };
    assert_eq!(r.decisions_a, n);
    // 2^8 paths, and single-side states do not merge (the log differs).
    assert_eq!(strict_steps.paths, Some(256));
    assert_eq!(strict_steps.states, 255);
    assert_eq!(strict.states, 255);
    // Agreement merges them, but only without the step counter.
    assert!(agree_steps.states < strict.states);
    assert_eq!(agree.states, n);
    assert_eq!(agree.paths, Some(256));
    assert!(!agree.capped);
}

#[test]
fn sequential_loops_are_three_to_the_l_paths() {
    let body: String = (0..5)
        .map(|i| format!("  WHILE @p > {i}\n  BEGIN\n    INSERT INTO dbo.t(a) VALUES ({i});\n    SET @p = @p - 1;\n  END\n"))
        .collect();
    let r = baseline(&proc_of(&body));
    assert_eq!(r.measures[0].paths, Some(243));
    assert_eq!(r.measures[3].desync, 0);
    assert!(r.measures[3].states < 20);
}

#[test]
fn the_meter_is_deterministic() {
    let body: String = (0..6)
        .map(|i| format!("  IF @p > {i}\n    UPDATE dbo.t SET a = {i};\n"))
        .collect();
    let src = proc_of(&body);
    let key = |rs: &[sqldiff_core::ir::behaviour::PairRow]| {
        format!("{:?}", rs.iter().map(|r| &r.measures).collect::<Vec<_>>())
    };
    assert_eq!(key(&rows(&src)), key(&rows(&src)));
}

#[test]
fn the_state_cap_is_reported_not_exceeded() {
    let body: String = (0..14)
        .map(|i| format!("  IF @p > {i}\n    INSERT INTO dbo.t{i}(a) VALUES (1);\n"))
        .collect();
    let src = proc_of(&body);
    let a = Artifact::new("a.sql", Dialect::Tsql, src.as_str());
    let b = Artifact::new("b.sql", Dialect::Tsql, src.as_str());
    let r = pair_states(&a, &b, &Options::default(), 500);
    let m = &r.iter().find(|r| r.env == "baseline").unwrap().measures[0];
    assert!(m.capped && m.paths.is_none() && m.states <= 500);
}
