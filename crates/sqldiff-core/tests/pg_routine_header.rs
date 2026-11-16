//! PostgreSQL routine headers: `public.` before the routine's own name and an
//! explicit `IN` argument mode are spellings of what the server defaults, so
//! two routines that differ only by them compare equal (the schema one under
//! ASM-DEFAULT-SCHEMA). Everything else in a header still decides.
//!
//! Every body ends in a write, so the comparison has an effect to observe.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

fn run(a: &str, b: &str) -> Report {
    diff(
        &Artifact::new("a.sql", Dialect::Postgres, a),
        &Artifact::new("b.sql", Dialect::Postgres, b),
        &Options::default(),
    )
}

fn rules(r: &Report) -> Vec<&str> {
    r.findings.iter().map(|f| f.rule.as_str()).collect()
}

fn asms(r: &Report) -> Vec<&str> {
    r.assumptions.iter().map(|a| a.id.as_str()).collect()
}

/// `CREATE PROCEDURE {head}({args}) …` with a body that writes.
fn proc_(head: &str, args: &str) -> String {
    format!(
        "CREATE OR REPLACE PROCEDURE {head}({args})\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  UPDATE t SET x = p_a WHERE id = 1;\nEND;\n$$;"
    )
}

fn assert_unknown_header(a: &str, b: &str) {
    let r = run(a, b);
    assert!(
        rules(&r).contains(&"header.modified"),
        "expected header.modified: {:?}\n{a}\n{b}",
        rules(&r)
    );
    assert_eq!(r.verdict(), Verdict::Unknown, "{:?}", rules(&r));
}

#[test]
fn public_qualifier_and_in_mode_equal_under_default_schema() {
    let a = proc_("bdg_x", "p_a varchar(128), INOUT p_b integer");
    let b = proc_(
        "public.\"bdg_x\"",
        "IN \"p_a\" varchar(128), INOUT \"p_b\" integer",
    );
    let r = run(&a, &b);
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:?}",
        rules(&r)
    );
    assert!(asms(&r).contains(&"ASM-DEFAULT-SCHEMA"), "{:?}", asms(&r));
    assert!(!rules(&r).contains(&"header.modified"));
}

#[test]
fn in_mode_alone_is_equal_without_an_assumption() {
    let r = run(&proc_("p", "p_a integer"), &proc_("p", "IN p_a integer"));
    assert_eq!(r.verdict(), Verdict::Equal, "{:?}", rules(&r));
    assert!(!asms(&r).contains(&"ASM-DEFAULT-SCHEMA"));
}

#[test]
fn public_on_both_sides_needs_no_assumption() {
    let r = run(
        &proc_("public.p", "p_a integer"),
        &proc_("PUBLIC.p", "p_a integer"),
    );
    assert_eq!(r.verdict(), Verdict::Equal, "{:?}", rules(&r));
    assert!(!asms(&r).contains(&"ASM-DEFAULT-SCHEMA"));
}

#[test]
fn quoted_lowercase_public_counts() {
    let r = run(
        &proc_("p", "p_a integer"),
        &proc_("\"public\".p", "p_a integer"),
    );
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:?}",
        rules(&r)
    );
    assert!(asms(&r).contains(&"ASM-DEFAULT-SCHEMA"));
}

#[test]
fn another_schema_still_differs() {
    assert_unknown_header(&proc_("p", "p_a integer"), &proc_("s1.p", "p_a integer"));
}

#[test]
fn quoted_upper_case_public_is_another_schema() {
    assert_unknown_header(
        &proc_("p", "p_a integer"),
        &proc_("\"PUBLIC\".p", "p_a integer"),
    );
}

#[test]
fn in_out_is_not_out() {
    // `IN OUT` is INOUT: dropping the `IN` would make it OUT.
    assert_unknown_header(
        &proc_("p", "OUT p_a integer"),
        &proc_("p", "IN OUT p_a integer"),
    );
}

#[test]
fn a_mode_after_the_name_is_left_alone() {
    assert_unknown_header(&proc_("p", "p_a integer"), &proc_("p", "p_a IN integer"));
}

#[test]
fn in_inside_a_default_is_left_alone() {
    let a = proc_("p", "p_a boolean DEFAULT 1 IN (1, 2)");
    let b = proc_("p", "p_a boolean DEFAULT 1 IN (1, 3)");
    let r = run(&a, &b);
    assert_ne!(r.verdict(), Verdict::Equal, "{:?}", rules(&r));
}
