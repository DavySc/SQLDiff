//! T-SQL `UPDATE … FROM` with joins becomes PostgreSQL `UPDATE … FROM` (rule
//! T4): the target leaves the FROM list, the condition that joined it moves
//! to WHERE, and the source catalog must show that a target row meets at most
//! one row, which the engine then proves on its own.

use sqldiff_convert::{check, rewrite_in, Conversion, Status};
use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{Artifact, Dialect, Options};

const TABLES: &str = "CREATE TABLE dbo.orders (order_id int NOT NULL PRIMARY KEY, qty int NOT NULL, price decimal(10,2) NOT NULL, total decimal(12,2) NULL);
CREATE TABLE dbo.order_items (item_id int NOT NULL PRIMARY KEY, order_id int NOT NULL, amount decimal(10,2) NOT NULL, code varchar(10) NOT NULL UNIQUE);
";

/// Converts `stmt` with the tables as the catalogs of both sides.
fn convert_to(stmt: &str, to: Dialect) -> Conversion {
    let src = format!("{stmt}\n");
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

fn tsql(stmt: &str) -> (Conversion, ()) {
    (convert_to(stmt, Dialect::Postgres), ())
}

/// The output of the last unit, which holds the statement.
fn last(c: &Conversion) -> (&str, &sqldiff_convert::Unit) {
    let u = c.units.last().unwrap();
    (&c.output[u.out.start..u.out.end], u)
}

#[test]
fn update_from_joins_become_postgres_update_from() {
    let cases = [
        // The target first, joined to its source.
        (
            "UPDATE o SET total = i.amount FROM dbo.orders o JOIN dbo.order_items i ON i.item_id = o.order_id WHERE i.amount > 0;",
            "UPDATE orders o SET total = i.amount FROM order_items i WHERE (i.item_id = o.order_id) AND (i.amount > 0);",
        ),
        // The target second, a qualified SET column, a LEFT JOIN after it.
        (
            "UPDATE o SET o.total = i.amount + COALESCE(p.amount, 0) FROM dbo.order_items AS i INNER JOIN dbo.orders AS o ON o.order_id = i.item_id LEFT OUTER JOIN dbo.order_items AS p ON p.item_id = i.order_id;",
            "UPDATE orders AS o SET total = i.amount + COALESCE(p.amount, 0) FROM order_items AS i LEFT JOIN order_items AS p ON p.item_id = i.order_id WHERE o.order_id = i.item_id;",
        ),
        // Comma joins: the conditions are already in WHERE.
        (
            "UPDATE orders SET total = i.amount FROM dbo.orders, dbo.order_items i WHERE i.item_id = orders.order_id;",
            "UPDATE orders SET total = i.amount FROM order_items i WHERE i.item_id = orders.order_id;",
        ),
        // A schema-qualified target, the only FROM table (SQL Server binds
        // it to the FROM reference of the same table).
        (
            "UPDATE dbo.orders SET total = 1 FROM dbo.orders WHERE order_id = 1;",
            "UPDATE orders SET total = 1 WHERE order_id = 1;",
        ),
        // ... and to an aliased reference of the same table.
        (
            "UPDATE dbo.orders SET total = i.amount FROM dbo.orders o JOIN dbo.order_items i ON i.item_id = o.order_id;",
            "UPDATE orders o SET total = i.amount FROM order_items i WHERE i.item_id = o.order_id;",
        ),
        // The target is not in FROM: SQL Server joins it in, as PostgreSQL
        // does.
        (
            "UPDATE dbo.orders SET total = i.amount FROM dbo.order_items i WHERE i.item_id = dbo.orders.order_id;",
            "UPDATE orders SET total = i.amount FROM order_items i WHERE i.item_id = orders.order_id;",
        ),
        (
            "UPDATE orders SET qty = 0 FROM dbo.order_items AS i WHERE i.item_id = 7 AND i.order_id = qty;",
            "UPDATE orders SET qty = 0 FROM order_items AS i WHERE i.item_id = 7 AND i.order_id = qty;",
        ),
        // A unique key matched against a variable.
        (
            "DECLARE @k int = 1;\nUPDATE o SET qty = 0 FROM dbo.orders o JOIN dbo.order_items i ON i.order_id = o.order_id AND i.item_id = @k;",
            "UPDATE orders o SET qty = 0 FROM order_items i WHERE i.order_id = o.order_id AND i.item_id = v_k;",
        ),
    ];
    for (stmt, want) in cases {
        let (c, _) = tsql(stmt);
        assert!(c.output.contains(want), "{stmt}\n{}", c.output);
        let (_, u) = last(&c);
        assert!(
            u.status < Status::Differs,
            "{stmt}: {:?}\n{:#?}\n{}",
            u.notes,
            c.report.findings,
            c.output
        );
        // The engine compared the statement itself.
        assert!(
            u.status < Status::Unproven,
            "{stmt}: {:?}\n{:?}",
            u.notes,
            c.report.unknown_reasons
        );
    }
}

/// A string key is unique on both sides, but T-SQL compares it without
/// regard to case and trailing spaces: the conversion is equal only under
/// ASM-CI-COLLATION (Davy, 2026-10-29: keep the text types, flag each case).
#[test]
fn a_string_key_match_is_flagged_by_collation() {
    let (c, _) = tsql("DECLARE @c varchar(10) = 'x';\nUPDATE o SET qty = 0 FROM dbo.orders o JOIN dbo.order_items i ON i.order_id = o.order_id AND i.code = @c;");
    assert!(
        c.output.contains("UPDATE orders o SET qty = 0 FROM order_items i WHERE i.order_id = o.order_id AND i.code = v_c;"),
        "{}",
        c.output
    );
    let u = last(&c).1;
    assert_eq!(u.status, Status::Assumptions, "{:?}", u.notes);
    assert!(
        u.notes.iter().any(|n| n.rule == "ASM-CI-COLLATION"),
        "{:?}",
        u.notes
    );
    assert!(!c
        .report
        .findings
        .iter()
        .any(|f| f.rule == "equiv.case-sensitivity" || f.rule == "equiv.trailing-spaces"));
}

#[test]
fn update_from_without_a_unique_match_is_refused() {
    for stmt in [
        // order_id is not a key of order_items: an order meets several items.
        "UPDATE o SET total = i.amount FROM dbo.orders o JOIN dbo.order_items i ON i.order_id = o.order_id;",
        // A key compared with something other than an equality.
        "UPDATE o SET total = i.amount FROM dbo.orders o JOIN dbo.order_items i ON i.item_id >= o.order_id;",
        // The LEFT JOIN's table is fixed only by WHERE, which does not limit it.
        "UPDATE o SET total = p.amount FROM dbo.orders o JOIN dbo.order_items i ON i.item_id = o.order_id LEFT JOIN dbo.order_items p ON p.order_id = i.order_id WHERE p.item_id = 1;",
        // An unknown table.
        "UPDATE o SET total = x.v FROM dbo.orders o JOIN dbo.other x ON x.id = o.order_id;",
        // A condition with OR is not a key match.
        "UPDATE o SET total = i.amount FROM dbo.orders o JOIN dbo.order_items i ON i.item_id = o.order_id OR i.item_id = 0;",
    ] {
        let (c, _) = tsql(stmt);
        let (out, u) = last(&c);
        assert_eq!(u.status, Status::NotConverted, "{stmt}\n{out}");
        assert!(
            u.notes.iter().any(|n| n.text.contains("at most one row")),
            "{stmt}: {:?}",
            u.notes
        );
    }
}

#[test]
fn update_from_shapes_without_a_rule_are_refused() {
    for stmt in [
        // A later join condition names the target.
        "UPDATE o SET total = 1 FROM dbo.orders o JOIN dbo.order_items i ON i.item_id = o.order_id LEFT JOIN dbo.order_items p ON p.item_id = o.qty;",
        // ... or may, through a bare column of the target.
        "UPDATE o SET total = 1 FROM dbo.orders o JOIN dbo.order_items i ON i.item_id = o.order_id LEFT JOIN dbo.order_items p ON p.item_id = qty;",
        // The target is outer-joined.
        "UPDATE o SET total = 1 FROM dbo.order_items i LEFT JOIN dbo.orders o ON o.order_id = i.item_id;",
        "UPDATE o SET total = 1 FROM dbo.order_items i RIGHT JOIN dbo.orders o ON o.order_id = i.item_id;",
        // The target is third in its chain.
        "UPDATE o SET total = 1 FROM dbo.order_items i JOIN dbo.order_items p ON p.item_id = i.item_id JOIN dbo.orders o ON o.order_id = i.item_id;",
        // Hints, APPLY, OPTION.
        "UPDATE o SET total = 1 FROM dbo.orders o WITH (UPDLOCK) JOIN dbo.order_items i ON i.item_id = o.order_id;",
        "UPDATE o SET total = 1 FROM dbo.orders o INNER LOOP JOIN dbo.order_items i ON i.item_id = o.order_id;",
        "UPDATE o SET total = 1 FROM dbo.orders o CROSS APPLY dbo.f(o.order_id) f;",
        "UPDATE o SET total = 1 FROM dbo.orders o JOIN dbo.order_items i ON i.item_id = o.order_id OPTION (MAXDOP 1);",
        // The target binds to no FROM table, or not by a modelled rule:
        // only one side states a schema, so the default schema decides
        // whether the two are the same table.
        "UPDATE orders SET total = 1 FROM dbo.orders o JOIN dbo.order_items i ON i.item_id = o.order_id;",
        "UPDATE dbo.orders SET total = 1 FROM orders JOIN dbo.order_items i ON i.item_id = orders.order_id;",
        // Three-part target.
        "UPDATE db.dbo.orders SET total = 1 FROM db.dbo.orders WHERE order_id = 1;",
        // The same table twice.
        "UPDATE dbo.orders SET total = 1 FROM dbo.orders JOIN dbo.orders o2 ON o2.order_id = dbo.orders.qty;",
        // A SET column qualified by another table.
        "UPDATE o SET i.amount = 1 FROM dbo.orders o JOIN dbo.order_items i ON i.item_id = o.order_id;",
    ] {
        let (c, _) = tsql(stmt);
        let (out, u) = last(&c);
        assert_eq!(u.status, Status::NotConverted, "{stmt}\n{out}");
    }
}

/// `WITH … UPDATE … FROM`: a CTE read as another FROM item stays a CTE in
/// PostgreSQL; its GROUP BY columns are the key the one-match check needs.
#[test]
fn with_update_from_keeps_a_grouped_cte() {
    let (c, _) = tsql(
        "WITH t AS (SELECT order_id, MAX(amount) AS amt FROM dbo.order_items GROUP BY order_id)\nUPDATE o SET total = t.amt FROM dbo.orders AS o JOIN t ON t.order_id = o.order_id WHERE o.qty > 0;",
    );
    let (out, u) = last(&c);
    assert!(
        out.contains("WITH t AS (SELECT order_id, MAX(amount) AS amt FROM order_items GROUP BY order_id) UPDATE orders AS o SET total = t.amt FROM t WHERE (t.order_id = o.order_id) AND (o.qty > 0);"),
        "{out}"
    );
    assert!(
        u.status < Status::Unproven,
        "{out}: {:?}\n{:?}",
        u.notes,
        c.report.unknown_reasons
    );
    // The same as a derived table.
    let (c, _) = tsql(
        "UPDATE o SET total = t.amt FROM dbo.orders AS o JOIN (SELECT order_id, MAX(amount) AS amt FROM dbo.order_items GROUP BY order_id) AS t ON t.order_id = o.order_id;",
    );
    let (out, u) = last(&c);
    assert!(
        out.contains("UPDATE orders AS o SET total = t.amt FROM (SELECT order_id, MAX(amount) AS amt FROM order_items GROUP BY order_id) AS t WHERE t.order_id = o.order_id;"),
        "{out}"
    );
    assert!(u.status < Status::Unproven, "{out}: {:?}", u.notes);
    for stmt in [
        // Grouped by more than the join matches.
        "UPDATE o SET total = t.amt FROM dbo.orders AS o JOIN (SELECT order_id, code, MAX(amount) AS amt FROM dbo.order_items GROUP BY order_id, code) AS t ON t.order_id = o.order_id;",
        // No GROUP BY: an order meets several items.
        "WITH t AS (SELECT order_id, amount AS amt FROM dbo.order_items)\nUPDATE o SET total = t.amt FROM dbo.orders AS o JOIN t ON t.order_id = o.order_id;",
        // The target is the CTE.
        "WITH t AS (SELECT order_id, total FROM dbo.orders)\nUPDATE x SET total = 0 FROM t AS x JOIN dbo.order_items i ON i.item_id = x.order_id;",
    ] {
        let (c, _) = tsql(stmt);
        let (out, u) = last(&c);
        assert_eq!(u.status, Status::NotConverted, "{stmt}\n{out}");
    }
}

/// A table variable converted to a temporary table is a FROM table like any
/// other: the target, or another table with a key the catalog shows.
#[test]
fn update_from_takes_converted_table_variables() {
    let (c, _) = tsql(
        "CREATE PROCEDURE dbo.p AS
BEGIN
  SET XACT_ABORT ON;
  DECLARE @t TABLE (id int NOT NULL PRIMARY KEY, v int NULL);
  UPDATE x SET v = 1 FROM @t AS x WHERE x.id = 1;
  UPDATE x SET v = o.qty FROM @t AS x JOIN dbo.orders AS o ON o.order_id = x.id;
END",
    );
    for want in [
        "UPDATE pg_temp.tv__dbo__p__t AS x SET v = 1 WHERE x.id = 1;",
        "UPDATE pg_temp.tv__dbo__p__t AS x SET v = o.qty FROM orders AS o WHERE o.order_id = x.id;",
    ] {
        // The statement's own unit: the smallest that holds it.
        let u = c
            .units
            .iter()
            .filter(|u| c.output[u.out.start..u.out.end].contains(want))
            .min_by_key(|u| u.out.end - u.out.start)
            .unwrap_or_else(|| panic!("{want}\n{}", c.output));
        assert!(
            u.status < Status::Unproven,
            "{want}: {:?} {:?}",
            u.status,
            u.notes
        );
    }
}

/// Several matches give one value when SET reads only the target and
/// columns WHERE or an inner join pins to a number or a variable (Ola's
/// `SET t.Selected = s.Selected … WHERE s.Selected = 1`).
#[test]
fn update_from_with_one_value_for_every_match_is_proven() {
    let (c, _) = tsql(
        "CREATE PROCEDURE dbo.p @n int AS
BEGIN
  SET XACT_ABORT ON;
  DECLARE @t TABLE (id int NOT NULL PRIMARY KEY, name nvarchar(128) NULL, selected bit NULL, pos int NULL);
  DECLARE @s TABLE (name nvarchar(max) NULL, selected bit NULL, pos int NULL);
  UPDATE t SET t.selected = s.selected FROM @t t INNER JOIN @s s ON t.name = s.name WHERE s.selected = 1;
  UPDATE t SET t.pos = s.pos + t.id FROM @t t INNER JOIN @s s ON t.name = s.name AND s.pos = @n;
  UPDATE t SET t.selected = 0 FROM @t t INNER JOIN @s s ON t.name = s.name;
END",
    );
    for want in [
        "SET selected = s.selected FROM pg_temp.tv__dbo__p__s s WHERE (t.name = s.name) AND (s.selected = TRUE);",
        "SET pos = s.pos + t.id FROM pg_temp.tv__dbo__p__s s WHERE t.name = s.name AND s.pos = p_n;",
        "SET selected = FALSE FROM pg_temp.tv__dbo__p__s s WHERE t.name = s.name;",
    ] {
        let u = c
            .units
            .iter()
            .filter(|u| c.output[u.out.start..u.out.end].contains(want))
            .min_by_key(|u| u.out.end - u.out.start)
            .unwrap_or_else(|| panic!("{want}\n{}", c.output));
        assert!(
            u.status < Status::Unproven,
            "{want}: {:?} {:?}",
            u.status,
            u.notes
        );
    }
    // A column pinned by nothing, or read through a subquery, still needs a key.
    let (c, _) = tsql(
        "CREATE PROCEDURE dbo.q AS
BEGIN
  SET XACT_ABORT ON;
  DECLARE @t TABLE (id int NOT NULL PRIMARY KEY, name nvarchar(128) NULL, pos int NULL);
  DECLARE @s TABLE (name nvarchar(max) NULL, pos int NULL);
  UPDATE t SET t.pos = s.pos FROM @t t INNER JOIN @s s ON t.name = s.name;
END",
    );
    assert!(
        c.units
            .iter()
            .any(|u| u.notes.iter().any(|n| n.text.contains("at most one row"))),
        "{}",
        c.output
    );
}

/// Shapes the UPDATE … FROM rule refuses but the fallback rules still
/// write: the engine must not call them clean.
#[test]
fn qualified_targets_without_a_modelled_binding_are_not_clean() {
    for stmt in [
        // Another FROM item's alias has the target's name.
        "UPDATE dbo.orders SET total = 1 FROM dbo.order_items orders WHERE orders.item_id = 1;",
        // The target is joined in implicitly, but an order meets several
        // items.
        "UPDATE dbo.orders SET total = i.amount FROM dbo.order_items i WHERE i.order_id = dbo.orders.order_id;",
        // The target is not in FROM, and a FROM table has its name.
        "UPDATE dbo.orders SET total = 1 FROM sales.orders WHERE sales.orders.order_id = 1;",
        // The target is not in FROM, and a join condition names it (an
        // error in both dialects).
        "UPDATE dbo.orders SET total = 1 FROM dbo.order_items i JOIN dbo.order_items p ON p.item_id = dbo.orders.qty;",
    ] {
        let (c, _) = tsql(stmt);
        let (out, u) = last(&c);
        assert!(u.status >= Status::Unproven, "{stmt}\n{out}: {:?}", u.notes);
    }
}

#[test]
fn db2_keeps_refusing_update_from() {
    let c = convert_to(
        "UPDATE o SET total = i.amount FROM dbo.orders o JOIN dbo.order_items i ON i.item_id = o.order_id;",
        Dialect::Db2i,
    );
    assert_eq!(c.units.last().unwrap().status, Status::NotConverted);
}

/// The assignments of an `UPDATE` can be a placeholder a generator fills, so
/// the template has no `SET` word. The rules cannot see what is assigned and
/// refuse the statement; they must not stop on the file.
#[test]
fn update_from_with_placeholder_assignments_is_refused_not_a_panic() {
    use sqldiff_convert::tree::{self, TemplateMode};

    let dir = std::env::temp_dir().join(format!("sqldiff-uf-set-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("body.sql"),
        "update t1\n<%ASSIGNMENTS%>\nfrom some_table t1\n",
    )
    .unwrap();
    let def: sqldiff_source::pack::TemplateSyntax =
        serde_json::from_value(serde_json::json!({ "open": "<%", "close": "%>" })).unwrap();
    let a = tree::Args {
        src: dir.clone(),
        from: Dialect::Tsql,
        to: Dialect::Postgres,
        jobs: Some(1),
        no_catalog: true,
        template: Some(TemplateMode::new(&def, None).unwrap()),
        extensions: tree::extensions(Vec::new()),
        helpers: Vec::new(),
        deployment: Default::default(),
        catalogs: Vec::new(),
        catalogs_b: Vec::new(),
    };
    let (inputs, unreadable) = tree::read(&a).unwrap();
    let t = tree::convert(&a, &inputs, unreadable, None).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let rules: Vec<&str> = t.conversions[0]
        .units
        .iter()
        .flat_map(|u| u.notes.iter().map(|n| n.rule.as_str()))
        .collect();
    assert!(!rules.contains(&"internal-error"), "{rules:?}");
    assert_eq!(
        t.conversions[0].units.last().unwrap().status,
        Status::NotConverted,
        "{rules:?}"
    );
}
