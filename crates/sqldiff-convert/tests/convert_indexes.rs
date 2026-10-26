//! Converted indexes, sequences and schemas, checked by the engine's
//! statement proofs (`equiv/ddl.rs`): a unit is converted (under
//! assumptions) only when the engine compared it.

use sqldiff_convert::{check, rewrite, Conversion, Status, Unit};
use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{Dialect, Options};
use std::sync::Arc;

const ITEMS: &str = "CREATE TABLE dbo.Items (ItemID int NOT NULL PRIMARY KEY, Code varchar(10) NULL, Qty int NULL, Bin int NOT NULL);\nGO\n";

fn catalog(text: &str, d: Dialect) -> Arc<Catalog> {
    let mut c = Catalog::default();
    ddl::load_tree(&mut c, &sqldiff_syntax::parse(text, d).syntax(), d);
    Arc::new(c)
}

fn convert(src: &str, to: Dialect) -> Conversion {
    let draft = rewrite(src, Dialect::Tsql, to);
    let opts = Options {
        facts_a: vec![catalog(src, Dialect::Tsql)],
        facts_b: vec![catalog(draft.output(), to)],
        ..Options::default()
    };
    check(src, draft, &opts)
}

fn unit<'a>(c: &'a Conversion, src: &str, needle: &str) -> &'a Unit {
    let at = src.find(needle).unwrap();
    c.units
        .iter()
        .find(|u| u.src.start <= at && at < u.src.end)
        .unwrap()
}

fn out<'a>(c: &'a Conversion, u: &Unit) -> &'a str {
    &c.output[u.out.start..u.out.end]
}

fn rules(u: &Unit) -> Vec<&str> {
    u.notes.iter().map(|n| n.rule.as_str()).collect()
}

#[test]
fn a_filtered_index_keeps_its_filter_and_loses_its_storage() {
    let src = format!("{ITEMS}CREATE NONCLUSTERED INDEX IX_Bin ON dbo.Items (Bin ASC, Qty DESC) INCLUDE (Code) WHERE Qty > 0 WITH (FILLFACTOR = 90, ONLINE = ON) ON [PRIMARY];\nGO\n");
    let c = convert(&src, Dialect::Postgres);
    let u = unit(&c, &src, "CREATE NONCLUSTERED");
    // PostgreSQL rejects WITH after WHERE.
    assert_eq!(
        out(&c, u).trim(),
        "CREATE INDEX IX_Bin ON Items (Bin ASC, Qty DESC) INCLUDE (Code) WHERE Qty > 0;"
    );
    assert_eq!(u.status, Status::Assumptions, "{:?}", u.notes);
    let r = rules(u);
    assert!(r.contains(&"ASM-INDEX-NAME-SCHEMA"), "{r:?}");
    assert!(r.contains(&"ASM-TSQL-FILTERED-INDEX-SET-OPTIONS"), "{r:?}");
}

#[test]
fn a_unique_index_holds_one_null_on_both_sides() {
    let src = format!("{ITEMS}CREATE UNIQUE INDEX UX_Qty ON dbo.Items (Qty, Bin);\nGO\n");
    let c = convert(&src, Dialect::Postgres);
    let u = unit(&c, &src, "CREATE UNIQUE");
    assert_eq!(
        out(&c, u).trim(),
        "CREATE UNIQUE INDEX UX_Qty ON Items (Qty, Bin) NULLS NOT DISTINCT;"
    );
    assert_eq!(u.status, Status::Assumptions, "{:?}", u.notes);
    // A string key compares case-insensitively in T-SQL: a difference.
    let src = format!(
        "{ITEMS}CREATE UNIQUE INDEX UX_Code ON dbo.Items (Code) WHERE Code IS NOT NULL;\nGO\n"
    );
    let c = convert(&src, Dialect::Postgres);
    let u = unit(&c, &src, "CREATE UNIQUE");
    assert_eq!(
        out(&c, u).trim(),
        "CREATE UNIQUE INDEX UX_Code ON Items (Code) NULLS NOT DISTINCT WHERE Code IS NOT NULL;"
    );
    assert_eq!(u.status, Status::Differs, "{:?}", u.notes);
    // Db2 for i: how a unique index treats NULLs is not verified.
    let src = format!("{ITEMS}CREATE UNIQUE INDEX UX_Bin ON dbo.Items (Bin);\nGO\n");
    let c = convert(&src, Dialect::Db2i);
    let u = unit(&c, &src, "CREATE UNIQUE");
    assert_eq!(u.status, Status::Unproven, "{:?}", u.notes);
}

#[test]
fn a_plain_index_converts_to_db2_for_i() {
    let src = format!("{ITEMS}CREATE INDEX IX_Bin ON dbo.Items (Bin DESC);\nGO\n");
    let c = convert(&src, Dialect::Db2i);
    let u = unit(&c, &src, "CREATE INDEX");
    assert_eq!(u.status, Status::Assumptions, "{:?}", u.notes);
}

#[test]
fn sequences_and_schemas() {
    let src = "CREATE SCHEMA Sequences AUTHORIZATION dbo;\nGO\nCREATE SEQUENCE Sequences.OrderNo AS int START WITH 1 CACHE 10;\nGO\n";
    for to in [Dialect::Postgres, Dialect::Db2i] {
        let c = convert(src, to);
        let s = unit(&c, src, "CREATE SCHEMA");
        assert_eq!(out(&c, s).trim(), "CREATE SCHEMA Sequences;", "{to:?}");
        assert_eq!(s.status, Status::Assumptions, "{to:?} {:?}", s.notes);
        assert!(rules(s).contains(&"ASM-SCHEMA-OWNER"));
        let q = unit(&c, src, "CREATE SEQUENCE");
        assert!(q.status <= Status::Assumptions, "{to:?} {:?}", q.notes);
    }
}

#[test]
fn a_partition_scheme_is_dropped_and_a_columnstore_index_refused() {
    let src = format!("{ITEMS}CREATE NONCLUSTERED INDEX IX_Bin\n    ON dbo.Items (Bin ASC)\n    ON PS_Bin (Bin);\nGO\nCREATE CLUSTERED COLUMNSTORE INDEX CCX ON dbo.Items ON PS_Bin (Bin);\nGO\n");
    for to in [Dialect::Postgres, Dialect::Db2i] {
        let c = convert(&src, to);
        let u = unit(&c, &src, "CREATE NONCLUSTERED");
        let o = out(&c, u);
        assert!(!o.contains("PS_Bin") && !o.contains("(Bin);"), "{o}");
        assert_eq!(u.status, Status::Assumptions, "{to:?} {:?}", u.notes);
        let u = unit(&c, &src, "COLUMNSTORE");
        assert_eq!(u.status, Status::NotConverted, "{to:?} {:?}", u.notes);
    }
}
