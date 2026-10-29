//! Statement equivalence rules: each rewrite has an equal pair and a near miss
//! (design section 3, soundness rules).

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const DDL: &str = "
CREATE TABLE customer (customer_id integer PRIMARY KEY, email varchar(254) NOT NULL,
  first_name varchar(50), last_name varchar(50) NOT NULL, nick varchar(10));
CREATE TABLE item (item_id integer PRIMARY KEY, name varchar(100) NOT NULL);
CREATE TABLE item_price (item_id integer PRIMARY KEY, unit_price decimal(12,2) NOT NULL, updated_at timestamp);
CREATE TABLE price_feed (item_id integer NOT NULL, unit_price decimal(12,2) NOT NULL);
CREATE TABLE orders (order_id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY, customer_id integer NOT NULL,
  created_at timestamp NOT NULL, shipped_at timestamp);
";

fn run(a: &str, da: Dialect, b: &str, db: Dialect, catalog: bool) -> Report {
    let opts = if catalog {
        Options {
            catalog_a: vec![Artifact::new("catalog.sql", da, DDL)],
            catalog_b: vec![Artifact::new("catalog.sql", db, DDL)],
            ..Options::default()
        }
    } else {
        Options::default()
    };
    diff(
        &Artifact::new("a.sql", da, a),
        &Artifact::new("b.sql", db, b),
        &opts,
    )
}

fn rules(r: &Report) -> Vec<&str> {
    r.findings.iter().map(|f| f.rule.as_str()).collect()
}

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;
const D: Dialect = Dialect::Db2i;

#[test]
fn isnull_equals_coalesce_only_when_the_replacement_fits() {
    let pg = "CREATE VIEW v AS SELECT customer_id, COALESCE(first_name, '') AS n FROM customer;";
    let ok =
        "CREATE VIEW dbo.v AS SELECT customer_id, ISNULL(first_name, '') AS n FROM dbo.customer;";
    assert_eq!(
        run(ok, T, pg, P, true).verdict(),
        Verdict::EqualUnderAssumptions
    );
    let pg2 = "CREATE VIEW v AS SELECT customer_id, COALESCE(nick, email) AS n FROM customer;";
    let narrow =
        "CREATE VIEW dbo.v AS SELECT customer_id, ISNULL(nick, email) AS n FROM dbo.customer;";
    let r = run(narrow, T, pg2, P, true);
    assert_eq!(r.verdict(), Verdict::Different, "{:?}", r.findings);
    assert!(rules(&r).contains(&"equiv.isnull-type"));
}

#[test]
fn upsert_spellings_and_the_missing_lock_hint() {
    let pg = "CREATE PROCEDURE set_price(p_item integer, p_price numeric(12,2)) LANGUAGE sql AS $$
      INSERT INTO item_price (item_id, unit_price) VALUES (p_item, p_price)
      ON CONFLICT (item_id) DO UPDATE SET unit_price = EXCLUDED.unit_price; $$;";
    let merge = |hint: &str| {
        format!(
            "CREATE PROCEDURE dbo.set_price @Item int, @Price decimal(12,2) AS
             BEGIN
               MERGE dbo.item_price {hint} AS t
               USING (SELECT @Item AS item_id, @Price AS unit_price) AS s ON t.item_id = s.item_id
               WHEN MATCHED THEN UPDATE SET unit_price = s.unit_price
               WHEN NOT MATCHED THEN INSERT (item_id, unit_price) VALUES (s.item_id, s.unit_price);
             END"
        )
    };
    assert_eq!(
        run(&merge("WITH (HOLDLOCK)"), T, pg, P, true).verdict(),
        Verdict::EqualUnderAssumptions
    );
    let r = run(&merge(""), T, pg, P, true);
    assert_eq!(r.verdict(), Verdict::Different);
    assert!(rules(&r).contains(&"equiv.upsert-race"));
}

#[test]
fn anti_join_delete_needs_the_null_test_on_a_join_column() {
    let pg = "CREATE PROCEDURE purge() LANGUAGE sql AS $$
      DELETE FROM price_feed pf WHERE NOT EXISTS (SELECT 1 FROM item i WHERE i.item_id = pf.item_id); $$;";
    let ts = |col: &str| {
        format!(
            "CREATE PROCEDURE dbo.purge AS
             BEGIN
               DELETE pf FROM dbo.price_feed AS pf LEFT JOIN dbo.item AS i ON i.item_id = pf.item_id
                WHERE i.{col} IS NULL;
             END"
        )
    };
    assert_eq!(
        run(&ts("item_id"), T, pg, P, true).verdict(),
        Verdict::EqualUnderAssumptions
    );
    // `i.name IS NULL` also keeps matched rows whose name is NULL: not the same rewrite.
    assert!(!matches!(
        run(&ts("name"), T, pg, P, false).verdict(),
        Verdict::Equal | Verdict::EqualUnderAssumptions
    ));
}

