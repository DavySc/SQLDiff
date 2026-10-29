//! `CREATE INDEX`, `CREATE SEQUENCE` and `CREATE SCHEMA` compared across
//! dialects: each rule has an equal pair and a near miss, and anything the
//! model does not read stays unknown. The facts were checked on SQL Server
//! 2022 and PostgreSQL 18 (docs/cross-dialect-proofs.md, "Indexes,
//! sequences and schemas").

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;
const D: Dialect = Dialect::Db2i;

fn run_to(a: &str, b: &str, to: Dialect) -> Report {
    diff(
        &Artifact::new("a.sql", T, a),
        &Artifact::new("b.sql", to, b),
        &Options::default(),
    )
}

fn run(a: &str, b: &str) -> Report {
    run_to(a, b, P)
}

fn rules(r: &Report) -> Vec<&str> {
    r.findings.iter().map(|f| f.rule.as_str()).collect()
}

fn asms(r: &Report) -> Vec<&str> {
    r.assumptions.iter().map(|a| a.id.as_str()).collect()
}

fn equal_to(a: &str, b: &str, to: Dialect) -> Report {
    let r = run_to(a, b, to);
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

/// Equal under assumptions, nothing undecided.
fn equal(a: &str, b: &str) -> Report {
    equal_to(a, b, P)
}

/// Different by `rule`, and the witness mentions `needle`.
fn differs(a: &str, b: &str, rule: &str, needle: &str) {
    let r = run(a, b);
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.unknown_reasons);
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == rule)
        .unwrap_or_else(|| panic!("{rule}: {:?}", rules(&r)));
    assert!(f.witness.contains(needle), "{}", f.witness);
}

/// Not decided, and a reason mentions `needle`.
fn unknown_to(a: &str, b: &str, to: Dialect, needle: &str) {
    let r = run_to(a, b, to);
    assert!(
        r.unknown_reasons.iter().any(|x| x.contains(needle)),
        "{needle}: {:?} {:?}",
        r.unknown_reasons,
        rules(&r)
    );
    assert_ne!(r.verdict(), Verdict::Equal);
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
}

fn unknown(a: &str, b: &str, needle: &str) {
    unknown_to(a, b, P, needle)
}

const TABLE_T: &str = "CREATE TABLE dbo.Items (
    ItemID INT NOT NULL PRIMARY KEY,
    Code VARCHAR(10) NULL,
    Qty INT NULL,
    Bin INT NOT NULL,
    Name NVARCHAR(50) NOT NULL
);\n";

const TABLE_P: &str = "CREATE TABLE Items (
    ItemID integer NOT NULL PRIMARY KEY,
    Code varchar(10) NULL,
    Qty integer NULL,
    Bin integer NOT NULL,
    Name varchar(50) NOT NULL
);\n";

fn t(ix: &str) -> String {
    format!("{TABLE_T}{ix}\n")
}

fn p(ix: &str) -> String {
    format!("{TABLE_P}{ix}\n")
}

#[test]
fn a_converted_index_is_equal_under_what_it_relies_on() {
    let r = equal(
        &t("CREATE NONCLUSTERED INDEX IX_Items_Bin ON dbo.Items (Bin ASC, Qty DESC) INCLUDE (Name, Code) WITH (FILLFACTOR = 90, ONLINE = ON) ON [PRIMARY];"),
        &p("CREATE INDEX IX_Items_Bin ON Items (Bin ASC, Qty DESC) INCLUDE (Code, Name);"),
    );
    let a = asms(&r);
    assert!(a.contains(&"ASM-INDEX-NAME-SCHEMA"), "{a:?}");
    assert!(
        r.normalizations.iter().any(|n| n.contains("FILLFACTOR")),
        "{:?}",
        r.normalizations
    );
    assert!(
        r.normalizations.iter().any(|n| n.contains("NULL position")),
        "{:?}",
        r.normalizations
    );
    let r = equal(
        &t("CREATE CLUSTERED INDEX CX_Items ON dbo.Items (Bin);"),
        &p("CREATE INDEX CX_Items ON Items (Bin);"),
    );
    assert!(asms(&r).contains(&"ASM-CLUSTERED"));
}

