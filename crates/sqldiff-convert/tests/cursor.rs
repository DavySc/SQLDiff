//! T-SQL cursor loops become PL/pgSQL cursor loops (rules `tsql.cursor.*`)
//! when they have the standard shape: DECLARE, OPEN, FETCH … INTO, `WHILE
//! @@FETCH_STATUS = 0` ending with the same FETCH, CLOSE, DEALLOCATE. The
//! engine compares every statement of the loop with what it became. Shapes
//! whose meaning differs between the two (a variable read after the loop,
//! transaction control in the loop, a cursor that is not STATIC over a table
//! the loop writes, SCROLL) stay refused, as does Db2 for i.

use sqldiff_convert::{check, rewrite_in, Conversion, Status, Unit};
use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{Artifact, Dialect, Options};

const TABLES: &str = "CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, v int NULL);\nCREATE TABLE dbo.log (id int NOT NULL, v int NULL);\n";

/// A procedure with `body` after `SET XACT_ABORT ON` and declarations of
/// `@id int`, `@v int`, `@total int = 0`, converted and checked with the
/// tables in both catalogs.
fn convert_to(body: &str, to: Dialect) -> Conversion {
    let src = format!(
        "CREATE PROCEDURE dbo.p @n int\nAS\nBEGIN\n  SET NOCOUNT ON;\n  SET XACT_ABORT ON;\n  DECLARE @id int;\n  DECLARE @v int;\n  DECLARE @total int = 0;\n{body}\nEND\n"
    );
    let mut cat = Catalog::default();
    let root = sqldiff_syntax::parse(TABLES, Dialect::Tsql).syntax();
    ddl::load_tree(&mut cat, &root, Dialect::Tsql);
    let target = rewrite_in(TABLES, Dialect::Tsql, to, None)
        .output()
        .to_string();
    let opts = Options {
        catalog_a: vec![Artifact::new("t.sql", Dialect::Tsql, TABLES)],
        catalog_b: vec![Artifact::new("t.sql", to, &target)],
        ..Options::default()
    };
    check(&src, rewrite_in(&src, Dialect::Tsql, to, Some(&cat)), &opts)
}

fn pg(body: &str) -> Conversion {
    convert_to(body, Dialect::Postgres)
}

/// The units of the statements on source line `line`.
fn at(c: &Conversion, line: usize) -> Vec<&Unit> {
    let v: Vec<&Unit> = c.units.iter().filter(|u| u.src.line == line).collect();
    assert!(!v.is_empty(), "no unit on line {line}");
    v
}

fn rules(u: &Unit) -> Vec<&str> {
    u.notes.iter().map(|n| n.rule.as_str()).collect()
}

/// The body's first line is line 9.
const DECL: &str = "  DECLARE c CURSOR LOCAL FAST_FORWARD FOR SELECT id, v FROM dbo.t ORDER BY id;";
const LOOP: &str = "  OPEN c;\n  FETCH NEXT FROM c INTO @id, @v;\n  WHILE @@FETCH_STATUS = 0\n  BEGIN\n    INSERT INTO dbo.log (id, v) VALUES (@id, @v);\n    FETCH NEXT FROM c INTO @id, @v;\n  END\n  CLOSE c;\n  DEALLOCATE c;";

fn body(decl: &str, rest: &str) -> String {
    format!("{decl}\n{rest}")
}

#[test]
fn the_standard_loop_becomes_a_plpgsql_cursor_loop() {
    let c = pg(&body(DECL, LOOP));
    let out = &c.output;
    for want in [
        "cur_c refcursor;",
        "OPEN cur_c FOR SELECT id, v FROM t ORDER BY id;",
        "FETCH NEXT FROM cur_c INTO v_id, v_v;",
        "WHILE FOUND LOOP",
        "END LOOP;",
        "CLOSE cur_c;",
        "-- DEALLOCATE c: PostgreSQL frees the cursor at CLOSE",
    ] {
        assert!(out.contains(want), "{want}\n{out}");
    }
    // Every statement of the loop compared: none is left unproven.
    for line in [9, 10, 11, 12, 14, 15, 17, 18] {
        for u in at(&c, line) {
            assert!(
                u.status <= Status::Assumptions,
                "line {line}: {:?}\n{:#?}",
                u.status,
                u.notes
            );
        }
    }
    assert!(rules(at(&c, 10)[0]).contains(&"ASM-CURSOR-SNAPSHOT"));
}

fn refused(body: &str, why: &str) {
    let c = pg(body);
    let u = at(&c, 9)[0];
    assert_eq!(u.status, Status::NotConverted, "{body}\n{}", c.output);
    assert!(u.notes[0].text.contains(why), "{body}: {}", u.notes[0].text);
    // The OPEN says why too.
    let open = c
        .units
        .iter()
        .find(|u| c.output[u.out.start..u.out.end].contains("OPEN c"))
        .map(|u| u.notes[0].text.clone());
    if let Some(t) = open {
        assert!(t.contains(why), "{t}");
    }
}

