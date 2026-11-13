//! The output check gate, cross-file context and the rules added for whole
//! code bases (WideWorldImporters frequencies).

use sqldiff_convert::{convert, rewrite_in, Conversion, Status, Unit};
use sqldiff_core::catalog::ddl;
use sqldiff_core::{Dialect, Options};

fn pg(text: &str) -> Conversion {
    convert(text, Dialect::Tsql, Dialect::Postgres).unwrap()
}

fn db2(text: &str) -> Conversion {
    convert(text, Dialect::Tsql, Dialect::Db2i).unwrap()
}

/// The smallest unit whose input contains `needle`; of a hoisted DECLARE
/// (declaration and assignment share the source), the worse one.
fn unit<'a>(c: &'a Conversion, input: &str, needle: &str) -> &'a Unit {
    let at = input
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} not in input"));
    c.units
        .iter()
        .filter(|u| u.src.start <= at && at < u.src.end.max(u.src.start + 1))
        .min_by_key(|u| (u.src.end - u.src.start, std::cmp::Reverse(u.status)))
        .unwrap_or_else(|| panic!("no unit for {needle}"))
}

/// The rules' own verdict on a unit the engine did not compare: the unit is
/// not proven, and this is the worst of its other notes.
fn rules_only(u: &sqldiff_convert::Unit) -> Status {
    assert_eq!(u.status, Status::Unproven, "{:?}", u.notes);
    assert!(
        u.notes
            .iter()
            .any(|n| n.rule == sqldiff_convert::NOT_COMPARED),
        "{:?}",
        u.notes
    );
    u.notes
        .iter()
        .filter(|n| n.rule != sqldiff_convert::NOT_COMPARED)
        .map(|n| n.status)
        .max()
        .unwrap_or(Status::Converted)
}

fn has_rule(u: &Unit, rule: &str) -> bool {
    u.notes.iter().any(|n| n.rule == rule)
}

#[test]
fn a_tsql_builtin_the_target_lacks_is_not_converted() {
    let src = "CREATE PROCEDURE p @n nvarchar(50) AS\nBEGIN\n  DECLARE @h varbinary(32) = HASHBYTES('SHA2_256', @n);\n  UPDATE t SET s = SERVERPROPERTY('Edition');\nEND\n";
    for c in [pg(src), db2(src)] {
        for needle in ["HASHBYTES", "SERVERPROPERTY"] {
            let u = unit(&c, src, needle);
            assert_eq!(u.status, Status::NotConverted, "{needle}\n{}", c.output);
            // SQL Server metadata is grouped under its own review cause.
            let rule = match needle {
                "SERVERPROPERTY" => "not-converted.server-metadata",
                _ => "not-converted",
            };
            assert!(has_rule(u, rule), "{:?}", u.notes);
        }
        assert!(
            !c.output
                .lines()
                .any(|l| !l.trim_start().starts_with("--") && l.contains("HASHBYTES")),
            "{}",
            c.output
        );
    }
}

#[test]
fn a_table_named_like_a_builtin_is_not_a_call() {
    let src = "INSERT INTO log (msg) VALUES ('x');\n";
    let c = pg(src);
    // Converted; equal only under what the engine assumes without a catalog.
    assert_eq!(c.units[0].status, Status::Assumptions);
    assert!(
        c.units[0]
            .notes
            .iter()
            .all(|n| n.rule.starts_with("ASM-BIND-") || n.rule == "ASM-UNTYPED-STORE"),
        "{:?}",
        c.units[0].notes
    );
}

#[test]
fn leftover_source_syntax_found_by_the_output_check_gates_the_unit() {
    // `SELECT @a = …, @b = …` style leftovers aside, the gate sees what the
    // rules let through: here a T-SQL-only built-in kept by a rule-less
    // pass-through inside a view.
    let src = "CREATE VIEW v AS SELECT SOUNDEX(name) AS s FROM t;\n";
    let c = pg(src);
    let u = &c.units[0];
    assert!(u.status >= Status::Differs, "{:?}\n{}", u.notes, c.output);
}

#[test]
fn extended_properties_with_bracketed_arguments() {
    let src = "CREATE SCHEMA [Hr Dept];\nGO\nCREATE TABLE [Hr Dept].[Staff List]([Staff Id] int NOT NULL);\nGO\nEXECUTE [sys].[sp_addextendedproperty] N'MS_Description', N'Who works here', N'SCHEMA', [Hr Dept], N'TABLE', [Staff List], N'COLUMN', [Staff Id];\nGO\nEXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = N'It''s staff', @level0type = N'SCHEMA', @level0name = [Hr Dept], @level1type = N'TABLE', @level1name = [Staff List];\nGO\n";
    let c = pg(src);
    assert!(
        c.output.contains(
            "COMMENT ON COLUMN \"Hr Dept\".\"Staff List\".\"Staff Id\" IS 'Who works here';"
        ),
        "{}",
        c.output
    );
    assert!(
        c.output
            .contains("COMMENT ON TABLE \"Hr Dept\".\"Staff List\" IS 'It''s staff';"),
        "{}",
        c.output
    );
    for u in c.units.iter().skip(2) {
        assert_eq!(rules_only(u), Status::Assumptions, "{:?}", u.notes);
        assert!(has_rule(u, "ASM-EXTPROP-COMMENT"));
    }
    // A bracketed identifier is only a name string where a name goes.
    let v = "EXEC sys.sp_addextendedproperty @name = N'D', @value = [x], @level0type = N'SCHEMA', @level0name = N'S';\n";
    assert_eq!(pg(v).units[0].status, Status::NotConverted);
}