#[test]
fn the_shape_of_a_plain_index_is_not_judged() {
    unknown(
        &t("CREATE INDEX IX_A ON dbo.Items (Bin, Qty);"),
        &p("CREATE INDEX IX_A ON Items (Qty, Bin);"),
        "key columns (bin, qty) against (qty, bin)",
    );
    unknown(
        &t("CREATE INDEX IX_A ON dbo.Items (Bin DESC);"),
        &p("CREATE INDEX IX_A ON Items (Bin);"),
        "sort order",
    );
    unknown(
        &t("CREATE INDEX IX_A ON dbo.Items (Bin) INCLUDE (Qty);"),
        &p("CREATE INDEX IX_A ON Items (Bin);"),
        "included columns",
    );
    unknown(
        &t("CREATE INDEX IX_A ON dbo.Items (Bin) WHERE Qty > 0;"),
        &p("CREATE INDEX IX_A ON Items (Bin);"),
        "only side A (T-SQL) filters the index",
    );
    unknown(
        &t("CREATE INDEX IX_A ON dbo.Items (Bin) WHERE Qty > 0;"),
        &p("CREATE INDEX IX_A ON Items (Bin) WHERE Qty > 1;"),
        "filters",
    );
}

#[test]
fn names_and_tables() {
    differs(
        &t("CREATE INDEX IX_A ON dbo.Items (Bin);"),
        &p("CREATE INDEX IX_B ON Items (Bin);"),
        "equiv.index-name",
        "DROP INDEX",
    );
    // T-SQL index names are per table; PostgreSQL's are per schema.
    let two_t = format!(
        "{TABLE_T}CREATE TABLE dbo.Other (ItemID INT NOT NULL);\nCREATE INDEX IX_1 ON dbo.Items (Bin);\nCREATE INDEX IX_1 ON dbo.Other (ItemID);\n"
    );
    let two_p = format!(
        "{TABLE_P}CREATE TABLE Other (ItemID integer NOT NULL);\nCREATE INDEX IX_1 ON Items (Bin);\nCREATE INDEX IX_1 ON Other (ItemID);\n"
    );
    differs(
        &two_t,
        &two_p,
        "equiv.index-name-clash",
        "relation already exists",
    );
    differs(
        &t("CREATE INDEX Items ON dbo.Items (Bin);"),
        &p("CREATE INDEX Items ON Items (Bin);"),
        "equiv.index-name-clash",
        "Items",
    );
    // An index the other side does not create.
    unknown(
        &t("CREATE INDEX IX_A ON dbo.Items (Bin);"),
        TABLE_P,
        "no counterpart",
    );
}

#[test]
fn unique_indexes_and_nulls() {
    // T-SQL holds one NULL; PostgreSQL's NULLS NOT DISTINCT does too.
    equal(
        &t("CREATE UNIQUE INDEX UX_Qty ON dbo.Items (Qty, Bin);"),
        &p("CREATE UNIQUE INDEX UX_Qty ON Items (Qty, Bin) NULLS NOT DISTINCT;"),
    );
    differs(
        &t("CREATE UNIQUE INDEX UX_Qty ON dbo.Items (Qty, Bin);"),
        &p("CREATE UNIQUE INDEX UX_Qty ON Items (Qty, Bin);"),
        "equiv.unique-nulls",
        "NULL in qty",
    );
    // NOT NULL key columns, or a filter that leaves NULLs out: no NULL keys.
    equal(
        &t("CREATE UNIQUE INDEX UX_Bin ON dbo.Items (Bin);"),
        &p("CREATE UNIQUE INDEX UX_Bin ON Items (Bin);"),
    );
    let r = equal(
        &t("CREATE UNIQUE INDEX UX_Qty ON dbo.Items (Qty) WHERE Qty IS NOT NULL;"),
        &p("CREATE UNIQUE INDEX UX_Qty ON Items (Qty) WHERE Qty IS NOT NULL;"),
    );
    assert!(asms(&r).contains(&"ASM-TSQL-FILTERED-INDEX-SET-OPTIONS"));
    // `Qty > 0` leaves NULLs out too, but only IS NOT NULL is decided.
    unknown(
        &t("CREATE UNIQUE INDEX UX_Qty ON dbo.Items (Qty) WHERE Qty > 0;"),
        &p("CREATE UNIQUE INDEX UX_Qty ON Items (Qty) WHERE Qty > 0;"),
        "other than `qty IS NOT NULL`",
    );
    // A filter that does not name the column keeps its NULLs.
    differs(
        &t("CREATE UNIQUE INDEX UX_Qty ON dbo.Items (Qty) WHERE Bin > 0;"),
        &p("CREATE UNIQUE INDEX UX_Qty ON Items (Qty) WHERE Bin > 0;"),
        "equiv.unique-nulls",
        "NULL in qty",
    );
    differs(
        &t("CREATE UNIQUE INDEX UX_Bin ON dbo.Items (Bin);"),
        &p("CREATE INDEX UX_Bin ON Items (Bin);"),
        "equiv.index-unique",
        "rejects the second",
    );
    differs(
        &t("CREATE UNIQUE INDEX UX_Bin ON dbo.Items (Bin, ItemID);"),
        &p("CREATE UNIQUE INDEX UX_Bin ON Items (Bin);"),
        "equiv.index-key",
        "different itemid",
    );
    // Key order of a unique index shapes it only; the rows it rejects are
    // the same.
    unknown(
        &t("CREATE UNIQUE INDEX UX_Bin ON dbo.Items (Bin, ItemID);"),
        &p("CREATE UNIQUE INDEX UX_Bin ON Items (ItemID, Bin);"),
        "key columns",
    );
    // Filters of unique indexes decide which rows are rejected.
    unknown(
        &t("CREATE UNIQUE INDEX UX_Bin ON dbo.Items (Bin) WHERE Qty > 0;"),
        &p("CREATE UNIQUE INDEX UX_Bin ON Items (Bin) WHERE Qty >= 0;"),
        "the index filters were not shown equal",
    );
}