#[test]
fn delete_join_is_a_semi_join_in_every_spelling() {
    let ts = |join: &str| {
        format!(
            "CREATE PROCEDURE dbo.purge AS
             BEGIN
               DELETE pf FROM dbo.price_feed AS pf {join} dbo.item AS i ON i.item_id = pf.item_id
                WHERE i.item_id > 100;
             END"
        )
    };
    let pg = |pred: &str| {
        format!(
            "CREATE PROCEDURE purge() LANGUAGE sql AS $$
             DELETE FROM price_feed AS pf USING item AS i WHERE {pred}; $$;"
        )
    };
    let db2 = |pred: &str| {
        format!(
            "CREATE PROCEDURE purge () LANGUAGE SQL
             BEGIN
               DELETE FROM price_feed AS pf WHERE EXISTS (SELECT 1 FROM item AS i WHERE {pred});
             END"
        )
    };
    let both = "i.item_id = pf.item_id AND i.item_id > 100";
    let equal = |r: Report| {
        assert!(
            matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
            "{:?}",
            r.findings
        )
    };
    equal(run(&ts("JOIN"), T, &pg(both), P, true));
    equal(run(&ts("INNER JOIN"), T, &db2(both), D, true));
    equal(run(&pg(both), P, &db2(both), D, true));
    // A dropped condition, or an outer join, is not the same delete.
    for (a, da, b, db) in [
        (ts("JOIN"), T, pg("i.item_id > 100"), P),
        (ts("JOIN"), T, db2("i.item_id = pf.item_id"), D),
        (ts("LEFT JOIN"), T, pg(both), P),
    ] {
        let r = run(&a, da, &b, db, true);
        assert!(
            !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
            "{a}\n{b}"
        );
    }
    // An unqualified name moves into the EXISTS subquery, where it resolves
    // to the inner table first. `name` is only in item: still the same.
    // `item_id` is in both tables, which T-SQL rejects (Msg 209): never equal.
    let bare = |col: &str| {
        let t = format!(
            "CREATE PROCEDURE dbo.purge AS
             BEGIN
               DELETE pf FROM dbo.price_feed AS pf JOIN dbo.item AS i ON i.item_id = pf.item_id
                WHERE {col} IS NULL;
             END"
        );
        (t, db2(&format!("i.item_id = pf.item_id AND {col} IS NULL")))
    };
    let (t, d) = bare("name");
    equal(run(&t, T, &d, D, true));
    let (t, d) = bare("item_id");
    let r = run(&t, T, &d, D, true);
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.findings
    );
}

/// T-SQL `UPDATE … FROM` with the target among the joined tables against
/// PostgreSQL `UPDATE … FROM` without it (rule T4): equal when the condition
/// that joined the target is in WHERE and the catalog shows a target row
/// meets at most one row; with several, both pick one unpredictably.
#[test]
fn update_from_join_moves_the_target_condition_to_where() {
    let ts = |join: &str, on: &str| {
        format!(
            "CREATE PROCEDURE dbo.reprice AS
             BEGIN
               UPDATE f SET unit_price = p.unit_price
               FROM dbo.item_price AS p {join} dbo.price_feed AS f ON {on}
               LEFT JOIN dbo.item AS i ON i.item_id = p.item_id
               WHERE i.name IS NOT NULL;
             END"
        )
    };
    let pg = |join: &str, pred: &str| {
        format!(
            "CREATE PROCEDURE reprice() LANGUAGE sql AS $$
             UPDATE price_feed AS f SET unit_price = p.unit_price
             FROM item_price AS p {join} item AS i ON i.item_id = p.item_id
             WHERE {pred}; $$;"
        )
    };
    let key = "f.item_id = p.item_id";
    let both = "f.item_id = p.item_id AND i.name IS NOT NULL";
    let equal = |r: Report| {
        assert!(
            matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
            "{:?}",
            r.findings
        )
    };
    let not_equal = |r: Report, why: &str| {
        assert!(
            !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
            "{why}: {:?}",
            r.findings
        )
    };
    equal(run(&ts("JOIN", key), T, &pg("LEFT JOIN", both), P, true));
    equal(run(
        &ts("INNER JOIN", key),
        T,
        &pg("LEFT OUTER JOIN", both),
        P,
        true,
    ));
    // A dropped condition, a LEFT JOIN that became inner, another key.
    not_equal(
        run(
            &ts("JOIN", key),
            T,
            &pg("LEFT JOIN", "i.name IS NOT NULL"),
            P,
            true,
        ),
        "dropped",
    );
    not_equal(
        run(&ts("JOIN", key), T, &pg("JOIN", both), P, true),
        "inner",
    );
    not_equal(
        run(
            &ts("JOIN", "f.item_id = p.item_id + 1"),
            T,
            &pg("LEFT JOIN", both),
            P,
            true,
        ),
        "key",
    );
    // Without the catalog no key is known.
    not_equal(
        run(&ts("JOIN", key), T, &pg("LEFT JOIN", both), P, false),
        "no catalog",
    );
    // price_feed has no key: an item price may meet several feed rows.
    let several = |d: Dialect| match d {
        Dialect::Tsql => "CREATE PROCEDURE dbo.reprice AS
             BEGIN
               UPDATE p SET unit_price = f.unit_price
               FROM dbo.item_price AS p JOIN dbo.price_feed AS f ON f.item_id = p.item_id;
             END"
        .to_string(),
        _ => "CREATE PROCEDURE reprice() LANGUAGE sql AS $$
             UPDATE item_price AS p SET unit_price = f.unit_price
             FROM price_feed AS f WHERE f.item_id = p.item_id; $$;"
            .to_string(),
    };
    not_equal(
        run(&several(T), T, &several(P), P, true),
        "several feed rows",
    );
    // PostgreSQL rejects the target in a join condition; T-SQL does not.
    let named = "CREATE PROCEDURE reprice() LANGUAGE sql AS $$
             UPDATE price_feed AS f SET unit_price = p.unit_price
             FROM item_price AS p LEFT JOIN item AS i ON i.item_id = f.item_id
             WHERE f.item_id = p.item_id AND i.name IS NOT NULL; $$;";
    not_equal(run(&ts("JOIN", key), T, named, P, true), "target in ON");
}