#[test]
fn extended_properties_in_dbo_drop_the_schema() {
    let src = "EXEC sys.sp_addextendedproperty N'D', N'Build', N'SCHEMA', [dbo], N'TABLE', [Version Info], N'COLUMN', [Version No];\nGO\n";
    let c = pg(src);
    assert!(
        c.output
            .contains("COMMENT ON COLUMN \"Version Info\".\"Version No\" IS 'Build';"),
        "{}",
        c.output
    );
    assert!(has_rule(&c.units[0], "ASM-DBO-SCHEMA"));
    let s = "EXEC sys.sp_addextendedproperty N'D', N'Build', N'SCHEMA', [dbo];\nGO\n";
    assert_eq!(pg(s).units[0].status, Status::NotConverted);
}

#[test]
fn extended_properties_become_comments() {
    let src = "EXECUTE sp_addextendedproperty @name = N'Description', @value = N'It''s a table', @level0type = N'SCHEMA', @level0name = N'Sales', @level1type = N'TABLE', @level1name = N'Orders';\nGO\nEXEC sys.sp_addextendedproperty N'Description', N'Key', N'SCHEMA', N'Sales', N'TABLE', N'Orders', N'COLUMN', N'OrderID';\nGO\n";
    let c = pg(src);
    assert!(
        c.output
            .contains("COMMENT ON TABLE Sales.Orders IS 'It''s a table';"),
        "{}",
        c.output
    );
    assert!(
        c.output
            .contains("COMMENT ON COLUMN Sales.Orders.OrderID IS 'Key';"),
        "{}",
        c.output
    );
    assert!(
        !c.output.contains("DO $$"),
        "named arguments need no block:\n{}",
        c.output
    );
    // The engine does not compare EXEC sp_addextendedproperty with COMMENT
    // ON yet: not proven, and under assumptions by the rules.
    for u in &c.units {
        assert_eq!(rules_only(u), Status::Assumptions, "{:?}", u.notes);
        assert!(has_rule(u, "ASM-EXTPROP-COMMENT"));
    }
    let idx = "EXEC sp_addextendedproperty @name = N'Description', @value = N'x', @level0type = N'SCHEMA', @level0name = N'Sales', @level1type = N'TABLE', @level1name = N'Orders', @level2type = N'INDEX', @level2name = N'IX_1';\n";
    assert!(pg(idx)
        .output
        .contains("COMMENT ON INDEX Sales.IX_1 IS 'x';"));
    // Db2 for i creates an index in the current schema, not the table's.
    assert_eq!(db2(idx).units[0].status, Status::NotConverted);
}

#[test]
fn sequences_write_out_every_bound() {
    let src = "CREATE SEQUENCE [Sequences].[CityID] AS INT START WITH 1 INCREMENT BY 1;\nGO\nCREATE SEQUENCE s AS int;\nGO\n";
    let c = pg(src);
    assert!(c.output.contains("CREATE SEQUENCE Sequences.CityID\n    AS integer\n    START WITH 1\n    INCREMENT BY 1\n    MINVALUE -2147483648\n    MAXVALUE 2147483647\n    NO CYCLE;"), "{}", c.output);
    // T-SQL starts an ascending sequence at the type's minimum.
    assert!(
        c.output
            .contains("CREATE SEQUENCE s\n    AS integer\n    START WITH -2147483648"),
        "{}",
        c.output
    );
    assert!(db2(src).output.contains("NO CACHE"));
    let bad = "CREATE SEQUENCE s AS decimal(10, 0);\n";
    assert_eq!(pg(bad).units[0].status, Status::NotConverted);
}

#[test]
fn next_value_for_becomes_nextval() {
    let src = "CREATE TABLE [Warehouse].[Colors] (\n    [ColorID] INT DEFAULT (NEXT VALUE FOR [Sequences].[ColorID]) NOT NULL\n);\n";
    let c = pg(src);
    assert!(
        c.output.contains("DEFAULT (nextval('Sequences.ColorID'))"),
        "{}",
        c.output
    );
    assert_eq!(
        c.units[0].status,
        Status::Converted,
        "{:?}",
        c.units[0].notes
    );
}

