//! T-SQL `IF @@ROWCOUNT = 0` (`<> 0`) against PostgreSQL `IF NOT FOUND`
//! (`FOUND`): both tell whether the statement right before touched or
//! returned a row (SQL Server 2022 and PostgreSQL 18, also with an AFTER
//! trigger writing other rows). Equal only when that statement comes right
//! before the IF on both sides and is one whose rows both count.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report};

fn run(a: &str, b: &str) -> Report {
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Postgres, b),
        &Options::default(),
    )
}

fn tsql(body: &str) -> String {
    format!(
        "CREATE PROCEDURE dbo.p @id int, @v int
AS
BEGIN
    SET XACT_ABORT ON;
{body}
END;
"
    )
}

fn pg(body: &str) -> String {
    format!(
        "CREATE OR REPLACE PROCEDURE p(p_id integer, p_v integer)
LANGUAGE plpgsql
AS $$
DECLARE
  v_n integer;
BEGIN
{body}
END;
$$;
"
    )
}

/// Whether the engine shows the two equal (no `equiv.*` finding), and
/// names ASM-ROWCOUNT-FOUND when it does.
fn equal(a: &str, b: &str) -> bool {
    let r = run(&tsql(a), &pg(b));
    let unproven: Vec<_> = r
        .findings
        .iter()
        .filter(|f| f.rule.starts_with("equiv."))
        .map(|f| f.witness.clone())
        .collect();
    if unproven.is_empty() {
        assert!(
            r.assumptions.iter().any(|x| x.id == "ASM-ROWCOUNT-FOUND"),
            "equal without ASM-ROWCOUNT-FOUND:\n{a}\n{b}"
        );
    }
    unproven.is_empty()
}

const UPDATE_A: &str = "    UPDATE dbo.t SET v = @v WHERE id = @id;";
const UPDATE_B: &str = "  UPDATE t SET v = p_v WHERE id = p_id;";

#[test]
fn rowcount_after_dml_is_found() {
    for (a, b) in [
        (
            "IF @@ROWCOUNT = 0 RETURN;",
            "IF NOT FOUND THEN RETURN; END IF;",
        ),
        (
            "IF @@ROWCOUNT <> 0 RETURN;",
            "IF FOUND THEN RETURN; END IF;",
        ),
        ("IF @@ROWCOUNT > 0 RETURN;", "IF FOUND THEN RETURN; END IF;"),
        (
            "IF (@@ROWCOUNT = 0) RETURN;",
            "IF NOT FOUND THEN RETURN; END IF;",
        ),
    ] {
        assert!(
            equal(
                &format!("{UPDATE_A}\n    {a}"),
                &format!("{UPDATE_B}\n  {b}")
            ),
            "{a} / {b}"
        );
    }
    for kw in [
        "INSERT INTO dbo.t (id, v) VALUES (@id, @v);",
        "DELETE FROM dbo.t WHERE id = @id;",
    ] {
        let b = kw
            .replace("dbo.", "")
            .replace("@id", "p_id")
            .replace("@v", "p_v");
        assert!(equal(
            &format!("    {kw}\n    IF @@ROWCOUNT = 0 RETURN;"),
            &format!("  {b}\n  IF NOT FOUND THEN RETURN; END IF;")
        ));
    }
}

/// PL/pgSQL's COMMIT keeps FOUND (PostgreSQL 18); the converter writes one
/// after each statement of an autocommit routine.
#[test]
fn a_postgresql_commit_in_between_keeps_found() {
    assert!(equal(
        &format!("{UPDATE_A}\n    IF @@ROWCOUNT = 0 RETURN;"),
        &format!("{UPDATE_B}\n  COMMIT;\n  IF NOT FOUND THEN RETURN; END IF;")
    ));
    assert!(!equal(
        &format!("{UPDATE_A}\n    IF @@ROWCOUNT = 0 RETURN;"),
        &format!("{UPDATE_B}\n  COMMIT AND CHAIN;\n  IF NOT FOUND THEN RETURN; END IF;")
    ));
}