/// How SQL Server binds a T-SQL `UPDATE … FROM` target (checked on SQL
/// Server 2022): to the FROM reference of the same table, aliased or not,
/// and to a join of its own when the table is not in FROM. Where only one
/// side states a schema, the default schema decides, which is not modelled.
#[test]
fn update_from_binds_a_qualified_target_by_table() {
    let ts = |stmt: &str| format!("CREATE PROCEDURE dbo.reprice AS BEGIN {stmt}; END");
    let pg = |stmt: &str| format!("CREATE PROCEDURE reprice() LANGUAGE sql AS $$ {stmt}; $$;");
    let verdict = |a: &str, b: &str| run(&ts(a), T, &pg(b), P, true);
    let equal = |r: Report, why: &str| {
        assert!(
            matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
            "{why}: {:?}",
            r.findings
        )
    };
    let not_equal = |r: Report, why: &str| {
        assert!(
            !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
            "{why}: {:?}",
            r.findings
        )
    };
    let aliased = "UPDATE price_feed AS f SET unit_price = p.unit_price FROM item_price AS p WHERE p.item_id = f.item_id";
    equal(
        verdict(
            "UPDATE dbo.price_feed SET unit_price = p.unit_price FROM dbo.price_feed AS f JOIN dbo.item_price AS p ON p.item_id = f.item_id",
            aliased,
        ),
        "aliased reference",
    );
    equal(
        verdict(
            "UPDATE dbo.price_feed SET unit_price = 0 FROM dbo.price_feed WHERE item_id = 1",
            "UPDATE price_feed SET unit_price = 0 WHERE item_id = 1",
        ),
        "the only FROM table",
    );
    equal(
        verdict(
            "UPDATE dbo.price_feed SET unit_price = p.unit_price FROM dbo.item_price AS p WHERE p.item_id = dbo.price_feed.item_id",
            "UPDATE price_feed SET unit_price = p.unit_price FROM item_price AS p WHERE p.item_id = price_feed.item_id",
        ),
        "joined in implicitly",
    );
    // Joined in implicitly, the target is not limited by the FROM rows.
    not_equal(
        verdict(
            "UPDATE dbo.price_feed SET unit_price = p.unit_price FROM dbo.item_price AS p WHERE p.item_id = 1",
            "UPDATE price_feed AS f SET unit_price = p.unit_price FROM item_price AS p WHERE p.item_id = 1 AND f.item_id = 1",
        ),
        "implicit join is a cross join",
    );
    // Only one side states a schema: not modelled.
    not_equal(
        verdict(
            "UPDATE price_feed SET unit_price = p.unit_price FROM dbo.price_feed AS f JOIN dbo.item_price AS p ON p.item_id = f.item_id",
            aliased,
        ),
        "mixed schema forms",
    );
}

#[test]
fn string_comparison_collation_is_explicit() {
    let ts = "CREATE FUNCTION dbo.f (@Email varchar(254)) RETURNS int AS
      BEGIN RETURN (SELECT customer_id FROM dbo.customer WHERE email = @Email); END";
    let pg = "CREATE FUNCTION f(p_email varchar(254)) RETURNS integer LANGUAGE plpgsql AS $$
      BEGIN RETURN (SELECT customer_id FROM customer WHERE email = p_email); END; $$;";
    let r = run(ts, T, pg, P, true);
    assert_eq!(r.verdict(), Verdict::Different);
    assert!(rules(&r).contains(&"equiv.case-sensitivity"));
    assert!(rules(&r).contains(&"equiv.trailing-spaces"));
    // Db2 pads like T-SQL; UPPER on both sides emulates case-insensitivity.
    let db2 = "CREATE FUNCTION f (p_email VARCHAR(254)) RETURNS INTEGER LANGUAGE SQL
      BEGIN RETURN (SELECT customer_id FROM customer WHERE email = p_email); END;";
    let r = run(ts, T, db2, D, true);
    assert_eq!(rules(&r), vec!["equiv.case-sensitivity"]);
}

#[test]
fn null_ordering_needs_a_not_null_key() {
    let page = |d: Dialect, col: &str| match d {
        Dialect::Tsql => format!(
            "CREATE FUNCTION dbo.f (@C int) RETURNS TABLE AS RETURN
               SELECT order_id FROM dbo.orders WHERE customer_id = @C
               ORDER BY {col}, order_id OFFSET 0 ROWS FETCH NEXT 10 ROWS ONLY;"
        ),
        _ => format!(
            "CREATE FUNCTION f(p_c integer) RETURNS TABLE (order_id integer) LANGUAGE sql AS $$
               SELECT o.order_id FROM orders o WHERE o.customer_id = p_c
               ORDER BY o.{col}, o.order_id LIMIT 10; $$;"
        ),
    };
    assert_eq!(
        run(&page(T, "created_at"), T, &page(P, "created_at"), P, true).verdict(),
        Verdict::EqualUnderAssumptions
    );
    let r = run(&page(T, "shipped_at"), T, &page(P, "shipped_at"), P, true);
    assert_eq!(r.verdict(), Verdict::Different);
    assert!(rules(&r).contains(&"equiv.null-ordering"));
    // Without catalog facts the nullability is unknown: never equal.
    assert_eq!(
        run(&page(T, "created_at"), T, &page(P, "created_at"), P, false).verdict(),
        Verdict::Unknown
    );
}

#[test]
fn quoted_names_keep_their_case() {
    let a = "CREATE VIEW v AS SELECT \"Name\" FROM item;";
    let b = "CREATE VIEW V AS SELECT NAME FROM ITEM;";
    assert!(!matches!(
        run(a, P, b, D, false).verdict(),
        Verdict::Equal | Verdict::EqualUnderAssumptions
    ));
    let a = "CREATE VIEW v AS SELECT name FROM item;";
    assert_eq!(
        run(a, P, b, D, false).verdict(),
        Verdict::EqualUnderAssumptions
    );
}

#[test]
fn integer_to_string_casts() {
    let pg = "CREATE VIEW v AS SELECT item_id::text AS k FROM item;";
    let ok = "CREATE VIEW dbo.v AS SELECT CAST(item_id AS varchar(11)) AS k FROM dbo.item;";
    let short = "CREATE VIEW dbo.v AS SELECT CAST(item_id AS varchar(5)) AS k FROM dbo.item;";
    assert_eq!(
        run(ok, T, pg, P, true).verdict(),
        Verdict::EqualUnderAssumptions
    );
    assert_eq!(run(short, T, pg, P, true).verdict(), Verdict::Unknown);
}

