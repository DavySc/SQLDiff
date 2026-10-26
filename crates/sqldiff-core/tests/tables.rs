//! `CREATE TABLE` compared across dialects: each rule has an equal pair and
//! a near miss, and anything the table model does not read stays unknown.

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

fn rules(r: &Report) -> Vec<&str> {
    r.findings.iter().map(|f| f.rule.as_str()).collect()
}

fn asms(r: &Report) -> Vec<&str> {
    r.assumptions.iter().map(|a| a.id.as_str()).collect()
}

/// Equal under assumptions, nothing undecided.
fn equal(a: &str, b: &str) -> Report {
    let r = run(a, b);
    assert_eq!(
        r.verdict,
        Verdict::EqualUnderAssumptions,
        "{:#?}\n{:#?}",
        r.findings,
        r.unknown_reasons
    );
    assert!(r.unknown_reasons.is_empty(), "{:?}", r.unknown_reasons);
    r
}

/// Different by `rule`, and the witness mentions `needle`.
fn differs(a: &str, b: &str, rule: &str, needle: &str) {
    let r = run(a, b);
    assert_eq!(r.verdict, Verdict::Different, "{:#?}", r.unknown_reasons);
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == rule)
        .unwrap_or_else(|| panic!("{rule}: {:?}", rules(&r)));
    assert!(f.witness.contains(needle), "{}", f.witness);
}

/// Not decided, and a reason mentions `needle`.
fn unknown(a: &str, b: &str, needle: &str) {
    let r = run(a, b);
    assert!(
        r.unknown_reasons.iter().any(|x| x.contains(needle)),
        "{needle}: {:?} {:?}",
        r.unknown_reasons,
        rules(&r)
    );
    assert_ne!(r.verdict, Verdict::Equal);
    assert_ne!(r.verdict, Verdict::EqualUnderAssumptions);
}

const ORDERS_T: &str = "CREATE TABLE dbo.Orders (
    OrderID INT IDENTITY(1,1) NOT NULL,
    CustomerID INT NOT NULL,
    Amount DECIMAL(10,2) NOT NULL CONSTRAINT DF_Orders_Amount DEFAULT ((0)),
    Paid BIT NOT NULL DEFAULT 0,
    PlacedAt DATETIME2(3) NOT NULL,
    Note NVARCHAR(200) NULL,
    CONSTRAINT PK_Orders PRIMARY KEY CLUSTERED (OrderID ASC) WITH (PAD_INDEX = OFF, FILLFACTOR = 90) ON [PRIMARY],
    CONSTRAINT FK_Orders_Customers FOREIGN KEY (CustomerID) REFERENCES dbo.Customers (CustomerID),
    CONSTRAINT CK_Orders_Amount CHECK (Amount >= 0)
) ON [PRIMARY] TEXTIMAGE_ON [PRIMARY];";

const ORDERS_P: &str = "CREATE TABLE Orders (
    OrderID integer GENERATED ALWAYS AS IDENTITY (START WITH 1 INCREMENT BY 1) NOT NULL,
    CustomerID integer NOT NULL,
    Amount numeric(10, 2) NOT NULL DEFAULT 0,
    Paid boolean NOT NULL DEFAULT FALSE,
    PlacedAt timestamp(3) NOT NULL,
    Note varchar(200) NULL,
    CONSTRAINT PK_Orders PRIMARY KEY (OrderID),
    CONSTRAINT FK_Orders_Customers FOREIGN KEY (CustomerID) REFERENCES Customers (CustomerID),
    CONSTRAINT CK_Orders_Amount CHECK (Amount >= 0)
);";

#[test]
fn a_converted_table_is_equal_under_what_it_relies_on() {
    let r = equal(ORDERS_T, ORDERS_P);
    let a = asms(&r);
    for id in [
        "ASM-IDENT-FOLD",
        "ASM-DEFAULT-SCHEMA",
        "ASM-IDENTITY-VALUES",
        "ASM-BIT-BOOLEAN-COLUMN",
        "ASM-COLUMN-RANGE",
    ] {
        assert!(a.contains(&id), "{id}: {a:?}");
    }
    // Storage clauses are passed over, and said so.
    assert!(
        r.normalizations
            .iter()
            .any(|n| n.contains("storage clause passed over") && n.contains("ON [PRIMARY]")),
        "{:?}",
        r.normalizations
    );
}

fn replace(text: &str, from: &str, to: &str) -> String {
    assert!(text.contains(from), "{from}");
    text.replacen(from, to, 1)
}

