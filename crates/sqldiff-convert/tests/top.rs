//! T-SQL TOP becomes PostgreSQL LIMIT and Db2 for i FETCH FIRST, at any
//! query level, and ORDER BY keeps T-SQL's NULL placement. Forms that are
//! not provably the same stay flagged: TOP without ORDER BY (any rows),
//! a variable count (NULL), PERCENT, WITH TIES, TOP in INSERT/UPDATE/DELETE,
//! TOP next to OFFSET and a count that is not an integer.

use sqldiff_convert::{check, rewrite_in, Conversion, Status, Unit};
use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{Artifact, Dialect, Options};

const TABLES: &str = "CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, v int NULL, name varchar(20) NULL);\nGO\nCREATE TABLE dbo.u (id int NOT NULL PRIMARY KEY, v int NULL);\nGO\n";

/// `stmt` converted with the tables in the source catalog, and checked with
/// both catalogs, as a tree conversion does.
fn convert(stmt: &str, to: Dialect) -> (Conversion, String) {
    let src = format!("{stmt}\n");
    let mut cat = Catalog::default();
    for f in [TABLES, &src] {
        let root = sqldiff_syntax::parse(f, Dialect::Tsql).syntax();
        ddl::load_tree(&mut cat, &root, Dialect::Tsql);
    }
    let target = rewrite_in(TABLES, Dialect::Tsql, to, None)
        .output()
        .to_string();
    let opts = Options {
        catalog_a: vec![Artifact::new("t.sql", Dialect::Tsql, TABLES)],
        catalog_b: vec![Artifact::new("t.sql", to, &target)],
        ..Options::default()
    };
    let c = check(&src, rewrite_in(&src, Dialect::Tsql, to, Some(&cat)), &opts);
    (c, src)
}

/// The unit of the (last) statement.
fn last(c: &Conversion) -> &Unit {
    c.units.last().unwrap()
}

fn out(c: &Conversion) -> &str {
    let u = last(c);
    &c.output[u.out.start..u.out.end]
}

fn rules(u: &Unit) -> Vec<&str> {
    u.notes.iter().map(|n| n.rule.as_str()).collect()
}

fn both(stmt: &str) -> [(Dialect, Conversion); 2] {
    [Dialect::Postgres, Dialect::Db2i].map(|d| (d, convert(stmt, d).0))
}

#[test]
fn top_with_a_unique_order_is_proven() {
    for (d, c) in both("SELECT TOP 5 id, v FROM dbo.t ORDER BY id;") {
        let want = match d {
            Dialect::Postgres => "SELECT id, v FROM t ORDER BY id\nLIMIT 5;",
            _ => "SELECT id, v FROM t ORDER BY id\nFETCH FIRST 5 ROWS ONLY;",
        };
        assert!(out(&c).ends_with(want), "{d:?}: {}", out(&c));
        let u = last(&c);
        assert_eq!(u.status, Status::Assumptions, "{d:?}: {:?}", u.notes);
        assert!(!rules(u).contains(&"ASM-ORDER-TOTAL"), "{:?}", rules(u));
    }
    // `TOP (5)` is the same count.
    let (c, _) = convert(
        "SELECT TOP (5) id FROM dbo.t ORDER BY id DESC;",
        Dialect::Postgres,
    );
    assert!(
        out(&c).ends_with("ORDER BY id DESC\nLIMIT 5;"),
        "{}",
        out(&c)
    );
}

#[test]
fn a_key_that_may_be_null_sorts_nulls_as_t_sql_does() {
    for (d, c) in both("SELECT TOP 3 id, v FROM dbo.t ORDER BY v DESC, name, id;") {
        assert!(
            out(&c).contains("ORDER BY v DESC NULLS LAST, name NULLS FIRST, id\n"),
            "{d:?}: {}",
            out(&c)
        );
        assert_ne!(last(&c).checked, Status::Differs, "{d:?}: {}", out(&c));
        if d == Dialect::Db2i {
            assert!(rules(last(&c)).contains(&"ASM-FACT-V19"));
        }
    }
    // Before, the engine found the NULLs on the other end.
    let (c, _) = convert("SELECT TOP 3 id FROM dbo.t ORDER BY v;", Dialect::Postgres);
    assert_eq!(last(&c).status, Status::Assumptions, "{:?}", last(&c).notes);
    // Every ORDER BY, not only next to TOP; a window's too.
    let (c, _) = convert(
        "SELECT id, ROW_NUMBER() OVER (ORDER BY v) AS rn FROM dbo.t ORDER BY v DESC;",
        Dialect::Postgres,
    );
    assert!(
        out(&c).contains("OVER (ORDER BY v NULLS FIRST) AS rn FROM t ORDER BY v DESC NULLS LAST"),
        "{}",
        out(&c)
    );
}

