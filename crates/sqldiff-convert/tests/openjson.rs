//! `OPENJSON(@v) WITH (...)` in FROM becomes a jsonb derived table for
//! PostgreSQL, which the engine compares with OPENJSON under
//! ASM-OPENJSON-VALUES; other forms are left as written, not converted.

use sqldiff_convert::{convert, Conversion, Status};
use sqldiff_core::Dialect;

fn proc(from: &str) -> String {
    format!(
        "CREATE PROCEDURE dbo.P @Colors nvarchar(max), @UserID int
AS
BEGIN
    INSERT INTO Warehouse.Colors (ColorName, LastEditedBy)
    SELECT ColorName, @UserID
    {from};
END
"
    )
}

fn run(from: &str) -> Conversion {
    convert(&proc(from), Dialect::Tsql, Dialect::Postgres).unwrap()
}

fn notes(c: &Conversion) -> Vec<(Status, String)> {
    c.units
        .iter()
        .flat_map(|u| u.notes.iter().map(|n| (n.status, n.rule.clone())))
        .collect()
}

#[test]
fn openjson_becomes_jsonb_elements_and_is_proven() {
    let c = run("FROM OPENJSON(@Colors) WITH (ColorName nvarchar(50), Code int N'strict $.Code', Price decimal(18,2) '$.Price', Valid date 'lax $.Valid', IsOn bit, Big bigint, Note nvarchar(max))");
    let out = &c.output;
    let want = "FROM (SELECT CAST(e ->> 'ColorName' AS varchar(50)) AS ColorName, \
CAST(jsonb_path_query_first(e, 'strict $.\"Code\"') #>> '{}' AS integer) AS Code, \
CAST(e ->> 'Price' AS numeric(18, 2)) AS Price, CAST(e ->> 'Valid' AS date) AS Valid, \
CAST(e ->> 'IsOn' AS boolean) AS IsOn, CAST(e ->> 'Big' AS bigint) AS Big, \
CAST(e ->> 'Note' AS text) AS Note \
FROM jsonb_array_elements(CASE jsonb_typeof(CAST(p_colors AS jsonb)) WHEN 'object' \
THEN CASE CAST(p_colors AS jsonb) WHEN '{}' THEN '[]' ELSE jsonb_build_array(CAST(p_colors AS jsonb)) END \
ELSE CAST(p_colors AS jsonb) END) AS e) AS openjson;";
    assert!(out.contains(want), "{out}");
    let n = notes(&c);
    assert!(
        n.contains(&(Status::Assumptions, "ASM-OPENJSON-VALUES".into())),
        "{n:?}"
    );
    let insert = c
        .units
        .iter()
        .find(|u| u.notes.iter().any(|n| n.rule == "ASM-OPENJSON-VALUES"));
    let s = insert.map(|u| u.status);
    assert_eq!(s, Some(Status::Assumptions), "{:#?}", c.units);
}

#[test]
fn an_alias_is_kept() {
    let c = run("FROM OPENJSON(@Colors) WITH (ColorName nvarchar(50)) AS j");
    assert!(c.output.contains("AS e) AS j;"), "{}", c.output);
    assert!(!c.output.contains("AS openjson"), "{}", c.output);
}

#[test]
fn other_forms_are_not_converted() {
    for (from, why) in [
        (
            "FROM OPENJSON(@Colors, '$.a') WITH (ColorName nvarchar(50))",
            "path argument",
        ),
        ("FROM OPENJSON(@Colors)", "without a WITH schema"),
        (
            "FROM OPENJSON(@Colors) WITH (ColorName nvarchar(max) AS JSON)",
            "AS JSON",
        ),
        (
            "FROM OPENJSON(@Colors) WITH (ColorName nvarchar(50) '$.a.b')",
            "path other than",
        ),
        (
            "FROM OPENJSON(@Colors) WITH (ColorName nvarchar(50) '$.\"a b\"')",
            "path other than",
        ),
        (
            "FROM OPENJSON(@Colors) WITH (ColorName varbinary(max))",
            "varbinary",
        ),
        (
            "FROM OPENJSON(@Colors) WITH (ColorName datetime2)",
            "datetime2",
        ),
        (
            "FROM OPENJSON(N'[]') WITH (ColorName nvarchar(50))",
            "other than a variable",
        ),
    ] {
        let c = run(from);
        let note = c
            .output
            .lines()
            .find(|l| l.contains("[not converted] OPENJSON in FROM"))
            .unwrap_or_default();
        assert!(note.contains(why), "{from}: {}", c.output);
        assert!(!c.output.contains("jsonb"), "{from}: {}", c.output);
    }
}