#[test]
fn column_types_compare_by_the_values_they_store() {
    // Narrower: values the original stores are rejected.
    differs(
        ORDERS_T,
        &replace(ORDERS_P, "CustomerID integer", "CustomerID smallint"),
        "equiv.column-type",
        "32768",
    );
    // Another scale rounds differently.
    differs(
        ORDERS_T,
        &replace(ORDERS_P, "numeric(10, 2)", "numeric(10, 3)"),
        "equiv.column-type",
        "decimal places",
    );
    // Unconstrained numeric keeps every decimal.
    differs(
        ORDERS_T,
        &replace(ORDERS_P, "numeric(10, 2)", "numeric"),
        "equiv.column-type",
        "keeps every decimal",
    );
    // datetime2(7), the T-SQL default, keeps a digit timestamp drops.
    differs(
        &replace(ORDERS_T, "DATETIME2(3)", "DATETIME2"),
        &replace(ORDERS_P, "timestamp(3)", "timestamp"),
        "equiv.column-type",
        "7 fraction digits",
    );
    differs(
        &replace(ORDERS_T, "DATETIME2(3)", "DATETIME"),
        ORDERS_P,
        "equiv.column-type",
        "1/300",
    );
    differs(
        &replace(ORDERS_T, "DATETIME2(3)", "DATETIMEOFFSET(3)"),
        &replace(ORDERS_P, "timestamp(3)", "timestamptz(3)"),
        "equiv.column-type",
        "offset",
    );
    // A shorter string column rejects what the original stores.
    differs(
        ORDERS_T,
        &replace(ORDERS_P, "varchar(200)", "varchar(100)"),
        "equiv.column-type",
        "200 characters",
    );
    // Blank-padded against not.
    differs(
        ORDERS_T,
        &replace(ORDERS_P, "varchar(200)", "char(200)"),
        "equiv.column-type",
        "pads",
    );
    // Wider is equal under the range assumption; varchar is code-page bound.
    let r = equal(
        &replace(ORDERS_T, "NVARCHAR(200)", "VARCHAR(MAX)"),
        &replace(ORDERS_P, "varchar(200)", "text"),
    );
    assert!(asms(&r).contains(&"ASM-TSQL-CODE-PAGE"));
    assert!(asms(&r).contains(&"ASM-VALUE-SIZE-1GB"));
    let r = equal(
        &replace(ORDERS_T, "CustomerID INT", "CustomerID TINYINT"),
        &replace(ORDERS_P, "CustomerID integer", "CustomerID smallint"),
    );
    assert!(
        r.normalizations
            .iter()
            .any(|n| n.contains("CustomerID (negative numbers")),
        "{:?}",
        r.normalizations
    );
}

#[test]
fn nullability_identity_and_defaults() {
    differs(
        ORDERS_T,
        &replace(ORDERS_P, "varchar(200) NULL", "varchar(200) NOT NULL"),
        "equiv.column-null",
        "rejects it",
    );
    // A T-SQL column that says nothing is nullable, under ANSI_NULL_DFLT_ON.
    let r = equal(
        &replace(ORDERS_T, "NVARCHAR(200) NULL", "NVARCHAR(200)"),
        ORDERS_P,
    );
    assert!(asms(&r).contains(&"ASM-TSQL-ANSI-NULL-DEFAULT"));
    differs(
        ORDERS_T,
        &replace(ORDERS_P, "START WITH 1", "START WITH 100"),
        "equiv.identity",
        "100",
    );
    differs(
        ORDERS_T,
        &replace(
            ORDERS_P,
            " GENERATED ALWAYS AS IDENTITY (START WITH 1 INCREMENT BY 1)",
            "",
        ),
        "equiv.identity",
        "numbers column OrderID",
    );
    // Without START WITH, PostgreSQL starts at 1 like IDENTITY(1,1).
    equal(
        ORDERS_T,
        &replace(ORDERS_P, " (START WITH 1 INCREMENT BY 1)", ""),
    );
    differs(
        ORDERS_T,
        &replace(ORDERS_P, "DEFAULT FALSE", "DEFAULT TRUE"),
        "equiv.column-default",
        "stores 0",
    );
    differs(
        ORDERS_T,
        &replace(ORDERS_P, " DEFAULT 0,", ","),
        "equiv.column-default",
        "column Amount",
    );
    // Defaults are compared by the engine.
    let r = run(ORDERS_T, &replace(ORDERS_P, "DEFAULT 0,", "DEFAULT 1,"));
    assert_ne!(r.verdict, Verdict::EqualUnderAssumptions, "{r:#?}");
}