#[test]
fn only_a_column_that_cannot_be_null_goes_without_nulls_first() {
    let cases = [
        // The other side of a LEFT JOIN can be NULL.
        (
            "SELECT TOP 3 t.id FROM dbo.t t LEFT JOIN dbo.u u ON u.id = t.v ORDER BY u.id, t.id;",
            "ORDER BY u.id NULLS FIRST, t.id\n",
        ),
        // A select-list alias hides the column.
        (
            "SELECT TOP 3 v AS id FROM dbo.t ORDER BY id;",
            "ORDER BY id NULLS FIRST\n",
        ),
        // A position, an expression.
        (
            "SELECT TOP 3 id, v FROM dbo.t ORDER BY 1, id + 1;",
            "ORDER BY 1 NULLS FIRST, id + 1 NULLS FIRST\n",
        ),
        // A qualified NOT NULL column of an inner join.
        (
            "SELECT TOP 3 t.id FROM dbo.t AS t JOIN dbo.u AS u ON u.id = t.id ORDER BY u.id;",
            "ORDER BY u.id\n",
        ),
    ];
    for (stmt, want) in cases {
        let (c, _) = convert(stmt, Dialect::Postgres);
        assert!(out(&c).contains(want), "{stmt}\n{}", out(&c));
        assert_ne!(last(&c).checked, Status::Differs, "{stmt}\n{}", out(&c));
    }
}

#[test]
fn top_inside_a_query_becomes_its_row_limit() {
    let cases = [
        (
            "SELECT id FROM dbo.t WHERE id IN (SELECT TOP 3 id FROM dbo.u ORDER BY id);",
            "SELECT id FROM t WHERE id IN (SELECT id FROM u ORDER BY id LIMIT 3);",
            "SELECT id FROM t WHERE id IN (SELECT id FROM u ORDER BY id FETCH FIRST 3 ROWS ONLY);",
        ),
        (
            "SELECT * FROM (SELECT TOP 3 id, v FROM dbo.t ORDER BY id) x;",
            "SELECT * FROM (SELECT id, v FROM t ORDER BY id LIMIT 3) x;",
            "SELECT * FROM (SELECT id, v FROM t ORDER BY id FETCH FIRST 3 ROWS ONLY) x;",
        ),
        (
            "SELECT (SELECT TOP 1 v FROM dbo.u WHERE u.id = t.id ORDER BY u.id) AS uv FROM dbo.t;",
            "SELECT (SELECT v FROM u WHERE u.id = t.id ORDER BY u.id LIMIT 1) AS uv FROM t;",
            "SELECT (SELECT v FROM u WHERE u.id = t.id ORDER BY u.id FETCH FIRST 1 ROWS ONLY) AS uv FROM t;",
        ),
        (
            "WITH c AS (SELECT TOP 3 id FROM dbo.t ORDER BY v) SELECT id FROM c;",
            "WITH c AS (SELECT id FROM t ORDER BY v NULLS FIRST LIMIT 3) SELECT id FROM c;",
            "WITH c AS (SELECT id FROM t ORDER BY v NULLS FIRST FETCH FIRST 3 ROWS ONLY) SELECT id FROM c;",
        ),
    ];
    for (stmt, pg, db2) in cases {
        for (d, c) in both(stmt) {
            let want = if d == Dialect::Postgres { pg } else { db2 };
            assert!(out(&c).ends_with(want), "{d:?}: {}", out(&c));
            assert_ne!(last(&c).status, Status::NotConverted, "{d:?}: {stmt}");
            assert_ne!(last(&c).checked, Status::Differs, "{d:?}: {stmt}");
            if d == Dialect::Db2i {
                assert!(
                    rules(last(&c)).contains(&"ASM-FACT-V32"),
                    "{:?}",
                    rules(last(&c))
                );
            }
        }
    }
    let stmt = "CREATE PROCEDURE dbo.p AS BEGIN SET NOCOUNT ON; INSERT INTO dbo.u (id, v) SELECT TOP 5 id, v FROM dbo.t ORDER BY id; END;";
    for (d, c) in both(stmt) {
        let want = if d == Dialect::Postgres {
            "INSERT INTO u (id, v) SELECT id, v FROM t ORDER BY id LIMIT 5;"
        } else {
            "INSERT INTO u (id, v) SELECT id, v FROM t ORDER BY id FETCH FIRST 5 ROWS ONLY;"
        };
        assert!(c.output.contains(want), "{d:?}: {}", c.output);
    }
}

