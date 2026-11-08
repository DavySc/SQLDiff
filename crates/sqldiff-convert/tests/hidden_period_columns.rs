//! System-versioned tables with HIDDEN period columns. T-SQL leaves hidden
//! columns out of `SELECT *`, `t.*` and an INSERT without a column list;
//! PostgreSQL has none, so by default (ASM-HIDDEN-PERIOD-COLUMN) the engine
//! must not call a star equal. With `RuleOptions::temporal_historyless`
//! (ASM-TEMPORAL-HISTORYLESS) the table loses its period columns, history
//! table and triggers, and the star is genuinely the same.

use std::path::PathBuf;

use sqldiff_convert::tree::{self, Tree};
use sqldiff_convert::{RuleOptions, Status};
use sqldiff_core::Dialect;

const TABLE: &str = "CREATE TABLE dbo.t (k INT NOT NULL PRIMARY KEY, f DATETIME2 GENERATED ALWAYS AS ROW START HIDDEN NOT NULL, e DATETIME2 GENERATED ALWAYS AS ROW END HIDDEN NOT NULL, v INT NULL, PERIOD FOR SYSTEM_TIME (f, e)) WITH (SYSTEM_VERSIONING = ON (HISTORY_TABLE = dbo.t_h));\n";
const ONE_HIDDEN: &str = "CREATE TABLE dbo.t (k INT NOT NULL PRIMARY KEY, f DATETIME2 GENERATED ALWAYS AS ROW START HIDDEN NOT NULL, e DATETIME2 GENERATED ALWAYS AS ROW END NOT NULL, v INT NULL, PERIOD FOR SYSTEM_TIME (f, e)) WITH (SYSTEM_VERSIONING = ON (HISTORY_TABLE = dbo.t_h));\n";
const VISIBLE: &str = "CREATE TABLE dbo.t (k INT NOT NULL PRIMARY KEY, f DATETIME2 GENERATED ALWAYS AS ROW START NOT NULL, e DATETIME2 GENERATED ALWAYS AS ROW END NOT NULL, v INT NULL, PERIOD FOR SYSTEM_TIME (f, e)) WITH (SYSTEM_VERSIONING = ON (HISTORY_TABLE = dbo.t_h));\n";
const OTHER: &str = "CREATE TABLE dbo.u (k INT NOT NULL PRIMARY KEY, w INT NULL);\n";
const HISTORY: &str =
    "CREATE TABLE dbo.t_h (k INT NOT NULL, f DATETIME2 NOT NULL, e DATETIME2 NOT NULL, v INT NULL);\n";