#[test]
fn temporal_tables_and_behavioural_options_are_not_storage() {
    let temporal = "CREATE TABLE t (a int, f DATETIME2 GENERATED ALWAYS AS ROW START NOT NULL, e DATETIME2 GENERATED ALWAYS AS ROW END NOT NULL, PERIOD FOR SYSTEM_TIME (f, e)) WITH (SYSTEM_VERSIONING = ON);\n";
    let memory = "CREATE TABLE t (a int) WITH (MEMORY_OPTIMIZED = ON, DURABILITY = SCHEMA_ONLY);\n";
    let dup = "CREATE UNIQUE INDEX ix ON t (a) WITH (IGNORE_DUP_KEY = ON);\n";
    // DROP_EXISTING = ON replaces an existing index; a plain CREATE INDEX fails instead.
    let replace = "CREATE INDEX ix ON t (a) WITH (DROP_EXISTING = ON);\n";
    for src in [temporal, memory, dup, replace] {
        assert_eq!(pg(src).units[0].status, Status::NotConverted, "{src}");
        assert_eq!(db2(src).units[0].status, Status::NotConverted, "{src}");
    }
    let harmless = "CREATE TABLE t (a int) WITH (DATA_COMPRESSION = PAGE);\n";
    // Converted; the engine's assumption about NULL defaults stays.
    let h = pg(harmless);
    assert_eq!(h.units[0].status, Status::Assumptions);
    assert!(
        h.units[0]
            .notes
            .iter()
            .all(|n| n.rule == "ASM-TSQL-ANSI-NULL-DEFAULT" || n.status == Status::Converted),
        "{:?}",
        h.units[0].notes
    );
    let off = "CREATE INDEX ix ON t (a) WITH (DROP_EXISTING = OFF, FILLFACTOR = 80);\n";
    assert!(
        pg(off).units[0].status < Status::NotConverted,
        "{:?}",
        pg(off).units[0].notes
    );
}

#[test]
fn a_qualified_system_type_is_one_type() {
    let src = "CREATE TABLE t (a int, loc [sys].[hierarchyid] NULL);\n";
    let c = pg(src);
    assert_eq!(c.units[0].status, Status::NotConverted, "{}", c.output);
    assert!(c.output.contains("HIERARCHYID"), "{}", c.output);
}

#[test]
fn binary_constants_stay_binary() {
    let src = "INSERT INTO t (b) VALUES (0xABC);\n";
    assert!(
        pg(src).output.contains("('\\x0ABC'::bytea)"),
        "{}",
        pg(src).output
    );
    assert!(
        db2(src).output.contains("(BX'0ABC')"),
        "{}",
        db2(src).output
    );
}

#[test]
fn quotename_rand_and_datefromparts() {
    let src = "CREATE PROCEDURE p @t sysname AS\nBEGIN\n  DECLARE @q nvarchar(300) = QUOTENAME(@t);\n  DECLARE @r float = RAND();\n  DECLARE @d date = DATEFROMPARTS(2024, 2, 29);\n  UPDATE x SET q = @q, r = @r, d = @d;\nEND\n";
    let c = pg(src);
    assert!(
        c.output.contains("v_q := quote_ident(p_t);"),
        "{}",
        c.output
    );
    assert!(c.output.contains("v_r := random();"), "{}", c.output);
    assert!(
        c.output.contains("v_d := make_date(2024, 2, 29);"),
        "{}",
        c.output
    );
    let by_out = |needle: &str| {
        c.units
            .iter()
            .find(|u| c.output[u.out.start..u.out.end].contains(needle))
            .unwrap()
    };
    assert!(has_rule(by_out("quote_ident"), "ASM-QUOTENAME-IDENT"));
    assert!(has_rule(by_out("random()"), "ASM-RAND-PER-QUERY"));
    // RAND(seed) starts a different sequence: no rule.
    let seeded = "SELECT RAND(42) AS r;\n";
    assert!(
        pg(seeded).units[0].status >= Status::Differs,
        "{}",
        pg(seeded).output
    );
}

#[test]
fn statements_building_dynamic_sql_are_not_converted_quietly() {
    let src = "CREATE PROCEDURE p @t sysname AS\nBEGIN\n  DECLARE @sql nvarchar(max) = N'SELECT TOP 1 * FROM ' + QUOTENAME(@t);\n  SET @sql += N' WITH (NOLOCK)';\n  DECLARE @n int = 1;\n  EXEC sp_executesql @sql;\nEND\n";
    let c = pg(src);
    for needle in ["N'SELECT TOP 1", "SET @sql +="] {
        let u = unit(&c, src, needle);
        assert_eq!(u.status, Status::Differs, "{needle}: {:?}", u.notes);
        assert!(has_rule(u, "dynamic-sql.text"));
    }
    assert!(!has_rule(unit(&c, src, "@n int = 1"), "dynamic-sql.text"));
}

fn source_catalog(files: &[&str]) -> sqldiff_core::catalog::Catalog {
    let mut cat = sqldiff_core::catalog::Catalog::default();
    for f in files {
        let root = sqldiff_syntax::parse(f, Dialect::Tsql).syntax();
        ddl::load_tree(&mut cat, &root, Dialect::Tsql);
    }
    cat
}