#[test]
fn top_inside_exists_goes() {
    for (d, c) in
        both("SELECT id FROM dbo.t WHERE EXISTS (SELECT TOP 1 1 FROM dbo.u WHERE u.v = t.v);")
    {
        assert!(
            out(&c).ends_with("SELECT id FROM t WHERE EXISTS (SELECT 1 FROM u WHERE u.v = t.v);"),
            "{d:?}: {}",
            out(&c)
        );
        let r = rules(last(&c));
        assert_eq!(last(&c).status, Status::Assumptions, "{d:?}: {r:?}");
        assert!(
            !r.contains(&"ASM-PAGE-UNORDERED") && !r.contains(&"ASM-FACT-V32"),
            "{r:?}"
        );
    }
    // TOP 0 keeps no row: EXISTS is false.
    let (c, _) = convert(
        "SELECT id FROM dbo.t WHERE EXISTS (SELECT TOP 0 1 FROM dbo.u);",
        Dialect::Postgres,
    );
    assert!(
        out(&c).contains("EXISTS (SELECT 1 FROM u LIMIT 0)"),
        "{}",
        out(&c)
    );
}

#[test]
fn top_without_order_by_keeps_any_rows_under_an_assumption() {
    for (d, c) in both("SELECT TOP 5 id, v FROM dbo.t;") {
        let r = rules(last(&c));
        assert!(r.contains(&"ASM-PAGE-UNORDERED"), "{d:?}: {r:?}");
        assert!(!r.contains(&"ASM-ORDER-TOTAL"), "{d:?}: {r:?}");
    }
    // A deployment profile cannot accept it: only a reading of the statement.
    let a = sqldiff_core::assumptions::lookup("ASM-PAGE-UNORDERED").unwrap();
    assert_eq!(a.discharge, sqldiff_core::assumptions::Discharge::Nothing);
}

#[test]
fn a_variable_count_may_be_null() {
    // T-SQL raises Msg 1014 for TOP (NULL); PostgreSQL's LIMIT NULL is no limit.
    let stmt = "CREATE PROCEDURE dbo.p @n int AS BEGIN SET NOCOUNT ON; INSERT INTO dbo.u (id, v) SELECT TOP (@n) id, v FROM dbo.t ORDER BY id; END;";
    for (d, c) in both(stmt) {
        let want = if d == Dialect::Postgres {
            "ORDER BY id LIMIT p_n;"
        } else {
            "ORDER BY id FETCH FIRST p_n ROWS ONLY;"
        };
        assert!(c.output.contains(want), "{d:?}: {}", c.output);
        let u = c
            .units
            .iter()
            .find(|u| c.output[u.out.start..u.out.end].contains("INSERT INTO u"))
            .unwrap();
        assert!(
            rules(u).contains(&"ASM-PAGE-COUNT-NOT-NULL"),
            "{d:?}: {:?}",
            rules(u)
        );
    }
    // An integer expression: PostgreSQL only (Db2 for i FETCH FIRST takes a
    // number or a variable).
    let stmt = "CREATE PROCEDURE dbo.p @n bigint AS BEGIN SET NOCOUNT ON; INSERT INTO dbo.u (id, v) SELECT TOP (@n + 1) id, v FROM dbo.t ORDER BY id; END;";
    let (c, _) = convert(stmt, Dialect::Postgres);
    assert!(
        c.output.contains("ORDER BY id LIMIT (p_n + 1);"),
        "{}",
        c.output
    );
    let (c, _) = convert(stmt, Dialect::Db2i);
    assert!(
        c.output.contains("[not converted] TOP (expression)"),
        "{}",
        c.output
    );
}

