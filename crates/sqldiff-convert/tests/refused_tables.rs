//! Tables of the tree whose CREATE TABLE the rules refused for a part of it
//! (a computed column, `DURABILITY = SCHEMA_ONLY`) stand in the target
//! catalog without that part, for binding only: the refused statement stays
//! not converted, what uses the table carries `ASM-TABLE-STAND-IN`, and what
//! may touch a refused column is not proven.

use std::path::PathBuf;

use sqldiff_convert::tree::{self, Tree};
use sqldiff_convert::Status;
use sqldiff_core::Dialect;

const PEOPLE: (&str, &str) = (
    "src/tables/people.sql",
    "CREATE TABLE app.people (\n    person_id int NOT NULL PRIMARY KEY,\n    full_name nvarchar(50) NOT NULL,\n    preferred_name nvarchar(50) NOT NULL,\n    search_name AS (concat(preferred_name, N' ', full_name)) PERSISTED NOT NULL,\n    custom_fields nvarchar(max) NULL,\n    other_languages AS (json_query(custom_fields, N'strict $.OtherLanguages'))\n);\nGO\nCREATE INDEX ix_people_full_name ON app.people (full_name);\nGO\n",
);

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str, files: &[(&str, &str)]) -> Fixture {
        let dir =
            std::env::temp_dir().join(format!("sqldiff-refused-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (p, text) in files {
            let f = dir.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        Fixture { dir }
    }

    fn convert(&self) -> Tree {
        let a = tree::Args {
            src: self.dir.join("src"),
            from: Dialect::Tsql,
            to: Dialect::Postgres,
            jobs: Some(2),
            no_catalog: false,
            template: None,
            extensions: tree::extensions(Vec::new()),
            helpers: Vec::new(),
            catalogs: Vec::new(),
            catalogs_b: Vec::new(),
            deployment: Default::default(),
        };
        let (inputs, unreadable) = tree::read(&a).unwrap();
        tree::convert(&a, &inputs, unreadable, None).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Each unit of `path`: its status and every rule its notes and findings name.
fn units(t: &Tree, path: &str) -> Vec<(Status, Vec<String>)> {
    let k = t
        .report
        .files
        .iter()
        .position(|f| f.path == path)
        .unwrap_or_else(|| panic!("{path} in the report"));
    t.conversions[k]
        .units
        .iter()
        .map(|u| {
            let mut r: Vec<String> = u.notes.iter().map(|n| n.rule.clone()).collect();
            r.extend(
                u.findings
                    .iter()
                    .map(|&i| t.conversions[k].report.findings[i].rule.clone()),
            );
            r.extend(
                u.output_findings
                    .iter()
                    .map(|&i| t.conversions[k].output_check.findings[i].rule.clone()),
            );
            (u.status, r)
        })
        .collect()
}

fn one(t: &Tree, path: &str) -> (Status, Vec<String>) {
    let u = units(t, path);
    assert_eq!(u.len(), 1, "{path}: {u:?}");
    u.into_iter().next().unwrap()
}

fn has(rules: &[String], rule: &str) -> bool {
    rules.iter().any(|r| r == rule)
}

#[test]
fn a_table_refused_for_computed_columns_binds_without_them() {
    let fx = Fixture::new(
        "computed",
        &[
            PEOPLE,
            (
                "src/scripts/rename.sql",
                "UPDATE app.people SET full_name = N'x' WHERE person_id = 1;\n",
            ),
            (
                "src/scripts/search.sql",
                "SELECT person_id FROM app.people WHERE search_name = N'x';\n",
            ),
            (
                "src/scripts/count.sql",
                "SELECT COUNT(*) FROM app.people;\n",
            ),
            ("src/scripts/star.sql", "SELECT * FROM app.people;\n"),
            (
                "src/scripts/insert_cols.sql",
                "INSERT INTO app.people (person_id, full_name, preferred_name) VALUES (1, N'a', N'b');\n",
            ),
            (
                "src/scripts/insert_pos.sql",
                "INSERT INTO app.people VALUES (1, N'a', N'b', NULL);\n",
            ),
        ],
    );
    let t = fx.convert();
    // The refused statement stays as it was, and its file never counts.
    let table = units(&t, "tables/people.sql");
    assert_eq!(table[0].0, Status::NotConverted, "{table:?}");
    assert!(!has(&table[0].1, "ASM-TABLE-STAND-IN"), "{table:?}");
    // The index on it binds now, under the stand-in assumption.
    assert!(table[1].0 < Status::Unproven, "{table:?}");
    assert!(has(&table[1].1, "ASM-TABLE-STAND-IN"), "{table:?}");

    for path in [
        "scripts/rename.sql",
        "scripts/count.sql",
        "scripts/insert_cols.sql",
    ] {
        let (st, rules) = one(&t, path);
        assert_eq!(st, Status::Assumptions, "{path}: {rules:?}");
        assert!(has(&rules, "ASM-TABLE-STAND-IN"), "{path}: {rules:?}");
        assert!(!has(&rules, "bind.unknown-table"), "{path}: {rules:?}");
        assert!(!has(&rules, "tree.refused-column"), "{path}: {rules:?}");
    }
    // A refused column, `*` or a positional INSERT: not compared.
    for path in [
        "scripts/search.sql",
        "scripts/star.sql",
        "scripts/insert_pos.sql",
    ] {
        let (st, rules) = one(&t, path);
        assert!(st >= Status::Unproven, "{path}: {rules:?}");
        assert!(has(&rules, "tree.refused-column"), "{path}: {rules:?}");
    }
}

#[test]
fn without_a_stand_in_the_cascade_is_what_it_was() {
    // A table refused for another reason (a column type with no rule) has
    // no stand-in: what uses it still fails to bind.
    let fx = Fixture::new(
        "other",
        &[
            (
                "src/tables/places.sql",
                "CREATE TABLE app.places (id int NOT NULL, loc hierarchyid NULL, label AS (CAST(id AS varchar(10))));\n",
            ),
            // The schema is known, so a table missing from it is missing.
            ("src/tables/other.sql", "CREATE TABLE app.other (id int NOT NULL);\n"),
            (
                "src/scripts/read.sql",
                "SELECT id FROM app.places;\n",
            ),
        ],
    );
    let t = fx.convert();
    let (st, rules) = one(&t, "scripts/read.sql");
    assert_eq!(st, Status::Differs, "{rules:?}");
    assert!(has(&rules, "bind.unknown-table"), "{rules:?}");
    assert!(!has(&rules, "ASM-TABLE-STAND-IN"), "{rules:?}");
}

#[test]
fn schema_only_tables_stand_in_without_their_durability() {
    let fx = Fixture::new(
        "schema-only",
        &[
            (
                "src/tables/staging.sql",
                "CREATE TABLE int.staging (id int NOT NULL PRIMARY KEY NONCLUSTERED, name nvarchar(20) NULL,\n    INDEX ix_staging_name NONCLUSTERED HASH (name) WITH (BUCKET_COUNT = 1024)\n) WITH (MEMORY_OPTIMIZED = ON, DURABILITY = SCHEMA_ONLY);\n",
            ),
            (
                "src/scripts/load.sql",
                "DELETE FROM int.staging WHERE id = 1;\n",
            ),
        ],
    );
    let t = fx.convert();
    assert_eq!(
        one(&t, "tables/staging.sql").0,
        Status::NotConverted,
        "the table stays refused"
    );
    let (st, rules) = one(&t, "scripts/load.sql");
    assert_eq!(st, Status::Assumptions, "{rules:?}");
    assert!(has(&rules, "ASM-TABLE-STAND-IN"), "{rules:?}");
    let note = &t.conversions[t
        .report
        .files
        .iter()
        .position(|f| f.path == "scripts/load.sql")
        .unwrap()]
    .units[0]
        .notes
        .iter()
        .find(|n| n.rule == "ASM-TABLE-STAND-IN")
        .unwrap()
        .text;
    assert!(note.contains("DURABILITY = SCHEMA_ONLY"), "{note}");
    assert!(note.contains("index ix_staging_name"), "{note}");
}

#[test]
fn a_comment_on_a_refused_column_is_not_proven() {
    // The column is named in a string on the source side: the output's
    // COMMENT ON COLUMN names it.
    let fx = Fixture::new(
        "extprop",
        &[
            PEOPLE,
            (
                "src/scripts/props.sql",
                "EXECUTE sp_addextendedproperty @name = N'Description', @value = 'Search', @level0type = N'SCHEMA', @level0name = 'app', @level1type = N'TABLE', @level1name = N'people', @level2type = N'COLUMN', @level2name = N'search_name';\nGO\nEXECUTE sp_addextendedproperty @name = N'Description', @value = 'Name', @level0type = N'SCHEMA', @level0name = 'app', @level1type = N'TABLE', @level1name = N'people', @level2type = N'COLUMN', @level2name = N'full_name';\nGO\n",
            ),
        ],
    );
    let t = fx.convert();
    let u = units(&t, "scripts/props.sql");
    assert_eq!(u.len(), 2, "{u:?}");
    assert!(u[0].0 >= Status::Unproven, "{u:?}");
    assert!(has(&u[0].1, "tree.refused-column"), "{u:?}");
    assert!(u[1].0 < Status::Unproven, "{u:?}");
    assert!(has(&u[1].1, "ASM-TABLE-STAND-IN"), "{u:?}");
}

#[test]
fn a_view_over_a_stand_in_carries_it_to_its_users() {
    let fx = Fixture::new(
        "view",
        &[
            PEOPLE,
            (
                "src/views/names.sql",
                "CREATE VIEW app.names AS SELECT person_id, full_name FROM app.people;\n",
            ),
            (
                "src/views/search.sql",
                "CREATE VIEW app.search AS SELECT person_id, search_name FROM app.people;\n",
            ),
            (
                "src/scripts/names.sql",
                "SELECT full_name FROM app.names WHERE person_id = 1;\n",
            ),
            (
                "src/scripts/search.sql",
                "SELECT person_id FROM app.search;\n",
            ),
        ],
    );
    let t = fx.convert();
    let (st, rules) = one(&t, "scripts/names.sql");
    assert!(st <= Status::Assumptions, "{rules:?}");
    assert!(has(&rules, "ASM-TABLE-STAND-IN"), "{rules:?}");
    // The view shows a refused column: every use may read it.
    let (st, rules) = one(&t, "scripts/search.sql");
    assert!(st >= Status::Unproven, "{rules:?}");
    assert!(has(&rules, "tree.refused-column"), "{rules:?}");
}

#[test]
fn dynamic_sql_naming_a_stand_in_carries_it() {
    // The engine compares dynamic SQL whose text is known, and that binds
    // against the stand-in: the name inside the string counts.
    let fx = Fixture::new(
        "dynamic",
        &[
            PEOPLE,
            (
                "src/scripts/rename.sql",
                "EXEC('UPDATE app.people SET full_name = N''x'' WHERE person_id = 1');\n",
            ),
            (
                "src/scripts/search.sql",
                "EXEC('SELECT person_id FROM app.people WHERE search_name = N''x''');\n",
            ),
        ],
    );
    let t = fx.convert();
    let (st, rules) = one(&t, "scripts/rename.sql");
    assert!(st >= Status::Assumptions, "{rules:?}");
    assert!(has(&rules, "ASM-TABLE-STAND-IN"), "{rules:?}");
    let (st, rules) = one(&t, "scripts/search.sql");
    assert!(st >= Status::Unproven, "{rules:?}");
    assert!(has(&rules, "tree.refused-column"), "{rules:?}");
}