#[test]
fn exec_with_output_needs_the_callee_from_the_tree() {
    let callee = "CREATE PROCEDURE Sales.NextOrder @CustomerID int, @OrderID int OUTPUT AS\nBEGIN\n  SET @OrderID = 1;\nEND\n";
    let caller = "CREATE PROCEDURE p AS\nBEGIN\n  DECLARE @id int;\n  EXEC Sales.NextOrder @CustomerID = 5, @OrderID = @id OUTPUT;\nEND\n";
    // Alone, the parameters are unknown.
    assert_eq!(
        unit(&pg(caller), caller, "EXEC").status,
        Status::NotConverted
    );
    // With the tree's catalog it converts, and named arguments need no assumption.
    let cat = source_catalog(&[callee, caller]);
    for to in [Dialect::Postgres, Dialect::Db2i] {
        let d = rewrite_in(caller, Dialect::Tsql, to, Some(&cat));
        let c = sqldiff_convert::check(caller, d, &Options::default());
        let u = unit(&c, caller, "EXEC");
        assert!(
            c.output
                .contains("CALL Sales.NextOrder(p_customer_id => 5, p_order_id => v_id);"),
            "{}",
            c.output
        );
        assert!(!has_rule(u, "ASM-PARAM-NAMES"), "{:?}", u.notes);
        assert!(u.status < Status::NotConverted, "{:?}", u.notes);
    }
    // An OUTPUT parameter passed without OUTPUT: a CALL would assign it back.
    let silent = "CREATE PROCEDURE p AS\nBEGIN\n  DECLARE @id int = 3;\n  EXEC Sales.NextOrder 5, @id;\nEND\n";
    let d = rewrite_in(silent, Dialect::Tsql, Dialect::Postgres, Some(&cat));
    let c = sqldiff_convert::check(silent, d, &Options::default());
    assert_eq!(
        unit(&c, silent, "EXEC").status,
        Status::NotConverted,
        "{}",
        c.output
    );
    // A missing OUTPUT argument.
    let missing = "CREATE PROCEDURE p AS\nBEGIN\n  EXEC Sales.NextOrder 5;\nEND\n";
    let d = rewrite_in(missing, Dialect::Tsql, Dialect::Postgres, Some(&cat));
    let c = sqldiff_convert::check(missing, d, &Options::default());
    assert_eq!(unit(&c, missing, "EXEC").status, Status::NotConverted);
}

#[test]
fn the_target_catalog_binds_names_from_other_files() {
    // A table the conversion could not create does not exist in the output tree.
    let table = "CREATE TABLE s.t (a int, loc hierarchyid);\n";
    let query = "CREATE PROCEDURE s.p AS\nBEGIN\n  UPDATE s.t SET a = 1;\nEND\n";
    let out_table = sqldiff_convert::rewrite(table, Dialect::Tsql, Dialect::Postgres);
    let mut target = sqldiff_core::catalog::Catalog::default();
    let root = sqldiff_syntax::parse(out_table.output(), Dialect::Postgres).syntax();
    ddl::load_tree(&mut target, &root, Dialect::Postgres);
    let mut ok = target.clone();
    let good = sqldiff_syntax::parse(
        "CREATE TABLE s.t (a integer);\nCREATE TABLE s.other (b integer);\n",
        Dialect::Postgres,
    )
    .syntax();
    ddl::load_tree(&mut ok, &good, Dialect::Postgres);

    let opts = |c: sqldiff_core::catalog::Catalog| Options {
        facts_b: vec![sqldiff_core::Facts::from(std::sync::Arc::new(c))],
        ..Options::default()
    };
    // Only the other table exists: the UPDATE does not bind.
    let mut only_other = sqldiff_core::catalog::Catalog::default();
    let other =
        sqldiff_syntax::parse("CREATE TABLE s.other (b integer);\n", Dialect::Postgres).syntax();
    ddl::load_tree(&mut only_other, &other, Dialect::Postgres);
    let c =
        sqldiff_convert::convert_with(query, Dialect::Tsql, Dialect::Postgres, &opts(only_other))
            .unwrap();
    let u = unit(&c, query, "UPDATE");
    assert_eq!(u.status, Status::Differs, "{:?}", u.notes);
    assert!(has_rule(u, "bind.unknown-table"), "{:?}", u.notes);
    // With the table there it binds.
    let c =
        sqldiff_convert::convert_with(query, Dialect::Tsql, Dialect::Postgres, &opts(ok)).unwrap();
    let u = unit(&c, query, "UPDATE");
    assert!(!has_rule(u, "bind.unknown-table"), "{:?}", u.notes);
}

#[test]
fn parameters_may_say_as() {
    let src = "CREATE PROCEDURE p\n  @FirstName AS NVARCHAR(20) OUTPUT\n, @Day AS date\nAS\nBEGIN\n  SET @FirstName = N'x';\nEND\n";
    let c = pg(src);
    assert!(
        c.output
            .contains("p(INOUT p_first_name varchar(20), p_day date)"),
        "{}",
        c.output
    );
}

#[test]
fn a_call_after_a_statement_only_one_side_writes_is_placed_by_the_trace() {
    // `RETURN 0` makes the conversion add `return_code := 0;` at the start,
    // so the PostgreSQL CALL has one statement more before it.
    let callee = "CREATE PROCEDURE Sales.Touch AS\nBEGIN\n  SET NOCOUNT ON;\nEND\n";
    let caller = "CREATE PROCEDURE p AS\nBEGIN\n  EXEC Sales.Touch;\n  RETURN 0;\nEND\n";
    let cat = source_catalog(&[callee, caller]);
    let d = rewrite_in(caller, Dialect::Tsql, Dialect::Postgres, Some(&cat));
    let c = sqldiff_convert::check(caller, d, &Options::default());
    let u = unit(&c, caller, "EXEC");
    assert!(
        !u.notes.iter().any(|n| n.text.contains("same place")),
        "{:?}\n{}",
        u.notes,
        c.output
    );
    assert!(
        u.status < Status::NotConverted,
        "{:?}\n{}",
        u.notes,
        c.output
    );
}

