//! Computed columns (T-SQL `c AS (expr)`) against PostgreSQL generated
//! columns (`c type GENERATED ALWAYS AS (expr) STORED`): the column types
//! SQL Server derives, and the expressions compared row by row.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;

fn run(a: &str, b: &str) -> Report {
    diff(
        &Artifact::new("a.sql", T, a),
        &Artifact::new("b.sql", P, b),
        &Options::default(),
    )
}

fn equal(a: &str, b: &str) -> Report {
    let r = run(a, b);
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:#?}\n{:#?}",
        r.findings,
        r.unknown_reasons
    );
    assert!(r.unknown_reasons.is_empty(), "{:?}", r.unknown_reasons);
    r
}

fn not_equal(a: &str, b: &str, needle: &str) {
    let r = run(a, b);
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:#?}",
        r.findings
    );
    let all: Vec<String> = r
        .findings
        .iter()
        .map(|f| format!("{} {}", f.rule, f.witness))
        .chain(r.unknown_reasons.iter().cloned())
        .collect();
    assert!(all.iter().any(|x| x.contains(needle)), "{needle}: {all:#?}");
}

fn tsql(col: &str) -> String {
    format!(
        "CREATE TABLE dbo.People (
    PersonID int NOT NULL PRIMARY KEY,
    FullName nvarchar(50) NOT NULL,
    PreferredName nvarchar(50) NULL,
    Notes nvarchar(max) NULL,
    {col}
);"
    )
}

fn pg(col: &str) -> String {
    format!(
        "CREATE TABLE People (
    PersonID integer NOT NULL PRIMARY KEY,
    FullName varchar(50) NOT NULL,
    PreferredName varchar(50) NULL,
    Notes text NULL,
    {col}
);"
    )
}

#[test]
fn concat_reads_null_as_empty() {
    equal(
        &tsql("SearchName AS (concat(PreferredName, N' ', FullName)) PERSISTED NOT NULL"),
        &pg("SearchName varchar(101) GENERATED ALWAYS AS (COALESCE(PreferredName, '') || ' ' || COALESCE(FullName, '')) STORED NOT NULL"),
    );
    // With an nvarchar(max) part the result is unbounded.
    equal(
        &tsql("SearchDetails AS (concat(FullName, N' ', Notes))"),
        &pg("SearchDetails text GENERATED ALWAYS AS (COALESCE(FullName, '') || ' ' || COALESCE(Notes, '')) STORED"),
    );
}

#[test]
fn concat_near_misses() {
    // `||` without COALESCE is NULL when a part is.
    not_equal(
        &tsql("SearchName AS (concat(PreferredName, N' ', FullName)) PERSISTED NOT NULL"),
        &pg("SearchName varchar(101) GENERATED ALWAYS AS (PreferredName || ' ' || FullName) STORED NOT NULL"),
        "SearchName",
    );
    // Parts in another order.
    not_equal(
        &tsql("SearchName AS (concat(PreferredName, N' ', FullName)) PERSISTED NOT NULL"),
        &pg("SearchName varchar(101) GENERATED ALWAYS AS (COALESCE(FullName, '') || ' ' || COALESCE(PreferredName, '')) STORED NOT NULL"),
        "SearchName",
    );
    // A shorter column type.
    not_equal(
        &tsql("SearchName AS (concat(PreferredName, N' ', FullName)) PERSISTED NOT NULL"),
        &pg("SearchName varchar(100) GENERATED ALWAYS AS (COALESCE(PreferredName, '') || ' ' || COALESCE(FullName, '')) STORED NOT NULL"),
        "equiv.column-type",
    );
    // A plain column against a computed one.
    not_equal(
        &tsql("SearchName AS (concat(PreferredName, N' ', FullName)) PERSISTED NOT NULL"),
        &pg("SearchName varchar(101) NOT NULL"),
        "equiv.column-computed",
    );
    // NOT NULL on one side only.
    not_equal(
        &tsql("SearchName AS (concat(PreferredName, N' ', FullName)) PERSISTED NOT NULL"),
        &pg("SearchName varchar(101) GENERATED ALWAYS AS (COALESCE(PreferredName, '') || ' ' || COALESCE(FullName, '')) STORED"),
        "equiv.column-null",
    );
}

const JQ: &str = "CASE WHEN json_typeof(CAST(Notes AS json) -> 'Tags') IN ('object', 'array') THEN CAST(CAST(Notes AS json) -> 'Tags' AS text) END";
const JV: &str = "CASE WHEN json_typeof(CAST(Notes AS json) -> 'Tags') NOT IN ('object', 'array') THEN CAST(Notes AS json) ->> 'Tags' END";

