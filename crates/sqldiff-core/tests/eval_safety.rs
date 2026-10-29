//! The witness evaluator never panics and never calls a pair equal on a
//! query shape it does not model: ORDER BY, GROUP BY, aggregates, DISTINCT,
//! TOP / OFFSET and EXISTS in any combination.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const T: Dialect = Dialect::Tsql;

const TSQL_DDL: &str = "
CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, g int NOT NULL, n int NOT NULL, s nvarchar(10) NULL);
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

fn result_proc(q: &str) -> String {
    format!(
        "CREATE PROCEDURE dbo.p AS
         BEGIN
           {q};
         END"
    )
}

fn has_witness(r: &Report) -> bool {
    r.findings
        .iter()
        .any(|f| f.rule == "equiv.edit" && f.verdict == Verdict::Different)
}

#[test]
fn grouped_result_ordered_by_an_ungrouped_column_does_not_panic() {
    // `ORDER BY n` with `GROUP BY g` does not compile; the evaluator must
    // not sort by a value it picked from an arbitrary row of the group.
    let r = run(
        &result_proc("SELECT TOP 1 g FROM dbo.t WHERE n > 0 GROUP BY g ORDER BY g"),
        &result_proc("SELECT TOP 1 g FROM dbo.t WHERE n > 0 GROUP BY g ORDER BY n"),
    );
    assert_ne!(r.verdict(), Verdict::Equal, "{:#?}", r.findings);
    assert!(!has_witness(&r), "{:#?}", r.findings);
}

#[test]
fn exists_honours_top_zero() {
    let r = run(
        &exists_proc("SELECT TOP 1 1 FROM dbo.t WHERE n > 0"),
        &exists_proc("SELECT TOP 0 1 FROM dbo.t WHERE n > 0"),
    );
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
    assert!(has_witness(&r), "{:#?}", r.findings);
}

#[test]
fn exists_honours_offset() {
    let r = run(
        &exists_proc("SELECT 1 FROM dbo.t WHERE n > 0 ORDER BY id OFFSET 0 ROWS"),
        &exists_proc("SELECT 1 FROM dbo.t WHERE n > 0 ORDER BY id OFFSET 1 ROWS"),
    );
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
    assert!(has_witness(&r), "{:#?}", r.findings);
}

#[test]
fn exists_over_distinct_grouped_query_applies_having() {
    // HAVING removes every group, so the DISTINCT query has no rows.
    let r = run(
        &exists_proc("SELECT DISTINCT g FROM dbo.t WHERE n > 0 GROUP BY g"),
        &exists_proc("SELECT DISTINCT g FROM dbo.t WHERE n > 0 GROUP BY g HAVING COUNT(*) > 5"),
    );
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
    assert!(has_witness(&r), "{:#?}", r.findings);
}

#[test]
fn exists_over_distinct_aggregate_always_has_a_row() {
    // An aggregate without GROUP BY returns one row even over no rows.
    let r = run(
        &exists_proc("SELECT 1 FROM dbo.t WHERE n > 0"),
        &exists_proc("SELECT DISTINCT COUNT(*) FROM dbo.t WHERE n > 0"),
    );
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.findings);
    assert!(has_witness(&r), "{:#?}", r.findings);
}

#[test]
fn exists_ordered_by_output_position_or_alias_does_not_panic() {
    for (a, b) in [
        (
            "SELECT TOP 1 g AS k FROM dbo.t GROUP BY g",
            "SELECT TOP 1 g AS k FROM dbo.t GROUP BY g ORDER BY k",
        ),
        (
            "SELECT TOP 1 g FROM dbo.t GROUP BY g",
            "SELECT TOP 1 g FROM dbo.t GROUP BY g ORDER BY 1",
        ),
        (
            "SELECT TOP 1 g FROM dbo.t GROUP BY g",
            "SELECT TOP 1 g FROM dbo.t GROUP BY g ORDER BY 2",
        ),
        (
            "SELECT TOP 1 COUNT(*) FROM dbo.t",
            "SELECT TOP 1 COUNT(*) FROM dbo.t ORDER BY COUNT(*) DESC",
        ),
    ] {
        let r = run(&exists_proc(a), &exists_proc(b));
        assert_ne!(r.verdict(), Verdict::Different, "{b}: {:#?}", r.findings);
    }
}