#[test]
fn unique_string_keys_compare_like_tsql_collations_do() {
    differs(
        &t("CREATE UNIQUE INDEX UX_Name ON dbo.Items (Name);"),
        &p("CREATE UNIQUE INDEX UX_Name ON Items (Name) NULLS NOT DISTINCT;"),
        "equiv.case-sensitivity",
        "'A' and 'a'",
    );
    differs(
        &t("CREATE UNIQUE INDEX UX_Name ON dbo.Items (Name);"),
        &p("CREATE UNIQUE INDEX UX_Name ON Items (Name) NULLS NOT DISTINCT;"),
        "equiv.trailing-spaces",
        "'a '",
    );
    // A key column whose table neither side knows.
    unknown(
        "CREATE UNIQUE INDEX UX_X ON dbo.Elsewhere (X);",
        "CREATE UNIQUE INDEX UX_X ON Elsewhere (X) NULLS NOT DISTINCT;",
        "not in a table both sides know",
    );
}

#[test]
fn what_the_model_does_not_read_stays_unknown() {
    for (a, b, needle) in [
        (
            "CREATE UNIQUE INDEX UX ON dbo.Items (Bin) WITH (IGNORE_DUP_KEY = ON);",
            "CREATE UNIQUE INDEX UX ON Items (Bin);",
            "IGNORE_DUP_KEY",
        ),
        (
            "CREATE NONCLUSTERED COLUMNSTORE INDEX CS ON dbo.Items (Bin);",
            "CREATE INDEX CS ON Items (Bin);",
            "columnstore",
        ),
        (
            "CREATE INDEX IX ON dbo.Items (Bin);",
            "CREATE INDEX CONCURRENTLY IX ON Items (Bin);",
            "CONCURRENTLY",
        ),
        (
            "CREATE INDEX IX ON dbo.Items (Name);",
            "CREATE INDEX IX ON Items ((lower(Name)));",
            "expression key",
        ),
        (
            "CREATE INDEX IX ON dbo.Items (Name);",
            "CREATE INDEX IX ON Items USING hash (Name);",
            "hash",
        ),
    ] {
        unknown(&t(a), &p(b), needle);
    }
}

#[test]
fn db2_for_i_indexes() {
    let table_d =
        "CREATE TABLE Items (ItemID INTEGER NOT NULL PRIMARY KEY, Bin INTEGER NOT NULL);\n";
    let ta = format!("{TABLE_T}CREATE INDEX IX_Bin ON dbo.Items (Bin DESC);\n");
    let tb = format!("{table_d}CREATE INDEX IX_Bin ON Items (Bin DESC);\n");
    let r = run_to(&ta, &tb, D);
    // The table is not compared for Db2 for i; the index is.
    assert!(
        r.unknown_reasons
            .iter()
            .all(|x| !x.contains("CREATE INDEX")),
        "{:?}",
        r.unknown_reasons
    );
    unknown_to(
        &format!("{TABLE_T}CREATE UNIQUE INDEX UX_Bin ON dbo.Items (Bin);\n"),
        &format!("{table_d}CREATE UNIQUE INDEX UX_Bin ON Items (Bin);\n"),
        D,
        "fact V29",
    );
}