#[test]
fn json_query_and_value_read_the_same_key() {
    let r = equal(
        &tsql("Tags AS (json_query([Notes],N'$.Tags'))"),
        &pg(&format!("Tags text GENERATED ALWAYS AS ({JQ}) STORED")),
    );
    assert!(r.assumptions.iter().any(|x| x.id == "ASM-JSON-PATH-VALUES"));
    equal(
        &tsql("Tags AS (json_value([Notes],N'$.Tags'))"),
        &pg(&format!(
            "Tags varchar(4000) GENERATED ALWAYS AS ({JV}) STORED"
        )),
    );
    // `lax` is the default mode.
    equal(
        &tsql("Tags AS (json_value(Notes, 'lax $.Tags'))"),
        &pg(&format!(
            "Tags varchar(4000) GENERATED ALWAYS AS ({JV}) STORED"
        )),
    );
}

#[test]
fn json_near_misses() {
    let q = tsql("Tags AS (json_query([Notes],N'$.Tags'))");
    let v = tsql("Tags AS (json_value([Notes],N'$.Tags'))");
    let gen = |ty: &str, e: &str| pg(&format!("Tags {ty} GENERATED ALWAYS AS ({e}) STORED"));
    // The value form for JSON_QUERY and the other way round.
    not_equal(&q, &gen("text", JV), "Tags");
    not_equal(&v, &gen("varchar(4000)", JQ), "Tags");
    // Another key, a key in another case, another source column.
    not_equal(&q, &gen("text", &JQ.replace("'Tags'", "'Tag'")), "Tags");
    not_equal(&q, &gen("text", &JQ.replace("'Tags'", "'tags'")), "Tags");
    not_equal(&q, &gen("text", &JQ.replace("Notes", "FullName")), "Tags");
    // jsonb rewrites the text of fragments and numbers.
    not_equal(
        &q,
        &gen(
            "text",
            &JQ.replace("AS json)", "AS jsonb)")
                .replace("json_typeof", "jsonb_typeof"),
        ),
        "Tags",
    );
    not_equal(
        &v,
        &gen(
            "varchar(4000)",
            &JV.replace("AS json)", "AS jsonb)")
                .replace("json_typeof", "jsonb_typeof"),
        ),
        "Tags",
    );
    // Without the type test: JSON_VALUE of an object is NULL, ->> its text.
    not_equal(
        &v,
        &gen("varchar(4000)", "CAST(Notes AS json) ->> 'Tags'"),
        "Tags",
    );
    not_equal(
        &q,
        &gen("text", "CAST(CAST(Notes AS json) -> 'Tags' AS text)"),
        "Tags",
    );
    // Only an object, or only an array, counts as a fragment.
    not_equal(
        &q,
        &gen("text", &JQ.replace("('object', 'array')", "('object')")),
        "Tags",
    );
    not_equal(
        &q,
        &gen(
            "text",
            &JQ.replace("('object', 'array')", "('array', 'object')"),
        ),
        "Tags",
    );
    // A shorter column type: JSON_VALUE is nvarchar(4000).
    not_equal(&v, &gen("varchar(3999)", JV), "equiv.column-type");
    // Strict mode raises an error where lax mode gives NULL.
    not_equal(
        &tsql("Tags AS (json_query([Notes],N'strict $.Tags'))"),
        &gen("text", JQ),
        "Tags",
    );
    // A nested path.
    not_equal(
        &tsql("Tags AS (json_query([Notes],N'$.a.Tags'))"),
        &gen("text", JQ),
        "Tags",
    );
}

#[test]
fn json_key_case_matters_between_tsql_versions() {
    // JSON keys are case-sensitive, whatever the collation does to names.
    let r = diff(
        &Artifact::new("a.sql", T, tsql("Tags AS (json_query([Notes],N'$.Tags'))")),
        &Artifact::new("b.sql", T, tsql("Tags AS (json_query([Notes],N'$.tags'))")),
        &Options::default(),
    );
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:#?}",
        r
    );
}

fn iso126(x: &str) -> String {
    sqldiff_core::equiv::canon::pg_template(sqldiff_core::equiv::canon::ISO126_PG, x)
}

