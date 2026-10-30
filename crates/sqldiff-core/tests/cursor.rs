//! T-SQL cursor loops compared with the PL/pgSQL loops they became
//! (`equiv::cursor`): the query of the OPEN, the assignments of each FETCH,
//! the loop test, and the conditions that make them mean the same (the
//! FETCH right before each test, no read of the fetched variables after the
//! loop, no write to the rows a cursor that is not STATIC still has to
//! fetch, no transaction control in the loop). Each has an equal pair and a
//! near miss.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const CATALOG_T: &str = "CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, v int NULL);\nCREATE TABLE dbo.log (id int NOT NULL, v int NULL);";
const CATALOG_P: &str = "CREATE TABLE t (id integer NOT NULL PRIMARY KEY, v integer NULL);\nCREATE TABLE log (id integer NOT NULL, v integer NULL);";

/// A T-SQL procedure with `body`, against a PostgreSQL one with `body_p`.
/// Both declare `@id int`, `@v int`, `@total int = 0`, `@min int = 1`; the
/// PostgreSQL one also `cur_c refcursor`.
fn run(body: &str, body_p: &str) -> Report {
    let a = format!(
        "CREATE PROCEDURE dbo.p\nAS\nBEGIN\n  SET NOCOUNT ON;\n  SET XACT_ABORT ON;\n  DECLARE @id int;\n  DECLARE @v int;\n  DECLARE @total int = 0;\n  DECLARE @min int = 1;\n{body}\nEND\n"
    );
    let b = format!(
        "CREATE OR REPLACE PROCEDURE p()\nLANGUAGE plpgsql\nAS $$\nDECLARE\n  v_id integer;\n  v_v integer;\n  v_total integer;\n  v_min integer;\n  cur_c refcursor;\nBEGIN\n  v_total := 0;\n  v_min := 1;\n{body_p}\nEND;\n$$;\n"
    );
    let opts = Options {
        catalog_a: vec![Artifact::new("ca.sql", Dialect::Tsql, CATALOG_T)],
        catalog_b: vec![Artifact::new("cb.sql", Dialect::Postgres, CATALOG_P)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, &a),
        &Artifact::new("b.sql", Dialect::Postgres, &b),
        &opts,
    )
}

fn ids(r: &Report) -> Vec<&str> {
    r.assumptions.iter().map(|x| x.id.as_str()).collect()
}

/// Equal under assumptions, nothing undecided.
fn equal(body: &str, body_p: &str) -> Report {
    let r = run(body, body_p);
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{body}\n{:#?}\n{:#?}",
        r.findings,
        r.unknown_reasons
    );
    assert!(r.unknown_reasons.is_empty(), "{:?}", r.unknown_reasons);
    r
}

fn not_equal(body: &str, body_p: &str) -> Report {
    let r = run(body, body_p);
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{body}\n{body_p}\n{:?}",
        ids(&r)
    );
    r
}

/// Not equal, and a reason names `why`.
fn refused(body: &str, body_p: &str, why: &str) {
    let r = not_equal(body, body_p);
    assert!(
        r.unknown_reasons.iter().any(|x| x.contains(why)),
        "{why}\n{:#?}",
        r.unknown_reasons
    );
}

const T_DECL: &str =
    "  DECLARE c CURSOR LOCAL FAST_FORWARD FOR SELECT id, v FROM dbo.t ORDER BY id;";
const T_LOOP: &str = "  OPEN c;\n  FETCH NEXT FROM c INTO @id, @v;\n  WHILE @@FETCH_STATUS = 0\n  BEGIN\n    SET @total = @total + @v;\n    FETCH NEXT FROM c INTO @id, @v;\n  END\n  CLOSE c;\n  DEALLOCATE c;\n  INSERT INTO dbo.log (id, v) VALUES (0, @total);";
const P_OPEN: &str = "  OPEN cur_c FOR SELECT id, v FROM t ORDER BY id;";
const P_LOOP: &str = "  FETCH NEXT FROM cur_c INTO v_id, v_v;\n  WHILE FOUND LOOP\n    v_total := v_total + v_v;\n    FETCH NEXT FROM cur_c INTO v_id, v_v;\n  END LOOP;\n  CLOSE cur_c;\n  INSERT INTO log (id, v) VALUES (0, v_total);";

