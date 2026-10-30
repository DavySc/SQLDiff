//! A T-SQL `#t` created inside a procedure against the PostgreSQL TEMP
//! table it became: columns, where it is created and dropped, the index on
//! it, and what keeps them from being compared.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

fn run(a: &str, b: &str) -> Report {
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Postgres, b),
        &Options::default(),
    )
}

fn asms(r: &Report) -> Vec<&str> {
    r.assumptions.iter().map(|a| a.id.as_str()).collect()
}

fn unknown(a: &str, b: &str, needle: &str) {
    let r = run(a, b);
    assert!(
        r.unknown_reasons.iter().any(|x| x.contains(needle)),
        "{needle}: {:?}",
        r.unknown_reasons
    );
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
    assert_ne!(r.verdict(), Verdict::Equal);
}

const T: &str = "CREATE PROCEDURE dbo.p AS
BEGIN
    SET XACT_ABORT ON;
    CREATE TABLE #c (id int NOT NULL, n int NULL);
    INSERT INTO #c (id, n) VALUES (1, 2);
    CREATE INDEX IX_c ON #c (id);
    DELETE FROM #c WHERE n IS NULL;
    DROP TABLE #c;
END;
";

fn p(body: &str) -> String {
    format!(
        "CREATE OR REPLACE PROCEDURE p()
LANGUAGE plpgsql
AS $$
BEGIN
{body}
END;
$$;
"
    )
}

const B: &str = "  CREATE TEMP TABLE c (id integer NOT NULL, n integer NULL);
  INSERT INTO c (id, n) VALUES (1, 2);
  CREATE INDEX IX_c ON c (id);
  DELETE FROM c WHERE n IS NULL;
  DROP TABLE c;";

#[test]
fn a_temp_table_meets_its_temp_table() {
    let r = run(T, &p(B));
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:#?}\n{:#?}",
        r.findings,
        r.unknown_reasons
    );
    assert!(asms(&r).contains(&"ASM-TEMP-TABLE"), "{:?}", asms(&r));
    assert!(
        asms(&r).contains(&"ASM-INDEX-NAME-SCHEMA"),
        "{:?}",
        asms(&r)
    );
}

#[test]
fn what_keeps_a_temp_table_from_being_compared() {
    // A column of another type is found.
    let r = run(T, &p(&B.replace("n integer NULL", "n smallint NULL")));
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
    // Not dropped: T-SQL drops it when the procedure ends, PostgreSQL keeps
    // it, so a second call fails.
    unknown(
        &T.replace("    DROP TABLE #c;\n", ""),
        &p(&B.replace("  DROP TABLE c;", "")),
        "second call fails",
    );
    // Dropped on one side only.
    unknown(T, &p(&B.replace("  DROP TABLE c;", "")), "one side only");
    // A RETURN between CREATE and DROP skips PostgreSQL's DROP.
    unknown(
        &T.replace(
            "    DELETE FROM #c WHERE n IS NULL;\n",
            "    IF 1 = 0 RETURN;\n    DELETE FROM #c WHERE n IS NULL;\n",
        ),
        &p(&B.replace(
            "  DELETE FROM c WHERE n IS NULL;",
            "  IF 1 = 0 THEN RETURN; END IF;\n  DELETE FROM c WHERE n IS NULL;",
        )),
        "not compared",
    );
    // Created inside a branch.
    unknown(
        &T.replace(
            "    CREATE TABLE #c (id int NOT NULL, n int NULL);",
            "    IF 1 = 1 CREATE TABLE #c (id int NOT NULL, n int NULL);",
        ),
        &p(&B.replace(
            "  CREATE TEMP TABLE c (id integer NOT NULL, n integer NULL);",
            "  IF 1 = 1 THEN CREATE TEMP TABLE c (id integer NOT NULL, n integer NULL); END IF;",
        )),
        "not",
    );
    // ON COMMIT DROP empties or drops it at commit.
    unknown(
        T,
        &p(&B.replace("n integer NULL)", "n integer NULL) ON COMMIT DROP")),
        "options after the column list",
    );
    // Without a conversion's trace, a DROP in another place is not compared.
    unknown(
        T,
        &p(&B.replace(
            "  DELETE FROM c WHERE n IS NULL;\n  DROP TABLE c;",
            "  DROP TABLE c;\n  DELETE FROM c WHERE n IS NULL;",
        )),
        "not compared",
    );
    // A permanent table of the same name is not the temporary one.
    unknown(
        T,
        &p(&B.replace("CREATE TEMP TABLE c", "CREATE TABLE c")),
        "not",
    );
}

#[test]
fn t_sql_goes_on_after_a_failing_ddl_statement() {
    // Without XACT_ABORT ON, T-SQL goes on after a failing TRUNCATE (a
    // foreign key refers to the table); PostgreSQL stops.
    let t = "CREATE PROCEDURE dbo.p AS\nBEGIN\n    TRUNCATE TABLE dbo.Orders;\n    INSERT INTO dbo.Log (id) VALUES (1);\nEND;\n";
    let b = p("  TRUNCATE TABLE Orders RESTART IDENTITY;\n  INSERT INTO Log (id) VALUES (1);");
    let r = run(t, &b);
    assert!(
        r.findings.iter().any(|f| f.rule == "equiv.error-continues"),
        "{:?} {:?}",
        r.findings.iter().map(|f| &f.rule).collect::<Vec<_>>(),
        r.unknown_reasons
    );
}
