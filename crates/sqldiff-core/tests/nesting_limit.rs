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
    // Past the limit (1000); IF used to overflow the engine stack near 3 000.
    let nest = |n: usize, v: u32| {
        format!(
            "CREATE PROCEDURE dbo.p @x int AS\n{}UPDATE dbo.t SET a = {v};\n",
            "IF @x > 1\n".repeat(n)
        )
    };
    let r = run(nest(1100, 1), nest(1100, 2));
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

/// One routine nested past the limit costs only its own batch: the objects
/// after a `GO` are still read and their differences still reported.
#[test]
fn a_deep_routine_does_not_swallow_the_batches_after_it() {
    let file = |v: &str, t: &str| {
        let n = 1100;
        format!(
            "CREATE PROCEDURE dbo.deep AS\n{}SELECT 1;\n{}\nGO\nCREATE TABLE dbo.z (a {t});\nGO\nCREATE PROCEDURE dbo.q AS SELECT {v};\n",
            "BEGIN\n".repeat(n),
            "END\n".repeat(n)
        )
    };
    let r = run(file("1", "int"), file("2", "bigint"));
    assert!(not_proven(&r), "{:?}", r.verdict());
    let text = format!("{:?}", r.findings);
    assert!(
        text.contains("`int` became `bigint`") && text.contains("`1` became `2`"),
        "{text}"
    );
    assert!(
        r.findings.len() >= 2,
        "the later batches differ twice: {} finding(s): {text}",
        r.findings.len()
    );
}

/// A dispatcher with hundreds of `ELSE IF` branches nests one statement per
/// branch and is still read.
#[test]
fn a_long_else_if_chain_is_still_compared() {
    let chain = |last: i32| {
        let mut s = String::from("CREATE PROCEDURE dbo.p @x int AS\nIF @x = 0 SELECT 0;\n");
        for i in 1..400 {
            s += &format!("ELSE IF @x = {i} SELECT {i};\n");
        }
        s + &format!("ELSE SELECT {last};\n")
    };
    let r = run(chain(1), chain(2));
    assert!(
        !r.diagnostics
            .iter()
            .any(|d| d.message.contains("nesting too deep")),
        "{:?}",
        r.diagnostics
    );
    assert_eq!(r.verdict(), Verdict::Different, "{:?}", r.unknown_reasons);
}