#[test]
fn parameter_shadowed_by_a_column_is_not_resolved() {
    let pg = "CREATE FUNCTION f(item_id integer) RETURNS varchar LANGUAGE plpgsql AS $$
      BEGIN RETURN (SELECT name FROM item WHERE item.item_id = item_id); END; $$;";
    let db2 = "CREATE FUNCTION f (item_id INTEGER) RETURNS VARCHAR(100) LANGUAGE SQL
      BEGIN RETURN (SELECT name FROM item WHERE item.item_id = item_id); END;";
    // Equivalence does not resolve the name; binding does: under PL/pgSQL's
    // default `variable_conflict = error` the PostgreSQL side raises
    // "column reference is ambiguous", while Db2 takes the column.
    let r = run(pg, P, db2, D, true);
    assert_eq!(r.verdict(), Verdict::Different, "{:?}", rules(&r));
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == "bind.ambiguous-name")
        .expect("ambiguous-name finding");
    assert_eq!(f.assumptions, ["ASM-PG-VARIABLE-CONFLICT-ERROR"]);
    assert!(r
        .assumptions
        .iter()
        .any(|a| a.id == "ASM-PG-VARIABLE-CONFLICT-ERROR"));
}

#[test]
fn unsupported_control_flow_stays_unknown() {
    let ts = "CREATE PROCEDURE dbo.p @X int AS
      BEGIN
        IF @X > 0 UPDATE dbo.item SET name = 'x' WHERE item_id = @X;
      END";
    let pg = "CREATE PROCEDURE p(p_x integer) LANGUAGE plpgsql AS $$
      BEGIN UPDATE item SET name = 'x' WHERE item_id = p_x; END; $$;";
    assert_eq!(run(ts, T, pg, P, true).verdict(), Verdict::Unknown);
}

fn not_equal(r: &Report) -> bool {
    !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions)
}

#[test]
fn literal_case_in_unlowered_statements_is_a_change() {
    let ts = |s: &str| {
        format!(
            "CREATE PROCEDURE dbo.p AS
             BEGIN
               UPDATE c SET nick = '{s}' FROM dbo.customer AS c JOIN dbo.orders AS o ON o.customer_id = c.customer_id;
             END"
        )
    };
    let r = run(&ts("abc"), T, &ts("ABC"), T, false);
    assert!(not_equal(&r), "{:?}", r.findings);
    // The stored value keeps its case even under a case-insensitive collation.
    assert!(
        rules(&r).contains(&"stmt.modified") || rules(&r).contains(&"equiv.edit"),
        "{:?}",
        rules(&r)
    );
}

#[test]
fn in_list_of_strings_depends_on_collation() {
    let ts =
        "CREATE VIEW dbo.v AS SELECT customer_id FROM dbo.customer WHERE email IN ('A@x.com');";
    let pg = "CREATE VIEW v AS SELECT customer_id FROM customer WHERE email IN ('A@x.com');";
    for catalog in [false, true] {
        let r = run(ts, T, pg, P, catalog);
        assert!(not_equal(&r), "catalog={catalog}: {:?}", r.verdict());
    }
}

#[test]
fn different_parameter_defaults_are_not_equal() {
    let ts = |take: u32| {
        format!(
            "CREATE FUNCTION dbo.top_items (@take int = {take}) RETURNS TABLE AS
             RETURN SELECT TOP (@take) item_id FROM dbo.item ORDER BY item_id;"
        )
    };
    let pg = "CREATE FUNCTION top_items(p_take integer DEFAULT 50) RETURNS TABLE (item_id integer)
              LANGUAGE sql AS $$ SELECT item_id FROM item ORDER BY item_id LIMIT p_take; $$;";
    assert_eq!(
        run(&ts(50), T, pg, P, true).verdict(),
        Verdict::EqualUnderAssumptions
    );
    let r = run(&ts(10), T, pg, P, true);
    assert!(not_equal(&r), "{:?}", r.verdict());
}

#[test]
fn a_row_count_that_may_be_null_is_an_assumption_across_dialects() {
    // T-SQL raises Msg 1014 for TOP (NULL); PostgreSQL's LIMIT NULL returns
    // every row (SQL Server 2022, PostgreSQL 18.6).
    let ts = |ty: &str| {
        format!(
            "CREATE FUNCTION dbo.top_items (@take {ty}) RETURNS TABLE AS
             RETURN SELECT TOP (@take) item_id FROM dbo.item ORDER BY item_id;"
        )
    };
    let pg = |ty: &str| {
        format!(
            "CREATE FUNCTION top_items(p_take {ty}) RETURNS TABLE (item_id integer)
             LANGUAGE sql AS $$ SELECT item_id FROM item ORDER BY item_id LIMIT p_take; $$;"
        )
    };
    let r = run(&ts("int"), T, &pg("integer"), P, true);
    assert_eq!(r.verdict(), Verdict::EqualUnderAssumptions);
    assert!(
        assumption_ids(&r).contains(&"ASM-PAGE-COUNT-NOT-NULL"),
        "{:?}",
        assumption_ids(&r)
    );
    // Within one dialect NULL means the same.
    let r = run(&pg("integer"), P, &pg("integer"), P, true);
    assert!(!assumption_ids(&r).contains(&"ASM-PAGE-COUNT-NOT-NULL"));
    let r = run(&ts("int"), T, &ts("int"), T, true);
    assert!(!assumption_ids(&r).contains(&"ASM-PAGE-COUNT-NOT-NULL"));
    // A fractional count is an error in T-SQL (Msg 1060), rounded in PostgreSQL.
    let r = run(&ts("decimal(10,2)"), T, &pg("numeric(10,2)"), P, true);
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.verdict()
    );
    // Without ORDER BY any rows will do, which only a reading of the code can say.
    let ts0 = "CREATE VIEW dbo.v AS SELECT TOP 5 item_id FROM dbo.item;";
    let pg0 = "CREATE VIEW v AS SELECT item_id FROM item LIMIT 5;";
    let r = run(ts0, T, pg0, P, true);
    assert!(
        assumption_ids(&r).contains(&"ASM-PAGE-UNORDERED"),
        "{:?}",
        assumption_ids(&r)
    );
    assert!(!assumption_ids(&r).contains(&"ASM-ORDER-TOTAL"));
}

