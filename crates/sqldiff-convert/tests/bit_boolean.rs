//! T-SQL BIT to PostgreSQL boolean and Db2 for i SMALLINT. PostgreSQL has no
//! comparison, arithmetic or assignment between boolean and the numbers, so
//! every place a bit meets a number is rewritten exactly (1/0 to TRUE/FALSE, a
//! number stored in a bit to `(x <> 0)`, ~ to NOT, ...) or refused. Each
//! rewrite was run on SQL Server 2022 and PostgreSQL 18 with the same results;
//! see eval/bit-boolean/.

use sqldiff_convert::{check, rewrite_in, Conversion, Status, Unit};
use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{Artifact, Dialect, Options};

const TABLE: &str = "CREATE TABLE dbo.Flags (
    Id int NOT NULL PRIMARY KEY,
    IsActive bit NOT NULL DEFAULT 1,
    IsDeleted bit NULL CONSTRAINT DF_Del DEFAULT ((0)),
    Score int NULL
);
";

fn source_catalog(files: &[&str]) -> Catalog {
    let mut cat = Catalog::default();
    for f in files {
        let root = sqldiff_syntax::parse(f, Dialect::Tsql).syntax();
        ddl::load_tree(&mut cat, &root, Dialect::Tsql);
    }
    cat
}

/// `src` converted with the Flags table (and `more`) in the source catalog,
/// and checked with both catalogs, as a tree conversion does.
fn conv_with(src: &str, to: Dialect, more: &[&str]) -> Conversion {
    let mut files = vec![TABLE, src];
    files.extend_from_slice(more);
    let cat = source_catalog(&files);
    let target = rewrite_in(TABLE, Dialect::Tsql, to, None)
        .output()
        .to_string();
    let opts = Options {
        catalog_a: vec![Artifact::new("t.sql", Dialect::Tsql, TABLE)],
        catalog_b: vec![Artifact::new("t.sql", to, &target)],
        ..Options::default()
    };
    check(src, rewrite_in(src, Dialect::Tsql, to, Some(&cat)), &opts)
}

fn pg(src: &str) -> Conversion {
    conv_with(src, Dialect::Postgres, &[])
}

fn db2(src: &str) -> Conversion {
    conv_with(src, Dialect::Db2i, &[])
}

/// The smallest unit whose input contains `needle`.
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

fn has(c: &Conversion, text: &str) {
    assert!(c.output.contains(text), "{text}\n{}", c.output);
}

#[test]
fn comparisons_with_one_and_zero_become_true_and_false() {
    let src = "CREATE VIEW dbo.V AS SELECT Id FROM dbo.Flags WHERE IsActive = 1 AND IsDeleted <> 0 OR IsActive IN (0, 1) OR IsDeleted = 'true';\n";
    let c = pg(src);
    has(
        &c,
        "WHERE IsActive = TRUE AND IsDeleted <> FALSE OR IsActive IN (FALSE, TRUE) OR IsDeleted = 'true'",
    );
    assert!(unit(&c, src, "VIEW").status < Status::Unproven, "{c:?}");
}

#[test]
fn a_bit_compared_with_another_number_is_not_kept() {
    // Kept as written, PostgreSQL rejects `boolean = integer`; nothing the
    // converter writes may claim it is equal.
    let src = "CREATE VIEW dbo.V AS SELECT Id FROM dbo.Flags WHERE IsActive = Score;\n";
    let u_status = unit(&pg(src), src, "VIEW").status;
    assert!(u_status >= Status::Unproven, "{u_status:?}");
}

#[test]
fn a_condition_without_an_exact_rewrite_is_refused() {
    let src = "CREATE PROCEDURE dbo.Q @Id int, @Flag bit = 1, @N int = 0\nAS\nBEGIN\n    IF @Flag = 1\n        UPDATE dbo.Flags SET IsDeleted = @Flag WHERE Id = @Id;\n    IF @Flag = 2\n        UPDATE dbo.Flags SET IsDeleted = 0 WHERE Id = @Id;\n    IF @Flag = @N\n        UPDATE dbo.Flags SET IsDeleted = 0 WHERE Id = @Id;\nEND\n";
    let c = pg(src);
    has(&c, "p_flag boolean DEFAULT TRUE");
    has(&c, "IF p_flag = TRUE THEN");
    for needle in ["@Flag = 2", "@Flag = @N"] {
        let u = unit(&c, src, needle);
        assert_eq!(u.status, Status::NotConverted, "{needle}\n{}", c.output);
        assert!(
            u.notes
                .iter()
                .any(|n| n.text.starts_with("a bit meets a number")),
            "{:?}",
            u.notes
        );
    }
}