#[test]
fn shapes_that_mean_something_else_stay_refused() {
    // T-SQL keeps the last row's @id, PostgreSQL's last FETCH sets NULL.
    refused(
        &body(
            DECL,
            &format!("{LOOP}\n  INSERT INTO dbo.log (id, v) VALUES (@id, 0);"),
        ),
        "is read after the loop",
    );
    // PostgreSQL closes the cursor at COMMIT.
    refused(
        &body(
            DECL,
            &LOOP.replace(
                "    FETCH NEXT FROM c INTO @id, @v;\n  END",
                "    COMMIT;\n    FETCH NEXT FROM c INTO @id, @v;\n  END",
            ),
        ),
        "transaction control in the cursor loop",
    );
    // A cursor that is not STATIC may see the loop's writes, unless they
    // only change the row just fetched, by its unique key.
    let upd = |set: &str| {
        body(
            DECL,
            &LOOP.replace("INSERT INTO dbo.log (id, v) VALUES (@id, @v);", set),
        )
    };
    for (set, why) in [
        // Not the key: a fetched v can name several rows.
        ("UPDATE dbo.t SET v = 0 WHERE v = @v;", "unique"),
        ("UPDATE dbo.t SET v = 0;", "without WHERE"),
        ("UPDATE dbo.t SET v = 0 WHERE id > @id;", "not keyed"),
        // Moves the row in the query's order.
        (
            "UPDATE dbo.t SET id = id + 100 WHERE id = @id;",
            "outside its select list",
        ),
        (
            "SET @id = @id + 1; UPDATE dbo.t SET v = 0 WHERE id = @id;",
            "assigned inside the loop",
        ),
        (
            "INSERT INTO dbo.t (id, v) VALUES (@id + 1000, 0);",
            "writes a table",
        ),
    ] {
        let c = pg(&upd(set));
        let u = at(&c, 9)[0];
        assert_eq!(u.status, Status::NotConverted, "{set}\n{}", c.output);
        let text = &u.notes[0].text;
        assert!(
            text.contains("writes t, which the cursor reads") && text.contains(why),
            "{set}: {text}"
        );
    }
    // Without an ORDER BY the query does not name the key, yet a dynamic
    // cursor scanning the key's index meets a row whose key was changed
    // again; and an UPDATE that assigns a variable changes it besides the
    // FETCH.
    let unordered = |set: &str| {
        body(
            "  DECLARE c CURSOR LOCAL FAST_FORWARD FOR SELECT id, v FROM dbo.t;",
            &LOOP.replace("INSERT INTO dbo.log (id, v) VALUES (@id, @v);", set),
        )
    };
    for (set, why) in [
        (
            "UPDATE dbo.t SET id = id + 100 WHERE id = @id;",
            "free of a key or index on",
        ),
        (
            "UPDATE dbo.t SET @id = 7, v = 0 WHERE id = @id;",
            "assigns a variable",
        ),
    ] {
        let c = pg(&unordered(set));
        let u = at(&c, 9)[0];
        assert_eq!(u.status, Status::NotConverted, "{set}\n{}", c.output);
        assert!(u.notes[0].text.contains(why), "{set}: {}", u.notes[0].text);
    }
    let c = pg(&unordered("UPDATE dbo.t SET v = 0 WHERE id = @id;"));
    assert!(c.output.contains("WHILE FOUND LOOP"), "{}", c.output);
    let c = pg(&upd("UPDATE dbo.t SET v = 0 WHERE id = @id;"));
    assert!(
        c.output.contains("UPDATE t SET v = 0 WHERE id = v_id;"),
        "{}",
        c.output
    );
    assert!(c.output.contains("WHILE FOUND LOOP"), "{}", c.output);
    assert_ne!(at(&c, 9)[0].status, Status::NotConverted);
    // Only forward-only cursors.
    refused(
        &body(
            "  DECLARE c CURSOR LOCAL SCROLL FOR SELECT id, v FROM dbo.t ORDER BY id;",
            LOOP,
        ),
        "cursor option SCROLL",
    );
    // Another test than @@FETCH_STATUS = 0.
    refused(
        &body(
            DECL,
            &LOOP.replace("@@FETCH_STATUS = 0", "@@FETCH_STATUS <> -1"),
        ),
        "standard loop",
    );
    // The body does not end with the FETCH.
    refused(
        &body(
            DECL,
            "  OPEN c;\n  FETCH NEXT FROM c INTO @id, @v;\n  WHILE @@FETCH_STATUS = 0\n  BEGIN\n    FETCH NEXT FROM c INTO @id, @v;\n    INSERT INTO dbo.log (id, v) VALUES (@id, @v);\n  END\n  CLOSE c;\n  DEALLOCATE c;",
        ),
        "does not end with the same FETCH",
    );
    // A parameter keeps the NULL of the last FETCH for the caller.
    let c = pg(&body(DECL, &LOOP.replace("@id, @v", "@n, @v")));
    let f = at(&c, 11)[0];
    assert_eq!(f.status, Status::NotConverted);
    assert!(
        f.notes[0].text.contains("not a local variable"),
        "{:?}",
        f.notes
    );
}

