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
    assert_eq!(run(ok, T, pg, P, true).verdict, Verdict::EqualUnderAssumptions);
    let pg2 = "CREATE VIEW v AS SELECT customer_id, COALESCE(nick, email) AS n FROM customer;";
    let narrow =
        "CREATE VIEW dbo.v AS SELECT customer_id, ISNULL(nick, email) AS n FROM dbo.customer;";
    let r = run(narrow, T, pg2, P, true);
    assert_eq!(r.verdict, Verdict::Different, "{:?}", r.findings);
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
        run(&merge("WITH (HOLDLOCK)"), T, pg, P, true).verdict,
        Verdict::EqualUnderAssumptions
    );
    let r = run(&merge(""), T, pg, P, true);
    assert_eq!(r.verdict, Verdict::Different);
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
    assert_eq!(run(&ts("item_id"), T, pg, P, true).verdict, Verdict::EqualUnderAssumptions);
    // `i.name IS NULL` also keeps matched rows whose name is NULL: not the same rewrite.
    assert!(!matches!(
        run(&ts("name"), T, pg, P, false).verdict,
        Verdict::Equal | Verdict::EqualUnderAssumptions
    ));
}

#[test]
fn string_comparison_collation_is_explicit() {
    let ts = "CREATE FUNCTION dbo.f (@Email varchar(254)) RETURNS int AS
      BEGIN RETURN (SELECT customer_id FROM dbo.customer WHERE email = @Email); END";
    let pg = "CREATE FUNCTION f(p_email varchar(254)) RETURNS integer LANGUAGE plpgsql AS $$
      BEGIN RETURN (SELECT customer_id FROM customer WHERE email = p_email); END; $$;";
    let r = run(ts, T, pg, P, true);
    assert_eq!(r.verdict, Verdict::Different);
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
        run(&page(T, "created_at"), T, &page(P, "created_at"), P, true).verdict,
        Verdict::EqualUnderAssumptions
    );
    let r = run(&page(T, "shipped_at"), T, &page(P, "shipped_at"), P, true);
    assert_eq!(r.verdict, Verdict::Different);
    assert!(rules(&r).contains(&"equiv.null-ordering"));
    // Without catalog facts the nullability is unknown: never equal.
    assert_eq!(
        run(&page(T, "created_at"), T, &page(P, "created_at"), P, false).verdict,
        Verdict::Unknown
    );
}

#[test]
fn quoted_names_keep_their_case() {
    let a = "CREATE VIEW v AS SELECT \"Name\" FROM item;";
    let b = "CREATE VIEW V AS SELECT NAME FROM ITEM;";
    assert!(!matches!(
        run(a, P, b, D, false).verdict,
        Verdict::Equal | Verdict::EqualUnderAssumptions
    ));
    let a = "CREATE VIEW v AS SELECT name FROM item;";
    assert_eq!(run(a, P, b, D, false).verdict, Verdict::EqualUnderAssumptions);
}

#[test]
fn integer_to_string_casts() {
    let pg = "CREATE VIEW v AS SELECT item_id::text AS k FROM item;";
    let ok = "CREATE VIEW dbo.v AS SELECT CAST(item_id AS varchar(11)) AS k FROM dbo.item;";
    let short = "CREATE VIEW dbo.v AS SELECT CAST(item_id AS varchar(5)) AS k FROM dbo.item;";
    assert_eq!(run(ok, T, pg, P, true).verdict, Verdict::EqualUnderAssumptions);
    assert_eq!(run(short, T, pg, P, true).verdict, Verdict::Unknown);
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
    assert_eq!(r.verdict, Verdict::Different, "{:?}", rules(&r));
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
    assert_eq!(run(ts, T, pg, P, true).verdict, Verdict::Unknown);
}

fn not_equal(r: &Report) -> bool {
    !matches!(r.verdict, Verdict::Equal | Verdict::EqualUnderAssumptions)
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
    assert!(rules(&r).contains(&"stmt.modified"), "{:?}", rules(&r));
}

