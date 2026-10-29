//! Dynamic SQL compared across dialects: T-SQL `EXEC (@sql)` /
//! `sp_executesql` against PL/pgSQL `EXECUTE '…'`, by the statement each
//! text runs. Text the code alone does not fix stays unknown.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;

const CAT_T: &str = "CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, a int NULL);\n";
const CAT_P: &str = "CREATE TABLE t (id int NOT NULL PRIMARY KEY, a int NULL);\n";

fn run(a: &str, b: &str) -> Report {
    let o = Options {
        catalog_a: vec![Artifact::new("cat.sql", T, CAT_T)],
        catalog_b: vec![Artifact::new("cat.pg.sql", P, CAT_P)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", T, a),
        &Artifact::new("b.sql", P, b),
        &o,
    )
}

fn tsql(body: &str) -> String {
    format!(
        "CREATE PROCEDURE dbo.p @x int\nAS\nBEGIN\n    SET XACT_ABORT ON;\n    DECLARE @SQL nvarchar(max);\n{body}\nEND;\n"
    )
}

fn pg(body: &str) -> String {
    format!(
        "CREATE OR REPLACE PROCEDURE p(p_x int)\nLANGUAGE plpgsql\nAS $$\nDECLARE\n    v_sql text;\nBEGIN\n{body}\nEND;\n$$;\n"
    )
}

fn dynamic_reasons(r: &Report) -> Vec<&String> {
    r.unknown_reasons
        .iter()
        .filter(|x| x.contains("dynamic SQL"))
        .collect()
}

#[test]
fn known_text_runs_the_same_statement() {
    let r = run(
        &tsql("    SET @SQL = N'UPDATE dbo.t SET a = 1 WHERE id = 2;';\n    EXECUTE (@SQL);"),
        &pg("    v_sql := 'UPDATE dbo.t SET a = 1 WHERE id = 2;';\n    EXECUTE 'UPDATE t SET a = 1 WHERE id = 2';"),
    );
    assert!(dynamic_reasons(&r).is_empty(), "{:?}", r.unknown_reasons);
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:#?} {:?}",
        r.findings,
        r.unknown_reasons
    );
    assert!(r
        .assumptions
        .iter()
        .any(|a| a.id == "ASM-TSQL-DYNAMIC-SESSION-OPTIONS"));
}

#[test]
fn a_literal_and_sp_executesql_read_the_same() {
    let r = run(
        &tsql("    EXEC sp_executesql N'DELETE FROM dbo.t WHERE id = 3';"),
        &pg("    EXECUTE $q$DELETE FROM t WHERE id = 3$q$;"),
    );
    assert!(dynamic_reasons(&r).is_empty(), "{:?}", r.unknown_reasons);
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:#?}",
        r.findings
    );
}

#[test]
fn another_statement_in_the_text_is_different() {
    let r = run(
        &tsql("    SET @SQL = N'UPDATE dbo.t SET a = 1 WHERE id = 2;';\n    EXECUTE (@SQL);"),
        &pg("    v_sql := 'UPDATE dbo.t SET a = 1 WHERE id = 2;';\n    EXECUTE 'UPDATE t SET a = 3 WHERE id = 2';"),
    );
    assert_eq!(r.verdict(), Verdict::Different, "{:?}", r.unknown_reasons);
    assert!(r
        .findings
        .iter()
        .any(|f| f.witness.contains("dynamic SQL") && f.witness.contains("(3, 2)")));
}

#[test]
fn a_witness_is_not_kept_after_statements_that_change_the_tables() {
    // The DELETE before leaves no row the witness needs: only unknown.
    let r = run(
        &tsql("    DELETE FROM dbo.t;\n    SET @SQL = N'UPDATE dbo.t SET a = 1 WHERE id = 2;';\n    EXECUTE (@SQL);"),
        &pg("    DELETE FROM t;\n    v_sql := 'UPDATE dbo.t SET a = 1 WHERE id = 2;';\n    EXECUTE 'UPDATE t SET a = 3 WHERE id = 2';"),
    );
    assert!(
        !r.findings
            .iter()
            .any(|f| f.verdict == Verdict::Different && f.witness.contains("dynamic SQL")),
        "{:#?}",
        r.findings
    );
    assert!(dynamic_reasons(&r)
        .iter()
        .any(|x| x.contains("may not leave")));
}

#[test]
fn text_from_a_parameter_is_unknown() {
    let r = run(
        "CREATE PROCEDURE dbo.p @s nvarchar(max)\nAS\nBEGIN\n    SET XACT_ABORT ON;\n    EXECUTE (@s);\nEND;\n",
        "CREATE OR REPLACE PROCEDURE p(p_s text)\nLANGUAGE plpgsql\nAS $$\nBEGIN\n    EXECUTE p_s;\nEND;\n$$;\n",
    );
    assert_ne!(r.verdict(), Verdict::Equal);
    assert!(
        dynamic_reasons(&r)
            .iter()
            .any(|x| x.contains("not known before it runs") || x.contains("not a constant")),
        "{:?}",
        r.unknown_reasons
    );
}

#[test]
fn texts_the_batch_runs_differently_are_unknown() {
    for (t, p, why) in [
        ("SELECT a FROM dbo.t", "SELECT a FROM t", "query"),
        (
            "INSERT INTO #x VALUES (1)",
            "INSERT INTO x VALUES (1)",
            "#temp",
        ),
        (
            "DELETE FROM dbo.t; DELETE FROM dbo.t",
            "DELETE FROM t; DELETE FROM t",
            "2 statements",
        ),
    ] {
        let r = run(
            &tsql(&format!("    EXECUTE (N'{t}');")),
            &pg(&format!("    EXECUTE '{p}';")),
        );
        assert_ne!(r.verdict(), Verdict::Equal, "{t}");
        assert!(
            dynamic_reasons(&r).iter().any(|x| x.contains(why)),
            "{why}: {:?}",
            r.unknown_reasons
        );
    }
}

#[test]
fn unequal_counts_are_unknown() {
    let r = run(
        &tsql("    EXECUTE (N'DELETE FROM dbo.t');\n    EXECUTE (N'DELETE FROM dbo.t');"),
        &pg("    EXECUTE 'DELETE FROM t';"),
    );
    assert_ne!(r.verdict(), Verdict::Equal);
    assert!(dynamic_reasons(&r)
        .iter()
        .any(|x| x.contains("2 and 1 times")));
}

#[test]
fn statements_inside_an_uncovered_if_are_each_not_compared() {
    let r = run(
        &tsql("    IF @x = 1\n    BEGIN\n        UPDATE dbo.t SET a = 1 WHERE id = 2;\n        EXECUTE (N'DELETE FROM dbo.t');\n    END;"),
        &pg("    IF p_x = 1 THEN\n        UPDATE t SET a = 9 WHERE id = 2;\n        EXECUTE 'DELETE FROM t';\n    END IF;"),
    );
    assert_ne!(r.verdict(), Verdict::Equal);
    for line in [8, 9] {
        assert!(
            r.unknown_reasons.iter().any(|x| x.starts_with(&format!(
                "side A (T-SQL) line {line}: a statement inside an IF statement"
            ))),
            "{line}: {:?}",
            r.unknown_reasons
        );
    }
}
