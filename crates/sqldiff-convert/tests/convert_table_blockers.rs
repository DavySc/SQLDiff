//! CREATE TABLE forms that kept whole tables out of the converted tree (and
//! so failed every statement using them): sequence defaults on Db2 for i,
//! computed columns on PostgreSQL, inline indexes and memory-optimized tables.

use sqldiff_convert::{convert, Conversion, Status};
use sqldiff_core::Dialect;

fn pg(text: &str) -> Conversion {
    convert(text, Dialect::Tsql, Dialect::Postgres).unwrap()
}

fn db2(text: &str) -> Conversion {
    convert(text, Dialect::Tsql, Dialect::Db2i).unwrap()
}

fn has(c: &Conversion, rule: &str) -> bool {
    c.units[0].notes.iter().any(|n| n.rule == rule)
}

const SEQ: &str = "CREATE TABLE [Warehouse].[StockItemStockGroups] (\n    [StockItemStockGroupID] INT CONSTRAINT [DF_Warehouse_SISG_ID] DEFAULT (NEXT VALUE FOR [Sequences].[StockItemStockGroupID]) NOT NULL,\n    [StockItemID] INT NOT NULL,\n    CONSTRAINT [PK_SISG] PRIMARY KEY CLUSTERED ([StockItemStockGroupID] ASC)\n);\n";

#[test]
fn db2_sequence_default_becomes_a_before_insert_trigger() {
    let c = db2(SEQ);
    assert!(
        c.units[0].status < Status::NotConverted,
        "{:?}",
        c.units[0].notes
    );
    assert!(
        c.output
            .contains("    StockItemStockGroupID INTEGER NOT NULL,\n"),
        "the default and its constraint name go: {}",
        c.output
    );
    assert!(
        c.output.contains(
            "CREATE TRIGGER Warehouse.StockItemStockGroups_StockItemStockGroupID_NEXT\n    NO CASCADE BEFORE INSERT ON Warehouse.StockItemStockGroups\n    REFERENCING NEW AS N\n    FOR EACH ROW MODE DB2SQL\n    WHEN (N.StockItemStockGroupID IS NULL)\n    SET N.StockItemStockGroupID = NEXT VALUE FOR Sequences.StockItemStockGroupID"
        ),
        "{}",
        c.output
    );
    assert!(!c.output.contains("DEFAULT NEXT"), "{}", c.output);
    assert!(has(&c, "ASM-SEQUENCE-DEFAULT-TRIGGER"));
    assert!(has(&c, "ASM-FACT-V34"));
    // PostgreSQL keeps the default (nextval).
    let p = pg(SEQ);
    assert!(p
        .output
        .contains("DEFAULT (nextval('Sequences.StockItemStockGroupID'))"));
    assert!(!p.output.contains("TRIGGER"));
}

#[test]
fn db2_trigger_names_and_quoted_columns() {
    let src = "CREATE TABLE [Dimension].[Stock Item] (\n    [Stock Item Key] INT DEFAULT (NEXT VALUE FOR [Sequences].[StockItemKey]) NOT NULL,\n    CONSTRAINT [PK_SI] PRIMARY KEY ([Stock Item Key])\n);\n";
    let c = db2(src);
    assert!(
        c.output.contains("CREATE TRIGGER Dimension.Stock_Item_Stock_Item_Key_NEXT\n    NO CASCADE BEFORE INSERT ON Dimension.\"Stock Item\""),
        "{}",
        c.output
    );
    assert!(
        c.output
            .contains("SET N.\"Stock Item Key\" = NEXT VALUE FOR Sequences.StockItemKey"),
        "{}",
        c.output
    );
    // A table-level primary key makes the column NOT NULL.
    let pk =
        "CREATE TABLE s.t (id INT DEFAULT (NEXT VALUE FOR s.q), CONSTRAINT pk PRIMARY KEY (id));\n";
    assert!(
        db2(pk).units[0].status < Status::NotConverted,
        "{}",
        db2(pk).output
    );
}

#[test]
fn db2_sequence_default_the_trigger_cannot_copy_is_refused() {
    // A NULL written into a nullable column stays NULL in T-SQL; the trigger
    // would fill it in.
    for src in [
        "CREATE TABLE s.t (id INT DEFAULT (NEXT VALUE FOR s.q) NULL);\n",
        "CREATE TABLE s.t (id INT DEFAULT (NEXT VALUE FOR s.q), CHECK (id IS NOT NULL));\n",
        // A declared temporary table has no triggers.
        "CREATE TABLE #t (id INT DEFAULT (NEXT VALUE FOR s.q) NOT NULL);\n",
        // Anything beyond NEXT VALUE FOR is not a plain sequence default.
        "CREATE TABLE s.t (id INT DEFAULT (NEXT VALUE FOR s.q + 1) NOT NULL);\n",
    ] {
        let c = db2(src);
        assert_eq!(
            c.units[0].status,
            Status::NotConverted,
            "{src}\n{}",
            c.output
        );
        assert!(!c.output.contains("CREATE TRIGGER"), "{}", c.output);
    }
}

const FINALIZED: &str = "CREATE TABLE [Sales].[CustomerTransactions] (\n    [FinalizationDate] DATE NULL,\n    [IsFinalized] AS (case when [FinalizationDate] IS NULL then CONVERT([bit],(0)) else CONVERT([bit],(1)) end) PERSISTED,\n    [Amount] INT NOT NULL\n);\n";