#[test]
fn exec_with_output_in_a_script_becomes_a_call_in_a_do_block() {
    let callee = "CREATE PROCEDURE Sales.NextOrder @CustomerID int, @OrderID int OUTPUT AS\nBEGIN\n  SET @OrderID = 1;\nEND\n";
    let callee_pg = "CREATE SCHEMA sales;\nCREATE PROCEDURE Sales.NextOrder(p_customer_id integer, INOUT p_order_id integer)\nLANGUAGE plpgsql AS $$ BEGIN p_order_id := 1; END $$;\n";
    // The PRINT batch becomes a block of its own on one side only, so the
    // script is compared as a whole rather than block by block.
    let script = "PRINT N'Adding';\nGO\nDECLARE @id int;\nEXEC Sales.NextOrder @CustomerID = 5, @OrderID = @id OUTPUT;\nGO\n";
    let cat = source_catalog(&[callee, script]);
    let d = rewrite_in(script, Dialect::Tsql, Dialect::Postgres, Some(&cat));
    let o = Options {
        catalog_a: vec![sqldiff_core::Artifact::new(
            "cat.sql",
            Dialect::Tsql,
            callee,
        )],
        catalog_b: vec![sqldiff_core::Artifact::new(
            "cat.pg.sql",
            Dialect::Postgres,
            callee_pg,
        )],
        ..Options::default()
    };
    let c = sqldiff_convert::check(script, d, &o);
    assert!(c.output.contains("DO $$"), "{}", c.output);
    let u = unit(&c, script, "EXEC");
    // Inside a DO block PL/pgSQL assigns the INOUT value back to the
    // variable, as T-SQL does with OUTPUT.
    assert!(
        !u.notes.iter().any(|n| n.text.contains("outside PL/pgSQL")),
        "{:?}\n{}",
        u.notes,
        c.output
    );
    assert!(
        u.status <= Status::Assumptions,
        "{:?}\n{}",
        u.notes,
        c.output
    );
}

#[test]
fn alias_types_become_domains() {
    let src = "CREATE TYPE [AccountNumber] FROM nvarchar(15) NULL;\nGO\nCREATE TYPE [Flag] FROM bit NOT NULL;\nGO\nCREATE TYPE [Name] FROM nvarchar(50) NULL;\nGO\nCREATE TYPE [Sales].[Money Amt] FROM decimal(19, 4) NOT NULL;\nGO\nCREATE TABLE [dbo].[Thing](\n    [Name] [Name] NOT NULL,\n    [Alt] [Name] NULL,\n    [Active] [Flag] NOT NULL,\n    [Seen] [dbo].[Flag],\n    [Maybe] [Flag] NULL,\n    [Dflt] [Flag] DEFAULT NULL,\n    [Acct] [AccountNumber],\n    [Amt] [Sales].[Money Amt] DEFAULT 0\n);\nGO\n";
    let c = pg(src);
    let o = &c.output;
    for want in [
        "CREATE DOMAIN AccountNumber AS varchar(15);",
        "CREATE DOMAIN Flag AS boolean;",
        // A domain named like a built-in type is never found unquoted.
        "CREATE DOMAIN \"Name\" AS varchar(50);",
        "CREATE DOMAIN sales.\"Money Amt\" AS numeric(19, 4);",
        "Name \"Name\" NOT NULL,",
        "Alt \"Name\" NULL,",
        "Maybe Flag NULL,",
        // NOT NULL comes from the alias type only where the column says nothing.
        "Seen Flag NOT NULL,",
        "Dflt Flag DEFAULT NULL NOT NULL,",
        "Acct AccountNumber,",
        "Amt sales.\"Money Amt\" DEFAULT 0 NOT NULL",
    ] {
        assert!(o.contains(want), "missing {want}\n{o}");
    }
    // The nullability stays off the domain: a column can still say NULL.
    assert!(!o.contains("DOMAIN Flag AS boolean NOT NULL"), "{o}");
    // A name PostgreSQL's own type would win over cannot be a domain.
    let lower = pg("CREATE TYPE [name] FROM nvarchar(50);\nGO\n");
    assert_eq!(lower.units[0].status, Status::NotConverted);
    // Db2 for i distinct types are not alias types.
    assert_eq!(
        db2("CREATE TYPE [Flag] FROM bit NOT NULL;\nGO\n").units[0].status,
        Status::NotConverted
    );
    // A table type is not an alias type.
    let t = pg("CREATE TYPE [dbo].[Ids] AS TABLE (Id int);\nGO\n");
    assert_eq!(t.units[0].status, Status::NotConverted);
}