fn t(decl: &str, rest: &str) -> String {
    format!("{decl}\n{rest}")
}

fn p(open: &str, rest: &str) -> String {
    format!("{open}\n{rest}")
}

#[test]
fn the_standard_loop() {
    let r = equal(&t(T_DECL, T_LOOP), &p(P_OPEN, P_LOOP));
    // Other sessions; ORDER BY id is the key, LOCAL needs nothing.
    assert!(ids(&r).contains(&"ASM-CURSOR-SNAPSHOT"), "{:?}", ids(&r));
    assert!(!ids(&r).contains(&"ASM-CURSOR-ORDER"), "{:?}", ids(&r));
    assert!(!ids(&r).contains(&"ASM-CURSOR-GLOBAL"), "{:?}", ids(&r));
    // FETCH without NEXT FROM, STATIC, parentheses around the test.
    equal(
        &t(
            "  DECLARE c CURSOR LOCAL STATIC FOR SELECT id, v FROM dbo.t ORDER BY id;",
            &T_LOOP
                .replace("FETCH NEXT FROM c", "FETCH c")
                .replace("WHILE @@FETCH_STATUS = 0", "WHILE (@@FETCH_STATUS = 0)"),
        ),
        &p(P_OPEN, P_LOOP),
    );
}

#[test]
fn the_query_and_its_order() {
    not_equal(
        &t(T_DECL, T_LOOP),
        &p(
            "  OPEN cur_c FOR SELECT id, v FROM t ORDER BY id DESC;",
            P_LOOP,
        ),
    );
    not_equal(
        &t(T_DECL, T_LOOP),
        &p(
            "  OPEN cur_c FOR SELECT id, v FROM t WHERE v > 0 ORDER BY id;",
            P_LOOP,
        ),
    );
    // Without ORDER BY the rows may come in another order on each side.
    let r = equal(
        &t(
            "  DECLARE c CURSOR LOCAL FAST_FORWARD FOR SELECT id, v FROM dbo.t;",
            T_LOOP,
        ),
        &p("  OPEN cur_c FOR SELECT id, v FROM t;", P_LOOP),
    );
    assert!(ids(&r).contains(&"ASM-CURSOR-ORDER"), "{:?}", ids(&r));
    // A GLOBAL cursor stays open after an error.
    let r = equal(
        &t(
            "  DECLARE c CURSOR FAST_FORWARD FOR SELECT id, v FROM dbo.t ORDER BY id;",
            T_LOOP,
        ),
        &p(P_OPEN, P_LOOP),
    );
    assert!(ids(&r).contains(&"ASM-CURSOR-GLOBAL"), "{:?}", ids(&r));
}

#[test]
fn the_fetch_assignments() {
    // The columns go to the variables in order.
    not_equal(
        &t(T_DECL, T_LOOP),
        &p(P_OPEN, &P_LOOP.replace("INTO v_id, v_v", "INTO v_v, v_id")),
    );
}

#[test]
fn the_loop_test_reads_the_fetch_before_it() {
    // Another test than FOUND.
    not_equal(
        &t(T_DECL, T_LOOP),
        &p(
            P_OPEN,
            &P_LOOP.replace("WHILE FOUND LOOP", "WHILE v_id IS NOT NULL LOOP"),
        ),
    );
    // The body does not end with the FETCH.
    let body = "  OPEN c;\n  FETCH NEXT FROM c INTO @id, @v;\n  WHILE @@FETCH_STATUS = 0\n  BEGIN\n    FETCH NEXT FROM c INTO @id, @v;\n    SET @total = @total + @v;\n  END\n  CLOSE c;\n  DEALLOCATE c;\n  INSERT INTO dbo.log (id, v) VALUES (0, @total);";
    refused(
        &t(T_DECL, body),
        &p(P_OPEN, P_LOOP),
        "does not end with the same FETCH",
    );
    // CONTINUE goes back to the test without the FETCH.
    let body = T_LOOP.replace(
        "    SET @total = @total + @v;\n",
        "    SET @total = @total + @v;\n    IF @v IS NULL CONTINUE;\n",
    );
    refused(
        &t(T_DECL, &body),
        &p(P_OPEN, P_LOOP),
        "CONTINUE in a cursor loop",
    );
}