#[test]
fn pg_computed_bit_case_becomes_a_stored_generated_column() {
    let c = pg(FINALIZED);
    assert!(
        c.units[0].status < Status::NotConverted,
        "{:?}",
        c.units[0].notes
    );
    assert!(
        c.output.contains("    IsFinalized boolean GENERATED ALWAYS AS (case when FinalizationDate IS NULL then ((0) <> 0) else ((1) <> 0) end) STORED,\n"),
        "{}",
        c.output
    );
    assert!(has(&c, "ASM-COMPUTED-STORED"));
    let cast = FINALIZED
        .replace("CONVERT([bit],(0))", "CAST(0 AS bit)")
        .replace(") PERSISTED", ") PERSISTED NOT NULL");
    let c = pg(&cast);
    assert!(
        c.output
            .contains("then FALSE else ((1) <> 0) end) STORED NOT NULL,"),
        "{}",
        c.output
    );
    // Db2 for i has no generated expression columns.
    assert_eq!(db2(FINALIZED).units[0].status, Status::NotConverted);
}

#[test]
fn pg_computed_columns_without_an_exact_rule_stay_refused() {
    for expr in [
        "(concat([a],N' ',[b]))",
        "(json_query([a],N'$.Tags'))",
        "([n]*(2))",
        "(case when [a] IS NULL then CONVERT([bit],(2)) else CONVERT([bit],(1)) end)",
        "(case when [a] IS NULL then CONVERT([int],(0)) else CONVERT([int],(1)) end)",
        "(case when [a] = N'x' then CONVERT([bit],(0)) else CONVERT([bit],(1)) end)",
        "(case when [a] IS NULL then CONVERT([bit],(0)) else [f] end)",
        "(case when getdate() IS NULL then CONVERT([bit],(0)) end)",
        "(case when [a] IS NULL then case when [b] IS NULL then CONVERT([bit],(0)) end end)",
    ] {
        let src = format!("CREATE TABLE s.t (a NVARCHAR(10) NULL, b NVARCHAR(10) NULL, n INT NULL, f BIT NULL, c AS {expr});\n");
        let c = pg(&src);
        assert_eq!(
            c.units[0].status,
            Status::NotConverted,
            "{expr}\n{}",
            c.output
        );
        assert!(
            c.units[0].notes[0].text.contains("computed column"),
            "{:?}",
            c.units[0].notes
        );
    }
}

#[test]
fn inline_index_becomes_create_index_after_the_table() {
    let src = "CREATE TABLE [Warehouse].[Readings] (\n    [ReadingID] BIGINT NOT NULL,\n    [SensorNumber] INT NOT NULL,\n    CONSTRAINT [PK_Readings] PRIMARY KEY NONCLUSTERED ([ReadingID] ASC),\n    INDEX [IX_Readings_SensorNumber] NONCLUSTERED ([SensorNumber])\n);\n";
    let c = pg(src);
    assert!(
        c.units[0].status < Status::NotConverted,
        "{:?}",
        c.units[0].notes
    );
    assert!(
        c.output.contains("CONSTRAINT PK_Readings PRIMARY KEY (ReadingID)\n);\n\nCREATE INDEX IX_Readings_SensorNumber ON Warehouse.Readings (SensorNumber)"),
        "{}",
        c.output
    );
    assert!(has(&c, "ASM-INDEX-NAME-SCHEMA"));
    let d = db2(src);
    assert!(
        d.output.contains(
            "CREATE INDEX Warehouse.IX_Readings_SensorNumber ON Warehouse.Readings (SensorNumber)"
        ),
        "{}",
        d.output
    );
    // A unique inline index keeps T-SQL's one-NULL rule on PostgreSQL.
    let u = pg(&src.replace(
        "NONCLUSTERED ([SensorNumber])",
        "UNIQUE NONCLUSTERED ([SensorNumber])",
    ));
    assert!(u.output.contains("CREATE UNIQUE INDEX IX_Readings_SensorNumber ON Warehouse.Readings (SensorNumber) NULLS NOT DISTINCT"), "{}", u.output);
    // Columnstore and hash indexes keep their refusal.
    for idx in [
        "INDEX cx CLUSTERED COLUMNSTORE",
        "INDEX ix NONCLUSTERED HASH (a) WITH (BUCKET_COUNT = 1024)",
    ] {
        let src = format!("CREATE TABLE s.t (a INT NOT NULL, {idx});\n");
        assert_eq!(
            pg(&src).units[0].status,
            Status::NotConverted,
            "{}",
            pg(&src).output
        );
    }
}

#[test]
fn memory_optimized_with_durable_rows_is_a_plain_table() {
    for with in [
        "MEMORY_OPTIMIZED = ON",
        "MEMORY_OPTIMIZED = ON, DURABILITY = SCHEMA_AND_DATA",
    ] {
        let src =
            format!("CREATE TABLE s.t (a INT NOT NULL PRIMARY KEY NONCLUSTERED) WITH ({with});\n");
        for c in [pg(&src), db2(&src)] {
            assert!(
                c.units[0].status < Status::NotConverted,
                "{src}\n{:?}",
                c.units[0].notes
            );
            assert!(has(&c, "ASM-MEMORY-OPTIMIZED"));
            assert!(
                !c.output.contains("MEMORY_OPTIMIZED") || c.output.starts_with("--"),
                "{}",
                c.output
            );
        }
    }
    // SCHEMA_ONLY loses every row at a restart: behaviour, refused.
    let src = "CREATE TABLE s.t (a INT NOT NULL PRIMARY KEY NONCLUSTERED) WITH (DURABILITY = SCHEMA_ONLY, MEMORY_OPTIMIZED = ON);\n";
    let c = pg(src);
    assert_eq!(c.units[0].status, Status::NotConverted);
    assert!(
        c.units[0].notes[0].text.contains("SCHEMA_ONLY"),
        "{:?}",
        c.units[0].notes
    );
}