#[test]
fn forms_that_are_not_the_same_are_refused() {
    let cases = [
        (
            "SELECT TOP 10 PERCENT id FROM dbo.t ORDER BY id;",
            "PERCENT",
        ),
        (
            "SELECT TOP 5 WITH TIES id, v FROM dbo.t ORDER BY v;",
            "WITH TIES",
        ),
        (
            "SELECT TOP 5 id FROM dbo.t ORDER BY id OFFSET 0 ROWS;",
            "Msg 10741",
        ),
        ("SELECT TOP (2.5) id FROM dbo.t ORDER BY id;", "Msg 1060"),
        (
            "SELECT id FROM dbo.t WHERE id IN (SELECT TOP 10 PERCENT id FROM dbo.u ORDER BY id);",
            "PERCENT",
        ),
        (
            "SELECT id FROM dbo.t WHERE id IN (SELECT TOP 2 id FROM dbo.u UNION SELECT 1);",
            "UNION",
        ),
        (
            "SELECT TOP 2 id FROM dbo.t UNION ALL SELECT id FROM dbo.u;",
            "UNION",
        ),
    ];
    for (stmt, why) in cases {
        for (d, c) in both(stmt) {
            let u = last(&c);
            assert_eq!(
                u.status,
                Status::NotConverted,
                "{d:?}: {stmt}\n{}",
                c.output
            );
            assert!(
                u.notes.iter().any(|n| n.text.contains(why)),
                "{d:?}: {stmt}: {:?}",
                u.notes
            );
        }
    }
    // A count whose type is not an integer (T-SQL raises Msg 1060 for 2.5,
    // PostgreSQL rounds it).
    let stmt = "CREATE PROCEDURE dbo.p @n decimal(10,2) AS BEGIN SET NOCOUNT ON; DELETE FROM dbo.u WHERE id IN (SELECT TOP (@n) id FROM dbo.u ORDER BY id); END;";
    for (d, c) in both(stmt) {
        let u = c
            .units
            .iter()
            .find(|u| c.output[u.out.start..u.out.end].contains("DELETE"))
            .unwrap();
        assert!(
            matches!(u.status, Status::NotConverted | Status::Unproven),
            "{d:?}: {:?}\n{}",
            u.status,
            c.output
        );
    }
    // TOP in INSERT/UPDATE/DELETE takes rows T-SQL picks (no ORDER BY there).
    for stmt in [
        "DELETE TOP (5) FROM dbo.t;",
        "UPDATE TOP (5) dbo.t SET v = 1;",
        "INSERT TOP (5) INTO dbo.u (id, v) SELECT id, v FROM dbo.t;",
    ] {
        let stmt = format!("CREATE PROCEDURE dbo.p AS BEGIN SET NOCOUNT ON; {stmt} END;");
        for (d, c) in both(&stmt) {
            assert!(
                c.output
                    .contains("[not converted] TOP in INSERT/UPDATE/DELETE"),
                "{d:?}: {}",
                c.output
            );
        }
    }
}

#[test]
fn top_100_percent_keeps_every_row() {
    for (d, c) in both("SELECT TOP 100 PERCENT id FROM dbo.t ORDER BY id;") {
        assert!(
            out(&c).ends_with("SELECT id FROM t ORDER BY id;"),
            "{d:?}: {}",
            out(&c)
        );
        assert_ne!(last(&c).status, Status::NotConverted);
        assert_ne!(
            last(&c).checked,
            Status::Unproven,
            "{d:?}: {:?}",
            last(&c).notes
        );
    }
    for (d, c) in both("SELECT x.id FROM (SELECT TOP (100) PERCENT id FROM dbo.t ORDER BY id) x;") {
        assert!(
            out(&c).ends_with("SELECT x.id FROM (SELECT id FROM t ORDER BY id) x;"),
            "{d:?}: {}",
            out(&c)
        );
        assert_ne!(
            last(&c).checked,
            Status::Unproven,
            "{d:?}: {:?}",
            last(&c).notes
        );
    }
}
