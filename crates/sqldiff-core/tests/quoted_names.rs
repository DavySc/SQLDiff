//! Quoted names that keep capitals in PostgreSQL (`"Stock Item Key"`, the
//! converter's spelling of `[Stock Item Key]`) meet the T-SQL name up to
//! letter case, like unquoted names do (ASM-IDENT-FOLD), but only where
//! PostgreSQL resolves them to the object the T-SQL name means: no other
//! name of the statement or the catalog differs from them only in letter
//! case, and they are spelled as the catalog spells them.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;

const CAT_T: &str = "CREATE SCHEMA Integration;
GO
CREATE TABLE Integration.Lineage (
    [Lineage Key] int NOT NULL CONSTRAINT PK_Lineage PRIMARY KEY,
    [Rows Loaded] int NULL
);
";

const CAT_P: &str = "CREATE SCHEMA integration;
CREATE TABLE Integration.Lineage (
    \"Lineage Key\" integer NOT NULL CONSTRAINT PK_Lineage PRIMARY KEY,
    \"Rows Loaded\" integer NULL
);
";

fn run_with(a: &str, b: &str, cat_p: Option<&str>) -> Report {
    let o = match cat_p {
        Some(c) => Options {
            catalog_a: vec![Artifact::new("cat.sql", T, CAT_T)],
            catalog_b: vec![Artifact::new("cat.pg.sql", P, c)],
            ..Options::default()
        },
        None => Options::default(),
    };
    diff(
        &Artifact::new("a.sql", T, a),
        &Artifact::new("b.sql", P, b),
        &o,
    )
}

fn run(a: &str, b: &str) -> Report {
    run_with(a, b, Some(CAT_P))
}

fn rules(r: &Report) -> Vec<&str> {
    r.findings.iter().map(|f| f.rule.as_str()).collect()
}

fn equal(r: &Report) {
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:?} {:?}",
        rules(r),
        r.unknown_reasons
    );
    assert!(r.assumptions.iter().any(|a| a.id == "ASM-IDENT-FOLD"));
}

fn not_equal(r: &Report, needle: &str) {
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.unknown_reasons
    );
    // Statements not checked are reported as `equiv.unsupported` findings.
    assert!(
        r.unknown_reasons
            .iter()
            .chain(r.findings.iter().map(|f| &f.witness))
            .any(|x| x.contains(needle)),
        "{needle}: {:?} {:?}",
        r.unknown_reasons,
        rules(r)
    );
}

/// The PostgreSQL catalog with a second column that differs from
/// "Rows Loaded" only in letter case.
fn clashing_catalog() -> String {
    CAT_P.replace(
        "\"Rows Loaded\" integer NULL",
        "\"Rows Loaded\" integer NULL,\n    \"rows loaded\" integer NULL",
    )
}

fn tsql_proc(body: &str) -> String {
    format!("CREATE PROCEDURE dbo.p @k int, @n int\nAS\nBEGIN\n    {body}\nEND;\n")
}

fn pg_proc(body: &str) -> String {
    format!("CREATE PROCEDURE p(p_k integer, p_n integer)\nLANGUAGE plpgsql AS $$\nBEGIN\n    {body}\nEND $$;\n")
}

const UPDATE_T: &str =
    "UPDATE Integration.Lineage SET [Rows Loaded] = @n WHERE [Lineage Key] = @k;";
const UPDATE_P: &str =
    "UPDATE Integration.Lineage SET \"Rows Loaded\" = p_n WHERE \"Lineage Key\" = p_k;";

#[test]
fn a_statement_on_quoted_columns_is_equal() {
    equal(&run(&tsql_proc(UPDATE_T), &pg_proc(UPDATE_P)));
}

#[test]
fn a_statement_whose_names_differ_only_in_case_is_not_equal() {
    // The catalog has "Rows Loaded" and "rows loaded": [Rows Loaded] could be either.
    let r = run_with(
        &tsql_proc(UPDATE_T),
        &pg_proc(UPDATE_P),
        Some(&clashing_catalog()),
    );
    not_equal(&r, "differ only in letter case");
    // Spelled in another case than the catalog's: PostgreSQL finds no column.
    let r = run(
        &tsql_proc(UPDATE_T),
        &pg_proc(&UPDATE_P.replace("\"Rows Loaded\"", "\"rows Loaded\"")),
    );
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?} {:?}",
        rules(&r),
        r.unknown_reasons
    );
    // Two names of the statement that differ only in case: `l` is not the
    // alias "L" in PostgreSQL.
    let r = run(
        &tsql_proc(ALIAS_T),
        &pg_proc(&ALIAS_P.replace("WHERE \"L\"", "WHERE l")),
    );
    not_equal(&r, "differ only in letter case");
}

