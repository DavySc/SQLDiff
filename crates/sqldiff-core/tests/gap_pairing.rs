//! Statement pairing across dialects when one side holds a construct the
//! comparison leaves out (a loop block, a not-converted placeholder): the
//! statements after it are compared with their own counterparts, never one
//! position off.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report};

fn run(a: &str, b: &str) -> Report {
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Postgres, b),
        &Options::default(),
    )
}

/// `(A line, B line)` of every statement pair the engine reported on.
fn compared(r: &Report) -> Vec<(usize, usize)> {
    r.findings
        .iter()
        .filter(|f| f.rule.starts_with("equiv."))
        .filter_map(|f| Some((f.a.as_ref()?.start_line, f.b.as_ref()?.start_line)))
        .collect()
}

const TSQL: &str = "CREATE PROCEDURE dbo.p @id int
AS
BEGIN
    DECLARE @x int = 5;
    SELECT @x = v FROM dbo.t WHERE id = @id GROUP BY v;
    SET @x = @x + 1;
    SET @x = @x * 2;
    UPDATE dbo.t SET v = @x WHERE id = @id;
END;
";

/// The converter's output for `TSQL`: the grouped assignment became a loop
/// block (lines 8 to 14).
const PG: &str = "CREATE OR REPLACE PROCEDURE p(p_id integer)
LANGUAGE plpgsql
AS $$
DECLARE
  v_x integer;
BEGIN
  v_x := 5;
  DECLARE
  sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT v AS sqldiff_c1 FROM t WHERE id = p_id GROUP BY v LOOP
  v_x := sqldiff_r.sqldiff_c1;
  END LOOP;
  END;
  v_x := v_x + 1;
  v_x := v_x * 2;
  UPDATE t SET v = v_x WHERE id = p_id;
END;
$$;
";

/// `PG` with a loop block the engine does not read (its body assigns an
/// expression, not the row's column), which the comparison leaves out.
fn pg_gap() -> String {
    PG.replace(
        "v_x := sqldiff_r.sqldiff_c1;",
        "v_x := sqldiff_r.sqldiff_c1 + 0;",
    )
}

#[test]
fn the_converted_loop_block_is_compared_with_its_select() {
    let r = run(TSQL, PG);
    assert!(
        !r.unknown_reasons.iter().any(|x| x.contains("not paired")
            || x.contains("no counterpart")
            || x.contains("not covered")),
        "{:#?}",
        r.unknown_reasons
    );
    // The one finding: T-SQL goes on after a failing statement (no SET
    // XACT_ABORT ON), PostgreSQL stops.
    let rules: Vec<&str> = r.findings.iter().map(|f| f.rule.as_str()).collect();
    assert_eq!(rules, vec!["equiv.error-continues"], "{:#?}", r.findings);
}

#[test]
fn statements_after_a_loop_block_meet_their_own_counterparts() {
    let r = run(TSQL, &pg_gap());
    // Before: A line 5 was compared with B line 15 and A line 6 with B line
    // 16, two spurious findings.
    assert_eq!(
        compared(&r),
        Vec::<(usize, usize)>::new(),
        "{:#?}",
        r.findings
    );
    assert!(
        r.unknown_reasons.iter().any(|x| x.starts_with(
            "side A (T-SQL) line 5: statement is not paired: its counterpart may be in a loop at side B (PostgreSQL) line 11"
        )),
        "{:#?}",
        r.unknown_reasons
    );
    // Nothing else is left without a counterpart.
    assert!(
        !r.unknown_reasons
            .iter()
            .any(|x| x.contains("no counterpart") || x.contains("more than one way")),
        "{:#?}",
        r.unknown_reasons
    );
}

#[test]
fn a_real_difference_after_a_loop_block_names_the_right_lines() {
    let r = run(
        TSQL,
        &pg_gap().replace("v_x := v_x * 2;", "v_x := v_x * 3;"),
    );
    assert_eq!(compared(&r), vec![(7, 16)], "{:#?}", r.findings);
    let f = r
        .findings
        .iter()
        .find(|f| f.rule.starts_with("equiv."))
        .unwrap();
    assert!(
        f.witness.contains("A has `2` where B has `3`"),
        "{}",
        f.witness
    );
}

#[test]
fn a_not_converted_placeholder_stands_for_its_statement() {
    let tsql = "CREATE PROCEDURE dbo.p @id int
AS
BEGIN
    DECLARE @x int = 1;
    UPDATE dbo.t SET v = SUSER_SNAME() WHERE id = @id;
    SET @x = @x + 1;
    SET @x = @x * 2;
END;
";
    let pg = "CREATE OR REPLACE PROCEDURE p(p_id integer)
LANGUAGE plpgsql
AS $$
DECLARE
  v_x integer;
BEGIN
  v_x := 1;
  RAISE EXCEPTION 'sqldiff: statement at source line 5 was not converted';
  v_x := v_x + 1;
  v_x := v_x * 2;
END;
$$;
";
    let r = run(tsql, pg);
    assert_eq!(
        compared(&r),
        Vec::<(usize, usize)>::new(),
        "{:#?}",
        r.findings
    );
    assert!(
        r.unknown_reasons.iter().any(|x| x.starts_with(
            "side A (T-SQL) line 5: statement is not paired: its counterpart may be in an error raise"
        )),
        "{:#?}",
        r.unknown_reasons
    );
}