#[test]
fn fetched_variables_are_not_read_after_the_loop() {
    // T-SQL keeps the last row's @id, PostgreSQL's last FETCH sets NULL.
    refused(
        &t(
            T_DECL,
            &T_LOOP.replace("VALUES (0, @total)", "VALUES (@id, @total)"),
        ),
        &p(
            P_OPEN,
            &P_LOOP.replace("VALUES (0, v_total)", "VALUES (v_id, v_total)"),
        ),
        "may be read after the cursor loop",
    );
    // Set again first: equal.
    equal(
        &t(
            T_DECL,
            &T_LOOP.replace(
                "  INSERT INTO dbo.log (id, v) VALUES (0, @total);",
                "  SET @id = 0;\n  INSERT INTO dbo.log (id, v) VALUES (@id, @total);",
            ),
        ),
        &p(
            P_OPEN,
            &P_LOOP.replace(
                "  INSERT INTO log (id, v) VALUES (0, v_total);",
                "  v_id := 0;\n  INSERT INTO log (id, v) VALUES (v_id, v_total);",
            ),
        ),
    );
    // Set again inside an IF only: the other path reads the stale value.
    refused(
        &t(
            T_DECL,
            &T_LOOP.replace(
                "  INSERT INTO dbo.log (id, v) VALUES (0, @total);",
                "  IF @total > 0 SET @id = 0;\n  INSERT INTO dbo.log (id, v) VALUES (@id, @total);",
            ),
        ),
        &p(
            P_OPEN,
            &P_LOOP.replace(
                "  INSERT INTO log (id, v) VALUES (0, v_total);",
                "  IF v_total > 0 THEN\n    v_id := 0;\n  END IF;\n  INSERT INTO log (id, v) VALUES (v_id, v_total);",
            ),
        ),
        "may be read after the cursor loop",
    );
    // Read in an IF or WHILE condition before it is set again.
    refused(
        &t(
            T_DECL,
            &T_LOOP.replace(
                "  INSERT INTO dbo.log (id, v) VALUES (0, @total);",
                "  IF @v IS NULL INSERT INTO dbo.log (id, v) VALUES (0, @total);\n  SET @v = 0;",
            ),
        ),
        &p(
            P_OPEN,
            &P_LOOP.replace(
                "  INSERT INTO log (id, v) VALUES (0, v_total);",
                "  IF v_v IS NULL THEN\n    INSERT INTO log (id, v) VALUES (0, v_total);\n  END IF;\n  v_v := 0;",
            ),
        ),
        "may be read after the cursor loop",
    );
    refused(
        &t(
            T_DECL,
            &T_LOOP.replace(
                "  INSERT INTO dbo.log (id, v) VALUES (0, @total);",
                "  WHILE @v IS NULL SET @total = 1;\n  SET @v = 0;",
            ),
        ),
        &p(
            P_OPEN,
            &P_LOOP.replace(
                "  INSERT INTO log (id, v) VALUES (0, v_total);",
                "  WHILE v_v IS NULL LOOP\n    v_total := 1;\n  END LOOP;\n  v_v := 0;",
            ),
        ),
        "may be read after the cursor loop",
    );
}