#[test]
fn in_list_of_strings_depends_on_collation() {
    let ts =
        "CREATE VIEW dbo.v AS SELECT customer_id FROM dbo.customer WHERE email IN ('A@x.com');";
    let pg = "CREATE VIEW v AS SELECT customer_id FROM customer WHERE email IN ('A@x.com');";
    for catalog in [false, true] {
        let r = run(ts, T, pg, P, catalog);
        assert!(not_equal(&r), "catalog={catalog}: {:?}", r.verdict);
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
    assert_eq!(run(&ts(50), T, pg, P, true).verdict, Verdict::EqualUnderAssumptions);
    let r = run(&ts(10), T, pg, P, true);
    assert!(not_equal(&r), "{:?}", r.verdict);
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
    assert_eq!(r.verdict, Verdict::Different, "{:?}", r.findings);
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
        with(&ts("unit_price > 0"), &pg("unit_price > 0")).verdict,
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
    assert_eq!(r.verdict, Verdict::Different, "{:?}", r.findings);
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
    assert!(not_equal(&r), "{:?} {:?}", r.verdict, rules(&r));
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
    let r = run(&ts_delete("WITH EXECUTE AS OWNER"), T, &pg_delete("SECURITY DEFINER"), P, true);
    assert_eq!(r.verdict, Verdict::EqualUnderAssumptions, "{:?} {:?}", r.findings, r.unknown_reasons);
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
    assert_eq!(r.verdict, Verdict::EqualUnderAssumptions, "{:?}", r.unknown_reasons);
    let r = run(&ts_delete("WITH EXECUTE AS OWNER"), T, &db2_delete("SET OPTION COMMIT = *CS, USRPRF = *OWNER"), D, true);
    assert_eq!(r.verdict, Verdict::EqualUnderAssumptions, "{:?}", r.unknown_reasons);
}

#[test]
fn owner_against_caller_privileges_is_a_difference() {
    // The converter used to drop EXECUTE AS OWNER: callers holding only
    // EXECUTE then fail on PostgreSQL.
    let r = run(&ts_delete("WITH EXECUTE AS OWNER"), T, &pg_delete(""), P, true);
    assert_eq!(r.verdict, Verdict::Different);
    assert!(rules(&r).contains(&"equiv.security-context"));
    let r = run(&ts_delete("WITH ENCRYPTION, EXECUTE AS OWNER"), T, &pg_delete(""), P, true);
    assert_eq!(r.verdict, Verdict::Different);
    let r = run(&pg_delete("SECURITY DEFINER"), P, &db2_delete("SET OPTION USRPRF = *USER"), D, true);
    assert_eq!(r.verdict, Verdict::Different);
    // T-SQL's default context can still skip checks along ownership chains:
    // against an owner's context the outcome depends on object ownership.
    let r = run(&ts_delete(""), T, &pg_delete("SECURITY DEFINER"), P, true);
    assert_eq!(r.verdict, Verdict::Unknown);
    // Db2 without USRPRF: caller's authority only as a named assumption.
    let r = run(&ts_delete(""), T, &db2_delete(""), D, true);
    assert_eq!(r.verdict, Verdict::EqualUnderAssumptions, "{:?}", r.unknown_reasons);
    assert!(assumption_ids(&r).contains(&"ASM-DB2-USRPRF-USER"));
    assert!(assumption_ids(&r).contains(&"ASM-TSQL-OWNERSHIP-CHAIN"));
    let r = run(&ts_delete("WITH EXECUTE AS OWNER"), T, &db2_delete(""), D, true);
    assert_eq!(r.verdict, Verdict::Unknown);
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
        assert!(not_equal(&r), "{opts}: {:?}", r.verdict);
    }
    for opts in ["SET TimeZone = 'UTC'", "SET lock_timeout TO '1s'", "PARALLEL SAFE", "SET search_path FROM CURRENT"] {
        let r = run(&ts_delete(""), T, &pg_delete(opts), P, true);
        assert!(not_equal(&r), "{opts}: {:?}", r.verdict);
    }
    for opts in [
        "SET OPTION DATFMT = *MDY",
        "COMMIT ON RETURN YES",
        "NEW SAVEPOINT LEVEL",
        "DYNAMIC RESULT SETS 1",
        "CONCURRENT ACCESS RESOLUTION SKIP LOCKED DATA",
    ] {
        let r = run(&ts_delete(""), T, &db2_delete(opts), D, true);
        assert!(not_equal(&r), "{opts}: {:?}", r.verdict);
    }
}

#[test]
fn routine_options_without_effect_or_with_a_named_assumption() {
    let r = run(&ts_delete("WITH RECOMPILE"), T, &pg_delete("SET work_mem = '64MB' COST 10"), P, true);
    assert_eq!(r.verdict, Verdict::EqualUnderAssumptions, "{:?}", r.unknown_reasons);
    let r = run(&ts_delete(""), T, &pg_delete("SET search_path = app, pg_temp"), P, true);
    assert_eq!(r.verdict, Verdict::EqualUnderAssumptions, "{:?}", r.unknown_reasons);
    assert!(assumption_ids(&r).contains(&"ASM-DEFAULT-SCHEMA"));
    let r = run(
        &ts_delete(""),
        T,
        &db2_delete("SPECIFIC purge_item1 NOT DETERMINISTIC SET OPTION COMMIT = *CS, USRPRF = *USER, DBGVIEW = *SOURCE"),
        D,
        true,
    );
    assert_eq!(r.verdict, Verdict::EqualUnderAssumptions, "{:?}", r.unknown_reasons);
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
    assert!(r.unknown_reasons.iter().any(|x| x.contains("labelled STABLE")), "{:?}", r.unknown_reasons);
    // SECURITY DEFINER procedures cannot COMMIT.
    let pg = "CREATE PROCEDURE purge_item(p_id integer) LANGUAGE plpgsql SECURITY DEFINER AS $$
      BEGIN DELETE FROM item_price WHERE item_id = p_id; COMMIT; END; $$;";
    let ts = "CREATE PROCEDURE dbo.purge_item @Id int WITH EXECUTE AS OWNER AS
      BEGIN BEGIN TRAN; DELETE FROM dbo.item_price WHERE item_id = @Id; COMMIT; END";
    let r = run(ts, T, pg, P, true);
    assert!(not_equal(&r));
    assert!(
        r.unknown_reasons.iter().any(|x| x.contains("invalid transaction termination")),
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
        assert_eq!(r.verdict, Verdict::EqualUnderAssumptions, "{s}");
        assert!(assumption_ids(&r).contains(&"ASM-DEFAULT-SCHEMA"));
    }
    assert!(not_equal(&run(&ts_delete(""), T, &pg("archive."), P, true)));
    let sales = TS_DELETE.replace("{opts}", "").replace("dbo.item_price", "Sales.item_price");
    assert_eq!(run(&sales, T, &pg("sales."), P, true).verdict, Verdict::EqualUnderAssumptions);
    assert!(not_equal(&run(&sales, T, &pg(""), P, true)));
    // Same dialect: moving a statement to another schema is a change.
    let archive = TS_DELETE.replace("{opts}", "").replace("dbo.item_price", "Archive.item_price");
    assert!(not_equal(&run(&sales, T, &archive, T, true)));
    let r = run(&ts_delete(""), T, &TS_DELETE.replace("{opts}", "").replace("dbo.item_price", "item_price"), T, true);
    assert!(matches!(r.verdict, Verdict::Equal | Verdict::EqualUnderAssumptions), "{:?}", r.findings);
}