#[test]
fn try_convert_style_126_reads_iso_text() {
    // In a conversion check: datetime2(7) against timestamp under
    // ASM-FRACTIONAL-SECONDS.
    let _ci = sqldiff_core::equiv::compare::ci_collation_scope(true);
    let jv = "CAST(Notes AS json) ->> 'DeliveredWhen'";
    let r = equal(
        &tsql("Delivered AS (TRY_CONVERT([datetime2](7),json_value([Notes],N'$.DeliveredWhen'),(126)))"),
        &pg(&format!("Delivered timestamp GENERATED ALWAYS AS ({}) STORED", iso126(jv))),
    );
    for id in [
        "ASM-ISO126-VALUES",
        "ASM-JSON-PATH-VALUES",
        "ASM-FRACTIONAL-SECONDS",
    ] {
        assert!(r.assumptions.iter().any(|x| x.id == id), "{id}");
    }
    // Of a string column.
    equal(
        &tsql("Delivered AS (TRY_CONVERT(datetime2, FullName, 126))"),
        &pg(&format!(
            "Delivered timestamp GENERATED ALWAYS AS ({}) STORED",
            iso126("FullName")
        )),
    );
}

#[test]
fn try_convert_near_misses() {
    let _ci = sqldiff_core::equiv::compare::ci_collation_scope(true);
    let t = tsql(
        "Delivered AS (TRY_CONVERT([datetime2](7),json_value([Notes],N'$.DeliveredWhen'),(126)))",
    );
    let jv = "CAST(Notes AS json) ->> 'DeliveredWhen'";
    let gen = |e: String| {
        pg(&format!(
            "Delivered timestamp GENERATED ALWAYS AS ({e}) STORED"
        ))
    };
    for (from, to) in [
        // A trailing '.' (what WWI writes) must stay NULL, as in SQL Server.
        ("{1,7}", "{0,7}"),
        // Year 0000 is out of range.
        ("^(?!0000)", "^"),
        // Without the date check 2013-02-30 would roll over to March.
        ("date_part('day'", "date_part('month'"),
        // The fraction in microseconds.
        ("rpad(substr", "lpad(substr"),
        ("'1 microsecond'", "'1 millisecond'"),
        (
            "(substr(__x__, 18, 2) AS double precision)",
            "(substr(__x__, 18, 4) AS double precision)",
        ),
    ] {
        let tpl = sqldiff_core::equiv::canon::ISO126_PG.replace(from, to);
        assert_ne!(tpl, sqldiff_core::equiv::canon::ISO126_PG, "{from}");
        not_equal(
            &t,
            &gen(sqldiff_core::equiv::canon::pg_template(&tpl, jv)),
            "Delivered",
        );
    }
    // Another key, and the plain cast PostgreSQL refuses in a generated column.
    not_equal(
        &t,
        &gen(iso126("CAST(Notes AS json) ->> 'deliveredWhen'")),
        "Delivered",
    );
    not_equal(&t, &gen(format!("CAST({jv} AS timestamp(6))")), "Delivered");
    // Other styles and types.
    for other in [
        "TRY_CONVERT([datetime2](7),json_value([Notes],N'$.DeliveredWhen'),(127))",
        "TRY_CONVERT([datetime2](3),json_value([Notes],N'$.DeliveredWhen'),(126))",
        "TRY_CONVERT([datetime],json_value([Notes],N'$.DeliveredWhen'),(126))",
    ] {
        not_equal(
            &tsql(&format!("Delivered AS ({other})")),
            &gen(iso126(jv)),
            "Delivered",
        );
    }
}

#[test]
fn isjson_check_rejects_the_same_rows() {
    let isjson =
        sqldiff_core::equiv::canon::pg_template(sqldiff_core::equiv::canon::ISJSON_PG, "Notes");
    let r = equal(
        &tsql("CONSTRAINT CK_json CHECK ([Notes] IS NULL OR isjson([Notes])<>(0))"),
        &pg(&format!(
            "CONSTRAINT CK_json CHECK (Notes IS NULL OR {isjson}<>(0))"
        )),
    );
    assert!(r.assumptions.iter().any(|x| x.id == "ASM-JSON-PATH-VALUES"));
    // Only an object, or no NULL arm.
    not_equal(
        &tsql("CONSTRAINT CK_json CHECK (isjson([Notes])<>(0))"),
        &pg(&format!(
            "CONSTRAINT CK_json CHECK ({}<>(0))",
            isjson.replace("('object', 'array')", "('object')")
        )),
        "CHECK",
    );
    not_equal(
        &tsql("CONSTRAINT CK_json CHECK (isjson([Notes])=(1))"),
        &pg(&format!(
            "CONSTRAINT CK_json CHECK ({}=(1))",
            isjson.replace("WHEN Notes IS NULL THEN NULL ", "")
        )),
        "CHECK",
    );
}
