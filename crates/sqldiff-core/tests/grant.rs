//! T-SQL GRANT compared with the PostgreSQL GRANT it became (`equiv::ddl`):
//! the same object privileges on the same table to the same principals,
//! with or without GRANT OPTION. Grants whose scope differs between the
//! dialects (a schema, a database permission, EXECUTE, ALL, columns), and
//! grants on an object that no catalog shows is a table, are not compared.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const CATALOG_T: &str = "CREATE SCHEMA Sales;\nCREATE TABLE Sales.Customers (id int NOT NULL);\nCREATE TABLE Sales.Orders (id int NOT NULL);\nCREATE TABLE dbo.t (id int NOT NULL);";
const CATALOG_P: &str = "CREATE SCHEMA sales;\nCREATE TABLE sales.customers (id integer NOT NULL);\nCREATE TABLE sales.orders (id integer NOT NULL);\nCREATE TABLE t (id integer NOT NULL);";

fn run(a: &str, b: &str) -> Report {
    let opts = Options {
        catalog_a: vec![Artifact::new("ca.sql", Dialect::Tsql, CATALOG_T)],
        catalog_b: vec![Artifact::new("cb.sql", Dialect::Postgres, CATALOG_P)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Postgres, b),
        &opts,
    )
}

fn ids(r: &Report) -> Vec<&str> {
    r.assumptions.iter().map(|x| x.id.as_str()).collect()
}

fn verdict(a: &str, b: &str) -> (Verdict, Report) {
    let r = run(a, b);
    (r.verdict(), r)
}

#[test]
fn the_same_privileges_on_the_same_table() {
    let (v, r) = verdict(
        "GRANT SELECT, UPDATE ON Sales.Customers TO [Great Lakes Sales];",
        "GRANT UPDATE, SELECT ON sales.customers TO \"Great Lakes Sales\";",
    );
    assert_eq!(
        v,
        Verdict::EqualUnderAssumptions,
        "{:#?}\n{:?}",
        r.findings,
        r.unknown_reasons
    );
    assert!(ids(&r).contains(&"ASM-GRANT-PRINCIPAL"), "{:?}", ids(&r));
    let (v, r) = verdict(
        "GRANT SELECT ON OBJECT::dbo.t TO WebApi WITH GRANT OPTION;",
        "GRANT SELECT ON TABLE t TO WebApi WITH GRANT OPTION;",
    );
    assert_eq!(
        v,
        Verdict::EqualUnderAssumptions,
        "{:#?}\n{:?}",
        r.findings,
        r.unknown_reasons
    );
}

#[test]
fn another_privilege_object_grantee_or_option_differs() {
    for b in [
        "GRANT SELECT ON sales.customers TO webapi;",
        "GRANT SELECT, UPDATE ON sales.orders TO webapi;",
        "GRANT SELECT, UPDATE ON sales.customers TO other;",
        "GRANT SELECT, UPDATE ON sales.customers TO webapi WITH GRANT OPTION;",
    ] {
        let (v, r) = verdict("GRANT SELECT, UPDATE ON Sales.Customers TO WebApi;", b);
        assert_eq!(v, Verdict::Different, "{b}\n{:#?}", r.findings);
    }
}

#[test]
fn grants_of_another_scope_are_not_compared() {
    for (a, b) in [
        // Not a table the catalog has: in T-SQL it may be a function.
        (
            "GRANT SELECT ON dbo.f TO WebApi;",
            "GRANT SELECT ON f TO webapi;",
        ),
        (
            "GRANT SELECT ON SCHEMA::WebApi TO WebApi;",
            "GRANT SELECT ON ALL TABLES IN SCHEMA webapi TO webapi;",
        ),
        (
            "GRANT CONNECT TO WebApi;",
            "GRANT CONNECT ON DATABASE wwi TO webapi;",
        ),
        (
            "GRANT EXECUTE ON dbo.p TO WebApi;",
            "GRANT EXECUTE ON PROCEDURE p() TO webapi;",
        ),
        (
            "GRANT SELECT (a) ON dbo.t TO WebApi;",
            "GRANT SELECT (a) ON t TO webapi;",
        ),
        (
            "GRANT ALL ON dbo.t TO WebApi;",
            "GRANT ALL PRIVILEGES ON t TO webapi;",
        ),
    ] {
        let (v, r) = verdict(a, b);
        assert!(
            !matches!(v, Verdict::Equal | Verdict::EqualUnderAssumptions),
            "{a}\n{:?}",
            r.unknown_reasons
        );
    }
}