const ALIAS_T: &str =
    "UPDATE Integration.Lineage SET [Rows Loaded] = @n WHERE Lineage.[Lineage Key] = @k;";
const ALIAS_P: &str = "UPDATE Integration.Lineage AS \"L\" SET \"Rows Loaded\" = p_n WHERE \"L\".\"Lineage Key\" = p_k;";

#[test]
fn a_quoted_alias_is_equal() {
    equal(&run(&tsql_proc(ALIAS_T), &pg_proc(ALIAS_P)));
}

const COMMENT_T: &str = "EXEC sys.sp_addextendedproperty @name = N'Description', @value = N'Rows loaded', @level0type = N'SCHEMA', @level0name = N'Integration', @level1type = N'TABLE', @level1name = N'Lineage', @level2type = N'COLUMN', @level2name = N'Rows Loaded';";
const COMMENT_P: &str = "COMMENT ON COLUMN Integration.Lineage.\"Rows Loaded\" IS 'Rows loaded';";

#[test]
fn a_comment_on_a_quoted_column_is_equal() {
    equal(&run(COMMENT_T, COMMENT_P));
}

#[test]
fn a_comment_on_a_column_the_catalog_also_has_in_another_case_is_unknown() {
    not_equal(
        &run_with(COMMENT_T, COMMENT_P, Some(&clashing_catalog())),
        "differ only in letter case",
    );
    // PostgreSQL resolves "rows Loaded" to no column.
    not_equal(
        &run(COMMENT_T, &COMMENT_P.replace("Rows Loaded", "rows Loaded")),
        "catalog has no column",
    );
}

const TABLE_T: &str = "CREATE TABLE dbo.[Order Lines] (
    [Line Key] int NOT NULL,
    [Order Note] nvarchar(20) NULL,
    CONSTRAINT PK_Order_Lines PRIMARY KEY ([Line Key])
);";

const TABLE_P: &str = "CREATE TABLE \"Order Lines\" (
    \"Line Key\" integer NOT NULL,
    \"Order Note\" varchar(20) NULL,
    CONSTRAINT PK_Order_Lines PRIMARY KEY (\"Line Key\")
);";

#[test]
fn a_table_with_quoted_names_is_equal() {
    equal(&run_with(TABLE_T, TABLE_P, None));
}

#[test]
fn a_table_whose_names_resolve_differently_is_not_equal() {
    for (p, needle) in [
        // PostgreSQL rejects this key: the table has no column "line key".
        (
            TABLE_P.replace("PRIMARY KEY (\"Line Key\")", "PRIMARY KEY (\"line key\")"),
            "no catalog object",
        ),
        // Two columns a T-SQL table cannot both have.
        (
            TABLE_P.replace(
                "\"Order Note\" varchar(20) NULL,",
                "\"Order Note\" varchar(20) NULL,\n    \"order note\" varchar(20) NULL,",
            ),
            "differ only in letter case",
        ),
        // A referenced table no catalog spells.
        (
            TABLE_P.replace(
                "CONSTRAINT PK_Order_Lines",
                "CONSTRAINT FK_Order FOREIGN KEY (\"Line Key\") REFERENCES \"Orders\" (\"Order Key\"),\n    CONSTRAINT PK_Order_Lines",
            ),
            "not in the catalog",
        ),
    ] {
        not_equal(&run_with(TABLE_T, &p, None), needle);
    }
}

const INDEX_T: &str = "CREATE INDEX IX_Lineage_Rows ON Integration.Lineage ([Rows Loaded]);";
const INDEX_P: &str = "CREATE INDEX IX_Lineage_Rows ON Integration.Lineage (\"Rows Loaded\");";

#[test]
fn an_index_on_a_quoted_column_is_equal() {
    equal(&run(INDEX_T, INDEX_P));
}

#[test]
fn an_index_whose_names_resolve_differently_is_unknown() {
    not_equal(
        &run(
            INDEX_T,
            &INDEX_P.replace("\"Rows Loaded\"", "\"rows loaded\""),
        ),
        "no catalog object",
    );
    not_equal(
        &run_with(INDEX_T, INDEX_P, Some(&clashing_catalog())),
        "differ only in letter case",
    );
    // No catalog: whether the table has the column is not known.
    not_equal(&run_with(INDEX_T, INDEX_P, None), "not in the catalog");
}