#[test]
fn sequences() {
    // T-SQL starts at the type's minimum; the conversion writes it out.
    equal(
        "CREATE SEQUENCE dbo.OrderNo AS int;",
        "CREATE SEQUENCE OrderNo AS integer START WITH -2147483648 INCREMENT BY 1 MINVALUE -2147483648 MAXVALUE 2147483647 NO CYCLE;",
    );
    equal(
        "CREATE SEQUENCE Sequences.OrderNo AS bigint START WITH 1 INCREMENT BY 1 CACHE 50;",
        "CREATE SEQUENCE Sequences.OrderNo AS bigint START WITH 1 INCREMENT BY 1 MINVALUE -9223372036854775808;",
    );
    differs(
        "CREATE SEQUENCE dbo.OrderNo AS int;",
        "CREATE SEQUENCE OrderNo AS integer;",
        "equiv.sequence",
        "first value is -2147483648",
    );
    differs(
        "CREATE SEQUENCE dbo.S AS int START WITH 1 INCREMENT BY 2;",
        "CREATE SEQUENCE S AS integer START WITH 1 INCREMENT BY 1 MINVALUE -2147483648;",
        "equiv.sequence",
        "second value",
    );
    differs(
        "CREATE SEQUENCE dbo.S AS int START WITH 1 MAXVALUE 9;",
        "CREATE SEQUENCE S AS integer START WITH 1 MINVALUE -2147483648 MAXVALUE 10;",
        "equiv.sequence",
        "after the value 9",
    );
    // Neither cycles: the near bound is never used.
    equal(
        "CREATE SEQUENCE dbo.S AS int START WITH 1 INCREMENT BY 1;",
        "CREATE SEQUENCE S AS integer START WITH 1 INCREMENT BY 1;",
    );
    equal(
        "CREATE SEQUENCE dbo.S AS int START WITH 1 INCREMENT BY 10 MAXVALUE 25;",
        "CREATE SEQUENCE S AS integer START WITH 1 INCREMENT BY 10 MAXVALUE 29;",
    );
    equal(
        "CREATE SEQUENCE dbo.S AS int START WITH -1 INCREMENT BY -1;",
        "CREATE SEQUENCE S AS integer START WITH -1 INCREMENT BY -1 MINVALUE -2147483648 MAXVALUE 5;",
    );
    // Both cycle: the restart point and the later rounds count.
    differs(
        "CREATE SEQUENCE dbo.S AS int START WITH 1 MAXVALUE 9 CYCLE;",
        "CREATE SEQUENCE S AS integer START WITH 1 MAXVALUE 9 CYCLE;",
        "equiv.sequence",
        "restarts at -2147483648",
    );
    differs(
        "CREATE SEQUENCE dbo.S AS int START WITH 1 INCREMENT BY 2 MINVALUE 0 MAXVALUE 9 CYCLE;",
        "CREATE SEQUENCE S AS integer START WITH 1 INCREMENT BY 2 MINVALUE 0 MAXVALUE 10 CYCLE;",
        "equiv.sequence",
        "the last value is 8",
    );
    equal(
        "CREATE SEQUENCE dbo.S AS int START WITH 1 MINVALUE 1 MAXVALUE 9 CYCLE;",
        "CREATE SEQUENCE S AS integer START WITH 1 MAXVALUE 9 CYCLE;",
    );
    differs(
        "CREATE SEQUENCE dbo.S AS int START WITH 5 MINVALUE 5 MAXVALUE 7 CYCLE;",
        "CREATE SEQUENCE S AS integer START WITH 5 MINVALUE 5 MAXVALUE 7;",
        "equiv.sequence",
        "restarts at 5",
    );
    equal(
        "CREATE SEQUENCE dbo.S AS tinyint START WITH 1;",
        "CREATE SEQUENCE S AS smallint START WITH 1 MAXVALUE 255;",
    );
    let r = equal(
        "CREATE SEQUENCE dbo.S AS tinyint;",
        "CREATE SEQUENCE S AS smallint START WITH 0 MINVALUE 0 MAXVALUE 255;",
    );
    assert!(asms(&r).contains(&"ASM-SEQUENCE-TYPE"));
    unknown(
        "CREATE SEQUENCE dbo.S AS int START WITH 1;",
        "CREATE SEQUENCE S AS integer START WITH 1 MINVALUE -2147483648 CACHE 20;",
        "caches 20 values per session",
    );
    // Db2 for i: every value written out, NO CACHE.
    equal_to(
        "CREATE SEQUENCE dbo.S AS int START WITH 1;",
        "CREATE SEQUENCE S AS INTEGER START WITH 1 INCREMENT BY 1 MINVALUE -2147483648 MAXVALUE 2147483647 NO CYCLE NO CACHE;",
        D,
    );
    unknown_to(
        "CREATE SEQUENCE dbo.S AS int START WITH 1;",
        "CREATE SEQUENCE S AS INTEGER START WITH 1;",
        D,
        "fact V29",
    );
}