#[test]
fn exists_ignores_a_limit_of_one_row_or_more() {
    let ts = |n: u32| {
        format!(
            "CREATE VIEW dbo.v AS SELECT customer_id FROM dbo.customer c
             WHERE EXISTS (SELECT TOP {n} 1 FROM dbo.orders o WHERE o.customer_id = c.customer_id ORDER BY o.created_at);"
        )
    };
    let pg = "CREATE VIEW v AS SELECT customer_id FROM customer c
              WHERE EXISTS (SELECT 1 FROM orders o WHERE o.customer_id = c.customer_id);";
    let r = run(&ts(1), T, pg, P, true);
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:?}",
        r.findings
    );
    assert!(!assumption_ids(&r).contains(&"ASM-PAGE-UNORDERED"));
    // TOP 0 keeps no row: the EXISTS is false.
    let r = run(&ts(0), T, pg, P, true);
    assert!(not_equal(&r), "{:?}", r.verdict());
}

#[test]
fn declared_initial_value_survives_zero_rows() {
    let ts = "CREATE FUNCTION dbo.f (@id int) RETURNS int AS
             BEGIN
               DECLARE @r int = -1;
               SELECT @r = customer_id FROM dbo.customer WHERE customer_id = @id;
               RETURN @r;
             END";
    let pg = "CREATE FUNCTION f(p_id integer) RETURNS integer LANGUAGE plpgsql AS $$
              DECLARE r integer := -1;
              BEGIN
                SELECT customer_id INTO r FROM customer WHERE customer_id = p_id;
                RETURN r;
              END; $$;";
    // T-SQL keeps -1 when no row qualifies; PL/pgSQL SELECT INTO sets NULL.
    let r = run(ts, T, pg, P, true);
    assert_eq!(r.verdict(), Verdict::Different, "{:?}", r.findings);
    assert!(rules(&r).contains(&"equiv.no-rows"));
}

#[test]
fn anti_join_delete_keeps_joined_table_columns_apart() {
    let ddl =
        "CREATE TABLE price_feed (item_id integer NOT NULL, unit_price decimal(12,2) NOT NULL);
               CREATE TABLE item (item_id integer PRIMARY KEY, discontinued integer);";
    let pg = |cond: &str| {
        format!(
            "CREATE PROCEDURE purge() LANGUAGE sql AS $$
             DELETE FROM price_feed pf WHERE NOT EXISTS (SELECT 1 FROM item i WHERE i.item_id = pf.item_id) AND {cond}; $$;"
        )
    };
    let ts = |cond: &str| {
        format!(
            "CREATE PROCEDURE dbo.purge AS
             BEGIN
               DELETE pf FROM dbo.price_feed AS pf LEFT JOIN dbo.item AS i ON i.item_id = pf.item_id
                WHERE i.item_id IS NULL AND {cond};
             END"
        )
    };
    let with = |a: &str, b: &str| {
        diff(
            &Artifact::new("a.sql", T, a),
            &Artifact::new("b.sql", P, b),
            &Options {
                catalog_a: vec![Artifact::new("c.sql", T, ddl)],
                catalog_b: vec![Artifact::new("c.sql", P, ddl)],
                ..Options::default()
            },
        )
    };
    // `discontinued` belongs to the joined table, not the delete target.
    let a = ts("discontinued = 1");
    let b = pg("discontinued = 1");
    assert!(not_equal(&run(&a, T, &b, P, false)));
    assert!(not_equal(&with(&a, &b)));
    // A target column is fine once the catalog shows it.
    assert_eq!(
        with(&ts("unit_price > 0"), &pg("unit_price > 0")).verdict(),
        Verdict::EqualUnderAssumptions
    );
}

#[test]
fn tsql_continues_after_a_failed_statement() {
    let ts = |setting: &str| {
        format!(
            "CREATE PROCEDURE dbo.create_order @Customer int, @OrderId int OUTPUT AS
             BEGIN
               {setting}
               INSERT INTO dbo.orders (customer_id, created_at) VALUES (@Customer, SYSDATETIME());
               SET @OrderId = SCOPE_IDENTITY();
             END"
        )
    };
    let pg = "CREATE PROCEDURE create_order(p_customer integer, INOUT p_order_id integer DEFAULT NULL)
              LANGUAGE plpgsql AS $$
              BEGIN
                INSERT INTO orders (customer_id, created_at) VALUES (p_customer, statement_timestamp())
                RETURNING order_id INTO p_order_id;
              END; $$;";
    let r = run(&ts(""), T, pg, P, true);
    assert_eq!(r.verdict(), Verdict::Different, "{:?}", r.findings);
    assert!(rules(&r).contains(&"equiv.error-continues"));
    let r = run(&ts("SET XACT_ABORT ON;"), T, pg, P, true);
    assert!(
        !rules(&r).contains(&"equiv.error-continues"),
        "{:?}",
        rules(&r)
    );
}

/// Large character types (T-SQL `ntext`, Db2 `clob`) compare as strings: the
/// collation difference keeps the pair from being equal.
#[test]
fn large_character_columns_are_strings() {
    let r = diff(
        &Artifact::new(
            "a.sql",
            T,
            "CREATE VIEW dbo.v AS SELECT id FROM dbo.notes WHERE body = title;",
        ),
        &Artifact::new(
            "b.sql",
            D,
            "CREATE VIEW v AS SELECT id FROM notes WHERE body = title;",
        ),
        &Options {
            catalog_a: vec![Artifact::new(
                "c.sql",
                T,
                "CREATE TABLE dbo.notes (id int NOT NULL PRIMARY KEY, body ntext NULL, title ntext NULL);",
            )],
            catalog_b: vec![Artifact::new(
                "c.sql",
                D,
                "CREATE TABLE notes (id int NOT NULL PRIMARY KEY, body clob(1M), title clob(1M));",
            )],
            ..Options::default()
        },
    );
    assert!(not_equal(&r), "{:?} {:?}", r.verdict(), rules(&r));
}

fn assumption_ids(r: &Report) -> Vec<&str> {
    r.assumptions.iter().map(|a| a.id.as_str()).collect()
}

const TS_DELETE: &str = "CREATE PROCEDURE dbo.purge_item @Id int {opts} AS
  BEGIN
    DELETE FROM dbo.item_price WHERE item_id = @Id;
  END";

fn ts_delete(opts: &str) -> String {
    TS_DELETE.replace("{opts}", opts)
}

