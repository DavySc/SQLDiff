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
            assert!(has_rule(u, "not-converted"), "{:?}", u.notes);
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
        facts_b: vec![std::sync::Arc::new(c)],
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