fn proc(name: &str, body: &str) -> String {
    format!("CREATE PROCEDURE dbo.{name} AS\nBEGIN\n  {body}\nEND\n")
}

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str, files: &[(&str, String)]) -> Fixture {
        let dir =
            std::env::temp_dir().join(format!("sqldiff-hidden-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (p, text) in files {
            let f = dir.join("src").join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        Fixture { dir }
    }

    fn convert(&self, historyless: bool) -> Tree {
        let a = tree::Args {
            rules: RuleOptions {
                temporal_historyless: historyless,
            },
            src: self.dir.join("src"),
            from: Dialect::Tsql,
            to: Dialect::Postgres,
            jobs: Some(2),
            no_catalog: false,
            recheck: Default::default(),
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

fn index(t: &Tree, path: &str) -> usize {
    t.report
        .files
        .iter()
        .position(|f| f.path == path)
        .unwrap_or_else(|| panic!("{path} in the report"))
}

/// The status of each unit of `path`.
fn statuses(t: &Tree, path: &str) -> Vec<Status> {
    t.conversions[index(t, path)]
        .units
        .iter()
        .map(|u| u.status)
        .collect()
}

/// Every rule the notes of `path`'s units name.
fn rules(t: &Tree, path: &str) -> Vec<String> {
    t.conversions[index(t, path)]
        .units
        .iter()
        .flat_map(|u| u.notes.iter().map(|n| n.rule.to_string()))
        .collect()
}

fn output<'a>(t: &'a Tree, path: &str) -> &'a str {
    &t.conversions[index(t, path)].output
}

fn clean(s: Status) -> bool {
    matches!(s, Status::Converted | Status::Assumptions)
}

/// The file as a whole is not converted: some unit (the statement) is not
/// plainly converted or under assumptions.
fn none_clean(t: &Tree, path: &str) {
    let f = &t.report.files[index(t, path)];
    assert!(
        !clean(f.status),
        "{path}: {:?} {:?}\n{}",
        f.status,
        statuses(t, path),
        output(t, path)
    );
}

fn all_clean(t: &Tree, path: &str) {
    let f = &t.report.files[index(t, path)];
    assert!(
        clean(f.status),
        "{path}: {:?} {:?}\n{}",
        f.status,
        statuses(t, path),
        output(t, path)
    );
}

fn tree_of(name: &str, table: &str, bodies: &[(&str, &str)]) -> Fixture {
    let mut files = vec![("a.sql", table.to_string()), ("u.sql", OTHER.to_string())];
    let names: Vec<String> = bodies.iter().map(|(n, _)| format!("{n}.sql")).collect();
    for (n, (proc_name, body)) in names.iter().zip(bodies) {
        files.push((n.as_str(), proc(proc_name, body)));
    }
    let files: Vec<(&str, String)> = files;
    Fixture::new(name, &files)
}

#[test]
fn a_star_over_hidden_columns_is_not_converted_by_default() {
    let fx = tree_of(
        "star",
        TABLE,
        &[
            ("plain", "SELECT * FROM dbo.t;"),
            ("qualified", "SELECT t.* FROM dbo.t;"),
            ("aliased", "SELECT x.* FROM dbo.t AS x;"),
            ("copy", "INSERT INTO dbo.u (k, w) SELECT * FROM dbo.t;"),
            ("positional", "INSERT INTO dbo.t VALUES (1, 2);"),
        ],
    );
    let t = fx.convert(false);
    for f in ["plain", "qualified", "aliased", "copy", "positional"] {
        none_clean(&t, &format!("{f}.sql"));
    }
    // The CREATE TABLE keeps its period columns and the history.
    assert!(
        output(&t, "a.sql").contains("CREATE TRIGGER"),
        "{}",
        output(&t, "a.sql")
    );
    assert!(rules(&t, "a.sql")
        .iter()
        .any(|r| r == "ASM-HIDDEN-PERIOD-COLUMN"));
}

#[test]
fn naming_the_columns_is_converted_by_default() {
    let fx = tree_of(
        "named",
        TABLE,
        &[
            ("named", "SELECT k, v FROM dbo.t;"),
            ("qualified", "SELECT t.k, t.v FROM dbo.t;"),
            ("insert", "INSERT INTO dbo.t (k, v) VALUES (1, 2);"),
        ],
    );
    let t = fx.convert(false);
    for f in ["named", "qualified", "insert"] {
        all_clean(&t, &format!("{f}.sql"));
    }
}

#[test]
fn one_hidden_column_is_left_out_of_the_star_too() {
    let fx = tree_of(
        "one",
        ONE_HIDDEN,
        &[
            ("plain", "SELECT * FROM dbo.t;"),
            ("named", "SELECT k, e, v FROM dbo.t;"),
        ],
    );
    for historyless in [false, true] {
        let t = fx.convert(historyless);
        none_clean(&t, "plain.sql");
        all_clean(&t, "named.sql");
        // One visible period column: the option leaves the table alone.
        let o = output(&t, "a.sql");
        assert!(o.contains("CREATE TRIGGER"), "{o}");
        assert!(
            rules(&t, "a.sql")
                .iter()
                .any(|r| r == "ASM-HIDDEN-PERIOD-COLUMN"),
            "{:?}",
            rules(&t, "a.sql")
        );
        assert!(!rules(&t, "a.sql")
            .iter()
            .any(|r| r == "ASM-TEMPORAL-HISTORYLESS"));
    }
}

#[test]
fn the_option_leaves_visible_period_columns_alone() {
    let fx = tree_of("visible", VISIBLE, &[("plain", "SELECT * FROM dbo.t;")]);
    let off = fx.convert(false);
    let on = fx.convert(true);
    assert_eq!(output(&off, "a.sql"), output(&on, "a.sql"));
    assert!(output(&on, "a.sql").contains("CREATE TRIGGER"));
    assert!(!rules(&on, "a.sql")
        .iter()
        .any(|r| r == "ASM-TEMPORAL-HISTORYLESS"));
    // Nothing is hidden: the star is the table's columns on both sides.
    all_clean(&on, "plain.sql");
}

#[test]
fn the_historyless_table_is_plain() {
    let fx = tree_of("plain-table", TABLE, &[]);
    let t = fx.convert(true);
    let o = output(&t, "a.sql");
    for gone in [
        "f ",
        "e ",
        "t_h",
        "TRIGGER",
        "FUNCTION",
        "SYSTEM_VERSIONING",
        "PERIOD",
        "-infinity",
    ] {
        assert!(!o.contains(gone), "{gone}\n{o}");
    }
    assert!(o.contains("CREATE TABLE t ("), "{o}");
    assert!(o.contains("k integer NOT NULL PRIMARY KEY"), "{o}");
    assert!(o.contains("v integer NULL"), "{o}");
    let r = rules(&t, "a.sql");
    assert!(r.iter().any(|r| r == "ASM-TEMPORAL-HISTORYLESS"), "{r:?}");
    assert!(!r.iter().any(|r| r == "ASM-HIDDEN-PERIOD-COLUMN"), "{r:?}");
    assert!(!r.iter().any(|r| r == "ASM-TEMPORAL-TRIGGER"), "{r:?}");
    let note = t.conversions[index(&t, "a.sql")].units[0]
        .notes
        .iter()
        .find(|n| n.rule == "ASM-TEMPORAL-HISTORYLESS")
        .unwrap();
    assert!(note.text.contains("future work"), "{}", note.text);
    // The default stays a full conversion.
    let d = fx.convert(false);
    assert!(output(&d, "a.sql").contains("CREATE TRIGGER"));
}

#[test]
fn the_star_is_the_same_without_the_hidden_columns() {
    let fx = tree_of(
        "same",
        TABLE,
        &[
            ("plain", "SELECT * FROM dbo.t;"),
            ("qualified", "SELECT t.* FROM dbo.t;"),
            ("aliased", "SELECT x.* FROM dbo.t AS x;"),
            ("copy", "INSERT INTO dbo.u (k, w) SELECT * FROM dbo.t;"),
            ("named", "SELECT k, v FROM dbo.t;"),
        ],
    );
    let t = fx.convert(true);
    for f in ["plain", "qualified", "aliased", "copy", "named"] {
        all_clean(&t, &format!("{f}.sql"));
    }
    assert!(output(&t, "plain.sql").contains("SELECT * FROM t"));
}

#[test]
fn what_reads_the_history_or_the_period_is_not_converted() {
    let fx = tree_of(
        "readers",
        TABLE,
        &[
            ("period", "SELECT f FROM dbo.t;"),
            ("qualified", "SELECT t.e FROM dbo.t;"),
            ("history", "SELECT * FROM dbo.t_h;"),
            (
                "asof",
                "SELECT k FROM dbo.t FOR SYSTEM_TIME AS OF '2020-01-01';",
            ),
            ("all", "SELECT * FROM dbo.t FOR SYSTEM_TIME ALL;"),
            (
                "write",
                "INSERT INTO dbo.t (k, f) VALUES (1, '2020-01-01');",
            ),
        ],
    );
    let t = fx.convert(true);
    for f in ["period", "qualified", "history", "asof", "all", "write"] {
        none_clean(&t, &format!("{f}.sql"));
    }
}

#[test]
fn a_positional_insert_stays_not_converted() {
    let fx = tree_of(
        "insert",
        TABLE,
        &[("positional", "INSERT INTO dbo.t VALUES (1, 2);")],
    );
    for historyless in [false, true] {
        none_clean(&fx.convert(historyless), "positional.sql");
    }
}

#[test]
fn the_history_table_of_a_historyless_table_is_not_created() {
    let mut files = vec![("a.sql", TABLE.to_string()), ("h.sql", HISTORY.to_string())];
    files.push(("r.sql", proc("r", "SELECT * FROM dbo.t_h;")));
    let fx = Fixture::new("history", &files);
    // By default it is a plain table, as ever.
    let d = fx.convert(false);
    all_clean(&d, "h.sql");
    let t = fx.convert(true);
    let st = statuses(&t, "h.sql");
    assert!(st.iter().all(|&s| s == Status::NotConverted), "{st:?}");
    let reason = t.conversions[index(&t, "h.sql")].units[0]
        .notes
        .iter()
        .map(|n| n.text.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(reason.contains("history table of dbo.t"), "{reason}");
    none_clean(&t, "r.sql");
}

#[test]
fn switching_the_versioning_of_a_historyless_table_is_refused() {
    let alter = "ALTER TABLE dbo.t SET (SYSTEM_VERSIONING = OFF);\n".to_string();
    let files = vec![("a.sql", TABLE.to_string()), ("s.sql", alter)];
    let fx = Fixture::new("switch", &files);
    let st = statuses(&fx.convert(false), "s.sql");
    assert!(st.iter().all(|&s| clean(s)), "{st:?}");
    let t = fx.convert(true);
    let st = statuses(&t, "s.sql");
    assert!(st.iter().all(|&s| s == Status::NotConverted), "{st:?}");
}

#[test]
fn a_constraint_on_a_period_column_is_refused() {
    let table = "CREATE TABLE dbo.t (k INT NOT NULL, f DATETIME2 GENERATED ALWAYS AS ROW START HIDDEN NOT NULL, e DATETIME2 GENERATED ALWAYS AS ROW END HIDDEN NOT NULL, v INT NULL, CONSTRAINT pk PRIMARY KEY (k, f), PERIOD FOR SYSTEM_TIME (f, e)) WITH (SYSTEM_VERSIONING = ON (HISTORY_TABLE = dbo.t_h));\n";
    let fx = Fixture::new("constraint", &[("a.sql", table.to_string())]);
    let st = statuses(&fx.convert(true), "a.sql");
    assert!(st.iter().all(|&s| s == Status::NotConverted), "{st:?}");
    // Without the option the table converts, with its history.
    let d = fx.convert(false);
    assert!(output(&d, "a.sql").contains("CREATE TRIGGER"));
}

#[test]
fn the_note_names_every_way_the_star_differs() {
    let fx = tree_of("note", TABLE, &[]);
    let t = fx.convert(false);
    let note = t.conversions[index(&t, "a.sql")].units[0]
        .notes
        .iter()
        .find(|n| n.rule == "ASM-HIDDEN-PERIOD-COLUMN")
        .unwrap();
    for want in ["t.*", "INSERT … SELECT *", "view", "by position", "shift"] {
        assert!(note.text.contains(want), "{want}: {}", note.text);
    }
}

#[test]
fn the_stored_result_key_differs_with_the_option() {
    use sqldiff_convert::store::CheckInput;
    use sqldiff_convert::{rewrite_with, Draft};
    let key = |d: &Draft, text: &str| {
        CheckInput {
            build: "b",
            from: Dialect::Tsql,
            to: Dialect::Postgres,
            trim: false,
            helpers: &[],
            text,
            draft: d,
            source: None,
            target: None,
        }
        .key()
    };
    // A text no mode changes: the key still tells the modes apart, so a
    // result checked under the other mode is never reused.
    let text = "SELECT 1;\n";
    let off = rewrite_with(
        text,
        Dialect::Tsql,
        Dialect::Postgres,
        None,
        RuleOptions::default(),
    );
    let on = rewrite_with(
        text,
        Dialect::Tsql,
        Dialect::Postgres,
        None,
        RuleOptions {
            temporal_historyless: true,
        },
    );
    assert_eq!(off.output(), on.output());
    assert_ne!(key(&off, text), key(&on, text));
    assert_eq!(
        key(&off, text),
        key(
            &rewrite_with(
                text,
                Dialect::Tsql,
                Dialect::Postgres,
                None,
                RuleOptions::default()
            ),
            text
        )
    );
}

#[test]
fn a_star_over_a_view_defined_with_a_star_follows_the_option() {
    let fx = Fixture::new(
        "view",
        &[
            ("a.sql", TABLE.to_string()),
            (
                "v.sql",
                "CREATE VIEW dbo.v AS SELECT * FROM dbo.t;\n".to_string(),
            ),
            ("reader.sql", proc("reader", "SELECT * FROM dbo.v;")),
            ("named.sql", proc("named", "SELECT k, v FROM dbo.v;")),
        ],
    );
    // By default the view returns the period columns on PostgreSQL, and the
    // reader's star would differ from SQL Server's.
    let t = fx.convert(false);
    none_clean(&t, "reader.sql");
    none_clean(&t, "v.sql");
    // Naming the columns is still fine.
    all_clean(&t, "named.sql");
    // Without the period columns both views return k and v.
    let t = fx.convert(true);
    all_clean(&t, "v.sql");
    all_clean(&t, "reader.sql");
    all_clean(&t, "named.sql");
}