#[test]
fn numbers_stored_in_a_bit() {
    let src = "CREATE PROCEDURE dbo.Q @Id int, @N int\nAS\nBEGIN\n    DECLARE @b bit = 2, @done bit;\n    SET @done = @N;\n    UPDATE dbo.Flags SET IsDeleted = @N, IsActive = 5, Score = 3 WHERE Id = @Id;\n    INSERT INTO dbo.Flags (Id, IsActive, IsDeleted, Score) VALUES (@Id + 1, 1, @N, 2), (@Id + 2, 0, NULL, 3);\nEND\n";
    let c = pg(src);
    has(&c, "v_b := TRUE;");
    has(&c, "v_done := (p_n <> 0);");
    has(&c, "SET IsDeleted = (p_n <> 0), IsActive = TRUE, Score = 3");
    has(
        &c,
        "VALUES (p_id + 1, TRUE, (p_n <> 0), 2), (p_id + 2, FALSE, NULL, 3)",
    );
    // Db2 for i: a literal is written as 0 or 1, as T-SQL stores it.
    let c = db2(src);
    has(&c, "SET v_b = 1;");
    has(&c, "IsActive = 1, Score = 3");
}

#[test]
fn select_assignment_and_return_in_a_bit_function() {
    let src = "CREATE FUNCTION dbo.F (@x int) RETURNS bit AS\nBEGIN\n    DECLARE @r bit;\n    SELECT @r = 1 FROM dbo.Flags WHERE Id = @x AND IsDeleted = 1;\n    IF @r IS NULL RETURN 0;\n    RETURN @x;\nEND\n";
    let c = pg(src);
    has(&c, "TRUE");
    has(&c, "IsDeleted = TRUE");
    has(&c, "RETURN FALSE;");
    has(&c, "RETURN (p_x <> 0);");
}

#[test]
fn exec_arguments_follow_the_callee_parameter() {
    let callee = "CREATE PROCEDURE dbo.Q2 @Id int, @B bit\nAS\nBEGIN\n    UPDATE dbo.Flags SET IsDeleted = @B WHERE Id = @Id;\nEND\n";
    let caller = "CREATE PROCEDURE dbo.Q1 @Id int\nAS\nBEGIN\n    EXEC dbo.Q2 @Id, 0;\n    EXEC dbo.Q2 @Id = @Id, @B = 7;\nEND\n";
    let c = conv_with(caller, Dialect::Postgres, &[callee]);
    has(&c, "CALL Q2(p_id, FALSE);");
    has(&c, "CALL Q2(p_id => p_id, p_b => TRUE);");
}

#[test]
fn bitwise_operators() {
    let src = "CREATE VIEW dbo.W AS SELECT Id, IsActive & IsDeleted AS a, IsActive | IsDeleted AS o, IsActive ^ IsDeleted AS x, IsActive ^ 1 AS x1, IsActive & 3 AS a3, ~IsActive AS n FROM dbo.Flags;\n";
    let c = pg(src);
    has(&c, "(IsActive::integer & IsDeleted::integer)::boolean AS a");
    has(&c, "(IsActive::integer | IsDeleted::integer)::boolean AS o");
    has(&c, "(IsActive <> IsDeleted) AS x");
    // ^ with a number is XOR, which PostgreSQL spells # (its ^ is a power).
    has(&c, "IsActive::integer # 1 AS x1");
    has(&c, "IsActive::integer & 3 AS a3");
    has(&c, "(NOT IsActive) AS n");
    let c = db2(src);
    has(&c, "(1 - IsActive) AS n");
}

#[test]
fn casts_to_and_from_bit() {
    let src = "CREATE VIEW dbo.V AS SELECT Id, CAST(Score AS bit) AS sb, CAST(1 AS bit) AS one, CAST(IsActive AS varchar(5)) AS s, CONVERT(int, IsDeleted) AS d FROM dbo.Flags;\n";
    let c = pg(src);
    has(&c, "(Score <> 0) AS sb");
    has(&c, "TRUE AS one");
    // boolean as text is 'true'; T-SQL writes the bit as '1'.
    has(&c, "CAST(IsActive::integer AS varchar(5)) AS s");
}