#[test]
fn schemas() {
    let r = equal(
        "CREATE SCHEMA Sales AUTHORIZATION dbo;",
        "CREATE SCHEMA Sales;",
    );
    assert!(asms(&r).contains(&"ASM-SCHEMA-OWNER"));
    unknown(
        "CREATE SCHEMA Sales AUTHORIZATION SalesAdmin;",
        "CREATE SCHEMA Sales;",
        "privileges are not compared",
    );
    differs(
        "CREATE SCHEMA Sales;",
        "CREATE SCHEMA Sale;",
        "equiv.schema-name",
        "Sale",
    );
    unknown(
        "CREATE SCHEMA Sales;",
        "CREATE SCHEMA IF NOT EXISTS Sales;",
        "IF NOT EXISTS",
    );
    equal_to("CREATE SCHEMA Sales;", "CREATE SCHEMA Sales;", D);
    unknown_to(
        "CREATE SCHEMA DataLoadSimulation;",
        "CREATE SCHEMA DataLoadSimulation;",
        D,
        "longer than 10 characters",
    );
}

#[test]
fn a_partition_scheme_shapes_only_storage() {
    let r = equal(
        &t("CREATE INDEX IX_Bin ON dbo.Items (Bin ASC) ON PS_Bin (Bin);"),
        &p("CREATE INDEX IX_Bin ON Items (Bin ASC);"),
    );
    assert!(
        r.normalizations.iter().any(|n| n.contains("PS_Bin")),
        "{:?}",
        r.normalizations
    );
}

#[test]
fn statements_beside_several_routines_are_each_reported() {
    // A file whose routines do not meet their counterparts in order is not
    // compared; the statements outside them must not count as compared
    // either. Routines that do meet are compared one by one, and so are the
    // statements between them.
    let a = "CREATE SCHEMA Sales;\nGO\nCREATE PROCEDURE dbo.A AS SELECT 1;\nGO\nCREATE PROCEDURE dbo.B AS SELECT 2;\nGO\nTRUNCATE TABLE dbo.Old;\n";
    let b = "CREATE SCHEMA Salesx;\nCREATE PROCEDURE A() LANGUAGE sql AS $$ SELECT 1 $$;\nCREATE PROCEDURE B() LANGUAGE sql AS $$ SELECT 2 $$;\nTRUNCATE TABLE Old;\n";
    for (b, why) in [
        (b.to_string(), "TRUNCATE"),
        (
            b.replace("PROCEDURE A()", "PROCEDURE C()"),
            "outside the routines",
        ),
    ] {
        let r = run(a, &b);
        assert!(rules(&r).contains(&"equiv.schema-name"), "{:?}", rules(&r));
        assert!(
            r.unknown_reasons
                .iter()
                .any(|x| x.starts_with("side A (T-SQL) line 7:") && x.contains(why)),
            "{why}: {:?}",
            r.unknown_reasons
        );
    }
}

#[test]
fn a_script_part_with_only_a_schema_or_index_is_compared() {
    // A plain part beside a block, holding only DDL the engine compares
    // statement against statement, meets its partner (it was left out as if
    // empty).
    let r = run(
        "CREATE SCHEMA Sales;\nGO\nDECLARE @x int = 1;\n",
        "CREATE SCHEMA Salesx;\nDO $$ DECLARE x integer := 1; BEGIN END $$;\n",
    );
    assert!(rules(&r).contains(&"equiv.schema-name"), "{:?}", rules(&r));
}
