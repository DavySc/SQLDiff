//! Condition identity (perf plan W6a): a row-count test is named by the
//! statement it reads and means "no rows from it" whichever way the dialect
//! spells it, and outcome pairs no input can produce are not compared.
use sqldiff_core::{diff, Artifact, Dialect, Options, Verdict};

fn run(a: &str, b: &str) -> Vec<(String, Verdict)> {
    let r = diff(
        &Artifact::new("a.sql", Dialect::Tsql, a.to_string()),
        &Artifact::new("b.sql", Dialect::Tsql, b.to_string()),
        &Options::default(),
    );
    r.findings
        .iter()
        .filter(|f| f.rule.starts_with("txn.") || f.rule.starts_with("error."))
        .map(|f| (f.rule.to_string(), f.verdict))
        .collect()
}

const HEAD: &str =
    "CREATE PROCEDURE dbo.p @i int AS\nBEGIN\n  UPDATE dbo.t SET a = 1 WHERE id = @i;\n";

/// The same routine with the row-count test written the other way round.
#[test]
fn a_row_count_test_means_the_same_in_either_polarity() {
    let a = format!(
        "{HEAD}  IF @@ROWCOUNT = 0\n    INSERT INTO dbo.t (id, a) VALUES (@i, 1);\nEND\nGO\n"
    );
    let b = format!("{HEAD}  IF @@ROWCOUNT > 0\n    PRINT 'x';\n  ELSE\n    INSERT INTO dbo.t (id, a) VALUES (@i, 1);\nEND\nGO\n");
    let same = run(&a, &b);
    assert!(
        same.iter().all(|(_, v)| *v != Verdict::Different),
        "{same:?}"
    );
}

/// And the polarity is not ignored: inserting when the update found rows is
/// not the same routine.
#[test]
fn the_opposite_branch_is_still_found_different() {
    let a = format!(
        "{HEAD}  IF @@ROWCOUNT = 0\n    INSERT INTO dbo.t (id, a) VALUES (@i, 1);\nEND\nGO\n"
    );
    let b = format!(
        "{HEAD}  IF @@ROWCOUNT > 0\n    INSERT INTO dbo.t (id, a) VALUES (@i, 1);\nEND\nGO\n"
    );
    let r = diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Tsql, b),
        &Options::default(),
    );
    assert!(
        r.findings
            .iter()
            .any(|f| f.verdict == Verdict::Different || f.verdict == Verdict::Unknown),
        "an inverted row-count test must not come out equal: {:?}",
        r.findings
            .iter()
            .map(|f| (&f.rule, f.verdict))
            .collect::<Vec<_>>()
    );
}

/// A row-count test read after a failed statement is not the same test in
/// the two dialects: T-SQL `@@ROWCOUNT` is 0 after an error, PG `FOUND` is
/// untouched by a statement that never completed. Both sides continue past
/// the fault (T-SQL has no XACT_ABORT, PG swallows it), so the pair (A true,
/// B false) is real and the durability/control divergence must still show.
#[test]
fn a_row_count_test_after_a_failed_statement_is_not_pruned_across_dialects() {
    let a = "CREATE PROCEDURE dbo.p @id int, @v int AS\nBEGIN\n  SELECT @v = a FROM dbo.t WHERE id = 1;\n  UPDATE dbo.t SET a = @v WHERE id = @id;\n  IF @@ROWCOUNT = 0\n    INSERT INTO dbo.log (id) VALUES (@id);\nEND\nGO\n";
    let b = "CREATE OR REPLACE PROCEDURE p(p_id integer, p_v integer) LANGUAGE plpgsql AS $$\nBEGIN\n  SELECT a INTO p_v FROM t WHERE id = 1;\n  BEGIN\n    UPDATE t SET a = p_v WHERE id = p_id;\n  EXCEPTION WHEN OTHERS THEN NULL;\n  END;\n  IF NOT FOUND THEN INSERT INTO log (id) VALUES (p_id); END IF;\n  IF p_v > 0 THEN RAISE NOTICE 'x'; END IF;\nEND; $$;\n";
    let r = diff(
        &Artifact::new("a.sql", Dialect::Tsql, a.to_string()),
        &Artifact::new("b.sql", Dialect::Postgres, b.to_string()),
        &Options::default(),
    );
    assert!(
        r.findings.iter().any(|f| f.rule == "txn.durability"
            && f.verdict == Verdict::Unknown
            && f.witness.contains("constraint violation")
            && f.witness
                .contains("side A (T-SQL) keeps the INSERT into log")),
        "{:?}",
        r.findings
            .iter()
            .map(|f| (&f.rule, f.verdict, &f.witness))
            .collect::<Vec<_>>()
    );
}