/// An alias type is its base type wherever it is used: a bit alias takes the
/// bit literal rewrites, the base type's notes appear at the use, and a name
/// that PostgreSQL reads in another way is not left unquoted.
#[test]
fn alias_types_behave_as_their_base_type() {
    let src = "CREATE TYPE [Flag] FROM bit NOT NULL;\nGO\nCREATE TYPE [Small] FROM tinyint NULL;\nGO\nCREATE TYPE [Serial] FROM int NOT NULL;\nGO\nCREATE TYPE [Date] FROM varchar(10) NULL;\nGO\nCREATE TABLE dbo.T(Active Flag DEFAULT 1, S Small, N Serial);\nGO\nCREATE TABLE dbo.T2(D date, E dbo.Date);\nGO\nCREATE VIEW dbo.V AS SELECT Active FROM dbo.T WHERE Active = 1;\nGO\nCREATE PROCEDURE dbo.P @s Small, @f Flag = 1\nAS\nBEGIN\n    SELECT @s;\n    IF @f = 1 SELECT 1;\nEND;\nGO\n";
    let c = pg(src);
    let o = &c.output;
    for want in [
        // The column default and the view's comparison are boolean.
        "Active Flag DEFAULT TRUE NOT NULL",
        "WHERE Active = TRUE",
        // `serial` would be an auto-increment column; `Serial` is quoted.
        "CREATE DOMAIN \"Serial\" AS integer;",
        "N \"Serial\" NOT NULL",
        // A plain `date` column is the built-in type, `dbo.Date` the alias.
        "D date",
        "E \"Date\"",
    ] {
        assert!(o.contains(want), "missing {want}\n{o}");
    }
    // The procedure's header carries the notes of the base types.
    let u = unit(&c, src, "CREATE PROCEDURE dbo.P");
    let notes: String = u.notes.iter().map(|n| format!("{} ", n.text)).collect();
    assert!(notes.to_ascii_lowercase().contains("tinyint"), "{notes}");
}

/// A column of an integer alias type takes the integer truncation: T-SQL
/// drops the fraction, PostgreSQL would round into the domain.
#[test]
fn integer_alias_columns_are_truncated() {
    let src = "CREATE TYPE [Qty] FROM int NOT NULL;\nGO\nCREATE TABLE dbo.S(Id int NOT NULL PRIMARY KEY, Q Qty);\nGO\nCREATE TABLE dbo.S2(Id int NOT NULL PRIMARY KEY, Q2 decimal(9,2) NOT NULL);\nGO\nCREATE PROCEDURE dbo.Up\nAS\nBEGIN\n    UPDATE dbo.S SET Q = Q * 1.5 WHERE Id = 1;\n    UPDATE s SET Q = s2.Q2 FROM dbo.S s JOIN dbo.S2 s2 ON s2.Id = s.Id;\nEND;\nGO\n";
    let o = pg(src).output;
    for want in [
        "SET Q = trunc(Q * 1.5) WHERE Id = 1;",
        "SET Q = trunc(s2.Q2) FROM S2 s2",
    ] {
        assert!(o.contains(want), "missing {want}\n{o}");
    }
}

#[test]
fn bulk_insert_is_refused_without_a_leftover_statement() {
    let src = "PRINT 'Loading';\nBULK INSERT [Sales].[Orders] FROM 'C:\\load\\o.csv'\nWITH (FIELDTERMINATOR = '+|', ROWTERMINATOR = '&|\\n', CODEPAGE = 'ACP', CHECK_CONSTRAINTS, TABLOCK)\nGO\n";
    for c in [pg(src), db2(src)] {
        assert_eq!(c.units.len(), 2, "{}", c.output);
        assert_eq!(c.units[1].status, Status::NotConverted);
        assert!(
            c.units[1]
                .notes
                .iter()
                .any(|n| n.text.starts_with("BULK INSERT")),
            "{:?}",
            c.units[1].notes
        );
        let live: Vec<&str> = c
            .output
            .lines()
            .filter(|l| !l.trim_start().starts_with("--"))
            .collect();
        assert!(
            !live
                .iter()
                .any(|l| l.to_ascii_uppercase().contains("INSERT")),
            "{live:?}"
        );
    }
}

#[test]
fn bulk_insert_as_an_if_or_while_body_is_never_live_sql() {
    for src in [
        "IF 1 = 1 BULK INSERT t FROM 'f'\nGO\n",
        "IF 1 = 1 BULK INSERT t FROM 'f' ELSE PRINT 'x'\nGO\n",
        "WHILE 1 = 1 BULK INSERT t FROM 'f'\nGO\n",
    ] {
        for c in [pg(src), db2(src)] {
            let live: Vec<&str> = c
                .output
                .lines()
                .filter(|l| !l.trim_start().starts_with("--"))
                .collect();
            assert!(
                !live
                    .iter()
                    .any(|l| l.to_ascii_uppercase().contains("INSERT")),
                "{src}: {live:?}"
            );
            assert!(
                c.units.iter().any(|u| u.status == Status::NotConverted),
                "{src}: {:?}",
                c.units.iter().map(|u| u.status).collect::<Vec<_>>()
            );
        }
    }
}