#[test]
fn a_fetch_names_why_its_query_was_not_read() {
    let r = not_equal(
        &t(
            "  DECLARE c CURSOR LOCAL FAST_FORWARD FOR SELECT id, v FROM dbo.t UNION ALL SELECT id, v FROM dbo.log ORDER BY id;",
            T_LOOP,
        ),
        &p(
            "  OPEN cur_c FOR SELECT id, v FROM t UNION ALL SELECT id, v FROM log ORDER BY id;",
            P_LOOP,
        ),
    );
    let texts: Vec<&str> = r
        .unknown_reasons
        .iter()
        .map(String::as_str)
        .chain(r.findings.iter().map(|f| f.witness.as_str()))
        .collect();
    assert!(
        texts.iter().any(|x| x
            .contains("a FETCH of a cursor whose query was not read (in its OPEN: set operations")),
        "{texts:#?}"
    );
    assert!(
        !texts.iter().any(|x| x.contains("OPEN was not read")),
        "{texts:#?}"
    );
}

#[test]
fn a_cursor_that_is_not_static_sees_the_loops_writes() {
    // Nothing after the loop: a failing statement there would keep T-SQL's
    // autocommitted UPDATEs and undo PostgreSQL's (the behaviour model's
    // finding, not this rule's).
    let write = T_LOOP
        .replace(
            "    SET @total = @total + @v;\n",
            "    UPDATE dbo.t SET v = 0 WHERE id = @id;\n",
        )
        .replace("\n  INSERT INTO dbo.log (id, v) VALUES (0, @total);", "");
    let write_p = P_LOOP
        .replace(
            "    v_total := v_total + v_v;\n",
            "    UPDATE t SET v = 0 WHERE id = v_id;\n",
        )
        .replace("\n  INSERT INTO log (id, v) VALUES (0, v_total);", "");
    refused(
        &t(T_DECL, &write),
        &p(P_OPEN, &write_p),
        "writes t, which the cursor reads",
    );
    // STATIC reads the rows at OPEN, as PostgreSQL does.
    equal(
        &t(
            "  DECLARE c CURSOR LOCAL STATIC FOR SELECT id, v FROM dbo.t ORDER BY id;",
            &write,
        ),
        &p(P_OPEN, &write_p),
    );
    // Writing another table is fine.
    equal(
        &t(
            T_DECL,
            &write.replace(
                "    UPDATE dbo.t SET v = 0 WHERE id = @id;\n",
                "    INSERT INTO dbo.log (id, v) VALUES (@id, @v);\n",
            ),
        ),
        &p(
            P_OPEN,
            &write_p.replace(
                "    UPDATE t SET v = 0 WHERE id = v_id;\n",
                "    INSERT INTO log (id, v) VALUES (v_id, v_v);\n",
            ),
        ),
    );
}

#[test]
fn no_transaction_control_in_the_loop() {
    refused(
        &t(
            T_DECL,
            &T_LOOP.replace(
                "    SET @total = @total + @v;\n",
                "    SET @total = @total + @v;\n    COMMIT;\n",
            ),
        ),
        &p(
            P_OPEN,
            &P_LOOP.replace(
                "    v_total := v_total + v_v;\n",
                "    v_total := v_total + v_v;\n    COMMIT;\n",
            ),
        ),
        "transaction control in a cursor loop",
    );
}

#[test]
fn the_query_variables_are_read_at_declare() {
    let decl = "  DECLARE c CURSOR LOCAL FAST_FORWARD FOR SELECT id, v FROM dbo.t WHERE v > @min ORDER BY id;";
    let open_p = "  OPEN cur_c FOR SELECT id, v FROM t WHERE v > v_min ORDER BY id;";
    equal(&t(decl, T_LOOP), &p(open_p, P_LOOP));
    // T-SQL's query still sees @min = 1; PostgreSQL's would see 5.
    refused(
        &format!("{decl}\n  SET @min = 5;\n{T_LOOP}"),
        &format!("  v_min := 5;\n{open_p}\n{P_LOOP}"),
        "between the cursor's DECLARE and OPEN",
    );
}

#[test]
fn a_refcursor_without_a_loop_is_not_compared() {
    // The cursor variable of a loop that is not of the shape.
    let r = run(
        &t(T_DECL, T_LOOP),
        &p(P_OPEN, &P_LOOP.replace("  CLOSE cur_c;\n", "")),
    );
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.unknown_reasons
    );
}
