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

// ---- T-SQL against its PostgreSQL conversion: a forward GOTO is a labelled
// block left with EXIT (statement-level equivalence).

fn tsql_file(body: &str) -> Artifact {
    Artifact::new(
        "p.sql",
        Dialect::Tsql,
        format!("CREATE TABLE dbo.t (c int, d int);\nGO\nCREATE PROCEDURE dbo.p @x int\nAS\nBEGIN\nSET XACT_ABORT ON;\nDECLARE @y int = 0;\n{body}\nEND\n"),
    )
}

fn pg_file(body: &str) -> Artifact {
    Artifact::new(
        "p.pg.sql",
        Dialect::Postgres,
        format!("CREATE TABLE t (c integer, d integer);\nCREATE OR REPLACE PROCEDURE p(p_x integer)\nLANGUAGE plpgsql\nAS $$\nDECLARE\n  v_y integer;\nBEGIN\n  v_y := 0;\n{body}\nEND;\n$$;\n"),
    )
}

fn cross(t: &str, p: &str) -> Verdict {
    diff(&tsql_file(t), &pg_file(p), &Options::default()).verdict()
}

const T_ONE: &str = "IF @x = 1\nBEGIN\nSET @y = 5;\nGOTO Done\nEND\nSET @y = @x + 1;\nDone:\nUPDATE dbo.t SET c = @y;";

fn p_one(block_end_after_update: bool, exit: &str, label: &str) -> String {
    let (a, b) = if block_end_after_update {
        ("", "END;")
    } else {
        ("END;", "")
    };
    format!(
        "<<{label}>>\nBEGIN\nIF p_x = 1 THEN\nv_y := 5;\n{exit}\nEND IF;\nv_y := p_x + 1;\n{a}\nUPDATE t SET c = v_y;\n{b}"
    )
}

#[test]
fn a_forward_goto_and_its_labelled_block_are_equal() {
    let v = cross(T_ONE, &p_one(false, "EXIT goto_done;", "goto_done"));
    assert!(
        matches!(v, Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{v:?}"
    );
    // The name of the label is not part of the meaning.
    let v = cross(T_ONE, &p_one(false, "EXIT other;", "other"));
    assert!(
        matches!(v, Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{v:?}"
    );
}

#[test]
fn a_block_that_does_not_match_the_jump_is_not_equal() {
    // No jump out of the block: the statement after the IF runs.
    let v = cross(T_ONE, &p_one(false, "NULL;", "goto_done"));
    assert!(
        !matches!(v, Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{v:?}"
    );
    // The block also holds the UPDATE the label is before: EXIT skips it.
    let v = cross(T_ONE, &p_one(true, "EXIT goto_done;", "goto_done"));
    assert!(
        !matches!(v, Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{v:?}"
    );
    // The block ends before the assignment the jump must skip.
    let early = "<<goto_done>>\nBEGIN\nIF p_x = 1 THEN\nv_y := 5;\nEXIT goto_done;\nEND IF;\nEND;\nv_y := p_x + 1;\nUPDATE t SET c = v_y;";
    let v = cross(T_ONE, early);
    assert!(
        !matches!(v, Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{v:?}"
    );
    // No block at all.
    let none = "IF p_x = 1 THEN\nv_y := 5;\nELSE\nv_y := p_x + 1;\nEND IF;\nUPDATE t SET c = v_y;";
    let v = cross(T_ONE, none);
    // Equal here is right (same behaviour without a jump): it must not fail
    // or claim a difference, but it may stay unproven.
    assert_ne!(v, Verdict::Different, "{v:?}");
}

#[test]
fn nested_blocks_leave_the_right_one() {
    let t = "IF @x = 2 GOTO B;\nIF @x = 1 GOTO A;\nSET @y = 3;\nA:\nSET @y = @y + 1;\nB:\nUPDATE dbo.t SET c = @y;";
    let p = |a: &str, b: &str| {
        format!("<<goto_b>>\nBEGIN\nIF p_x = 2 THEN EXIT {b}; END IF;\n<<goto_a>>\nBEGIN\nIF p_x = 1 THEN EXIT {a}; END IF;\nv_y := 3;\nEND;\nv_y := v_y + 1;\nEND;\nUPDATE t SET c = v_y;")
    };
    let v = cross(t, &p("goto_a", "goto_b"));
    assert!(
        matches!(v, Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{v:?}"
    );
    // The jumps swapped: each leaves the other block.
    let v = cross(t, &p("goto_b", "goto_a"));
    assert!(
        !matches!(v, Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{v:?}"
    );
}
