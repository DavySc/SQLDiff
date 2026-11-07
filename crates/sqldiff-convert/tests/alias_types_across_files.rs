//! An alias type (`CREATE TYPE … FROM base`) that one file of a tree creates
//! reads as its base type in the other files of the tree, as it does in the
//! file that creates it: a comparison of a bit alias column with 1 is
//! boolean wherever the column is used.

use std::path::PathBuf;

use sqldiff_convert::tree::{self, Tree};
use sqldiff_core::Dialect;

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str, files: &[(&str, &str)]) -> Fixture {
        let dir = std::env::temp_dir().join(format!("sqldiff-alias-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (p, text) in files {
            std::fs::write(dir.join(p), text).unwrap();
        }
        Fixture { dir }
    }

    fn convert(&self) -> Tree {
        let a = tree::Args {
            rules: Default::default(),
            src: self.dir.clone(),
            from: Dialect::Tsql,
            to: Dialect::Postgres,
            jobs: Some(1),
            no_catalog: false,
            recheck: Default::default(),
            template: None,
            extensions: tree::extensions(Vec::new()),
            helpers: Vec::new(),
            deployment: Default::default(),
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

const TYPES: &str = "CREATE TYPE [Flag] FROM bit NOT NULL;\nGO\n";
const ITEMS: &str = "CREATE TABLE dbo.Item(Id int NOT NULL, Active Flag NOT NULL);\nGO\nCREATE VIEW dbo.V AS SELECT Id FROM dbo.Item WHERE Active = 1;\nGO\n";

#[test]
fn a_bit_alias_created_in_another_file_takes_the_boolean_rewrite() {
    let f = Fixture::new("other-file", &[("types.sql", TYPES), ("items.sql", ITEMS)]);
    let out = f.convert().outputs.join("\n");
    assert!(out.contains("WHERE Active = TRUE"), "{out}");
    assert!(!out.contains("Active = 1"), "{out}");
}

#[test]
fn without_the_type_file_the_column_stays_untyped() {
    // The control: nothing in the tree says what Flag is, so the comparison
    // is left as written (and the output check reports the unknown type).
    let f = Fixture::new("no-type", &[("items.sql", ITEMS)]);
    let out = f.convert().outputs.join("\n");
    assert!(out.contains("WHERE Active = 1"), "{out}");
}

#[test]
fn the_same_file_still_works() {
    let both = format!("{TYPES}{ITEMS}");
    let f = Fixture::new("same-file", &[("all.sql", both.as_str())]);
    let out = f.convert().outputs.join("\n");
    assert!(out.contains("WHERE Active = TRUE"), "{out}");
}
