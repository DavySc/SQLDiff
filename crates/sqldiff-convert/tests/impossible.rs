//! SQL Server-only features are marked impossible to convert automatically:
//! still not converted (never converted, never counted as clean), but named
//! per feature and counted in a bucket of their own.

use sqldiff_convert::impossible;
use sqldiff_convert::{convert, Conversion, Status};
use sqldiff_core::Dialect;

fn pg(src: &str) -> Conversion {
    convert(src, Dialect::Tsql, Dialect::Postgres).unwrap()
}

#[track_caller]
fn marked(src: &str, want: impossible::Feature) {
    let c = pg(src);
    let u = c
        .units
        .iter()
        .find(|u| u.impossible().is_some())
        .unwrap_or_else(|| {
            panic!(
                "no impossible unit in {:?}\n{}",
                c.units
                    .iter()
                    .map(|u| (u.status, u.notes.clone()))
                    .collect::<Vec<_>>(),
                c.output
            )
        });
    assert_eq!(u.impossible(), Some(want), "{src}");
    assert_eq!(u.status, Status::NotConverted, "{src}");
    assert!(c.counts.impossible >= 1 && c.counts.not_converted >= c.counts.impossible);
    assert!(c.output.contains("-- [not converted]"), "{}", c.output);
}

#[test]
fn statements_with_no_counterpart_are_marked() {
    marked("USE Sales;\n", impossible::DATABASE_LEVEL);
    marked(
        "ALTER DATABASE Sales SET RECOVERY SIMPLE;\n",
        impossible::DATABASE_LEVEL,
    );
    marked("CREATE DATABASE Sales;\n", impossible::DATABASE_LEVEL);
    marked(
        "CREATE SECURITY POLICY p ADD FILTER PREDICATE dbo.f(a) ON dbo.t;\n",
        impossible::SECURITY_POLICY,
    );
    marked(
        "CREATE SERVER AUDIT a TO FILE (FILEPATH = 'x');\n",
        impossible::SERVER_AUDIT,
    );
    marked(
        "CREATE EXTERNAL DATA SOURCE s WITH (LOCATION = 'x');\n",
        impossible::POLYBASE,
    );
    marked("CREATE FULLTEXT CATALOG c;\n", impossible::FULL_TEXT);
    marked(
        "CREATE XML SCHEMA COLLECTION c AS N'<x/>';\n",
        impossible::XML_SCHEMA,
    );
    marked(
        "CREATE PARTITION FUNCTION f (int) AS RANGE LEFT FOR VALUES (1);\n",
        impossible::PARTITIONING,
    );
    marked(
        "CREATE COLUMN MASTER KEY k WITH (KEY_STORE_PROVIDER_NAME = 'p', KEY_PATH = 'x');\n",
        impossible::ALWAYS_ENCRYPTED,
    );
    marked(
        "CREATE NONCLUSTERED COLUMNSTORE INDEX i ON dbo.t (a);\n",
        impossible::COLUMNSTORE,
    );
    marked(
        "CREATE TABLE dbo.t (a int, h hierarchyid);\n",
        impossible::HIERARCHYID,
    );
    marked(
        "CREATE TABLE dbo.t (a int) WITH (MEMORY_OPTIMIZED = ON, DURABILITY = SCHEMA_ONLY);\n",
        impossible::IN_MEMORY_OLTP,
    );
}

#[test]
fn a_missing_rule_is_not_impossible() {
    let c = pg("TRUNCATE TABLE dbo.t WITH (PARTITIONS (1));\nALTER INDEX i ON dbo.t REBUILD;\n");
    assert_eq!(c.counts.impossible, 0);
    assert_eq!(c.counts.not_converted, 2);
}

#[test]
fn hashes_and_instance_metadata_in_a_body_are_marked() {
    let c = pg("CREATE PROCEDURE dbo.p AS\nBEGIN\n  DECLARE @h varbinary(32)\n  SET @h = HASHBYTES('SHA2_256', 'x')\nEND\n");
    let u = c
        .units
        .iter()
        .find(|u| u.impossible().is_some())
        .expect("marked");
    assert_eq!(u.impossible(), Some(impossible::HASH_FUNCTIONS));
    let c = pg("CREATE PROCEDURE dbo.p AS\nBEGIN\n  DECLARE @v sql_variant\n  SET @v = SERVERPROPERTY('Edition')\nEND\n");
    assert!(
        c.units
            .iter()
            .any(|u| u.impossible() == Some(impossible::INSTANCE_METADATA)),
        "{:?}",
        c.units.iter().map(|u| &u.notes).collect::<Vec<_>>()
    );
}

#[test]
fn a_marked_unit_never_counts_as_converted() {
    let c = pg("USE Sales;\nSELECT 1;\n");
    assert!(c.counts.impossible == 1);
    assert_eq!(c.counts.converted + c.counts.assumptions, c.units.len() - 1);
}
