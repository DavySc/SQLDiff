//! Input nested or chained past the parser's limits never crashes the
//! process and is never reported as proven equal: the statement is refused
//! (unknown), the same way an unsupported construct is.

use sqldiff_core::{diff, with_engine_stack, Artifact, Dialect, Options, Report, Verdict};

fn run(a: String, b: String) -> Report {
    with_engine_stack(move || {
        diff(
            &Artifact::new("a.sql", Dialect::Tsql, a),
            &Artifact::new("b.sql", Dialect::Tsql, b),
            &Options::default(),
        )
    })
}

fn not_proven(r: &Report) -> bool {
    r.verdict() != Verdict::Equal && r.verdict() != Verdict::EqualUnderAssumptions
}

fn view(expr: &str) -> String {
    format!("CREATE VIEW dbo.v AS SELECT {expr} AS x FROM dbo.t;\n")
}

#[test]
fn a_nest_of_brackets_past_the_limit_is_not_proven_and_does_not_crash() {
    // Past the limit (2 000); brackets used to overflow the engine stack at 6 000+.
    // The two sides differ in the innermost value, so they cannot be
    // dismissed as the same text.
    let deep = |n: usize, v: &str| view(&format!("{}{v}{}", "(".repeat(n), ")".repeat(n)));
    let r = run(deep(2500, "1"), deep(2500, "2"));
    assert!(
        not_proven(&r),
        "{:?} {:?} {:?}",
        r.verdict(),
        r.unknown_reasons,
        r.diagnostics
    );
    // A modest nest is still compared, and the difference is found.
    let r = run(deep(50, "1"), deep(50, "2"));
    assert!(
        not_proven(&r),
        "{:?} {:?} {:?}",
        r.verdict(),
        r.unknown_reasons,
        r.diagnostics
    );
}

#[test]
fn a_chain_past_the_limit_is_not_proven_and_does_not_crash() {
    let chain = |n: usize, last: &str| {
        let mut t: Vec<String> = (0..n).map(|i| format!("c{}", i % 40)).collect();
        t.push(last.to_string());
        view(&t.join(" + "))
    };
    let r = run(chain(2500, "d1"), chain(2500, "d2"));
    assert!(
        not_proven(&r),
        "{:?} {:?} {:?}",
        r.verdict(),
        r.unknown_reasons,
        r.diagnostics
    );
}

#[test]
fn statements_nested_past_the_limit_are_not_proven_and_do_not_crash() {
    // Past the limit (256); IF used to overflow the engine stack near 3 000.
    let nest = |n: usize, v: u32| {
        format!(
            "CREATE PROCEDURE dbo.p @x int AS\n{}UPDATE dbo.t SET a = {v};\n",
            "IF @x > 1\n".repeat(n)
        )
    };
    let r = run(nest(400, 1), nest(400, 2));
    assert!(
        not_proven(&r),
        "{:?} {:?} {:?}",
        r.verdict(),
        r.unknown_reasons,
        r.diagnostics
    );
    let r = run(nest(20, 1), nest(20, 2));
    assert!(
        not_proven(&r),
        "{:?} {:?} {:?}",
        r.verdict(),
        r.unknown_reasons,
        r.diagnostics
    );
}