/// T-SQL drops the fraction of a number stored in an integer (eval/casts);
/// PostgreSQL rounds it, so the conversion truncates where the value has one.
#[test]
fn fractions_stored_in_integers_are_truncated() {
    let src = "CREATE TABLE dbo.Cities(CityID int NOT NULL, Pop bigint NULL, Cnt int NOT NULL, Price money NULL);\nGO\nCREATE PROCEDURE dbo.Grow @f decimal(5,2), @i int\nAS\nBEGIN\n    DECLARE @n int = @f * 2;\n    DECLARE @b bigint = 5;\n    UPDATE dbo.Cities SET Pop = Pop * 1.04, Cnt = @i * 2, Price = Pop * 1.5 WHERE CityID = 1;\n    SET @n = @f * 3;\n    SET @n += @f;\n    SET @b = @b * 2;\n    SET @b = (@b + 1) / 2;\n    SELECT @n = Pop * 0.5 FROM dbo.Cities WHERE CityID = 1;\n    INSERT INTO dbo.Cities(CityID, Pop, Cnt) VALUES (2, @f * 100, @i * 2), (3, 7, @i * 1.5);\n    UPDATE dbo.Cities SET Cnt = CAST(Pop * 1.04 AS int) WHERE CityID = 1;\nEND;\nGO\n";
    let o = pg(src).output;
    for want in [
        "v_n integer := trunc(p_f * 2);",
        "UPDATE Cities SET Pop = trunc(Pop * 1.04), Cnt = p_i * 2, Price = Pop * 1.5 WHERE CityID = 1;",
        "v_n := trunc(p_f * 3);",
        "v_n := trunc(v_n + p_f);",
        "v_b := v_b * 2;",
        "v_b := (v_b + 1) / 2;",
        "SELECT trunc(Pop * 0.5) AS sqldiff_c1",
        "VALUES (2, trunc(p_f * 100), p_i * 2), (3, 7, trunc(p_i * 1.5));",
        "SET Cnt = CAST(trunc(Pop * 1.04) AS integer)",
    ] {
        // DECLARE initialisers are hoisted; only check the ones present.
        if want.starts_with("v_n integer") {
            assert!(o.contains("v_n := trunc(p_f * 2);"), "{o}");
            continue;
        }
        assert!(o.contains(want), "missing {want}\n{o}");
    }
    // Db2 for i is left as it was: no checked rule for its assignment.
    let d =
        db2("CREATE TABLE T(Pop bigint NULL);\nGO\nUPDATE T SET Pop = Pop * 1.04;\nGO\n").output;
    assert!(!d.to_ascii_lowercase().contains("trunc"), "{d}");
}

/// SQL Server takes one trailing comma in a CREATE TABLE column list; the
/// conversion has none, and the engine reads both sides alike.
#[test]
fn a_trailing_comma_in_the_column_list_is_dropped() {
    let src = "CREATE TABLE [dbo].[A](\n    [ID] int NOT NULL,\n    [Name] nvarchar(50) NOT NULL, -- last\n);\nGO\nCREATE TABLE [dbo].[B](\n    [ID] int NOT NULL,\n    CONSTRAINT [PK_B] PRIMARY KEY CLUSTERED ([ID]),\n) ON [PRIMARY];\nGO\n";
    for c in [pg(src), db2(src)] {
        assert_eq!(c.units.len(), 2);
        for u in &c.units {
            assert_ne!(u.status, Status::NotConverted, "{:?}", u.notes);
        }
        assert!(!c.output.contains(",\n)"), "{}", c.output);
        assert!(!c.output.contains(", )"), "{}", c.output);
    }
    // Two commas are still an empty item.
    assert_eq!(
        pg("CREATE TABLE [dbo].[C](\n    [ID] int NOT NULL,,\n);\nGO\n").units[0].status,
        Status::NotConverted
    );
}

/// INSERT … SELECT and UPDATE … FROM store values in integer columns the same
/// way as INSERT … VALUES and plain UPDATE: the fraction is dropped.
#[test]
fn fractions_stored_by_insert_select_and_update_from_are_truncated() {
    let src = "CREATE TABLE dbo.Src(ID int NOT NULL PRIMARY KEY, Amt decimal(9,2) NOT NULL, Qty int NOT NULL);\nCREATE TABLE dbo.Dst(ID int NOT NULL, Cnt int NOT NULL, Big bigint NULL, Price decimal(9,2) NULL);\nGO\nCREATE PROCEDURE dbo.Fill @f decimal(5,2)\nAS\nBEGIN\n    INSERT INTO dbo.Dst(ID, Cnt, Big, Price) SELECT s.ID, s.Amt * 1.5, s.Qty * 2, s.Amt * 1.5 FROM dbo.Src s WHERE s.ID > 0;\n    INSERT INTO dbo.Dst(ID, Cnt) SELECT ID, Amt * @f AS c FROM dbo.Src;\n    UPDATE d SET d.Cnt = s.Amt * 2, d.Price = s.Amt * 2 FROM dbo.Dst d JOIN dbo.Src s ON s.ID = d.ID WHERE s.Qty > 0;\n    UPDATE dbo.Dst SET Cnt = s.Amt / 3 FROM dbo.Dst JOIN dbo.Src s ON s.ID = Dst.ID;\nEND;\nGO\n";
    let o = pg(src).output;
    for want in [
        // Cnt takes a fraction; Big an integer product; Price is a decimal.
        "SELECT s.ID, trunc(s.Amt * 1.5), s.Qty * 2, s.Amt * 1.5 FROM Src s",
        "SELECT ID, trunc(Amt * p_f) AS c FROM Src",
        "SET Cnt = trunc(s.Amt * 2), Price = s.Amt * 2",
        "SET Cnt = trunc(s.Amt / 3)",
    ] {
        assert!(o.contains(want), "missing {want}\n{o}");
    }
}