#[test]
fn rowcount_after_select_assign_is_found() {
    // The converter's block for a SELECT that assigns variables.
    let b = "  DECLARE
  sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT v AS sqldiff_c1 FROM t WHERE id > p_id ORDER BY id NULLS FIRST LIMIT 1 LOOP
  p_v := sqldiff_r.sqldiff_c1;
  END LOOP;
  END;";
    let a = "    SELECT TOP 1 @v = v FROM dbo.t WHERE id > @id ORDER BY id;";
    assert!(equal(
        &format!("{a}\n    IF @@ROWCOUNT = 0 RETURN;"),
        &format!("{b}\n  IF NOT FOUND THEN RETURN; END IF;")
    ));
    // At most one row: a count of 1 is a row found.
    assert!(equal(
        &format!("{a}\n    IF @@ROWCOUNT = 1 RETURN;"),
        &format!("{b}\n  IF FOUND THEN RETURN; END IF;")
    ));
    // Without TOP 1 a count of 1 is not "a row found".
    let many = "    SELECT @v = v FROM dbo.t WHERE id > @id;";
    assert!(!equal(
        &format!("{many}\n    IF @@ROWCOUNT = 1 RETURN;"),
        &format!("{b}\n  IF FOUND THEN RETURN; END IF;")
    ));
}

#[test]
fn rowcount_kept_apart() {
    for (a, b) in [
        // The opposite test.
        (format!("{UPDATE_A}\n    IF @@ROWCOUNT = 0 RETURN;"), format!("{UPDATE_B}\n  IF FOUND THEN RETURN; END IF;")),
        // A statement in between resets @@ROWCOUNT (T-SQL) or may set
        // FOUND (PostgreSQL).
        (
            format!("{UPDATE_A}\n    SET @v = 1;\n    IF @@ROWCOUNT = 0 RETURN;"),
            format!("{UPDATE_B}\n  p_v := 1;\n  IF NOT FOUND THEN RETURN; END IF;"),
        ),
        (
            format!("{UPDATE_A}\n    IF @@ROWCOUNT = 0 RETURN;"),
            format!("{UPDATE_B}\n  PERFORM 1;\n  IF NOT FOUND THEN RETURN; END IF;"),
        ),
        // GET DIAGNOSTICS, not FOUND, and no other comparison of the count.
        (
            format!("{UPDATE_A}\n    IF @@ROWCOUNT = 0 RETURN;"),
            format!("{UPDATE_B}\n  GET DIAGNOSTICS v_n = ROW_COUNT;\n  IF v_n = 0 THEN RETURN; END IF;"),
        ),
        (format!("{UPDATE_A}\n    IF @@ROWCOUNT = 2 RETURN;"), format!("{UPDATE_B}\n  IF FOUND THEN RETURN; END IF;")),
        // The statements differ in kind.
        (
            format!("{UPDATE_A}\n    IF @@ROWCOUNT = 0 RETURN;"),
            "  DELETE FROM t WHERE id = p_id;\n  IF NOT FOUND THEN RETURN; END IF;".to_string(),
        ),
        // RETURNING and OUTPUT are left out.
        (
            format!("{UPDATE_A}\n    IF @@ROWCOUNT = 0 RETURN;"),
            "  UPDATE t SET v = p_v WHERE id = p_id RETURNING v INTO v_n;\n  IF NOT FOUND THEN RETURN; END IF;".to_string(),
        ),
        // Not right after a statement of the block.
        (
            "    IF @@ROWCOUNT = 0 RETURN;".to_string(),
            "  IF NOT FOUND THEN RETURN; END IF;".to_string(),
        ),
    ] {
        assert!(!equal(&a, &b), "shown equal:\n{a}\n{b}");
    }
}