fn pg_delete(opts: &str) -> String {
    format!(
        "CREATE PROCEDURE purge_item(p_id integer) LANGUAGE plpgsql {opts} AS $$
         BEGIN DELETE FROM item_price WHERE item_id = p_id; END; $$;"
    )
}

fn db2_delete(opts: &str) -> String {
    format!(
        "CREATE PROCEDURE purge_item (IN p_id INTEGER) LANGUAGE SQL MODIFIES SQL DATA {opts}
         BEGIN DELETE FROM item_price WHERE item_id = p_id; END;"
    )
}

#[test]
fn execute_as_owner_matches_security_definer() {
    let r = run(
        &ts_delete("WITH EXECUTE AS OWNER"),
        T,
        &pg_delete("SECURITY DEFINER"),
        P,
        true,
    );
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:?} {:?}",
        r.findings,
        r.unknown_reasons
    );
    assert!(assumption_ids(&r).contains(&"ASM-OWNER-PRIVILEGES"));
    assert!(assumption_ids(&r).contains(&"ASM-IDENT-FOLD"));
    // Options after a comma count too.
    let r = run(
        &ts_delete("WITH RECOMPILE, EXECUTE AS OWNER"),
        T,
        &pg_delete("SECURITY DEFINER"),
        P,
        true,
    );
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:?}",
        r.unknown_reasons
    );
    let r = run(
        &ts_delete("WITH EXECUTE AS OWNER"),
        T,
        &db2_delete("SET OPTION COMMIT = *CS, USRPRF = *OWNER"),
        D,
        true,
    );
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:?}",
        r.unknown_reasons
    );
}

#[test]
fn owner_against_caller_privileges_is_a_difference() {
    // The converter used to drop EXECUTE AS OWNER: callers holding only
    // EXECUTE then fail on PostgreSQL.
    let r = run(
        &ts_delete("WITH EXECUTE AS OWNER"),
        T,
        &pg_delete(""),
        P,
        true,
    );
    assert_eq!(r.verdict(), Verdict::Different);
    assert!(rules(&r).contains(&"equiv.security-context"));
    let r = run(
        &ts_delete("WITH ENCRYPTION, EXECUTE AS OWNER"),
        T,
        &pg_delete(""),
        P,
        true,
    );
    assert_eq!(r.verdict(), Verdict::Different);
    let r = run(
        &pg_delete("SECURITY DEFINER"),
        P,
        &db2_delete("SET OPTION USRPRF = *USER"),
        D,
        true,
    );
    assert_eq!(r.verdict(), Verdict::Different);
    // T-SQL's default context can still skip checks along ownership chains:
    // against an owner's context the outcome depends on object ownership.
    let r = run(&ts_delete(""), T, &pg_delete("SECURITY DEFINER"), P, true);
    assert_eq!(r.verdict(), Verdict::Unknown);
    // Db2 without USRPRF: caller's authority only as a named assumption.
    let r = run(&ts_delete(""), T, &db2_delete(""), D, true);
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:?}",
        r.unknown_reasons
    );
    assert!(assumption_ids(&r).contains(&"ASM-DB2-USRPRF-USER"));
    assert!(assumption_ids(&r).contains(&"ASM-TSQL-OWNERSHIP-CHAIN"));
    let r = run(
        &ts_delete("WITH EXECUTE AS OWNER"),
        T,
        &db2_delete(""),
        D,
        true,
    );
    assert_eq!(r.verdict(), Verdict::Unknown);
}

#[test]
fn routine_options_outside_the_comparison_stay_unknown() {
    let pg = pg_delete("");
    for opts in [
        "WITH EXECUTE AS SELF",
        "WITH EXECUTE AS 'app_user'",
        "WITH NATIVE_COMPILATION, SCHEMABINDING",
        "FOR REPLICATION",
    ] {
        let r = run(&ts_delete(opts), T, &pg, P, true);
        assert!(not_equal(&r), "{opts}: {:?}", r.verdict());
    }
    for opts in [
        "SET TimeZone = 'UTC'",
        "SET lock_timeout TO '1s'",
        "PARALLEL SAFE",
        "SET search_path FROM CURRENT",
    ] {
        let r = run(&ts_delete(""), T, &pg_delete(opts), P, true);
        assert!(not_equal(&r), "{opts}: {:?}", r.verdict());
    }
    for opts in [
        "SET OPTION DATFMT = *MDY",
        "COMMIT ON RETURN YES",
        "NEW SAVEPOINT LEVEL",
        "DYNAMIC RESULT SETS 1",
        "CONCURRENT ACCESS RESOLUTION SKIP LOCKED DATA",
    ] {
        let r = run(&ts_delete(""), T, &db2_delete(opts), D, true);
        assert!(not_equal(&r), "{opts}: {:?}", r.verdict());
    }
}

#[test]
fn routine_options_without_effect_or_with_a_named_assumption() {
    let r = run(
        &ts_delete("WITH RECOMPILE"),
        T,
        &pg_delete("SET work_mem = '64MB' COST 10"),
        P,
        true,
    );
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:?}",
        r.unknown_reasons
    );
    for path in [
        "public, pg_temp",
        "\"$user\", public",
        "pg_catalog, public",
        "'public'",
        "PUBLIC",
    ] {
        let r = run(
            &ts_delete(""),
            T,
            &pg_delete(&format!("SET search_path = {path}")),
            P,
            true,
        );
        assert_eq!(
            r.verdict(),
            Verdict::EqualUnderAssumptions,
            "{path}: {:?}",
            r.unknown_reasons
        );
        assert!(assumption_ids(&r).contains(&"ASM-DEFAULT-SCHEMA"));
    }
    // Unqualified names then resolve in another schema than dbo's counterpart.
    for path in [
        "other",
        "other, public",
        "pg_temp, public",
        "'other, public'",
        "app, pg_temp",
        "\"PUBLIC\"",
    ] {
        let r = run(
            &ts_delete(""),
            T,
            &pg_delete(&format!("SET search_path TO {path}")),
            P,
            true,
        );
        assert!(not_equal(&r), "{path}: {:?}", r.verdict());
    }
    let r = run(
        &ts_delete(""),
        T,
        &db2_delete("SPECIFIC purge_item1 NOT DETERMINISTIC SET OPTION COMMIT = *CS, USRPRF = *USER, DBGVIEW = *SOURCE"),
        D,
        true,
    );
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:?}",
        r.unknown_reasons
    );
    assert!(!assumption_ids(&r).contains(&"ASM-DB2-USRPRF-USER"));
}

