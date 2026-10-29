//! The witness evaluator never panics and never calls a pair equal on a
//! query shape it does not model: ORDER BY, GROUP BY, aggregates, DISTINCT,
//! TOP / OFFSET and EXISTS in any combination.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const T: Dialect = Dialect::Tsql;

const TSQL_DDL: &str = "
CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, g int NOT NULL, n int NOT NULL);
";

fn run(a: &str, b: &str) -> Report {
    let opts = Options {
        catalog_a: vec![Artifact::new("catalog.sql", T, TSQL_DDL)],
        catalog_b: vec![Artifact::new("catalog.sql", T, TSQL_DDL)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", T, a),
        &Artifact::new("b.sql", T, b),
        &opts,
    )
}

fn exists_proc(q: &str) -> String {
    format!(
        "CREATE PROCEDURE dbo.p AS
         BEGIN
           IF EXISTS ({q}) RETURN 1;
           RETURN 0;
         END"
    )
}

#[test]
fn exists_over_grouped_query_with_order_by_does_not_panic() {
    let r = run(
        &exists_proc("SELECT TOP 1 1 FROM dbo.t WHERE n > 0 GROUP BY g"),
        &exists_proc("SELECT TOP 1 1 FROM dbo.t WHERE n > 0 GROUP BY g ORDER BY n"),
    );
    assert_ne!(r.verdict(), Verdict::Equal, "{:#?}", r.findings);
}
