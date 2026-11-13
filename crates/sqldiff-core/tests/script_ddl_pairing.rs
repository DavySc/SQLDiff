//! Script DDL pairing: several constraints added by one ALTER TABLE, sysname
//! columns, and tables that sit beside statements the other dialect cannot
//! take (a CREATE TABLE … AS SELECT, a CREATE EXTERNAL TABLE).

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

fn run(a: &str, b: &str) -> Report {
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Postgres, b),
        &Options::default(),
    )
}

fn equal(a: &str, b: &str) {
    let r = run(a, b);
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:#?}\n{:#?}",
        r.findings,
        r.unknown_reasons
    );
    assert!(r.unknown_reasons.is_empty(), "{:?}", r.unknown_reasons);
}

fn not_equal(a: &str, b: &str, needle: &str) {
    let r = run(a, b);
    let text = format!("{:?} {:?}", r.unknown_reasons, r.findings);
    assert!(text.contains(needle), "{needle}: {text}");
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{text}"
    );
}

const T_TABLES: &str = "CREATE TABLE dbo.Parent (Id int NOT NULL PRIMARY KEY, Code int NOT NULL UNIQUE);
CREATE TABLE dbo.Child (Id int NOT NULL PRIMARY KEY, ParentId int NOT NULL, ParentCode int NOT NULL);
";
const P_TABLES: &str = "CREATE TABLE Parent (Id integer NOT NULL PRIMARY KEY, Code integer NOT NULL UNIQUE);
CREATE TABLE Child (Id integer NOT NULL PRIMARY KEY, ParentId integer NOT NULL, ParentCode integer NOT NULL);
";

fn t_alter(body: &str) -> String {
    format!("{T_TABLES}ALTER TABLE dbo.Child WITH CHECK ADD {body};\n")
}

fn p_alter(body: &str) -> String {
    format!("{P_TABLES}ALTER TABLE Child {body};\n")
}

const FK1: &str = "CONSTRAINT FK_Child_Parent_Id FOREIGN KEY (ParentId) REFERENCES Parent (Id)";
const FK2: &str =
    "CONSTRAINT FK_Child_Parent_Code FOREIGN KEY (ParentCode) REFERENCES Parent (Code)";

#[test]
fn several_constraints_added_at_once_are_compared_one_by_one() {
    equal(
        &t_alter(&format!("{FK1},\n    {FK2}")),
        &p_alter(&format!("ADD {FK1},\n    ADD {FK2}")),
    );
}

#[test]
fn one_constraint_that_differs_is_found_among_several() {
    // The second foreign key refers to another column on the PostgreSQL side.
    let b = p_alter(&format!(
        "ADD {FK1}, ADD CONSTRAINT FK_Child_Parent_Code FOREIGN KEY (ParentCode) REFERENCES Parent (Id)"
    ));
    let r = run(&t_alter(&format!("{FK1}, {FK2}")), &b);
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:#?}",
        r.unknown_reasons
    );
}

#[test]
fn a_statement_split_on_one_side_only_is_not_paired() {
    let b = format!("{P_TABLES}ALTER TABLE Child ADD {FK1};\nALTER TABLE Child ADD {FK2};\n");
    not_equal(&t_alter(&format!("{FK1}, {FK2}")), &b, "at once");
}

#[test]
fn a_constraint_missing_on_one_side_is_not_equal() {
    not_equal(
        &t_alter(&format!("{FK1}, {FK2}")),
        &p_alter(&format!("ADD {FK1}")),
        "no counterpart",
    );
}

#[test]
fn a_constraint_name_used_twice_in_one_statement_is_not_compared() {
    let dup = "CONSTRAINT FK_Child_Parent_Id FOREIGN KEY (ParentCode) REFERENCES Parent (Code)";
    let r = run(
        &t_alter(&format!("{FK1}, {dup}")),
        &p_alter(&format!("ADD {FK1}, ADD {dup}")),
    );
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:#?}",
        r.unknown_reasons
    );
}

#[test]
fn a_comma_inside_a_column_list_does_not_split_a_constraint() {
    equal(
        "CREATE TABLE dbo.K (A int NOT NULL, B int NOT NULL);
ALTER TABLE dbo.K ADD CONSTRAINT PK_K PRIMARY KEY (A, B);",
        "CREATE TABLE K (A integer NOT NULL, B integer NOT NULL);
ALTER TABLE K ADD CONSTRAINT PK_K PRIMARY KEY (A, B);",
    );
}

#[test]
fn sysname_columns_are_compared_as_nvarchar_128() {
    equal(
        "CREATE TABLE dbo.Names (Id int NOT NULL, N sysname NOT NULL);",
        "CREATE TABLE Names (Id integer NOT NULL, N varchar(128) NOT NULL);",
    );
    // Not NVARCHAR(100).
    not_equal(
        "CREATE TABLE dbo.Names (Id int NOT NULL, N sysname NOT NULL);",
        "CREATE TABLE Names (Id integer NOT NULL, N varchar(100) NOT NULL);",
        "N",
    );
    // A sysname column that does not say NULL or NOT NULL is not relied on.
    not_equal(
        "CREATE TABLE dbo.Names (Id int NOT NULL, N sysname);",
        "CREATE TABLE Names (Id integer NOT NULL, N varchar(128) NOT NULL);",
        "sysname",
    );
}

#[test]
fn a_table_beside_a_select_into_table_pairs_by_name() {
    let a = "CREATE TABLE dbo.Plain (Id int NOT NULL);
CREATE TABLE dbo.Loaded WITH (DISTRIBUTION = ROUND_ROBIN) AS SELECT 1 AS X;";
    let b = "CREATE TABLE Plain (Id integer NOT NULL);";
    let r = run(a, b);
    // The statement without a counterpart is not compared; Plain is.
    assert!(
        r.unknown_reasons
            .iter()
            .any(|x| x.contains("no table of that name")),
        "{:?}",
        r.unknown_reasons
    );
    assert!(
        r.findings.iter().all(|f| f.verdict == Verdict::Equal),
        "{:#?}",
        r.findings
    );
}