#[test]
fn a_static_cursor_may_write_what_it_reads() {
    let c = pg(&body(
        "  DECLARE c CURSOR LOCAL STATIC FOR SELECT id, v FROM dbo.t ORDER BY id;",
        &LOOP.replace(
            "INSERT INTO dbo.log (id, v) VALUES (@id, @v);",
            "UPDATE dbo.t SET v = 0 WHERE id = @id;",
        ),
    ));
    assert!(c.output.contains("WHILE FOUND LOOP"), "{}", c.output);
    assert_ne!(at(&c, 9)[0].status, Status::NotConverted);
}

/// The steps that convert the cursor's query belong to the DECLARE's unit
/// when the OPEN is converted, and to nobody when the OPEN is refused.
#[test]
fn the_querys_steps_go_to_the_declare_only_when_the_open_converts() {
    let step = "tsql.expr.dbo-schema";
    let has = |c: &Conversion| {
        at(c, 9)
            .iter()
            .any(|u| u.trace.iter().any(|s| s.rule == step))
    };
    let c = pg(&body(DECL, LOOP));
    assert!(c.output.contains("OPEN cur_c FOR"), "{}", c.output);
    assert!(has(&c), "{:#?}", at(&c, 9)[0].trace);
    for u in &c.units {
        for s in &u.trace {
            assert!(u.src.start <= s.start && s.end <= u.src.end, "{s:?}");
        }
    }
    // DIFFERENCE is not converted: the OPEN is refused, and the DECLARE's
    // unit keeps no step for a query that was not converted.
    let c = pg(&body(
        "  DECLARE c CURSOR LOCAL STATIC FOR SELECT id, DIFFERENCE(CAST(v AS nvarchar(10)), N'x') FROM dbo.t ORDER BY id;",
        LOOP,
    ));
    assert!(!c.output.contains("OPEN cur_c FOR"), "{}", c.output);
    assert!(!has(&c), "{:#?}", at(&c, 9)[0].trace);
}

#[test]
fn db2_keeps_cursors_refused() {
    let c = convert_to(&body(DECL, LOOP), Dialect::Db2i);
    assert_eq!(at(&c, 9)[0].status, Status::NotConverted);
}

#[test]
fn a_cursor_declared_in_one_routine_is_not_found_by_the_next() {
    // The second procedure uses a cursor `c` it never declares: it must not
    // pick up the first one's plan.
    let src = format!(
        "CREATE PROCEDURE dbo.a @n int\nAS\nBEGIN\n  DECLARE @id int;\n  DECLARE @v int;\n{DECL}\n{LOOP}\nEND\nCREATE PROCEDURE dbo.b @n int\nAS\nBEGIN\n  DECLARE @id int;\n  DECLARE @v int;\n  OPEN c;\n  FETCH NEXT FROM c INTO @id, @v;\n  CLOSE c;\nEND\n"
    );
    let mut cat = Catalog::default();
    let root = sqldiff_syntax::parse(TABLES, Dialect::Tsql).syntax();
    ddl::load_tree(&mut cat, &root, Dialect::Tsql);
    let c = rewrite_in(&src, Dialect::Tsql, Dialect::Postgres, Some(&cat));
    let out = c.output();
    let tail = out
        .split("PROCEDURE b(")
        .nth(1)
        .unwrap_or_else(|| panic!("no second procedure:\n{out}"));
    assert!(
        !tail.contains("OPEN cur_c") && tail.contains("not converted"),
        "the second routine opened the first one's cursor:\n{out}"
    );
}

#[test]
fn a_cursor_closed_in_a_later_batch_is_not_converted_with_the_earlier_plan() {
    // Batch two CLOSEs a cursor batch one declared and finished: batch two's
    // DO block has no `cur_c`, so the CLOSE must not become `CLOSE cur_c`.
    let src = format!("DECLARE @id int;\nDECLARE @v int;\n{DECL}\n{LOOP}\nGO\nCLOSE c;\n");
    let mut cat = Catalog::default();
    let root = sqldiff_syntax::parse(TABLES, Dialect::Tsql).syntax();
    ddl::load_tree(&mut cat, &root, Dialect::Tsql);
    let c = rewrite_in(&src, Dialect::Tsql, Dialect::Postgres, Some(&cat));
    let out = c.output();
    let tail = out.rsplit("END LOOP;").next().unwrap();
    let after = tail.rsplit("$$;").next().unwrap();
    assert!(!after.contains("CLOSE cur_c"), "{out}");
}
