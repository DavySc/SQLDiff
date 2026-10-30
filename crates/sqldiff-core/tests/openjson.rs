//! T-SQL `OPENJSON(@v) WITH (...)` against the PostgreSQL jsonb derived
//! table the converter writes for it (`canon::pg_json_rows`). Both engines'
//! behaviour was checked on SQL Server 2022 and PostgreSQL 18; see the
//! OPENJSON section of docs/vendor-facts-verification.md.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const MS_DDL: &str = "CREATE TABLE dbo.Colors (ColorID int NOT NULL PRIMARY KEY,
  ColorName nvarchar(20) NOT NULL, IsActive bit NULL, Price decimal(18,2) NULL, ValidFrom date NULL);";
const PG_DDL: &str = "CREATE TABLE Colors (ColorID integer NOT NULL PRIMARY KEY,
  ColorName varchar(20) NOT NULL, IsActive boolean NULL, Price numeric(18, 2) NULL, ValidFrom date NULL);";

const TSQL: &str = "CREATE PROCEDURE dbo.P @Colors nvarchar(max)
AS
BEGIN
    INSERT INTO dbo.Colors (ColorID, ColorName, IsActive, Price, ValidFrom)
    SELECT ColorID, ColorName, IsActive, Price, ValidFrom
    FROM OPENJSON(@Colors)
    WITH (ColorID int N'strict $.ColorID', ColorName nvarchar(20) N'$.ColorName',
          IsActive bit, Price decimal(18,2) 'lax $.Price', ValidFrom date);
END;";

const ROWS: &str = "jsonb_array_elements(CASE jsonb_typeof(CAST(p_colors AS jsonb)) WHEN 'object' THEN CASE CAST(p_colors AS jsonb) WHEN '{}' THEN '[]' ELSE jsonb_build_array(CAST(p_colors AS jsonb)) END ELSE CAST(p_colors AS jsonb) END) AS e";

const COLS: &str = "CAST(jsonb_path_query_first(e, 'strict $.\"ColorID\"') #>> '{}' AS integer) AS ColorID, CAST(e ->> 'ColorName' AS varchar(20)) AS ColorName, CAST(e ->> 'IsActive' AS boolean) AS IsActive, CAST(e ->> 'Price' AS numeric(18, 2)) AS Price, CAST(e ->> 'ValidFrom' AS date) AS ValidFrom";

fn pg(cols: &str, rows: &str, tail: &str) -> String {
    format!(
        "CREATE OR REPLACE PROCEDURE P(p_colors text)
LANGUAGE plpgsql
AS $$
BEGIN
  INSERT INTO Colors (ColorID, ColorName, IsActive, Price, ValidFrom)
  SELECT ColorID, ColorName, IsActive, Price, ValidFrom
  FROM (SELECT {cols} FROM {rows}) AS openjson{tail};
END;
$$;"
    )
}

fn run(a: &str, b: &str) -> Report {
    let opts = Options {
        catalog_a: vec![Artifact::new("catalog.sql", Dialect::Tsql, MS_DDL)],
        catalog_b: vec![Artifact::new("catalog.sql", Dialect::Postgres, PG_DDL)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Postgres, b),
        &opts,
    )
}

fn equal(r: &Report) -> bool {
    matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions)
}

#[test]
fn the_jsonb_form_is_equal_under_the_openjson_assumption() {
    let r = run(TSQL, &pg(COLS, ROWS, ""));
    assert_eq!(r.verdict(), Verdict::EqualUnderAssumptions, "{r:#?}");
    let ids: Vec<&str> = r.assumptions.iter().map(|a| a.id.as_str()).collect();
    assert!(ids.contains(&"ASM-OPENJSON-VALUES"), "{ids:?}");
}