#[test]
fn volatility_labels_and_restricted_procedures() {
    // A STABLE function that writes fails in PostgreSQL.
    let pg = "CREATE FUNCTION f(p_id integer) RETURNS integer LANGUAGE plpgsql STABLE AS $$
      BEGIN DELETE FROM item_price WHERE item_id = p_id; RETURN 1; END; $$;";
    let ts = "CREATE FUNCTION dbo.f (@Id int) RETURNS int AS BEGIN RETURN 1; END";
    let r = run(ts, T, pg, P, true);
    assert!(not_equal(&r));
    assert!(
        r.unknown_reasons
            .iter()
            .any(|x| x.contains("labelled STABLE")),
        "{:?}",
        r.unknown_reasons
    );
    // SECURITY DEFINER procedures cannot COMMIT.
    let pg = "CREATE PROCEDURE purge_item(p_id integer) LANGUAGE plpgsql SECURITY DEFINER AS $$
      BEGIN DELETE FROM item_price WHERE item_id = p_id; COMMIT; END; $$;";
    let ts = "CREATE PROCEDURE dbo.purge_item @Id int WITH EXECUTE AS OWNER AS
      BEGIN BEGIN TRAN; DELETE FROM dbo.item_price WHERE item_id = @Id; COMMIT; END";
    let r = run(ts, T, pg, P, true);
    assert!(not_equal(&r));
    assert!(
        r.unknown_reasons
            .iter()
            .any(|x| x.contains("invalid transaction termination")),
        "{:?}",
        r.unknown_reasons
    );
}

#[test]
fn schema_names_are_compared() {
    let pg = |schema: &str| {
        format!(
            "CREATE PROCEDURE purge_item(p_id integer) LANGUAGE plpgsql AS $$
             BEGIN DELETE FROM {schema}item_price WHERE item_id = p_id; END; $$;"
        )
    };
    // dbo and public are the default schemas.
    for s in ["", "public."] {
        let r = run(&ts_delete(""), T, &pg(s), P, true);
        assert_eq!(r.verdict(), Verdict::EqualUnderAssumptions, "{s}");
        assert!(assumption_ids(&r).contains(&"ASM-DEFAULT-SCHEMA"));
    }
    assert!(not_equal(&run(&ts_delete(""), T, &pg("archive."), P, true)));
    let sales = TS_DELETE
        .replace("{opts}", "")
        .replace("dbo.item_price", "Sales.item_price");
    assert_eq!(
        run(&sales, T, &pg("sales."), P, true).verdict(),
        Verdict::EqualUnderAssumptions
    );
    assert!(not_equal(&run(&sales, T, &pg(""), P, true)));
    // Same dialect: moving a statement to another schema is a change.
    let archive = TS_DELETE
        .replace("{opts}", "")
        .replace("dbo.item_price", "Archive.item_price");
    assert!(not_equal(&run(&sales, T, &archive, T, true)));
    let r = run(
        &ts_delete(""),
        T,
        &TS_DELETE
            .replace("{opts}", "")
            .replace("dbo.item_price", "item_price"),
        T,
        true,
    );
    assert!(
        matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.findings
    );
}

/// Db2 for i's `FROM SYSIBM.SYSDUMMY1` is the one-row FROM the others leave out.
#[test]
fn db2_sysdummy1_is_no_from() {
    let r = run(
        "SELECT 1 AS x;",
        T,
        "SELECT 1 AS x FROM SYSIBM.SYSDUMMY1;",
        D,
        false,
    );
    // Equal, except that T-SQL returns the column as `x` and Db2 for i as
    // `X` (decision 1: result column names are observed).
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:?}",
        rules(&r)
    );
    let ids: Vec<&str> = r.assumptions.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, ["ASM-RESULT-NAMES-CI"]);
    let r = run(
        "SELECT 1 AS x;",
        T,
        "SELECT 2 AS x FROM SYSIBM.SYSDUMMY1;",
        D,
        false,
    );
    assert_ne!(r.verdict(), Verdict::Equal, "{:?}", rules(&r));
    // Its column is not an empty FROM's.
    let r = run(
        "SELECT 'Y' AS x;",
        T,
        "SELECT IBMREQD AS x FROM SYSIBM.SYSDUMMY1;",
        D,
        false,
    );
    assert_ne!(r.verdict(), Verdict::Equal, "{:?}", rules(&r));
    // Only Db2 for i has it.
    let r = run(
        "SELECT 1 AS x;",
        T,
        "SELECT 1 AS x FROM sysibm.sysdummy1;",
        P,
        false,
    );
    assert_ne!(r.verdict(), Verdict::Equal, "{:?}", rules(&r));
}

/// An unterminated string literal has no value: it used to panic (or, for
/// `'abc`, read as `ab`).
#[test]
fn unterminated_string_literal_is_not_read() {
    for text in ["SELECT '", "SELECT 'é", "SELECT 'abc", "SELECT 'it''"] {
        for d in [Dialect::Tsql, Dialect::Postgres, Dialect::Db2i] {
            let a = sqldiff_core::Artifact::new("a.sql", d, text);
            let b = sqldiff_core::Artifact::new("b.sql", d, "SELECT 'ab'");
            let r = sqldiff_core::diff(&a, &b, &sqldiff_core::Options::default());
            assert_ne!(
                r.verdict(),
                sqldiff_core::Verdict::Equal,
                "{text:?} ({d:?})"
            );
        }
    }
}