#[test]
fn isnull_replacement_is_stored_as_a_bit() {
    let src = "CREATE VIEW dbo.V AS SELECT Id, ISNULL(IsDeleted, 5) AS e, COALESCE(IsDeleted, IsActive) AS f FROM dbo.Flags;\n";
    let c = pg(src);
    has(&c, "COALESCE(IsDeleted, TRUE) AS e");
    let c = db2(src);
    has(&c, "COALESCE(IsDeleted, 1) AS e");
}

#[test]
fn coalesce_and_case_mixing_a_bit_with_numbers_are_not_equal() {
    // COALESCE(b, 5) is 5 when b is NULL (unlike ISNULL), and a CASE mixing
    // bit and int is an int: neither has a boolean form that keeps the value.
    for src in [
        "CREATE VIEW dbo.V AS SELECT Id, COALESCE(IsDeleted, 5) AS f FROM dbo.Flags;\n",
        "CREATE VIEW dbo.V AS SELECT Id, CASE WHEN Score > 1 THEN IsActive ELSE 2 END AS g FROM dbo.Flags;\n",
        "CREATE VIEW dbo.V AS SELECT Id, IIF(Score > 1, IsActive, Score) AS g FROM dbo.Flags;\n",
    ] {
        let c = pg(src);
        let u = unit(&c, src, "VIEW");
        assert!(u.status >= Status::Unproven, "{src}\n{:?}\n{}", u.notes, c.output);
    }
}

#[test]
fn table_defaults_and_checks() {
    let src = "CREATE TABLE dbo.T3 (\n    Id int NOT NULL,\n    A bit NOT NULL CONSTRAINT DF_A DEFAULT (2),\n    B bit NULL CHECK (B = 1 OR B = 0),\n    C bit NULL DEFAULT 0\n);\n";
    let c = pg(src);
    has(&c, "A boolean NOT NULL CONSTRAINT DF_A DEFAULT TRUE");
    has(&c, "CHECK (B = TRUE OR B = FALSE)");
    has(&c, "C boolean NULL DEFAULT FALSE");
    // The engine does not compare CHECK constraints: no exact rewrite, no conversion.
    let bad = "CREATE TABLE dbo.T4 (Id int NOT NULL, B bit NULL CHECK (B = 2));\n";
    let c = pg(bad);
    assert_eq!(c.units[0].status, Status::NotConverted, "{}", c.output);
    assert!(
        !c.output.lines().any(|l| l.starts_with("CREATE")),
        "{}",
        c.output
    );
}

#[test]
fn filtered_index_and_added_check_use_the_table_columns() {
    let src = "CREATE INDEX IX_Act ON dbo.Flags (Id) WHERE IsActive = 1;\nALTER TABLE dbo.Flags ADD CONSTRAINT CK_Del CHECK (IsDeleted = 0 OR IsActive = 1);\n";
    let c = pg(src);
    has(&c, "WHERE IsActive = TRUE;");
    has(&c, "CHECK (IsDeleted = FALSE OR IsActive = TRUE);");
    let bad = "ALTER TABLE dbo.Flags ADD CONSTRAINT CK_S CHECK (IsActive = Score);\n";
    let c = pg(bad);
    assert_eq!(c.units[0].status, Status::NotConverted, "{}", c.output);
}

#[test]
fn db2_reads_boolean_strings_as_one_and_zero() {
    let src = "CREATE VIEW dbo.V AS SELECT Id FROM dbo.Flags WHERE IsActive = 'true' OR IsDeleted = 'FALSE';\n";
    let c = db2(src);
    has(&c, "WHERE IsActive = 1 OR IsDeleted = 0");
    assert!(unit(&c, src, "VIEW").status < Status::Unproven, "{c:?}");
}

/// One text holding the table and the view, converted without catalogs (as
/// the MCP tool and the GUI convert one text): what the rules cannot rewrite
/// exactly is still not equal.
#[test]
fn without_catalogs_a_bit_meeting_a_number_is_not_equal() {
    for view in [
        "CREATE VIEW dbo.V AS SELECT Id FROM dbo.Flags WHERE IsActive = Score;\n",
        "CREATE VIEW dbo.V AS SELECT Id, COALESCE(IsDeleted, 5) AS f FROM dbo.Flags;\n",
        "CREATE VIEW dbo.V AS SELECT Id, CASE WHEN Score > 1 THEN IsActive ELSE 2 END AS g FROM dbo.Flags;\n",
        "CREATE VIEW dbo.V AS SELECT Id, IIF(Score > 1, Score, IsActive) AS g FROM dbo.Flags;\n",
    ] {
        let src = format!("{TABLE}GO\n{view}");
        let c = sqldiff_convert::convert(&src, Dialect::Tsql, Dialect::Postgres).unwrap();
        let u = unit(&c, &src, "VIEW");
        assert!(u.status >= Status::Unproven, "{view}\n{:?}\n{}", u.notes, c.output);
    }
}