/// A small deterministic generator (no extra dependency).
struct Lcg(u64);

impl Lcg {
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        xs[((self.0 >> 33) % xs.len() as u64) as usize]
    }
}

/// A query from ORDER BY / GROUP BY / aggregate / DISTINCT / TOP / OFFSET
/// pieces, valid or not: the evaluator sees whatever the parser accepts.
fn gen_query(r: &mut Lcg, cond: &str) -> String {
    let distinct = r.pick(&["", "DISTINCT "]);
    let top = r.pick(&["", "", "TOP 0 ", "TOP 1 ", "TOP 2 ", "TOP (1) "]);
    let items = r.pick(&[
        "1",
        "g",
        "n",
        "g AS k",
        "g, COUNT(*)",
        "COUNT(*)",
        "MAX(n) AS k",
        "g, n",
        "s",
        "COUNT(DISTINCT s)",
        "(SELECT MAX(n) FROM dbo.t)",
    ]);
    let group = r.pick(&["", "", " GROUP BY g", " GROUP BY g, n", " GROUP BY s"]);
    let having = if group.is_empty() {
        ""
    } else {
        r.pick(&[
            "",
            "",
            " HAVING COUNT(*) > 1",
            " HAVING MAX(n) > 0",
            " HAVING n > 0",
        ])
    };
    let order = r.pick(&[
        "",
        "",
        " ORDER BY g",
        " ORDER BY n",
        " ORDER BY 1",
        " ORDER BY 2",
        " ORDER BY 3",
        " ORDER BY k",
        " ORDER BY COUNT(*) DESC",
        " ORDER BY g DESC, n",
        " ORDER BY s",
        " ORDER BY id",
    ]);
    let offset = if order.is_empty() || !top.is_empty() {
        ""
    } else {
        r.pick(&[
            "",
            "",
            " OFFSET 1 ROWS",
            " OFFSET 0 ROWS FETCH NEXT 1 ROWS ONLY",
        ])
    };
    format!("SELECT {distinct}{top}{items} FROM dbo.t WHERE {cond}{group}{having}{order}{offset}")
}

fn gen_proc(r: &mut Lcg, cond: &str) -> String {
    let q = gen_query(r, cond);
    match r.pick(&["exists", "not exists", "result", "scalar", "in"]) {
        "exists" => exists_proc(&q),
        "not exists" => format!(
            "CREATE PROCEDURE dbo.p AS
             BEGIN
               IF NOT EXISTS ({q}) RETURN 1;
               RETURN 0;
             END"
        ),
        "result" => result_proc(&q),
        "scalar" => format!(
            "CREATE PROCEDURE dbo.p AS
             BEGIN
               DECLARE @x int;
               SET @x = ({q});
               SELECT @x;
             END"
        ),
        _ => format!(
            "CREATE PROCEDURE dbo.p AS
             BEGIN
               SELECT id FROM dbo.t WHERE g IN ({q}) ORDER BY id;
             END"
        ),
    }
}

#[test]
fn fuzz_order_group_top_exists_never_panics() {
    let mut panics = Vec::new();
    for seed in 0..300u64 {
        let mut r = Lcg(seed);
        let a = gen_proc(&mut r, "n > 0");
        // The same procedure with an equivalent condition, and an unrelated one.
        let mut r2 = Lcg(seed);
        let same = gen_proc(&mut r2, "0 < n");
        let mut r3 = Lcg(seed.wrapping_mul(7919).wrapping_add(1));
        let other = gen_proc(&mut r3, "n >= 0");
        for (b, equivalent) in [(same, true), (other, false)] {
            let got = std::panic::catch_unwind(|| run(&a, &b));
            match got {
                Err(_) => panics.push(format!("A:\n{a}\nB:\n{b}")),
                Ok(rep) if equivalent => assert_ne!(
                    rep.verdict(),
                    Verdict::Different,
                    "equivalent rewrite called different\nA:\n{a}\nB:\n{b}\n{:#?}",
                    rep.findings
                ),
                Ok(_) => {}
            }
        }
    }
    assert!(
        panics.is_empty(),
        "{} panics, first:\n{}",
        panics.len(),
        panics[0]
    );
}