#[test]
fn every_other_reading_is_not_equal() {
    let mutations: &[(&str, &str)] = &[
        // Strict and lax differ on a missing key.
        ("'strict $.\"ColorID\"'", "'lax $.\"ColorID\"'"),
        (
            "jsonb_path_query_first(e, 'strict $.\"ColorID\"') #>> '{}'",
            "e ->> 'ColorID'",
        ),
        (
            "e ->> 'Price' AS",
            "jsonb_path_query_first(e, 'strict $.\"Price\"') #>> '{}' AS",
        ),
        // Keys are case-sensitive.
        ("e ->> 'ColorName'", "e ->> 'colorname'"),
        (
            "CAST(e ->> 'ValidFrom' AS date)",
            "CAST(e ->> 'Price' AS date)",
        ),
        // Truncation, rounding and range belong to the type.
        (
            "AS varchar(20)) AS ColorName",
            "AS varchar(21)) AS ColorName",
        ),
        ("AS varchar(20)) AS ColorName", "AS text) AS ColorName"),
        ("numeric(18, 2)) AS Price", "numeric(18, 3)) AS Price"),
        ("AS integer) AS ColorID", "AS bigint) AS ColorID"),
        // Columns by name.
        ("AS boolean) AS IsActive", "AS boolean) AS IsActiveX"),
        ("#>> '{}'", "#>> '{a}'"),
        ("jsonb_path_query_first(e,", "jsonb_path_query_first(e2,"),
        // The row shape: `{}` has no rows, an object one, an array one per element.
        ("WHEN '{}' THEN '[]'", "WHEN '[]' THEN '[]'"),
        (
            "ELSE jsonb_build_array(CAST(p_colors AS jsonb)) END",
            "ELSE CAST(p_colors AS jsonb) END",
        ),
        ("WHEN 'object'", "WHEN 'array'"),
        ("jsonb_array_elements(", "jsonb_array_elements_text("),
    ];
    let base = pg(COLS, ROWS, "");
    let mut bad = Vec::new();
    for (from, to) in mutations {
        assert!(base.contains(from), "{from}");
        let r = run(TSQL, &base.replacen(from, to, 1));
        if equal(&r) {
            bad.push(format!("{from} -> {to}"));
        }
    }
    for tail in [" WHERE ColorID > 0", " WHERE IsActive"] {
        if equal(&run(TSQL, &pg(COLS, ROWS, tail))) {
            bad.push(tail.to_string());
        }
    }
    assert!(bad.is_empty(), "equal although changed: {bad:#?}");
}

#[test]
fn other_openjson_forms_are_not_compared() {
    let forms = [
        // A path argument, AS JSON, a nested or quoted path, an untested type.
        ("OPENJSON(@Colors)", "OPENJSON(@Colors, '$.a')"),
        ("ValidFrom date);", "ValidFrom nvarchar(max) AS JSON);"),
        ("N'$.ColorName'", "N'$.a.ColorName'"),
        ("N'$.ColorName'", "N'$.\"ColorName\"'"),
        ("ValidFrom date);", "ValidFrom datetime2);"),
    ];
    for (from, to) in forms {
        let a = TSQL.replacen(from, to, 1);
        assert_ne!(a, TSQL, "{to}");
        let r = run(&a, &pg(COLS, ROWS, ""));
        assert!(!equal(&r), "{to}: {:?}", r.verdict());
    }
}

