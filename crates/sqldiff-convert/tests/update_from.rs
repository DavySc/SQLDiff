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
/// regard to case and trailing spaces: the engine reports the difference.
#[test]
fn a_string_key_match_differs_by_collation() {
    let (c, _) = tsql("DECLARE @c varchar(10) = 'x';\nUPDATE o SET qty = 0 FROM dbo.orders o JOIN dbo.order_items i ON i.order_id = o.order_id AND i.code = @c;");
    assert!(
        c.output.contains("UPDATE orders o SET qty = 0 FROM order_items i WHERE i.order_id = o.order_id AND i.code = v_c;"),
        "{}",
        c.output
    );
    assert_eq!(last(&c).1.status, Status::Differs);
    assert!(c
        .report
        .findings
        .iter()
        .any(|f| f.rule == "equiv.case-sensitivity"));
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
        "UPDATE o SET total = 1 FROM dbo.orders o JOIN dbo.other x ON x.id = o.order_id;",
        // A condition with OR is not a key match.
        "UPDATE o SET total = 1 FROM dbo.orders o JOIN dbo.order_items i ON i.item_id = o.order_id OR i.item_id = 0;",
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
        // The target binds to no FROM table, or not by a modelled rule.
        "UPDATE dbo.orders SET total = 1 FROM dbo.orders o JOIN dbo.order_items i ON i.item_id = o.order_id;",
        "UPDATE orders SET total = 1 FROM dbo.orders o JOIN dbo.order_items i ON i.item_id = o.order_id;",
        // A SET column qualified by another table.
        "UPDATE o SET i.amount = 1 FROM dbo.orders o JOIN dbo.order_items i ON i.item_id = o.order_id;",
    ] {
        let (c, _) = tsql(stmt);
        let (out, u) = last(&c);
        assert_eq!(u.status, Status::NotConverted, "{stmt}\n{out}");
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
        catalogs: Vec::new(),
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
