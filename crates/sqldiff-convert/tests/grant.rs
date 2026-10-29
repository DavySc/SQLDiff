//! T-SQL GRANT of object privileges on a table or view becomes the same
//! PostgreSQL GRANT (rule `tsql.stmt.grant`), which the engine compares
//! when the catalogs show the object is a table.
//! Grants whose scope differs (a schema, a database permission, EXECUTE,
//! ALL, columns) stay refused with the reason.

use sqldiff_convert::{check, rewrite_in, Conversion, Status};
use sqldiff_core::{Artifact, Dialect, Options};

const TABLES: &str = "CREATE SCHEMA Sales;\nGO\nCREATE SCHEMA Application;\nGO\nCREATE TABLE Sales.Customers (id int NOT NULL);\nCREATE TABLE Application.Logs (id int NOT NULL);\n";

/// Converted and checked with the tables in both catalogs.
fn pg(src: &str) -> Conversion {
    let target = rewrite_in(TABLES, Dialect::Tsql, Dialect::Postgres, None)
        .output()
        .to_string();
    let opts = Options {
        catalog_a: vec![Artifact::new("t.sql", Dialect::Tsql, TABLES)],
        catalog_b: vec![Artifact::new("t.sql", Dialect::Postgres, &target)],
        ..Options::default()
    };
    check(
        src,
        rewrite_in(src, Dialect::Tsql, Dialect::Postgres, None),
        &opts,
    )
}

#[test]
fn object_privileges_are_granted_alike() {
    let c = pg("GRANT SELECT, UPDATE ON Sales.Customers TO [Great Lakes Sales];\nGO\nGRANT INSERT ON OBJECT::Application.Logs TO WebApi WITH GRANT OPTION;\n");
    assert!(
        c.output
            .contains("GRANT SELECT, UPDATE ON Sales.Customers TO \"Great Lakes Sales\";"),
        "{}",
        c.output
    );
    assert!(
        c.output
            .contains("GRANT INSERT ON Application.Logs TO WebApi WITH GRANT OPTION;"),
        "{}",
        c.output
    );
    for u in &c.units {
        assert_eq!(u.status, Status::Assumptions, "{:#?}", u.notes);
        assert!(u.notes.iter().any(|n| n.rule == "ASM-GRANT-PRINCIPAL"));
    }
}

#[test]
fn a_grant_on_an_object_no_catalog_shows_is_a_table_stays_unproven() {
    let c = pg("GRANT SELECT ON dbo.f TO WebApi;\n");
    assert!(
        c.output.contains("GRANT SELECT ON f TO WebApi;"),
        "{}",
        c.output
    );
    assert_eq!(
        c.units[0].status,
        Status::Unproven,
        "{:#?}",
        c.units[0].notes
    );
}

#[test]
fn grants_of_another_scope_stay_refused() {
    for (src, why) in [
        (
            "GRANT SELECT ON SCHEMA::WebApi TO WebApi;",
            "also covers objects created later",
        ),
        ("GRANT CONNECT TO WebApi;", "a database permission"),
        (
            "GRANT EXECUTE ON dbo.p TO WebApi;",
            "ON FUNCTION or ON PROCEDURE",
        ),
        (
            "GRANT ALL ON dbo.t TO WebApi;",
            "another set of permissions",
        ),
        ("GRANT SELECT (a) ON dbo.t TO WebApi;", "on columns"),
    ] {
        let c = pg(src);
        let u = &c.units[0];
        assert_eq!(u.status, Status::NotConverted, "{src}");
        assert!(u.notes[0].text.contains(why), "{src}: {}", u.notes[0].text);
    }
}