#[test]
fn update_from_one_object() {
    let a = "CREATE PROCEDURE dbo.U @Colors nvarchar(max), @ID int
AS
BEGIN
    UPDATE dbo.Colors SET ColorName = j.ColorName, Price = j.Price
    FROM OPENJSON(@Colors) WITH (ColorName nvarchar(20) N'strict $.ColorName', Price decimal(18,2)) AS j
    WHERE dbo.Colors.ColorID = @ID;
END;";
    let b = |set: &str| {
        format!(
            "CREATE OR REPLACE PROCEDURE U(p_colors text, p_id integer)
LANGUAGE plpgsql
AS $$
BEGIN
  UPDATE Colors SET {set} FROM (SELECT CAST(jsonb_path_query_first(e, 'strict $.\"ColorName\"') #>> '{{}}' AS varchar(20)) AS ColorName, CAST(e ->> 'Price' AS numeric(18, 2)) AS Price FROM {ROWS}) AS j WHERE Colors.ColorID = p_id;
END;
$$;"
        )
    };
    let r = run(a, &b("ColorName = j.ColorName, Price = j.Price"));
    assert_eq!(r.verdict(), Verdict::EqualUnderAssumptions, "{r:#?}");
    let ids: Vec<&str> = r.assumptions.iter().map(|a| a.id.as_str()).collect();
    assert!(ids.contains(&"ASM-OPENJSON-ONE-ROW"), "{ids:?}");
    let r = run(a, &b("ColorName = j.ColorName, Price = j.Price + 1"));
    assert!(!equal(&r));
}

fn run_tsql(a: &str, b: &str) -> Report {
    let opts = Options {
        catalog_a: vec![Artifact::new("catalog.sql", Dialect::Tsql, MS_DDL)],
        catalog_b: vec![Artifact::new("catalog.sql", Dialect::Tsql, MS_DDL)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Tsql, b),
        &opts,
    )
}

fn view(with: &str) -> String {
    format!("CREATE PROCEDURE dbo.V @J nvarchar(max) AS BEGIN INSERT INTO dbo.Colors (ColorID, Price) SELECT A, B FROM OPENJSON(@J) WITH ({with}); END;")
}

/// Review of #170: which name reads which key is part of the term.
#[test]
fn names_stay_tied_to_their_keys() {
    let a = view("A int '$.x', B int '$.y'");
    // T-SQL against T-SQL: the same keys in the same order, names swapped.
    assert!(!equal(&run_tsql(&a, &view("B int '$.x', A int '$.y'"))));
    assert!(equal(&run_tsql(&a, &view("A int '$.x', B int '$.y'"))));
    // Default paths read the column name as written: `$.Id` is not `$.ID`.
    assert!(!equal(&run_tsql(
        &view("A int, B int"),
        &view("a int, B int")
    )));
    // Across dialects.
    let pg = |cols: &str| {
        format!(
            "CREATE OR REPLACE PROCEDURE V(p_j text)
LANGUAGE plpgsql
AS $$
BEGIN
  INSERT INTO Colors (ColorID, Price) SELECT A, B FROM (SELECT {cols} FROM {}) AS openjson;
END;
$$;",
            ROWS.replace("p_colors", "p_j")
        )
    };
    let ok = pg("CAST(e ->> 'x' AS integer) AS A, CAST(e ->> 'y' AS integer) AS B");
    assert!(equal(&run(&a, &ok)), "{:?}", run(&a, &ok).verdict());
    let swapped = pg("CAST(e ->> 'x' AS integer) AS B, CAST(e ->> 'y' AS integer) AS A");
    assert!(!equal(&run(&a, &swapped)));
}

#[test]
fn update_from_names_stay_tied_to_their_keys() {
    let a = "CREATE PROCEDURE dbo.U @J nvarchar(max)
AS
BEGIN
    UPDATE t SET Price = j.Price FROM dbo.Colors AS t JOIN OPENJSON(@J) WITH (ColorID int, Price decimal(18,2)) AS j ON t.ColorID = j.ColorID;
END;";
    let b = |c1: &str, c2: &str| {
        format!(
            "CREATE OR REPLACE PROCEDURE U(p_j text)
LANGUAGE plpgsql
AS $$
BEGIN
  UPDATE Colors AS t SET Price = j.Price FROM (SELECT CAST(e ->> 'ColorID' AS integer) AS {c1}, CAST(e ->> 'Price' AS numeric(18, 2)) AS {c2} FROM {}) AS j WHERE t.ColorID = j.ColorID;
END;
$$;",
            ROWS.replace("p_colors", "p_j")
        )
    };
    let r = run(a, &b("ColorID", "Price"));
    assert!(equal(&r), "{:?}", r.verdict());
    assert!(!equal(&run(a, &b("Price", "ColorID"))));
}
