//! Keys, foreign keys and indexes added with ALTER TABLE … ADD CONSTRAINT or
//! CREATE INDEX, compared across dialects (T-SQL against PostgreSQL): at
//! script level after the tables, and as statements of a procedure (a data
//! load that drops and re-adds them).

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

fn run(a: &str, b: &str) -> Report {
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Postgres, b),
        &Options::default(),
    )
}

fn rules(r: &Report) -> Vec<&str> {
    r.findings.iter().map(|f| f.rule.as_str()).collect()
}

fn asms(r: &Report) -> Vec<&str> {
    r.assumptions.iter().map(|a| a.id.as_str()).collect()
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

fn differs(a: &str, b: &str, rule: &str) {
    let r = run(a, b);
    assert_eq!(r.verdict(), Verdict::Different, "{:#?}", r.unknown_reasons);
    assert!(rules(&r).contains(&rule), "{rule}: {:?}", rules(&r));
}

fn unknown(a: &str, b: &str, needle: &str) {
    let r = run(a, b);
    assert!(
        r.unknown_reasons.iter().any(|x| x.contains(needle)),
        "{needle}: {:?}",
        r.unknown_reasons
    );
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
    assert_ne!(r.verdict(), Verdict::Equal);
}

const TABLES_T: &str = "CREATE TABLE dbo.Customer (id int NOT NULL, code varchar(10) NOT NULL);
GO
CREATE TABLE dbo.Sale (id int NOT NULL, customer_id int NOT NULL, note int NULL);
GO
";

const TABLES_P: &str = "CREATE TABLE Customer (id integer NOT NULL, code varchar(10) NOT NULL);
CREATE TABLE Sale (id integer NOT NULL, customer_id integer NOT NULL, note integer NULL);
";

fn script(t: &str, p: &str) -> (String, String) {
    (format!("{TABLES_T}{t}\n"), format!("{TABLES_P}{p}\n"))
}

#[test]
fn keys_added_after_the_tables_are_compared() {
    let (t, p) = script(
        "ALTER TABLE dbo.Customer ADD CONSTRAINT PK_Customer PRIMARY KEY (id);\nGO\nALTER TABLE dbo.Sale ADD CONSTRAINT FK_Sale_Customer FOREIGN KEY (customer_id) REFERENCES dbo.Customer (id);",
        "ALTER TABLE Customer ADD CONSTRAINT PK_Customer PRIMARY KEY (id);\nALTER TABLE Sale ADD CONSTRAINT FK_Sale_Customer FOREIGN KEY (customer_id) REFERENCES Customer (id);",
    );
    let r = equal(&t, &p);
    assert!(asms(&r).contains(&"ASM-CLUSTERED"), "{:?}", asms(&r));

    // Another key column, another name, another kind, another action.
    let (t, p) = script(
        "ALTER TABLE dbo.Customer ADD CONSTRAINT PK_Customer PRIMARY KEY (id);",
        "ALTER TABLE Customer ADD CONSTRAINT PK_Customer PRIMARY KEY (id, code);",
    );
    differs(&t, &p, "equiv.table-key");
    let (t, p) = script(
        "ALTER TABLE dbo.Customer ADD CONSTRAINT PK_Customer PRIMARY KEY (id);",
        "ALTER TABLE Customer ADD CONSTRAINT UQ_Customer UNIQUE (id);",
    );
    unknown(&t, &p, "no counterpart");
    let (t, p) = script(
        "ALTER TABLE dbo.Customer ADD CONSTRAINT K_Customer PRIMARY KEY (id);",
        "ALTER TABLE Customer ADD CONSTRAINT K_Customer UNIQUE (id);",
    );
    differs(&t, &p, "equiv.constraint-kind");
    let (t, p) = script(
        "ALTER TABLE dbo.Customer ADD CONSTRAINT UQ_Customer UNIQUE (id);\nGO\nALTER TABLE dbo.Sale ADD CONSTRAINT FK_Sale_Customer FOREIGN KEY (customer_id) REFERENCES dbo.Customer (id);",
        "ALTER TABLE Customer ADD CONSTRAINT UQ_Customer UNIQUE (id);\nALTER TABLE Sale ADD CONSTRAINT FK_Sale_Customer FOREIGN KEY (customer_id) REFERENCES Customer (id) ON DELETE CASCADE;",
    );
    differs(&t, &p, "equiv.foreign-key-action");
}

#[test]
fn keys_not_compared() {
    // Rows not checked: NOCHECK and NOT VALID.
    let (t, p) = script(
        "ALTER TABLE dbo.Sale WITH NOCHECK ADD CONSTRAINT FK_Sale_Customer FOREIGN KEY (customer_id) REFERENCES dbo.Customer (id);",
        "ALTER TABLE Sale ADD CONSTRAINT FK_Sale_Customer FOREIGN KEY (customer_id) REFERENCES Customer (id) NOT VALID;",
    );
    unknown(&t, &p, "WITH NOCHECK");
    unknown(&t, &p, "NOT VALID");
    // A key on a nullable column: SQL Server refuses it (Msg 8111),
    // PostgreSQL sets NOT NULL.
    let (t, p) = script(
        "ALTER TABLE dbo.Sale ADD CONSTRAINT PK_Sale PRIMARY KEY (note);",
        "ALTER TABLE Sale ADD CONSTRAINT PK_Sale PRIMARY KEY (note);",
    );
    unknown(&t, &p, "Msg 8111");
}

const LOAD_T: &str = "CREATE TABLE dbo.Customer (id int NOT NULL PRIMARY KEY, code varchar(10) NOT NULL);
GO
CREATE TABLE dbo.Sale (id int NOT NULL, customer_id int NOT NULL, note int NULL);
GO
CREATE PROCEDURE dbo.Load
AS
BEGIN
    SET NOCOUNT ON;
    SET XACT_ABORT ON;
    DELETE FROM dbo.Sale WHERE id < 0;
    ALTER TABLE dbo.Sale WITH CHECK ADD CONSTRAINT FK_Sale_Customer FOREIGN KEY (customer_id) REFERENCES dbo.Customer (id);
    ALTER TABLE dbo.Sale CHECK CONSTRAINT FK_Sale_Customer;
    CREATE INDEX IX_Sale_Customer ON dbo.Sale (customer_id);
END;
";

fn load_p(body: &str) -> String {
    format!(
        "CREATE TABLE Customer (id integer NOT NULL PRIMARY KEY, code varchar(10) NOT NULL);
CREATE TABLE Sale (id integer NOT NULL, customer_id integer NOT NULL, note integer NULL);
CREATE OR REPLACE PROCEDURE load()
LANGUAGE plpgsql
AS $$
BEGIN
{body}
END;
$$;
"
    )
}

const LOAD_P: &str = "  DELETE FROM Sale WHERE id < 0;
  ALTER TABLE Sale ADD CONSTRAINT FK_Sale_Customer FOREIGN KEY (customer_id) REFERENCES Customer (id);
  ALTER TABLE Sale VALIDATE CONSTRAINT FK_Sale_Customer;
  CREATE INDEX IX_Sale_Customer ON Sale (customer_id);";

#[test]
fn keys_and_indexes_added_in_a_procedure_are_compared() {
    equal(LOAD_T, &load_p(LOAD_P));
    differs(
        LOAD_T,
        &load_p(&LOAD_P.replace(
            "REFERENCES Customer (id);",
            "REFERENCES Customer (id) ON DELETE CASCADE;",
        )),
        "equiv.foreign-key-action",
    );
    // Another index is a speed difference, not judged.
    unknown(
        LOAD_T,
        &load_p(&LOAD_P.replace("ON Sale (customer_id)", "ON Sale (id)")),
        "key columns",
    );
    // Moved before the DELETE: out of place among the data statements.
    let moved = LOAD_P.replace("  DELETE FROM Sale WHERE id < 0;\n", "")
        + "\n  DELETE FROM Sale WHERE id < 0;";
    unknown(LOAD_T, &load_p(&moved), "data statement(s)");
}

#[test]
fn routine_ddl_not_compared() {
    // CHECK CONSTRAINT away from the ADD re-checks the rows: not a no-op.
    let t = LOAD_T.replace(
        "    ALTER TABLE dbo.Sale CHECK CONSTRAINT FK_Sale_Customer;\n",
        "",
    ).replace(
        "    CREATE INDEX IX_Sale_Customer ON dbo.Sale (customer_id);\n",
        "    CREATE INDEX IX_Sale_Customer ON dbo.Sale (customer_id);\n    ALTER TABLE dbo.Sale CHECK CONSTRAINT FK_Sale_Customer;\n",
    );
    let p = load_p(
        &LOAD_P
            .replace(
                "  ALTER TABLE Sale VALIDATE CONSTRAINT FK_Sale_Customer;\n",
                "",
            )
            .replace(
                "ON Sale (customer_id);",
                "ON Sale (customer_id);\n  ALTER TABLE Sale VALIDATE CONSTRAINT FK_Sale_Customer;",
            ),
    );
    unknown(&t, &p, "not covered");
    // Inside an IF: runs zero or one times.
    let t = LOAD_T.replace(
        "    CREATE INDEX IX_Sale_Customer",
        "    IF 1 = 1 CREATE INDEX IX_Sale_Customer",
    );
    let p = load_p(&LOAD_P.replace(
        "  CREATE INDEX IX_Sale_Customer ON Sale (customer_id);",
        "  IF 1 = 1 THEN CREATE INDEX IX_Sale_Customer ON Sale (customer_id); END IF;",
    ));
    unknown(&t, &p, "inside an IF");
}

#[test]
fn constraint_names_reused_where_one_side_refuses_them_are_not_compared() {
    // One FK name on two tables: SQL Server refuses the second (names are
    // unique per schema), PostgreSQL takes it (unique per table).
    let (t, p) = script(
        "ALTER TABLE dbo.Customer ADD CONSTRAINT PK_Customer PRIMARY KEY (id);\nGO\nALTER TABLE dbo.Sale ADD CONSTRAINT FK_X FOREIGN KEY (customer_id) REFERENCES dbo.Customer (id);\nGO\nALTER TABLE dbo.Customer ADD CONSTRAINT FK_X FOREIGN KEY (id) REFERENCES dbo.Customer (id);",
        "ALTER TABLE Customer ADD CONSTRAINT PK_Customer PRIMARY KEY (id);\nALTER TABLE Sale ADD CONSTRAINT FK_X FOREIGN KEY (customer_id) REFERENCES Customer (id);\nALTER TABLE Customer ADD CONSTRAINT FK_X FOREIGN KEY (id) REFERENCES Customer (id);",
    );
    unknown(&t, &p, "Msg 2714");
    // A UNIQUE named like an index of another table: PostgreSQL refuses it.
    let (t, p) = script(
        "CREATE INDEX IX_S ON dbo.Customer (code);\nGO\nALTER TABLE dbo.Sale ADD CONSTRAINT IX_S UNIQUE (id);",
        "CREATE INDEX IX_S ON Customer (code);\nALTER TABLE Sale ADD CONSTRAINT IX_S UNIQUE (id);",
    );
    unknown(&t, &p, "index's name");
    // The same in a procedure.
    let t = LOAD_T.replace(
        "    CREATE INDEX IX_Sale_Customer ON dbo.Sale (customer_id);\n",
        "    CREATE INDEX IX_Sale_Customer ON dbo.Sale (customer_id);\n    ALTER TABLE dbo.Customer ADD CONSTRAINT IX_Sale_Customer UNIQUE (code);\n",
    );
    let p = load_p(&format!(
        "{LOAD_P}\n  ALTER TABLE Customer ADD CONSTRAINT IX_Sale_Customer UNIQUE (code);"
    ));
    unknown(&t, &p, "index's name");
    // A name declared inside a CREATE TABLE, used again by an ALTER.
    let t = "CREATE TABLE dbo.A (id int NOT NULL CONSTRAINT PK_A PRIMARY KEY);\nGO\nCREATE TABLE dbo.B (id int NOT NULL);\nGO\nALTER TABLE dbo.B ADD CONSTRAINT PK_A UNIQUE (id);\n";
    let p = "CREATE TABLE A (id integer NOT NULL CONSTRAINT PK_A PRIMARY KEY);\nCREATE TABLE B (id integer NOT NULL);\nALTER TABLE B ADD CONSTRAINT PK_A UNIQUE (id);\n";
    unknown(t, p, "used again");
    // Two CREATE TABLEs declaring one FK name.
    let t = "CREATE TABLE dbo.A (id int NOT NULL PRIMARY KEY);\nGO\nCREATE TABLE dbo.B (id int NOT NULL, CONSTRAINT FK_X FOREIGN KEY (id) REFERENCES dbo.A (id));\nGO\nCREATE TABLE dbo.C (id int NOT NULL, CONSTRAINT FK_X FOREIGN KEY (id) REFERENCES dbo.A (id));\n";
    let p = "CREATE TABLE A (id integer NOT NULL PRIMARY KEY);\nCREATE TABLE B (id integer NOT NULL, CONSTRAINT FK_X FOREIGN KEY (id) REFERENCES A (id));\nCREATE TABLE C (id integer NOT NULL, CONSTRAINT FK_X FOREIGN KEY (id) REFERENCES A (id));\n";
    unknown(t, p, "Msg 2714");
    // A constraint named like a table, or like a DEFAULT constraint: one
    // `sys.objects` namespace in SQL Server.
    let (t, p) = script(
        "ALTER TABLE dbo.Customer ADD CONSTRAINT Sale UNIQUE (id);",
        "ALTER TABLE Customer ADD CONSTRAINT Sale_k UNIQUE (id);",
    );
    unknown(&t, &p, "Msg 2714");
    let (t, p) = script(
        "ALTER TABLE dbo.Sale ADD CONSTRAINT DF_X DEFAULT (0) FOR note;\nGO\nALTER TABLE dbo.Customer ADD CONSTRAINT DF_X UNIQUE (id);",
        "ALTER TABLE Sale ALTER COLUMN note SET DEFAULT 0;\nALTER TABLE Customer ADD CONSTRAINT DF_X UNIQUE (id);",
    );
    unknown(&t, &p, "Msg 2714");
    let t = "CREATE TABLE dbo.A (id int NOT NULL, n int NOT NULL CONSTRAINT DF_X DEFAULT (0));\nGO\nALTER TABLE dbo.A ADD CONSTRAINT DF_X UNIQUE (id);\n";
    let p = "CREATE TABLE A (id integer NOT NULL, n integer NOT NULL DEFAULT 0);\nALTER TABLE A ADD CONSTRAINT DF_X UNIQUE (id);\n";
    unknown(t, p, "Msg 2714");
}
