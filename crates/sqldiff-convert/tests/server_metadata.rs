//! SQL Server metadata to PostgreSQL: a check whether a dbo table or
//! procedure exists becomes the same check on pg_catalog and is proven under
//! ASM-CATALOG-EXISTS; every other `sys.*` view, DMV or system function and
//! the instance-metadata built-ins are refused with a reason, all under one
//! rule (`not-converted.server-metadata`) so a reviewer finds them together.

use sqldiff_convert::{convert, Conversion, Status, Unit};
use sqldiff_core::Dialect;

fn run(body: &str) -> Conversion {
    let src = format!(
        "CREATE PROCEDURE dbo.p @LogToTable nvarchar(max)
AS
BEGIN
  DECLARE @e int = 0
  {body}
END
"
    );
    convert(&src, Dialect::Tsql, Dialect::Postgres).unwrap()
}

/// The unit of the statement on line 5.
fn unit(c: &Conversion) -> &Unit {
    c.units.iter().find(|u| u.src.line == 5).unwrap()
}

#[track_caller]
fn proven(body: &str, want: &str) {
    let c = run(body);
    assert!(c.output.contains(want), "{}", c.output);
    let u = unit(&c);
    assert_eq!(u.checked, Status::Converted, "{:?}\n{}", u.notes, c.output);
    assert_eq!(u.status, Status::Assumptions, "{:?}", u.notes);
    assert!(
        u.notes.iter().any(|n| n.rule == "ASM-CATALOG-EXISTS"),
        "{:?}",
        u.notes
    );
    assert!(!c.output.contains("sys."), "{}", c.output);
}

#[track_caller]
fn refused(body: &str, why: &str) {
    let c = run(body);
    let u = unit(&c);
    assert_eq!(u.status, Status::NotConverted, "{body}: {:?}", u.notes);
    let note = u
        .notes
        .iter()
        .find(|n| n.rule == "not-converted.server-metadata")
        .unwrap_or_else(|| panic!("{body}: {:?}", u.notes));
    assert!(note.text.contains(why), "{body}: {}", note.text);
}

const PG_TABLE: &str = "EXISTS (SELECT 1 FROM pg_catalog.pg_class pc INNER JOIN pg_catalog.pg_namespace pn ON pn.oid = pc.relnamespace WHERE pn.nspname = current_schema() AND lower(pc.relname) = 'commandlog' AND pc.relkind IN ('r', 'p'))";
const PG_PROC: &str = "EXISTS (SELECT 1 FROM pg_catalog.pg_proc pp INNER JOIN pg_catalog.pg_namespace pn ON pn.oid = pp.pronamespace WHERE pn.nspname = current_schema() AND lower(pp.proname) = 'commandexecute' AND pp.prokind = 'p')";

#[test]
fn existence_checks_of_dbo_tables_and_procedures_convert() {
    // Ola Hallengren's two forms.
    proven(
        "IF @LogToTable = 'Y' AND NOT EXISTS (SELECT * FROM sys.objects objects INNER JOIN sys.schemas schemas ON objects.[schema_id] = schemas.[schema_id] WHERE objects.[type] = 'U' AND schemas.[name] = 'dbo' AND objects.[name] = 'CommandLog') SET @e = 1",
        &format!("IF p_log_to_table = 'Y' AND NOT {PG_TABLE} THEN"),
    );
    proven(
        "IF NOT EXISTS (SELECT * FROM sys.objects WHERE object_id = OBJECT_ID(N'[dbo].[CommandLog]') AND type in (N'U')) SET @e = 1",
        &format!("IF NOT {PG_TABLE} THEN"),
    );
    proven(
        "IF EXISTS (SELECT * FROM sys.objects WHERE object_id = OBJECT_ID(N'[dbo].[CommandExecute]') AND type in (N'P', N'PC')) SET @e = 1",
        &format!("IF {PG_PROC} THEN"),
    );
}

#[test]
fn other_catalog_lookups_are_refused_with_their_reason() {
    // Not a dbo table or procedure check.
    refused(
        "IF EXISTS (SELECT * FROM sys.objects WHERE object_id = OBJECT_ID(N'[Sales].[Orders]') AND type in (N'U')) SET @e = 1",
        "Only a check whether a dbo table or procedure exists is converted",
    );
    refused(
        "IF NOT EXISTS (SELECT 1 FROM sys.tables WHERE name = N'Orders' AND is_memory_optimized <> 0) SET @e = 1",
        "sys.tables: SQL Server's system catalog",
    );
    refused(
        "IF EXISTS (SELECT 1 FROM sys.database_principals WHERE name = N'r' AND type = N'R') SET @e = 1",
        "PostgreSQL roles are shared by the whole cluster",
    );
    refused(
        "IF NOT EXISTS (SELECT 1 FROM sys.server_audits WHERE name = N'WWI_Audit') SET @e = 1",
        "SQL Server Audit",
    );
    refused(
        "IF EXISTS (SELECT 1 FROM sys.column_master_keys WHERE name = N'k') SET @e = 1",
        "Always Encrypted",
    );
    refused(
        "IF EXISTS (SELECT * FROM sys.dm_exec_requests WHERE command = 'BACKUP DATABASE') SET @e = 1",
        "dynamic management view",
    );
    refused(
        "IF EXISTS (SELECT * FROM [master].sys.objects WHERE [type] = 'X' AND [name] = 'xp_backup_database') SET @e = 1",
        "sys.objects",
    );
    refused(
        "IF EXISTS (SELECT * FROM [sys].[databases] WHERE name = N'x') SET @e = 1",
        "sys.databases",
    );
}

#[test]
fn instance_metadata_is_refused_by_property() {
    refused(
        "IF SERVERPROPERTY(N'IsXTPSupported') = 0 SET @e = 1",
        "SERVERPROPERTY('IsXTPSupported'): IsXTPSupported asks whether the instance supports In-Memory OLTP",
    );
    refused(
        "SET @e = CAST(SERVERPROPERTY('EngineEdition') AS int)",
        "PostgreSQL has no editions",
    );
    refused("SET @e = DB_ID(N'x')", "SQL Server database id");
    refused("SET @e = OBJECT_ID(N'dbo.t')", "SQL Server object id");
}

/// Db2 for i: no rule converts the check; the view is refused instead of
/// being written into the output.
#[test]
fn db2_refuses_sys_views() {
    let src = "CREATE PROCEDURE dbo.p
AS
BEGIN
  DECLARE @e int = 0
  IF EXISTS (SELECT * FROM sys.databases WHERE name = N'x') SET @e = 1
END
";
    let c = convert(src, Dialect::Tsql, Dialect::Db2i).unwrap();
    let u = c.units.iter().find(|u| u.src.line == 5).unwrap();
    assert_eq!(u.status, Status::NotConverted, "{:?}", u.notes);
    assert!(
        u.notes
            .iter()
            .any(|n| n.rule == "not-converted.server-metadata"),
        "{:?}",
        u.notes
    );
}