#[test]
fn statements_that_line_up_in_two_ways_are_not_paired() {
    // `SET @x = 1` could be the first loop and `SET @x = 2` the assignment,
    // or the other way round: neither is compared.
    let tsql = "CREATE PROCEDURE dbo.p
AS
BEGIN
    DECLARE @x int;
    SET @x = 1;
    SET @x = 2;
END;
";
    let pg = "CREATE OR REPLACE PROCEDURE p()
LANGUAGE plpgsql
AS $$
DECLARE
  v_x integer;
BEGIN
  WHILE v_x < 3 LOOP
    v_x := v_x + 1;
  END LOOP;
  v_x := 2;
  WHILE v_x < 5 LOOP
    v_x := v_x + 1;
  END LOOP;
END;
$$;
";
    let r = run(tsql, pg);
    assert_eq!(
        compared(&r),
        Vec::<(usize, usize)>::new(),
        "{:#?}",
        r.findings
    );
    for line in [5, 6] {
        let want = format!(
            "side A (T-SQL) line {line}: statement is not paired: the statements around it line up with the other side in more than one way"
        );
        assert!(
            r.unknown_reasons.contains(&want),
            "{want}\n{:#?}",
            r.unknown_reasons
        );
    }
}

/// Anchors linking each `(A text, B text)`: the byte spans of the first
/// occurrence of each.
fn anchors(a: &str, b: &str, links: &[(&str, &str)]) -> Vec<sqldiff_core::equiv::Anchor> {
    let span = |t: &str, s: &str| {
        let at = t.find(s).unwrap_or_else(|| panic!("{s} not in {t}"));
        (at, at + s.len())
    };
    links
        .iter()
        .map(|(x, y)| sqldiff_core::equiv::Anchor {
            a: span(a, x),
            b: span(b, y),
        })
        .collect()
}

fn run_traced(a: &str, b: &str, links: &[(&str, &str)]) -> Report {
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Postgres, b),
        &Options {
            anchors: anchors(a, b, links),
            ..Options::default()
        },
    )
}

const TWO_SETS: &str = "CREATE PROCEDURE dbo.p
AS
BEGIN
    DECLARE @x int;
    SET @x = 1;
    SET @x = 2;
END;
";

#[test]
fn the_trace_pairs_a_statement_with_the_output_written_from_it() {
    // The conversion dropped `SET @x = 1` (a dead store): by position or
    // by key, `SET @x = 1` would meet `v_x := 2` and differ.
    let pg = "CREATE OR REPLACE PROCEDURE p()
LANGUAGE plpgsql
AS $$
DECLARE
  v_x integer;
BEGIN
  -- SET @x = 1: dropped
  v_x := 2;
END;
$$;
";
    let untraced = run(TWO_SETS, pg);
    assert_eq!(
        compared(&untraced),
        vec![(5, 8)],
        "{:#?}",
        untraced.findings
    );
    let r = run_traced(
        TWO_SETS,
        pg,
        &[
            ("SET @x = 1;", "-- SET @x = 1: dropped"),
            ("SET @x = 2;", "v_x := 2;"),
        ],
    );
    assert_eq!(
        compared(&r),
        Vec::<(usize, usize)>::new(),
        "{:#?}",
        r.findings
    );
    assert!(
        r.unknown_reasons.contains(
            &"side A (T-SQL) line 5: statement has no counterpart on the other side".to_string()
        ),
        "{:#?}",
        r.unknown_reasons
    );
}

#[test]
fn the_trace_never_pairs_statements_it_does_not_link() {
    // Same number of statements on both sides, so they would be paired by
    // position; the trace says each was written from the other one, out
    // of order. Comparing them one by one says nothing about the order, so
    // neither pair is compared.
    let pg = "CREATE OR REPLACE PROCEDURE p()
LANGUAGE plpgsql
AS $$
DECLARE
  v_x integer;
BEGIN
  v_x := 2;
  v_x := 1;
END;
$$;
";
    let r = run_traced(
        TWO_SETS,
        pg,
        &[("SET @x = 1;", "v_x := 1;"), ("SET @x = 2;", "v_x := 2;")],
    );
    assert_eq!(
        compared(&r),
        Vec::<(usize, usize)>::new(),
        "{:#?}",
        r.findings
    );
    for line in [5, 6] {
        let want = format!(
            "side A (T-SQL) line {line}: statement is not paired: the conversion put it in another order among the statements it is paired with"
        );
        assert!(
            r.unknown_reasons.contains(&want),
            "{want}\n{:#?}",
            r.unknown_reasons
        );
    }
    assert_ne!(r.verdict(), sqldiff_core::Verdict::Equal);
    // A statement in no anchor has no counterpart.
    let r = run_traced(TWO_SETS, pg, &[("SET @x = 2;", "v_x := 2;")]);
    assert!(
        r.unknown_reasons.contains(
            &"side A (T-SQL) line 5: statement has no counterpart on the other side".to_string()
        ),
        "{:#?}",
        r.unknown_reasons
    );
}