#[test]
fn sequence_defaults_name_the_same_sequence() {
    let t = "CREATE TABLE [Warehouse].[Colors] ([ColorID] INT DEFAULT (NEXT VALUE FOR [Sequences].[ColorID]) NOT NULL);";
    let p = "CREATE TABLE Warehouse.Colors (ColorID integer DEFAULT (nextval('Sequences.ColorID')) NOT NULL);";
    equal(t, p);
    equal(
        t,
        &p.replace(
            "(nextval('Sequences.ColorID'))",
            "nextval('sequences.colorid'::regclass)",
        ),
    );
    differs(
        t,
        &p.replace("Sequences.ColorID", "Sequences.SizeID"),
        "equiv.column-default",
        "sequence",
    );
}

#[test]
fn keys_and_their_string_columns() {
    let t = "CREATE TABLE dbo.Codes (Code NVARCHAR(10) NOT NULL PRIMARY KEY, Alt INT NULL UNIQUE);";
    // T-SQL compares the key case-insensitively and blank-padded.
    let p = "CREATE TABLE Codes (Code varchar(10) NOT NULL PRIMARY KEY, Alt integer NULL UNIQUE NULLS NOT DISTINCT);";
    differs(t, p, "equiv.case-sensitivity", "'A' and 'a'");
    differs(t, p, "equiv.trailing-spaces", "'a '");
    // A unique column holds one NULL in T-SQL, any number in PostgreSQL.
    let t2 = "CREATE TABLE dbo.Codes (Code INT NOT NULL PRIMARY KEY, Alt INT NULL UNIQUE);";
    let p2 = "CREATE TABLE Codes (Code integer NOT NULL PRIMARY KEY, Alt integer NULL UNIQUE NULLS NOT DISTINCT);";
    equal(t2, p2);
    differs(
        t2,
        &p2.replace(" NULLS NOT DISTINCT", ""),
        "equiv.unique-nulls",
        "NULL in alt",
    );
    differs(
        t2,
        &p2.replace(" UNIQUE NULLS NOT DISTINCT", ""),
        "equiv.table-key",
        "(alt)",
    );
    differs(
        "CREATE TABLE dbo.t (a INT NOT NULL, CONSTRAINT PK_t PRIMARY KEY (a));",
        "CREATE TABLE t (a integer NOT NULL, CONSTRAINT t_pkey PRIMARY KEY (a));",
        "equiv.constraint-name",
        "PK_t",
    );
}

#[test]
fn foreign_keys_and_checks() {
    differs(
        ORDERS_T,
        &replace(ORDERS_P, "REFERENCES Customers", "REFERENCES Clients"),
        "equiv.foreign-key",
        "Clients",
    );
    differs(
        ORDERS_T,
        &replace(
            ORDERS_P,
            "(CustomerID),\n",
            "(CustomerID) ON DELETE CASCADE,\n",
        ),
        "equiv.foreign-key-action",
        "CASCADE",
    );
    // A check is compared by the rows it rejects.
    let r = run(
        ORDERS_T,
        &replace(ORDERS_P, "CHECK (Amount >= 0)", "CHECK (Amount > 0)"),
    );
    assert_ne!(r.verdict, Verdict::EqualUnderAssumptions, "{r:#?}");
    equal(
        ORDERS_T,
        &replace(ORDERS_P, "CHECK (Amount >= 0)", "CHECK ((0 <= Amount))"),
    );
    differs(
        ORDERS_T,
        &replace(
            ORDERS_P,
            ",\n    CONSTRAINT CK_Orders_Amount CHECK (Amount >= 0)",
            "",
        ),
        "equiv.check",
        "CHECK",
    );
}

#[test]
fn shape_and_names() {
    differs(
        ORDERS_T,
        &replace(ORDERS_P, "CREATE TABLE Orders", "CREATE TABLE sales.Orders"),
        "equiv.table-name",
        "sales.Orders",
    );
    differs(
        ORDERS_T,
        &replace(ORDERS_P, "    Note varchar(200) NULL,\n", ""),
        "equiv.table-columns",
        "SELECT *",
    );
    differs(
        ORDERS_T,
        &replace(ORDERS_P, "Note varchar", "Remark varchar"),
        "equiv.table-columns",
        "Remark",
    );
}