#[test]
fn case_and_iif_results_of_zero_and_one_next_to_a_bit() {
    let src = "CREATE VIEW dbo.V AS SELECT Id, CASE WHEN Score > 1 THEN IsActive ELSE 0 END AS g, IIF(Score > 2, 1, IsDeleted) AS h, CASE WHEN Score > 1 THEN 1 ELSE 0 END AS k FROM dbo.Flags;\n";
    let c = pg(src);
    has(&c, "CASE WHEN Score > 1 THEN IsActive ELSE FALSE END AS g");
    has(
        &c,
        "(CASE WHEN Score > 2 THEN TRUE ELSE IsDeleted END) AS h",
    );
    // No bit among the results: numbers stay numbers.
    has(&c, "CASE WHEN Score > 1 THEN 1 ELSE 0 END AS k");
    assert!(unit(&c, src, "VIEW").status < Status::Unproven, "{c:?}");
}

/// A CASE stored in a bit stores the branch taken (WideWorldImporters'
/// RecordDeliveryVanTemperatures sets a bit from CASE … THEN 1 ELSE 0 END).
#[test]
fn a_case_stored_in_a_bit() {
    let src = "CREATE PROCEDURE dbo.Q @Id int, @N int\nAS\nBEGIN\n    SET XACT_ABORT ON;\n    DECLARE @done bit;\n    SET @done = CASE WHEN @N > 1 THEN 1 ELSE 0 END;\n    UPDATE dbo.Flags SET IsActive = CASE WHEN Score > 2 THEN 2 ELSE Score END WHERE Id = @Id;\nEND\n";
    let c = pg(src);
    has(&c, "v_done := CASE WHEN p_n > 1 THEN TRUE ELSE FALSE END;");
    has(
        &c,
        "SET IsActive = CASE WHEN Score > 2 THEN TRUE ELSE (Score <> 0) END",
    );
    for needle in ["SET @done", "UPDATE"] {
        let u = unit(&c, src, needle);
        assert!(
            u.status < Status::Differs,
            "{needle}: {:?} {:?}\n{}",
            u.status,
            c.report.findings,
            c.output
        );
    }
    // A string result is converted to the CASE's number type first.
    let bad = "CREATE PROCEDURE dbo.Q @Id int\nAS\nBEGIN\n    UPDATE dbo.Flags SET IsActive = CASE WHEN Score > 2 THEN 'true' ELSE 0 END WHERE Id = @Id;\nEND\n";
    let c = pg(bad);
    assert_eq!(
        unit(&c, bad, "UPDATE").status,
        Status::NotConverted,
        "{}",
        c.output
    );
}

/// The idiom rules (MERGE, INSERT + SCOPE_IDENTITY) know nothing of bits: a
/// statement where a bit meets a number is left to the bit rules.
#[test]
fn idiom_rules_leave_bits_to_the_bit_rules() {
    let src = "CREATE PROCEDURE dbo.Q @Id int, @NewId int OUTPUT\nAS\nBEGIN\n    SET XACT_ABORT ON;\n    INSERT INTO dbo.Flags (Id, IsActive) VALUES (@Id, 1);\n    SET @NewId = SCOPE_IDENTITY();\nEND\n";
    let c = pg(src);
    has(&c, "VALUES (p_id, TRUE)");
    let merge = "CREATE PROCEDURE dbo.Q @Id int\nAS\nBEGIN\n    MERGE dbo.Flags AS t USING (SELECT @Id AS Id) AS s ON t.Id = s.Id\n    WHEN MATCHED THEN UPDATE SET IsActive = 2\n    WHEN NOT MATCHED THEN INSERT (Id, IsActive) VALUES (s.Id, 1);\nEND\n";
    let c = pg(merge);
    let u = unit(&c, merge, "MERGE");
    assert!(
        u.status == Status::NotConverted || !c.output.contains("IsActive = 2"),
        "{:?}\n{}",
        u.notes,
        c.output
    );
}