/// `SELECT @r = <expr> FROM ...` against `SELECT <expr> INTO r FROM ...`,
/// with `r` declared as -1 on both sides and returned.
fn select_assign(ts_query: &str, pg_query: &str) -> Report {
    let ts = format!(
        "CREATE FUNCTION dbo.f (@id int) RETURNS int AS
         BEGIN
           DECLARE @r int = -1;
           {ts_query};
           RETURN @r;
         END"
    );
    let pg = format!(
        "CREATE FUNCTION f(p_id integer) RETURNS integer LANGUAGE plpgsql AS $$
         DECLARE r integer := -1;
         BEGIN
           {pg_query};
           RETURN r;
         END; $$;"
    );
    run(&ts, T, &pg, P, true)
}

#[test]
fn ungrouped_aggregate_assign_always_returns_one_row() {
    // Verified on SQL Server 2022 and PostgreSQL 18: on an empty table (or a
    // WHERE no row meets) both assign 0 for COUNT and NULL for MAX; neither
    // keeps the old value.
    for (ts, pg) in [
        (
            "SELECT @r = COUNT(*) FROM dbo.customer",
            "SELECT count(*) INTO r FROM customer",
        ),
        (
            "SELECT @r = COUNT(*) FROM dbo.customer WHERE customer_id = @id",
            "SELECT count(*) INTO r FROM customer WHERE customer_id = p_id",
        ),
        (
            "SELECT @r = MAX(customer_id) FROM dbo.customer WHERE customer_id > @id",
            "SELECT max(customer_id) INTO r FROM customer WHERE customer_id > p_id",
        ),
        (
            "SELECT @r = ISNULL(MAX(customer_id), 0) FROM dbo.customer",
            "SELECT COALESCE(max(customer_id), 0) INTO r FROM customer",
        ),
    ] {
        let r = select_assign(ts, pg);
        assert!(
            matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
            "{ts}: {:?} {:?}",
            r.verdict(),
            r.findings
        );
    }
}

#[test]
fn aggregate_assign_that_can_return_no_row_keeps_the_finding() {
    // Verified on SQL Server 2022 and PostgreSQL 18: on an empty table each
    // returns no row, so T-SQL keeps -1 and SELECT INTO sets NULL.
    for (ts, pg) in [
        (
            "SELECT @r = COUNT(*) FROM dbo.customer GROUP BY last_name",
            "SELECT count(*) INTO r FROM customer GROUP BY last_name",
        ),
        (
            "SELECT @r = COUNT(*) FROM dbo.customer HAVING COUNT(*) > 1",
            "SELECT count(*) INTO r FROM customer HAVING count(*) > 1",
        ),
        (
            "SELECT TOP 0 @r = COUNT(*) FROM dbo.customer",
            "SELECT count(*) INTO r FROM customer LIMIT 0",
        ),
        // The aggregate is in a subquery: one row per customer, none when empty.
        (
            "SELECT @r = (SELECT MAX(item_id) FROM dbo.item) FROM dbo.customer",
            "SELECT (SELECT max(item_id) FROM item) INTO r FROM customer",
        ),
    ] {
        let r = select_assign(ts, pg);
        assert_eq!(r.verdict(), Verdict::Different, "{ts}: {:?}", r.findings);
        assert!(
            rules(&r).contains(&"equiv.no-rows"),
            "{ts}: {:?}",
            r.findings
        );
    }
}

#[test]
fn select_assign_without_from_is_a_plain_assignment() {
    // SQL Server 2022: `SELECT @r = (subquery)` assigns NULL when the subquery
    // has no row, as `SET` and PL/pgSQL `:=` do.
    let r = select_assign(
        "SELECT @r = (SELECT COUNT(*) FROM dbo.customer)",
        "r := (SELECT count(*) FROM customer)",
    );
    assert!(
        matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?} {:?}",
        r.verdict(),
        r.findings
    );
    // The literal checks read it as the assignment it is, as for `SET @r = 1`
    // (SQL Server 2022: SELECT and SET assign, round, cut off and overflow alike).
    for ts in ["SET @r = 1", "SELECT @r = 1"] {
        let r = select_assign(ts, "r := 1");
        assert!(
            matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
            "{ts}: {:?} {:?}",
            r.verdict(),
            r.findings
        );
    }
    // With a WHERE the query can return no row: T-SQL then keeps -1.
    let r = select_assign("SELECT @r = 1 WHERE @id = 0", "r := 1");
    assert!(not_equal(&r), "{:?}", r.verdict());
}

#[test]
fn ungrouped_sum_and_avg_return_one_row_but_stay_the_dialects_own() {
    // Verified on SQL Server 2022 and PostgreSQL 18: on an empty table SUM and
    // AVG give one NULL row on both, so no side keeps the old value; AVG of
    // ints is 1 in T-SQL and 1.5 in PostgreSQL, so they are not equal either.
    for (ts, pg) in [
        (
            "SELECT @r = SUM(customer_id) FROM dbo.customer",
            "SELECT sum(customer_id) INTO r FROM customer",
        ),
        (
            "SELECT @r = AVG(customer_id) FROM dbo.customer WHERE customer_id > @id",
            "SELECT avg(customer_id) INTO r FROM customer WHERE customer_id > p_id",
        ),
    ] {
        let r = select_assign(ts, pg);
        assert!(not_equal(&r), "{ts}: {:?}", r.verdict());
        assert!(
            !rules(&r).contains(&"equiv.no-rows"),
            "{ts}: {:?}",
            r.findings
        );
    }
    let r = select_assign(
        "SELECT @r = SUM(customer_id) FROM dbo.customer GROUP BY last_name",
        "SELECT sum(customer_id) INTO r FROM customer GROUP BY last_name",
    );
    assert!(rules(&r).contains(&"equiv.no-rows"), "{:?}", r.findings);
}

#[test]
fn a_qualified_call_is_a_user_function_not_the_built_in() {
    // SQL Server 2022: `dbo.max` is a user scalar function, called once per
    // row; on an empty table the variable keeps its value.
    let r = select_assign(
        "SELECT @r = dbo.max(customer_id) FROM dbo.customer",
        "SELECT max(customer_id) INTO r FROM customer",
    );
    assert!(not_equal(&r), "{:?} {:?}", r.verdict(), r.findings);
}