#[test]
fn what_the_model_does_not_read_stays_unknown() {
    for (t, p, why) in [
        (
            replace(
                ORDERS_T,
                "NVARCHAR(200) NULL",
                "NVARCHAR(200) COLLATE Latin1_General_CS_AS NULL",
            ),
            ORDERS_P.to_string(),
            "collation",
        ),
        (
            replace(ORDERS_T, "Note NVARCHAR(200) NULL", "Note AS (Amount * 2)"),
            replace(
                ORDERS_P,
                "Note varchar(200) NULL",
                "Note numeric GENERATED ALWAYS AS (Amount * 2) STORED",
            ),
            "computed column",
        ),
        (
            replace(ORDERS_T, "FILLFACTOR = 90", "IGNORE_DUP_KEY = ON"),
            ORDERS_P.to_string(),
            "IGNORE_DUP_KEY",
        ),
        (
            replace(
                ORDERS_T,
                "TEXTIMAGE_ON [PRIMARY]",
                "WITH (MEMORY_OPTIMIZED = ON)",
            ),
            ORDERS_P.to_string(),
            "MEMORY_OPTIMIZED",
        ),
        (
            replace(ORDERS_T, "Note NVARCHAR(200)", "Note XML"),
            replace(ORDERS_P, "Note varchar(200)", "Note xml"),
            "XML",
        ),
        (
            ORDERS_T.to_string(),
            replace(
                ORDERS_P,
                "CREATE TABLE Orders",
                "CREATE UNLOGGED TABLE Orders",
            ),
            "UNLOGGED",
        ),
        (
            ORDERS_T.to_string(),
            replace(ORDERS_P, "CREATE TABLE Orders", "CREATE TABLE \"Orders\""),
            "keeps its capitals",
        ),
    ] {
        unknown(&t, &p, why);
    }
    // Db2 for i column types have no verified facts yet.
    let r = diff(
        &Artifact::new("a.sql", T, "CREATE TABLE t (a INT NOT NULL);"),
        &Artifact::new(
            "b.sql",
            Dialect::Db2i,
            "CREATE TABLE t (a INTEGER NOT NULL);",
        ),
        &Options::default(),
    );
    assert!(
        r.unknown_reasons.iter().any(|x| x.contains("Db2 for i")),
        "{:?}",
        r.unknown_reasons
    );
}

#[test]
fn tables_beside_a_routine_are_compared_too() {
    let t = format!(
        "{ORDERS_T}\nGO\nCREATE PROCEDURE dbo.Clear AS\nBEGIN\n  DELETE FROM dbo.Orders WHERE Amount = 0;\nEND\nGO\n"
    );
    let p = format!(
        "{ORDERS_P}\nCREATE OR REPLACE PROCEDURE Clear()\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  DELETE FROM Orders WHERE Amount = 0;\nEND;\n$$;\n"
    );
    let r = run(&t, &p);
    assert!(
        !r.unknown_reasons
            .iter()
            .any(|x| x.contains("outside the routine")),
        "{:?}",
        r.unknown_reasons
    );
    let r = run(&t, &replace(&p, "numeric(10, 2)", "numeric(8, 2)"));
    assert!(rules(&r).contains(&"equiv.column-type"), "{r:#?}");
}

#[test]
fn a_default_the_column_converts_is_not_compared() {
    // The column stores its default converted to the column's type, and the
    // dialects convert differently: T-SQL truncates 1.5 into an int, PostgreSQL
    // rounds it to 2; 1234567e0 is a float in T-SQL and numeric in PostgreSQL.
    for (t, p) in [
        ("q INT DEFAULT 1.5", "q integer DEFAULT 1.5"),
        ("q TINYINT DEFAULT 3.7", "q smallint DEFAULT 3.7"),
        (
            "q VARCHAR(20) DEFAULT 1234567e0",
            "q varchar(20) DEFAULT 1234567e0",
        ),
        (
            "q DECIMAL(5,2) DEFAULT 1.005",
            "q numeric(5, 2) DEFAULT 1.005",
        ),
        ("q INT DEFAULT '7'", "q integer DEFAULT '7'"),
        ("q DATE DEFAULT '20200101'", "q date DEFAULT '20200101'"),
        ("q INT DEFAULT (1 + 1)", "q integer DEFAULT (1 + 1)"),
        ("q VARCHAR(3) DEFAULT 'abcd'", "q varchar(3) DEFAULT 'abcd'"),
    ] {
        unknown(
            &format!("CREATE TABLE dbo.D (id INT NOT NULL PRIMARY KEY, {t});"),
            &format!("CREATE TABLE D (id integer NOT NULL PRIMARY KEY, {p});"),
            "converted to the column's type",
        );
    }
    // Values the column holds as written are compared.
    for (t, p) in [
        ("q INT DEFAULT ((-5))", "q integer DEFAULT -5"),
        ("q TINYINT DEFAULT 255", "q smallint DEFAULT 255"),
        ("q DECIMAL(5,2) DEFAULT 1.5", "q numeric(5, 2) DEFAULT 1.5"),
        ("q VARCHAR(3) DEFAULT 'abc'", "q varchar(3) DEFAULT 'abc'"),
        ("q INT DEFAULT NULL", "q integer DEFAULT NULL"),
    ] {
        equal(
            &format!("CREATE TABLE dbo.D (id INT NOT NULL PRIMARY KEY, {t});"),
            &format!("CREATE TABLE D (id integer NOT NULL PRIMARY KEY, {p});"),
        );
    }
}