/// The rewrite must not change what the SELECT or UPDATE does with the
/// unconverted value, nor guess a type it does not know.
#[test]
fn insert_select_keeps_shapes_it_cannot_truncate_safely() {
    let src = "CREATE TABLE dbo.Src(ID int NOT NULL, Amt decimal(9,2) NOT NULL);\nCREATE TABLE dbo.Dst(ID int NOT NULL, Cnt int NOT NULL);\nGO\nCREATE PROCEDURE dbo.Fill\nAS\nBEGIN\n    INSERT INTO dbo.Dst(ID, Cnt) SELECT DISTINCT ID, Amt * 1.5 FROM dbo.Src;\n    INSERT INTO dbo.Dst(ID, Cnt) SELECT ID, Amt * 1.5 FROM dbo.Src ORDER BY Amt * 1.5;\n    INSERT INTO dbo.Dst(ID, Cnt) SELECT ID, Amt * 1.5 FROM dbo.Src UNION ALL SELECT ID, 1 FROM dbo.Src;\n    INSERT INTO dbo.Dst(ID, Cnt) SELECT ID, x.Amt * 1.5 FROM dbo.Nowhere x;\nEND;\nGO\n";
    let o = pg(src).output;
    assert!(!o.to_ascii_lowercase().contains("trunc"), "{o}");
}

/// A fraction passed to an integer parameter of a known procedure, or
/// returned by an integer function, is truncated as T-SQL does.
#[test]
fn fractions_passed_to_int_parameters_and_returned_by_functions_are_truncated() {
    let callee = "CREATE PROCEDURE dbo.Take @n int, @label varchar(10), @d decimal(9,2)\nAS\nBEGIN\n    SET NOCOUNT ON;\nEND;\nGO\n";
    let caller = "CREATE PROCEDURE dbo.Give @f decimal(5,2), @i int\nAS\nBEGIN\n    EXEC dbo.Take @f * 2, 'a', @f * 2;\n    EXEC dbo.Take @n = @f / 3, @label = 'b', @d = @f / 3;\n    EXEC dbo.Take @i * 2, 'c', @i;\nEND;\nGO\n";
    let func = "CREATE FUNCTION dbo.Half(@f decimal(9,2)) RETURNS int\nAS\nBEGIN\n    RETURN @f / 2;\nEND;\nGO\nCREATE FUNCTION dbo.Whole(@i int) RETURNS int\nAS\nBEGIN\n    RETURN @i * 2;\nEND;\nGO\nCREATE FUNCTION dbo.Exact(@f decimal(9,2)) RETURNS decimal(9,2)\nAS\nBEGIN\n    RETURN @f / 2;\nEND;\nGO\n";
    let all = format!("{callee}{caller}{func}");
    let cat = source_catalog(&[&all]);
    let d = rewrite_in(&all, Dialect::Tsql, Dialect::Postgres, Some(&cat));
    let o = sqldiff_convert::check(&all, d, &Options::default()).output;
    for want in [
        "CALL Take(trunc(p_f * 2), 'a', p_f * 2);",
        "CALL Take(p_n => trunc(p_f / 3), p_label => 'b', p_d => p_f / 3);",
        "CALL Take(p_i * 2, 'c', p_i);",
        "RETURN trunc(p_f / 2);",
        "RETURN p_i * 2;",
    ] {
        assert!(o.contains(want), "missing {want}\n{o}");
    }
    // The decimal function keeps its value.
    assert_eq!(o.matches("trunc(").count(), 3, "{o}");
}

/// The engine compares the truncating forms the converter now writes for
/// INSERT … SELECT and UPDATE … FROM (a statement alone in its routine, so
/// the transaction findings of two writes do not mix in).
#[test]
fn engine_accepts_truncation_in_insert_select_and_update_from() {
    let tables = "CREATE TABLE dbo.Src(ID int NOT NULL PRIMARY KEY, Amt decimal(9,2) NOT NULL);\nCREATE TABLE dbo.Dst(ID int NOT NULL, Cnt int NOT NULL);\nGO\n";
    for body in [
        "INSERT INTO dbo.Dst(ID, Cnt) SELECT s.ID, s.Amt * 1.5 FROM dbo.Src s;",
        "UPDATE d SET d.Cnt = s.Amt * 2 FROM dbo.Dst d JOIN dbo.Src s ON s.ID = d.ID;",
    ] {
        let src = format!("{tables}CREATE PROCEDURE dbo.P\nAS\nBEGIN\n    {body}\nEND;\nGO\n");
        let c = pg(&src);
        let u = unit(&c, &src, "CREATE PROCEDURE");
        assert!(
            u.status <= Status::Assumptions,
            "{body}\n{:?}\n{}",
            u.notes,
            c.output
        );
        assert!(c.output.contains("trunc("), "{body}\n{}", c.output);
    }
}
