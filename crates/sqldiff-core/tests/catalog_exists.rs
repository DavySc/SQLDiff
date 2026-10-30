//! Checks whether a dbo table or procedure exists: the T-SQL forms on
//! sys.objects / sys.schemas against the pg_catalog form the converter
//! writes (`equiv::objcheck`). Equal only for the same kind and name, and
//! only under ASM-CATALOG-EXISTS. Behaviour of both forms was run on SQL
//! Server 2022 (SQL_Latin1_General_CP1_CI_AS) and PostgreSQL 18.

use sqldiff_core::{diff, Artifact, Dialect, Options, Verdict};

fn equal(a: &str, b: &str) -> (bool, Vec<String>) {
    let r = diff(
        &Artifact::new(
            "a.sql",
            Dialect::Tsql,
            format!("CREATE VIEW dbo.V AS SELECT CASE WHEN {a} THEN 1 ELSE 0 END AS x;"),
        ),
        &Artifact::new(
            "b.sql",
            Dialect::Postgres,
            format!("CREATE OR REPLACE VIEW V AS SELECT CASE WHEN {b} THEN 1 ELSE 0 END AS x;"),
        ),
        &Options::default(),
    );
    let eq = matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions);
    (eq, r.assumptions.iter().map(|x| x.id.clone()).collect())
}

const BY_ID_TABLE: &str = "EXISTS (SELECT * FROM sys.objects WHERE object_id = OBJECT_ID(N'[dbo].[CommandLog]') AND type in (N'U'))";
const BY_NAME_TABLE: &str = "EXISTS (SELECT * FROM sys.objects objects INNER JOIN sys.schemas schemas ON objects.[schema_id] = schemas.[schema_id] WHERE objects.[type] = 'U' AND schemas.[name] = 'dbo' AND objects.[name] = 'CommandLog')";
const BY_ID_PROC: &str = "EXISTS (SELECT * FROM sys.objects WHERE object_id = OBJECT_ID(N'[dbo].[CommandExecute]') AND type in (N'P', N'PC'))";
const BY_NAME_PROC: &str = "EXISTS (SELECT * FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE s.name = 'dbo' AND o.name = 'CommandExecute' AND o.type = 'P')";

const PG_TABLE: &str = "EXISTS (SELECT 1 FROM pg_catalog.pg_class pc INNER JOIN pg_catalog.pg_namespace pn ON pn.oid = pc.relnamespace WHERE pn.nspname = current_schema() AND lower(pc.relname) = 'commandlog' AND pc.relkind IN ('r', 'p'))";
const PG_PROC: &str = "EXISTS (SELECT 1 FROM pg_catalog.pg_proc pp INNER JOIN pg_catalog.pg_namespace pn ON pn.oid = pp.pronamespace WHERE pn.nspname = current_schema() AND lower(pp.proname) = 'commandexecute' AND pp.prokind = 'p')";

#[test]
fn same_kind_and_name_are_equal_under_the_assumption() {
    for (a, b) in [
        (BY_ID_TABLE, PG_TABLE),
        (BY_NAME_TABLE, PG_TABLE),
        (BY_ID_PROC, PG_PROC),
        (BY_NAME_PROC, PG_PROC),
    ] {
        let (eq, ids) = equal(a, b);
        assert!(eq, "not equal:\n{a}\n{b}");
        assert!(ids.iter().any(|x| x == "ASM-CATALOG-EXISTS"), "{ids:?}");
        let (eq, _) = equal(&format!("NOT {a}"), &format!("NOT {b}"));
        assert!(eq, "NOT not equal:\n{a}\n{b}");
    }
}

#[test]
fn another_kind_name_or_schema_is_not_equal() {
    let cases = [
        // Kind.
        (BY_ID_TABLE, PG_PROC),
        (BY_ID_PROC, PG_TABLE),
        // Views (type V) are not tables.
        ("EXISTS (SELECT * FROM sys.objects WHERE object_id = OBJECT_ID(N'[dbo].[CommandLog]') AND type in (N'U', N'V'))", PG_TABLE),
        // A CLR-only or function check is not a procedure check.
        ("EXISTS (SELECT * FROM sys.objects WHERE object_id = OBJECT_ID(N'[dbo].[CommandExecute]') AND type in (N'PC'))", PG_PROC),
        // Name.
        (BY_ID_TABLE, &PG_TABLE.replace("'commandlog'", "'queue'")),
        // Another schema than dbo is not read.
        (&BY_ID_TABLE.replace("[dbo]", "[Sales]"), PG_TABLE),
        (&BY_NAME_TABLE.replace("= 'dbo'", "= 'Sales'"), PG_TABLE),
        // `name` unqualified next to sys.schemas is ambiguous.
        (&BY_NAME_TABLE.replace("objects.[name] =", "[name] ="), PG_TABLE),
        // An extra condition is not dropped.
        (&BY_ID_TABLE.replace("AND type", "AND is_ms_shipped = 0 AND type"), PG_TABLE),
        // PostgreSQL: any relation kind, another schema, a case-sensitive name.
        (BY_ID_TABLE, &PG_TABLE.replace(" AND pc.relkind IN ('r', 'p')", "")),
        (BY_ID_TABLE, &PG_TABLE.replace("current_schema()", "'public'")),
        (BY_ID_TABLE, &PG_TABLE.replace("lower(pc.relname)", "pc.relname")),
        (BY_ID_TABLE, &PG_TABLE.replace("relkind IN ('r', 'p')", "relkind IN ('r', 'v')")),
        (BY_ID_PROC, &PG_PROC.replace("prokind = 'p'", "prokind = 'f'")),
        // A capital in the PostgreSQL literal never matches lower().
        (BY_ID_TABLE, &PG_TABLE.replace("'commandlog'", "'CommandLog'")),
    ];
    for (a, b) in cases {
        let (eq, _) = equal(a, b);
        assert!(!eq, "equal:\n{a}\n{b}");
    }
}

/// T-SQL `sys.databases` is SQL Server's catalog view; a PostgreSQL table
/// that happens to be called sys.databases is not its counterpart (before,
/// the two compared equal under ASM-BIND-TABLE).
#[test]
fn a_sys_view_is_not_a_postgres_table_of_the_same_name() {
    let r = diff(
        &Artifact::new(
            "a.sql",
            Dialect::Tsql,
            "CREATE PROCEDURE dbo.p @n int OUTPUT AS\nBEGIN\n  SELECT @n = COUNT(*) FROM sys.databases;\nEND\n",
        ),
        &Artifact::new(
            "b.sql",
            Dialect::Postgres,
            "CREATE OR REPLACE PROCEDURE p(INOUT p_n integer)\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  SELECT COUNT(*) INTO p_n FROM sys.databases;\nEND;\n$$;\n",
        ),
        &Options::default(),
    );
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.verdict()
    );
    // The same lookup on one side only is still equal to itself.
    let same = |d: Dialect, text: &str| {
        diff(&Artifact::new("a.sql", d, text), &Artifact::new("b.sql", d, text), &Options::default()).verdict()
    };
    assert!(matches!(
        same(Dialect::Tsql, "SELECT COUNT(*) FROM sys.databases;"),
        Verdict::Equal | Verdict::EqualUnderAssumptions
    ));
}
