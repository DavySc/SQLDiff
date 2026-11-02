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
        "(concat([a],N' ',[n]))",
        "(concat([a],N' ',[ch]))",
        "(concat([a],N' ',upper([b])))",
        "(concat([a],[w],[w]))",
        "(concat([a]))",
        "(json_query([a],N'strict $.Tags'))",
        "(json_query([a],N'$.x.Tags'))",
        "(json_query([a],N'$.\"Tags\"'))",
        "(json_query([a],N'$'))",
        "(json_value([n],N'$.Tags'))",
        "(json_value([ch],N'$.Tags'))",
        "(json_value(upper([a]),N'$.Tags'))",
        "(json_value([a],N'$.Tags',1))",
        "([n]/(2))",
        "([a]*(2))",
        "(case when [a] IS NULL then CONVERT([bit],(2)) else CONVERT([bit],(1)) end)",
        "(case when [a] IS NULL then CONVERT([int],(0)) else CONVERT([int],(1)) end)",
        "(case when [a] = N'x' then CONVERT([bit],(0)) else CONVERT([bit],(1)) end)",
        "(case when [a] IS NULL then CONVERT([bit],(0)) else [f] end)",
        "(case when getdate() IS NULL then CONVERT([bit],(0)) end)",
        "(case when [a] IS NULL then case when [b] IS NULL then CONVERT([bit],(0)) end end)",
        "(case when CURRENT_TIMESTAMP IS NULL then CONVERT([bit],(0)) end)",
    ] {
        let src = format!("CREATE TABLE s.t (a NVARCHAR(10) NULL, b NVARCHAR(10) NULL, n INT NULL, f BIT NULL, ch NCHAR(5) NULL, w NVARCHAR(2000) NULL, c AS {expr});\n");
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
    // Hash and nonclustered columnstore indexes keep their refusal.
    for idx in [
        "INDEX cx NONCLUSTERED COLUMNSTORE (a)",
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

#[test]
fn pg_geography_becomes_postgis_geography() {
    let src = "CREATE TABLE [Application].[Cities] (\n    [CityID] INT NOT NULL PRIMARY KEY,\n    [Location] [sys].[geography] NULL\n);\n";
    let c = pg(src);
    assert!(
        c.units[0].status < Status::NotConverted,
        "{:?}",
        c.units[0].notes
    );
    assert!(
        c.output.contains("    Location geography NULL\n"),
        "{}",
        c.output
    );
    assert!(has(&c, "ASM-POSTGIS-GEOGRAPHY"));
    // The output check takes PostGIS's type under the same assumption.
    assert!(
        !c.units[0].notes.iter().any(|n| n.rule.starts_with("bind.")),
        "{:?}",
        c.units[0].notes
    );
    // Spatial expressions without a rule (geography::Point and STGeomFromWKB
    // have one, `geography_wkb.rs`) stay refused; Db2 for i keeps refusing
    // the type.
    let point = "DECLARE @p geography = geography::STGeomFromText('POINT(2 1)', 4326);\n";
    assert!(
        pg(point).units.iter().any(|u| u.status >= Status::Unproven),
        "{}",
        pg(point).output
    );
    assert_eq!(db2(src).units[0].status, Status::NotConverted);
    // GEOMETRY stays refused.
    let geo = "CREATE TABLE s.t (g geometry NULL);\n";
    assert_eq!(pg(geo).units[0].status, Status::NotConverted);
}

/// From the review: readers of a computed bit column see a bit (T-SQL gives
/// the CASE its results' type), so they get TRUE / FALSE against the boolean
/// generated column, not `= 1`, which PostgreSQL rejects.
#[test]
fn pg_readers_of_a_computed_bit_column_compare_booleans() {
    let table = "CREATE TABLE dbo.T (Id int NOT NULL, D date NULL,\n  IsOpen AS (CASE WHEN D IS NULL THEN CONVERT(bit,1) ELSE CONVERT(bit,0) END));\nGO\n";
    for (reader, want, not) in [
        (
            "CREATE PROCEDURE dbo.P @n int OUTPUT AS SELECT @n = COUNT(*) FROM dbo.T WHERE IsOpen = 1;\n",
            "WHERE IsOpen = TRUE",
            "IsOpen = 1",
        ),
        (
            "CREATE PROCEDURE dbo.P @n int OUTPUT AS SELECT @n = COUNT(*) FROM dbo.T AS t JOIN dbo.T AS u ON u.Id = t.Id AND t.IsOpen = 0;\n",
            "t.IsOpen = FALSE",
            "IsOpen = 0",
        ),
        (
            "CREATE PROCEDURE dbo.P @b bit OUTPUT AS SET @b = (SELECT IsOpen FROM dbo.T WHERE Id = 1);\n",
            "INOUT p_b boolean",
            "p_b smallint",
        ),
    ] {
        let c = pg(&format!("{table}{reader}"));
        assert!(c.output.contains(want), "{reader}\n{}", c.output);
        assert!(!c.output.contains(not), "{reader}\n{}", c.output);
    }
}

#[test]
fn pg_computed_concat_becomes_coalesce_parts() {
    let src = "CREATE TABLE [Application].[People] (\n    [PersonID] INT NOT NULL,\n    [FullName] NVARCHAR (50) NOT NULL,\n    [PreferredName] NVARCHAR (50) NOT NULL,\n    [SearchName] AS (concat([PreferredName],N' ',[FullName])) PERSISTED NOT NULL,\n    [Notes] NVARCHAR (MAX) NULL,\n    [SearchDetails] AS (concat([FullName],N' ',[Notes])),\n    CONSTRAINT [PK_People] PRIMARY KEY CLUSTERED ([PersonID] ASC)\n);\n";
    let c = pg(src);
    assert!(
        c.output.contains("    SearchName varchar(101) GENERATED ALWAYS AS (COALESCE(PreferredName, '') || ' ' || COALESCE(FullName, '')) STORED NOT NULL,\n"),
        "{}",
        c.output
    );
    assert!(
        c.output.contains("    SearchDetails text GENERATED ALWAYS AS (COALESCE(FullName, '') || ' ' || COALESCE(Notes, '')) STORED,\n"),
        "{}",
        c.output
    );
    // The engine compared the table, generated columns included.
    assert_eq!(
        c.units[0].checked,
        Status::Converted,
        "{:#?}",
        c.units[0].notes
    );
    assert!(
        matches!(c.units[0].status, Status::Converted | Status::Assumptions),
        "{:#?}",
        c.units[0].notes
    );
    assert!(has(&c, "ASM-COMPUTED-STORED"));
}

#[test]
fn pg_clustered_columnstore_index_is_left_out() {
    let src = "CREATE TABLE [Application].[Logs](\n\t[Message] NVARCHAR(4000) NOT NULL,\n\t[Level] VARCHAR(16) NOT NULL,\n\tINDEX CCX_Application_Logs CLUSTERED COLUMNSTORE\n)\nGO\nEXECUTE sp_addextendedproperty @name = N'Description', @value = 'x', @level0type = N'SCHEMA', @level0name = N'Application', @level1type = N'TABLE', @level1name = N'Logs', @level2type = N'INDEX', @level2name = N'CCX_Application_Logs';\nGO\n";
    let c = pg(src);
    assert!(!c.output.contains("CREATE INDEX"), "{}", c.output);
    assert_eq!(
        c.units[0].checked,
        Status::Converted,
        "{:#?}",
        c.units[0].notes
    );
    assert!(has(&c, "tsql.table.columnstore"));
    // The comment on the index has nothing to land on.
    let u = c
        .units
        .iter()
        .find(|u| {
            u.notes
                .iter()
                .any(|n| n.text.contains("left the (columnstore) index out"))
        })
        .expect("the index comment is refused");
    assert_eq!(u.status, Status::NotConverted);
    // Db2 for i keeps the refusal.
    assert_eq!(db2(src).units[0].status, Status::NotConverted);
}

#[test]
fn pg_computed_json_reads_the_key_with_json_operators() {
    let src = "CREATE TABLE [Warehouse].[StockItems] (\n    [StockItemID] INT NOT NULL,\n    [CustomFields] NVARCHAR (MAX) NULL,\n    [Tags] AS (json_query([CustomFields],N'$.Tags')),\n    [Brand] AS (json_value([CustomFields],N'$.Brand')),\n    CONSTRAINT [PK_StockItems] PRIMARY KEY CLUSTERED ([StockItemID] ASC)\n);\n";
    let c = pg(src);
    assert!(
        c.output.contains("    Tags text GENERATED ALWAYS AS (CASE WHEN json_typeof(CAST(CustomFields AS json) -> 'Tags') IN ('object', 'array') THEN CAST(CAST(CustomFields AS json) -> 'Tags' AS text) END) STORED,\n"),
        "{}",
        c.output
    );
    assert!(
        c.output.contains("    Brand varchar(4000) GENERATED ALWAYS AS (CASE WHEN json_typeof(CAST(CustomFields AS json) -> 'Brand') NOT IN ('object', 'array') THEN CAST(CustomFields AS json) ->> 'Brand' END) STORED,\n"),
        "{}",
        c.output
    );
    // The engine compared the table, generated columns included.
    assert_eq!(
        c.units[0].checked,
        Status::Converted,
        "{:#?}",
        c.units[0].notes
    );
    assert_eq!(
        c.units[0].status,
        Status::Assumptions,
        "{:#?}",
        c.units[0].notes
    );
    assert!(has(&c, "ASM-JSON-PATH-VALUES"));
}

#[test]
fn pg_invoices_json_date_and_isjson_check() {
    let src = "CREATE TABLE [Sales].[Invoices] (\n    [InvoiceID] INT NOT NULL,\n    [ReturnedDeliveryData] NVARCHAR (MAX) NULL,\n    [ConfirmedDeliveryTime] AS (TRY_CONVERT([datetime2](7),json_value([ReturnedDeliveryData],N'$.DeliveredWhen'),(126))),\n    [ConfirmedReceivedBy] AS (json_value([ReturnedDeliveryData],N'$.ReceivedBy')),\n    CONSTRAINT [PK_Sales_Invoices] PRIMARY KEY CLUSTERED ([InvoiceID] ASC),\n    CONSTRAINT [CK_Sales_Invoices_ReturnedDeliveryData_Must_Be_Valid_JSON] CHECK ([ReturnedDeliveryData] IS NULL OR isjson([ReturnedDeliveryData])<>(0))\n);\n";
    let c = pg(src);
    let iso = sqldiff_core::equiv::canon::pg_template(
        sqldiff_core::equiv::canon::ISO126_PG,
        "CAST(ReturnedDeliveryData AS json) ->> 'DeliveredWhen'",
    );
    assert!(
        c.output.contains(&format!(
            "    ConfirmedDeliveryTime timestamp GENERATED ALWAYS AS ({iso}) STORED,\n"
        )),
        "{}",
        c.output
    );
    assert!(
        c.output.contains("CHECK (ReturnedDeliveryData IS NULL OR CASE WHEN ReturnedDeliveryData IS NULL THEN NULL WHEN json_typeof(CAST(ReturnedDeliveryData AS json)) IN ('object', 'array') THEN 1 ELSE 0 END<>(0))"),
        "{}",
        c.output
    );
    assert_eq!(
        c.units[0].checked,
        Status::Converted,
        "{:#?}",
        c.units[0].notes
    );
    assert_eq!(
        c.units[0].status,
        Status::Assumptions,
        "{:#?}",
        c.units[0].notes
    );
    for id in [
        "ASM-ISO126-VALUES",
        "ASM-JSON-PATH-VALUES",
        "ASM-FRACTIONAL-SECONDS",
    ] {
        assert!(has(&c, id), "{id}");
    }
}

#[test]
fn pg_try_convert_and_isjson_without_a_rule_stay_refused() {
    for expr in [
        "(TRY_CONVERT([datetime2](3),json_value([a],N'$.d'),(126)))",
        "(TRY_CONVERT([datetime],json_value([a],N'$.d'),(126)))",
        "(TRY_CONVERT([datetime2](7),json_value([a],N'$.d'),(127)))",
        "(TRY_CONVERT([datetime2](7),json_query([a],N'$.d'),(126)))",
        "(TRY_CONVERT([datetime2](7),[n],(126)))",
        "(TRY_CONVERT([datetime2](7),upper([a]),(126)))",
        "(isjson([a]))",
    ] {
        let src = format!("CREATE TABLE s.t (a NVARCHAR(10) NULL, n INT NULL, c AS {expr});\n");
        let c = pg(&src);
        assert_eq!(
            c.units[0].status,
            Status::NotConverted,
            "{expr}\n{}",
            c.output
        );
    }
    // ISJSON outside a CHECK constraint keeps its refusal.
    let c = pg("SELECT ISJSON(N'{}');\n");
    assert_eq!(c.units[0].status, Status::NotConverted, "{}", c.output);
}

/// One table with the columns and one computed column `c AS …`.
fn computed_table(col: &str) -> String {
    format!("CREATE TABLE dbo.K (\n    Si smallint NOT NULL,\n    I int NOT NULL,\n    Bi bigint NOT NULL,\n    D51 decimal(5,1) NOT NULL,\n    D92 decimal(9,2) NOT NULL,\n    D82 decimal(8,2) NOT NULL,\n    {col}\n);\n")
}

/// Arithmetic computed columns over integer and decimal columns become
/// STORED columns of the type SQL Server derives (sys.columns on SQL Server
/// 2022, eval/builtins/2026-11-02-o/computed-types-tsql.out and
/// eval/pr-review/2026-11-02-pr254): a bare int literal next to a decimal is
/// decimal(digits, 0), and the engine proves each one.
#[test]
fn pg_computed_arithmetic_has_the_derived_type() {
    for (col, want) in [
        (
            "C AS ISNULL(D92 - D82, 0.00)",
            "C numeric(10, 2) GENERATED ALWAYS AS (COALESCE(D92 - D82, 0.00)) STORED NOT NULL",
        ),
        (
            "C AS D51 + 1",
            "C numeric(6, 1) GENERATED ALWAYS AS (D51 + 1) STORED",
        ),
        (
            "C AS D51 * 2",
            "C numeric(7, 1) GENERATED ALWAYS AS (D51 * 2) STORED",
        ),
        (
            "C AS D92 - 100000",
            "C numeric(10, 2) GENERATED ALWAYS AS (D92 - 100000) STORED",
        ),
        (
            "C AS (Si + 1) * D51",
            "C numeric(16, 1) GENERATED ALWAYS AS ((Si + 1) * D51) STORED",
        ),
        (
            "C AS D51 * 2147483647",
            "C numeric(16, 1) GENERATED ALWAYS AS (D51 * 2147483647) STORED",
        ),
        (
            "C AS D92 * 10",
            "C numeric(12, 2) GENERATED ALWAYS AS (D92 * 10) STORED",
        ),
        (
            "C AS Bi * D51",
            "C numeric(25, 1) GENERATED ALWAYS AS (Bi * D51) STORED",
        ),
        (
            "C AS D51 - 0",
            "C numeric(6, 1) GENERATED ALWAYS AS (D51 - 0) STORED",
        ),
        (
            "C AS (1 + 2) * D51",
            "C numeric(16, 1) GENERATED ALWAYS AS ((1 + 2) * D51) STORED",
        ),
        (
            "C AS D51 * (2)",
            "C numeric(7, 1) GENERATED ALWAYS AS (D51 * (2)) STORED",
        ),
        (
            "C AS D51 * 2 * 3",
            "C numeric(9, 1) GENERATED ALWAYS AS (D51 * 2 * 3) STORED",
        ),
        (
            "C AS (D92 + I)",
            "C numeric(13, 2) GENERATED ALWAYS AS (D92 + I) STORED",
        ),
        (
            "C AS (Si * Si) PERSISTED NOT NULL",
            "C smallint GENERATED ALWAYS AS (Si * Si) STORED NOT NULL",
        ),
        (
            "C AS I - Si PERSISTED",
            "C integer GENERATED ALWAYS AS (I - Si) STORED",
        ),
        (
            "C AS ISNULL(Bi * I, 0) PERSISTED",
            "C bigint GENERATED ALWAYS AS (COALESCE(Bi * I, 0)) STORED NOT NULL",
        ),
    ] {
        let c = pg(&computed_table(col));
        assert!(c.output.contains(want), "{col}\n{want}\n{}", c.output);
        assert_eq!(
            c.units[0].checked,
            Status::Converted,
            "{col}: {:#?}",
            c.units[0].notes
        );
        assert!(
            matches!(c.units[0].status, Status::Converted | Status::Assumptions),
            "{col}: {:#?}",
            c.units[0].notes
        );
        assert!(has(&c, "ASM-COMPUTED-STORED"));
    }
}

/// What the rule does not read stays refused: integer arithmetic in a
/// column that is not PERSISTED (SQL Server fails when the column is read,
/// PostgreSQL when the row is written), money and tinyint, decimal literals,
/// a decimal product past 38 digits, division, an ISNULL fallback that does
/// not fit.
#[test]
fn pg_computed_arithmetic_refusals() {
    for col in [
        "C AS I * I",
        "C AS Si * Si",
        "C AS ISNULL(I - Si, 0)",
        "C AS Si + 1",
        "C AS (M * 2)",
        "C AS (T + T) PERSISTED",
        "C AS (D51 * 1.5)",
        "C AS (W * W)",
        "C AS (I / 2) PERSISTED",
        "C AS ISNULL(I - I, 3000000000) PERSISTED",
        "C AS ISNULL(Si - Si, 40000) PERSISTED",
    ] {
        let src = format!("CREATE TABLE dbo.T (\n    M money NOT NULL,\n    T tinyint NOT NULL,\n    D51 decimal(5,1) NOT NULL,\n    W decimal(30,5) NOT NULL,\n    I int NOT NULL,\n    Si smallint NOT NULL,\n    {col}\n);\n");
        let c = pg(&src);
        assert_eq!(
            c.units[0].status,
            Status::NotConverted,
            "{col}\n{}",
            c.output
        );
    }
}

fn money_table(col: &str) -> String {
    format!("CREATE TABLE dbo.P (\n    A money NOT NULL,\n    B money NOT NULL,\n    Q int NOT NULL,\n    Sm smallint NOT NULL,\n    R decimal(9,2) NOT NULL,\n    Sx smallmoney NOT NULL,\n    Big bigint NOT NULL,\n    {col}\n);\n")
}

/// Money columns: a sum, a difference or a product with an integer is money
/// again (numeric(19, 4)), money with a decimal is a decimal, and the engine
/// proves each one (sys.columns on SQL Server 2022,
/// eval/builtins/2026-11-02-o/computed-types-tsql.out and
/// eval/builtins/2026-11-02-money). Money arithmetic raises its overflow
/// error when SQL Server reads a column that is not PERSISTED, so only
/// PERSISTED columns convert.
#[test]
fn pg_computed_money_arithmetic() {
    for (col, want) in [
        (
            "C AS A + B PERSISTED",
            "C numeric(19,4) GENERATED ALWAYS AS (A + B) STORED",
        ),
        (
            "C AS (A - B) PERSISTED NOT NULL",
            "C numeric(19,4) GENERATED ALWAYS AS (A - B) STORED NOT NULL",
        ),
        (
            "C AS Sm * A PERSISTED",
            "C numeric(19,4) GENERATED ALWAYS AS (Sm * A) STORED",
        ),
        (
            "C AS A * Q PERSISTED",
            "C numeric(19,4) GENERATED ALWAYS AS (A * Q) STORED",
        ),
        (
            "C AS A - 1 PERSISTED",
            "C numeric(19,4) GENERATED ALWAYS AS (A - 1) STORED",
        ),
        (
            "C AS ISNULL(Q * A, 0) PERSISTED",
            "C numeric(19,4) GENERATED ALWAYS AS (COALESCE(Q * A, 0)) STORED NOT NULL",
        ),
        (
            "C AS A + R",
            "C numeric(20, 4) GENERATED ALWAYS AS (A + R) STORED",
        ),
        (
            "C AS R * A",
            "C numeric(29, 6) GENERATED ALWAYS AS (R * A) STORED",
        ),
    ] {
        let c = pg(&money_table(col));
        assert!(c.output.contains(want), "{col}\n{want}\n{}", c.output);
        assert_eq!(
            c.units[0].checked,
            Status::Converted,
            "{col}: {:#?}",
            c.units[0].notes
        );
        assert!(has(&c, "ASM-COMPUTED-STORED"));
    }
}

/// Money uses SQL Server rounds or reads differently stay refused: a product
/// or quotient of money, smallmoney, a column that is not PERSISTED, a float
/// or a string operand.
#[test]
fn pg_computed_money_refusals() {
    for col in [
        "C AS A + B",
        // A bigint is converted to money first and may not fit.
        "C AS A + Big PERSISTED",
        "C AS Big * A PERSISTED",
        // A product inside a chain is still a rounded money product.
        "C AS A + B * B PERSISTED",
        "C AS A * B PERSISTED",
        "C AS A / Q PERSISTED",
        "C AS A / B PERSISTED",
        "C AS Sx + Sx PERSISTED",
        "C AS A + Sx PERSISTED",
        "C AS A * 2.5 PERSISTED",
        "C AS ISNULL(A + B, 0.00001) PERSISTED",
    ] {
        let c = pg(&money_table(col));
        assert_eq!(
            c.units[0].status,
            Status::NotConverted,
            "{col}\n{}",
            c.output
        );
    }
}
