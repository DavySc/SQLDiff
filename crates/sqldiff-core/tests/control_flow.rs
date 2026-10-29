//! IF and WHILE across dialects: the branches and loops are compared marker
//! against marker, and every statement only with the counterpart that sits
//! in the same branch.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

fn run(a: &str, b: &str, d: Dialect) -> Report {
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", d, b),
        &Options::default(),
    )
}

fn equiv(r: &Report) -> Vec<String> {
    r.findings
        .iter()
        .filter(|f| f.rule.starts_with("equiv."))
        .map(|f| f.witness.clone())
        .collect()
}

const TSQL: &str = "CREATE PROCEDURE dbo.p @n int
AS
BEGIN
    SET XACT_ABORT ON;
    DECLARE @x int = 0;
    IF @n > 10
        SET @x = 1;
    ELSE IF @n > 5
    BEGIN
        SET @x = 2;
        UPDATE dbo.t SET v = @x WHERE id = @n;
    END
    ELSE
        SET @x = 3;
    WHILE @x < 100
    BEGIN
        SET @x = @x * 2;
        IF @x = 64 BREAK;
    END
    UPDATE dbo.t SET v = @x WHERE id = @n;
END;
";

const PG: &str = "CREATE OR REPLACE PROCEDURE p(p_n integer)
LANGUAGE plpgsql
AS $$
DECLARE
  v_x integer := 0;
BEGIN
  IF p_n > 10 THEN
    v_x := 1;
  ELSIF p_n > 5 THEN
    v_x := 2;
    UPDATE t SET v = v_x WHERE id = p_n;
  ELSE
    v_x := 3;
  END IF;
  WHILE v_x < 100 LOOP
    v_x := v_x * 2;
    IF v_x = 64 THEN
      EXIT;
    END IF;
  END LOOP;
  UPDATE t SET v = v_x WHERE id = p_n;
END;
$$;
";

const DB2: &str = "CREATE OR REPLACE PROCEDURE p (IN p_n INTEGER)
LANGUAGE SQL
BEGIN
  DECLARE v_x INTEGER;
  DECLARE EXIT HANDLER FOR SQLEXCEPTION
  BEGIN
    ROLLBACK;
    RESIGNAL;
  END;
  SET v_x = 0;
  IF p_n > 10 THEN
    SET v_x = 1;
  ELSEIF p_n > 5 THEN
    SET v_x = 2;
    UPDATE t SET v = v_x WHERE id = p_n;
  ELSE
    SET v_x = 3;
  END IF;
  loop_1: WHILE v_x < 100 DO
    SET v_x = v_x * 2;
    IF v_x = 64 THEN
      LEAVE loop_1;
    END IF;
  END WHILE loop_1;
  UPDATE t SET v = v_x WHERE id = p_n;
END;
";

fn not_covered(r: &Report) -> Vec<&String> {
    r.unknown_reasons
        .iter()
        .filter(|x| x.contains("not covered") || x.contains("not paired"))
        .collect()
}

#[test]
fn if_elsif_else_and_while_with_break_are_compared() {
    let r = run(TSQL, PG, Dialect::Postgres);
    assert!(not_covered(&r).is_empty(), "{:#?}", r.unknown_reasons);
    assert!(equiv(&r).is_empty(), "{:#?}", r.findings);
    let r = run(TSQL, DB2, Dialect::Db2i);
    assert!(not_covered(&r).is_empty(), "{:#?}", r.unknown_reasons);
    assert!(equiv(&r).is_empty(), "{:#?}", r.findings);
}

#[test]
fn a_changed_condition_is_not_equal() {
    let r = run(
        TSQL,
        &PG.replace("ELSIF p_n > 5", "ELSIF p_n >= 5"),
        Dialect::Postgres,
    );
    let w = equiv(&r);
    assert!(w.iter().any(|x| x.contains("A line 8, B line 9")), "{w:#?}");
    assert_ne!(r.verdict(), Verdict::Equal);
}

