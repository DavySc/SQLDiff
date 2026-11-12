//! Forward `GOTO` in the behaviour model: a jump to a label later in the
//! same or an enclosing statement list is followed; every other form stays
//! "not modelled" (never a wrong "equal").

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

fn proc_(body: &str) -> Artifact {
    Artifact::new(
        "p.sql",
        Dialect::Tsql,
        format!("CREATE PROCEDURE dbo.p @x int\nAS\nBEGIN\n{body}\nEND\n"),
    )
}

fn run(a: &str, b: &str) -> Report {
    diff(&proc_(a), &proc_(b), &Options::default())
}

fn rules(r: &Report) -> Vec<&str> {
    r.findings.iter().map(|f| f.rule.as_str()).collect()
}

const FORWARD: &str = "IF @x = 1\nBEGIN\nINSERT dbo.t (c) VALUES (1);\nGOTO Done\nEND\nUPDATE dbo.t SET c = @x;\nDone:\nINSERT dbo.log (c) VALUES (@x);";

#[test]
fn the_same_goto_routine_is_equal() {
    assert_eq!(run(FORWARD, FORWARD).verdict(), Verdict::Equal);
}

#[test]
fn a_forward_goto_skips_the_statements_before_the_label() {
    // Without the jump the UPDATE also runs when @x = 1: a real difference,
    // found because the jump is followed (it was "not modelled" before).
    let no_jump = FORWARD.replace("GOTO Done\n", "");
    let r = run(FORWARD, &no_jump);
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
    let r = run(&no_jump, FORWARD);
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
    assert!(rules(&r).contains(&"error.control"), "{:?}", rules(&r));
}

#[test]
fn a_goto_to_another_label_is_a_different_path() {
    let other = "IF @x = 1\nBEGIN\nINSERT dbo.t (c) VALUES (1);\nGOTO Done2\nEND\nUPDATE dbo.t SET c = @x;\nDone:\nINSERT dbo.log (c) VALUES (@x);\nDone2:\nDELETE dbo.t;";
    let base = "IF @x = 1\nBEGIN\nINSERT dbo.t (c) VALUES (1);\nGOTO Done\nEND\nUPDATE dbo.t SET c = @x;\nDone:\nINSERT dbo.log (c) VALUES (@x);\nDone2:\nDELETE dbo.t;";
    let r = run(base, other);
    assert_ne!(r.verdict(), Verdict::Equal, "{:#?}", r.findings);
}

#[test]
fn a_jump_out_of_nested_blocks_is_followed() {
    let nested = "IF @x = 1\nBEGIN\nBEGIN\nINSERT dbo.t (c) VALUES (1);\nGOTO Done\nEND\nEND\nUPDATE dbo.t SET c = @x;\nDone:\nINSERT dbo.log (c) VALUES (@x);";
    let no_jump = nested.replace("GOTO Done\n", "");
    let r = run(nested, &no_jump);
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
}

#[test]
fn a_jump_out_of_a_catch_block_is_followed() {
    let src = "BEGIN TRY\nINSERT dbo.t (c) VALUES (1);\nEND TRY\nBEGIN CATCH\nINSERT dbo.err (c) VALUES (1);\nGOTO Done\nEND CATCH\nUPDATE dbo.t SET c = @x;\nDone:\nINSERT dbo.log (c) VALUES (@x);";
    let no_jump = src.replace("GOTO Done\n", "");
    let r = run(src, &no_jump);
    assert_ne!(r.verdict(), Verdict::Equal, "{:#?}", r.findings);
    // The jump is followed: nothing says it is not modelled.
    let text = format!("{:#?}", r.findings);
    assert!(!text.contains("GOTO is not modelled"), "{text}");
}

#[test]
fn a_backward_goto_is_not_modelled() {
    let src = "DECLARE @n int = 0;\nagain:\nSET @n = @n + 1;\nINSERT dbo.t (c) VALUES (@n);\nIF @n < 3 GOTO again;";
    let other = src.replace("< 3", "< 4");
    let r = run(src, &other);
    assert_ne!(r.verdict(), Verdict::Equal, "{:#?}", r.findings);
}

#[test]
fn a_label_named_twice_is_not_a_target() {
    let src = "IF @x = 1 GOTO l;\nUPDATE dbo.t SET c = 1;\nl:\nINSERT dbo.t (c) VALUES (1);\nl:\nINSERT dbo.t (c) VALUES (2);";
    let other = src.replace("UPDATE dbo.t SET c = 1;\n", "");
    let r = run(src, &other);
    assert_ne!(r.verdict(), Verdict::Equal, "{:#?}", r.findings);
}

#[test]
fn a_jump_into_a_block_is_not_followed() {
    let src = "IF @x = 1 GOTO l;\nUPDATE dbo.t SET c = 1;\nIF @x = 2\nBEGIN\nl:\nINSERT dbo.t (c) VALUES (1);\nEND";
    let other = src.replace("UPDATE dbo.t SET c = 1;\n", "");
    let r = run(src, &other);
    assert_ne!(r.verdict(), Verdict::Equal, "{:#?}", r.findings);
}
