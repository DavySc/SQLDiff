//! Pack helpers are routines the dialects provide that no file in the tree
//! defines. The converted output's check resolves calls to them, so a call
//! to one is not reported as a routine missing from the catalog.

use std::path::PathBuf;

use sqldiff_convert::tree::{self, Tree};
use sqldiff_core::catalog::builtins::Helper;
use sqldiff_core::Dialect;

const SCHEMA: &str = "CREATE FUNCTION dbo.known_fn() RETURNS int AS BEGIN RETURN 1; END\n";
const CALLER: &str = "CREATE PROCEDURE dbo.show_site AS\nBEGIN\n    DECLARE @n nvarchar(50) = dbo.site_name();\n    UPDATE dbo.t SET a = @n;\nEND\n";

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir =
            std::env::temp_dir().join(format!("sqldiff-helpers-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (p, text) in [
            ("schema/known.sql", SCHEMA),
            ("procs/show_site.sql", CALLER),
        ] {
            let f = dir.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        Fixture { dir }
    }

    fn convert(&self, helpers: Vec<Helper>) -> Tree {
        let a = tree::Args {
            src: self.dir.clone(),
            from: Dialect::Tsql,
            to: Dialect::Postgres,
            jobs: Some(1),
            no_catalog: false,
            template: None,
            extensions: tree::extensions(Vec::new()),
            helpers,
            catalogs: Vec::new(),
            catalogs_b: Vec::new(),
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

fn helper(dialect: Dialect) -> Helper {
    Helper {
        name: "site_name".to_string(),
        dialect,
        returns: Some("text".to_string()),
    }
}

/// The rules of the findings the converted check raised for the caller.
fn check_rules(t: &Tree) -> Vec<String> {
    let at = t
        .report
        .files
        .iter()
        .position(|f| f.path.ends_with("show_site.sql"))
        .expect("the caller is in the report");
    t.conversions[at]
        .output_check
        .findings
        .iter()
        .map(|f| f.rule.clone())
        .collect()
}

#[test]
fn a_routine_no_file_defines_is_missing_without_a_helper() {
    let fx = Fixture::new("missing");
    let rules = check_rules(&fx.convert(Vec::new()));
    assert!(
        rules.iter().any(|r| r == "bind.unknown-routine"),
        "without a helper the call is unknown: {rules:?}"
    );
}

#[test]
fn a_helper_of_the_target_dialect_resolves_the_call() {
    let fx = Fixture::new("resolved");
    let rules = check_rules(&fx.convert(vec![helper(Dialect::Postgres)]));
    assert!(
        !rules.iter().any(|r| r == "bind.unknown-routine"),
        "a PostgreSQL helper resolves the converted call: {rules:?}"
    );
}

#[test]
fn a_helper_of_another_dialect_does_not_resolve_the_call() {
    let fx = Fixture::new("other");
    let rules = check_rules(&fx.convert(vec![helper(Dialect::Db2i)]));
    assert!(
        rules.iter().any(|r| r == "bind.unknown-routine"),
        "a Db2 for i helper says nothing about PostgreSQL: {rules:?}"
    );
}