#[test]
fn swapped_branches_are_not_equal() {
    let pg = PG
        .replace("v_x := 1;", "v_x := @@ONE@@;")
        .replace("v_x := 3;", "v_x := 1;")
        .replace("v_x := @@ONE@@;", "v_x := 3;");
    let r = run(TSQL, &pg, Dialect::Postgres);
    assert!(!equiv(&r).is_empty(), "{:#?}", r.findings);
    assert_ne!(r.verdict(), Verdict::Equal);
}

#[test]
fn continue_is_not_break() {
    let r = run(&TSQL.replace("BREAK;", "CONTINUE;"), PG, Dialect::Postgres);
    let w = equiv(&r);
    assert!(
        w.iter()
            .any(|x| x.contains("A has a CONTINUE where B has a loop exit")),
        "{w:#?}"
    );
}

#[test]
fn a_statement_moved_out_of_its_branch_is_not_paired_with_itself() {
    // The UPDATE runs only when 5 < @n <= 10 on side A, on every call on B.
    let pg = PG.replace(
        "    v_x := 2;\n    UPDATE t SET v = v_x WHERE id = p_n;\n  ELSE\n    v_x := 3;\n  END IF;\n",
        "    v_x := 2;\n  ELSE\n    v_x := 3;\n  END IF;\n  UPDATE t SET v = v_x WHERE id = p_n;\n",
    );
    let r = run(TSQL, &pg, Dialect::Postgres);
    assert_ne!(r.verdict(), Verdict::Equal);
    assert!(
        r.unknown_reasons
            .iter()
            .any(|x| x.starts_with("side A (T-SQL) line 11:")),
        "{:#?}",
        r.unknown_reasons
    );
}

#[test]
fn db2_leave_of_an_outer_loop_is_not_covered() {
    let tsql = "CREATE PROCEDURE dbo.p
AS
BEGIN
    SET XACT_ABORT ON;
    DECLARE @x int = 0;
    WHILE @x < 10
    BEGIN
        SET @x = @x + 1;
        WHILE @x < 5
        BEGIN
            SET @x = @x + 2;
            BREAK;
        END
    END
END;
";
    let db2 = "CREATE OR REPLACE PROCEDURE p ()
LANGUAGE SQL
BEGIN
  DECLARE v_x INTEGER;
  DECLARE EXIT HANDLER FOR SQLEXCEPTION
  BEGIN
    ROLLBACK;
    RESIGNAL;
  END;
  SET v_x = 0;
  loop_1: WHILE v_x < 10 DO
    SET v_x = v_x + 1;
    loop_2: WHILE v_x < 5 DO
      SET v_x = v_x + 2;
      LEAVE loop_1;
    END WHILE loop_2;
  END WHILE loop_1;
END;
";
    let r = run(tsql, db2, Dialect::Db2i);
    assert!(
        r.unknown_reasons.iter().any(|x| x.starts_with(
            "side B (Db2 for i) line 15: `LEAVE` of another loop or block is not covered"
        )),
        "{:#?}",
        r.unknown_reasons
    );
    assert_ne!(r.verdict(), Verdict::Equal);
    let r = run(
        tsql,
        &db2.replace("LEAVE loop_1", "LEAVE loop_2"),
        Dialect::Db2i,
    );
    assert!(not_covered(&r).is_empty(), "{:#?}", r.unknown_reasons);
}

#[test]
fn statements_inside_an_uncovered_catch_are_named_line_by_line() {
    // A CATCH that does not only roll back and rethrow is not compared; the
    // statements inside it were not compared either.
    let tsql = "CREATE PROCEDURE dbo.p
AS
BEGIN
    BEGIN TRY
        DELETE FROM dbo.t;
    END TRY
    BEGIN CATCH
        INSERT INTO dbo.log (msg) VALUES ('failed');
        UPDATE dbo.t SET v = 0;
    END CATCH
END;
";
    let pg = "CREATE OR REPLACE PROCEDURE p()
LANGUAGE plpgsql
AS $$
BEGIN
  DELETE FROM t;
EXCEPTION WHEN OTHERS THEN
  INSERT INTO log (msg) VALUES ('failed');
  UPDATE t SET v = 0;
END;
$$;
";
    let r = run(tsql, pg, Dialect::Postgres);
    for want in [
        "side A (T-SQL) line 8: a statement inside a CATCH block at line 7 is not covered",
        "side A (T-SQL) line 9: a statement inside a CATCH block at line 7 is not covered",
    ] {
        assert!(
            r.unknown_reasons.iter().any(|x| x.starts_with(want)),
            "{want}\n{:#?}",
            r.unknown_reasons
        );
    }
}

#[test]
fn an_error_before_a_branch_names_what_may_run_after_it() {
    // Without XACT_ABORT the T-SQL procedure goes on after a failed
    // statement; what runs next is decided by the IF.
    let tsql = "CREATE PROCEDURE dbo.p @n int
AS
BEGIN
    UPDATE dbo.t SET v = 1 WHERE id = @n;
    IF @n > 0
        INSERT INTO dbo.log (msg) VALUES ('x');
END;
";
    let pg = "CREATE OR REPLACE PROCEDURE p(p_n integer)
LANGUAGE plpgsql
AS $$
BEGIN
  UPDATE t SET v = 1 WHERE id = p_n;
  IF p_n > 0 THEN
    INSERT INTO log (msg) VALUES ('x');
  END IF;
END;
$$;
";
    let r = run(tsql, pg, Dialect::Postgres);
    assert!(
        r.unknown_reasons.iter().any(|x| x.starts_with(
            "side A (T-SQL) line 4: if this statement fails, side A (T-SQL) reports the error and goes on"
        ) && x.contains("the statement at line 6 may run after it")),
        "{:#?}",
        r.unknown_reasons
    );
    assert!(
        !r.findings.iter().any(|f| f.rule == "equiv.error-continues"),
        "{:#?}",
        r.findings
    );
    // An effect after the IF runs whatever the IF decides.
    let r = run(
        &tsql.replace("END;\n", "    DELETE FROM dbo.t WHERE id = @n;\nEND;\n"),
        &pg.replace(
            "  END IF;\n",
            "  END IF;\n  DELETE FROM t WHERE id = p_n;\n",
        ),
        Dialect::Postgres,
    );
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == "equiv.error-continues")
        .expect("error-continues finding");
    assert!(
        f.witness
            .contains("fails, side A (T-SQL) reports the error and continues with line 7"),
        "{}",
        f.witness
    );
}

#[test]
fn a_tsql_declaration_inside_a_branch_is_its_initial_assignment() {
    let tsql = "CREATE PROCEDURE dbo.p @n int
AS
BEGIN
    SET XACT_ABORT ON;
    IF @n > 0
    BEGIN
        DECLARE @x int = @n * 2;
        UPDATE dbo.t SET v = @x WHERE id = @n;
    END
END;
";
    let pg = "CREATE OR REPLACE PROCEDURE p(p_n integer)
LANGUAGE plpgsql
AS $$
DECLARE
  v_x integer;
BEGIN
  IF p_n > 0 THEN
    v_x := p_n * 2;
    UPDATE t SET v = v_x WHERE id = p_n;
  END IF;
END;
$$;
";
    let r = run(tsql, pg, Dialect::Postgres);
    assert!(not_covered(&r).is_empty(), "{:#?}", r.unknown_reasons);
    assert!(equiv(&r).is_empty(), "{:#?}", r.findings);
    // A PostgreSQL block inside the IF declares its variable again each
    // time it runs: not compared.
    let pg = pg.replace(
        "    v_x := p_n * 2;\n",
        "    DECLARE\n      v_y integer;\n    BEGIN\n      v_y := 1;\n    END;\n    v_x := p_n * 2;\n",
    );
    let r = run(tsql, &pg, Dialect::Postgres);
    assert!(
        r.unknown_reasons
            .iter()
            .any(|x| x.contains("a declaration inside an IF or a loop")),
        "{:#?}",
        r.unknown_reasons
    );
}
