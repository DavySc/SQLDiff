//! `sqldiff drift`: the template trees of several dialects kept in step.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sqldiff-drift-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_sqldiff"))
        .args(args)
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

const PACK: &str = r#"
[pack]
name = "drift"
version = "0.1.0"
schema = 1

[template]
open = "<%"
close = "%>"
name_pattern = "[A-Z_][A-Z0-9_]*"
special_forms = [{ name = "include", pattern = "<%@include\\s+(\\S+)\\s*%>" }]
include = "include"

[[author_markers]]
kind = "intentional_difference"
pattern = "--\\s*SYNC-OK:\\s*(.*)"
"#;

/// The same templates in three dialects.
fn trees(d: &Path) {
    write(&d.join("pack.toml"), PACK);
    for (dir, table, proc) in [
        (
            "mssql",
            "CREATE TABLE <%SCHEMA%>.<%TABLE%> (Id int NOT NULL, Price int NOT NULL);\n",
            "<%@include header.sql %>CREATE PROCEDURE <%SCHEMA%>.Touch<%TABLE%> @Id int AS\nBEGIN\n  UPDATE <%SCHEMA%>.<%TABLE%> SET Price = 0 WHERE Id = @Id;\nEND\n",
        ),
        (
            "pg",
            "CREATE TABLE <%SCHEMA%>.<%TABLE%> (Id integer NOT NULL, Price integer NOT NULL);\n",
            "<%@include header.sql %>CREATE OR REPLACE PROCEDURE <%SCHEMA%>.Touch<%TABLE%>(p_id integer)\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  UPDATE <%SCHEMA%>.<%TABLE%> SET Price = 0 WHERE Id = p_id;\nEND;\n$$;\n",
        ),
        (
            "ibmi",
            "CREATE TABLE <%SCHEMA%>.<%TABLE%> (ID INTEGER NOT NULL, PRICE INTEGER NOT NULL);\n",
            "<%@include header.sql %>CREATE OR REPLACE PROCEDURE <%SCHEMA%>.TOUCH<%TABLE%> (IN P_ID INTEGER)\nBEGIN\n  UPDATE <%SCHEMA%>.<%TABLE%> SET PRICE = 0 WHERE ID = P_ID;\nEND;\n",
        ),
    ] {
        write(&d.join(dir).join("tables/entity.sql"), table);
        write(&d.join(dir).join("procs/touch.sql"), proc);
        write(&d.join(dir).join("procs/header.sql"), "-- generated\n");
    }
}

fn drift(d: &Path, extra: &[&str]) -> (std::process::Output, serde_json::Value) {
    let (pack, out) = (d.join("pack.toml"), d.join("out"));
    let trees: Vec<String> = [("tsql", "mssql"), ("postgres", "pg"), ("db2i", "ibmi")]
        .iter()
        .filter(|(_, dir)| d.join(dir).is_dir())
        .map(|(dl, dir)| format!("{dl}={}", d.join(dir).display()))
        .collect();
    let mut args = vec!["drift"];
    for t in &trees {
        args.extend(["--tree", t.as_str()]);
    }
    args.extend([
        "-o",
        out.to_str().unwrap(),
        "--overwrite",
        "--pack",
        pack.to_str().unwrap(),
        "--isolated",
    ]);
    args.extend_from_slice(extra);
    let o = run(&args);
    let r = std::fs::read_to_string(out.join("drift-report.json"))
        .map(|t| serde_json::from_str(&t).unwrap())
        .unwrap_or(serde_json::Value::Null);
    (o, r)
}

fn items(r: &serde_json::Value) -> Vec<(String, String, String)> {
    let mut v = Vec::new();
    for t in r["drift"].as_array().unwrap() {
        for i in t["items"].as_array().unwrap() {
            v.push((
                t["path"].as_str().unwrap().to_string(),
                i["dialect"].as_str().unwrap().to_string(),
                i["rule"].as_str().unwrap().to_string(),
            ));
        }
    }
    v
}

#[test]
fn trees_in_step_have_no_drift() {
    let d = scratch("clean");
    trees(&d);
    let (o, r) = drift(&d, &[]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(r["templates"], 3);
    assert!(items(&r).is_empty(), "{r}");
}

#[test]
fn finds_missing_templates_placeholders_and_directives() {
    let d = scratch("structure");
    trees(&d);
    // A template added to one tree only.
    write(
        &d.join("mssql/procs/purge.sql"),
        "DELETE FROM <%SCHEMA%>.<%TABLE%>;\n",
    );
    write(
        &d.join("pg/procs/purge.sql"),
        "DELETE FROM <%SCHEMA%>.<%TABLE%>;\n",
    );
    // A placeholder renamed in one tree, and a directive dropped in another.
    let pg = d.join("pg/tables/entity.sql");
    let text = std::fs::read_to_string(&pg).unwrap();
    write(&pg, &text.replace("<%TABLE%>", "<%ENTITY%>"));
    write(
        &d.join("ibmi/procs/touch.sql"),
        "CREATE OR REPLACE PROCEDURE <%SCHEMA%>.TOUCH<%TABLE%> (IN P_ID INTEGER)\nBEGIN\n  UPDATE <%SCHEMA%>.<%TABLE%> SET PRICE = 0 WHERE ID = P_ID;\nEND;\n",
    );
    let (o, r) = drift(&d, &[]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        items(&r),
        [
            (
                "procs/purge.sql".into(),
                "db2i".into(),
                "drift.missing".into()
            ),
            (
                "procs/touch.sql".into(),
                "db2i".into(),
                "drift.directives".into()
            ),
            (
                "tables/entity.sql".into(),
                "postgres".into(),
                "drift.placeholders".into()
            ),
        ]
    );
    let md = std::fs::read_to_string(d.join("out/drift-report.md")).unwrap();
    assert!(
        md.contains("only here: ENTITY; only in T-SQL: TABLE"),
        "{md}"
    );
}

#[test]
fn a_change_in_one_tree_only_is_drift_unless_marked() {
    let d = scratch("changed");
    trees(&d);
    // A marker committed long ago explains no later change.
    write(
        &d.join("mssql/procs/header.sql"),
        "-- SYNC-OK: old reason\n-- generated\n",
    );
    git(&d, &["init", "-q", "-b", "main"]);
    git(&d, &["add", "."]);
    git(&d, &["commit", "-q", "-m", "base"]);
    git(&d, &["checkout", "-q", "-b", "work"]);
    // Changed in T-SQL only.
    let t = d.join("mssql/tables/entity.sql");
    let text = std::fs::read_to_string(&t).unwrap();
    write(&t, &text.replace("Price int", "Price bigint"));
    // Changed in PostgreSQL only, on purpose.
    let p = d.join("pg/procs/touch.sql");
    let text = std::fs::read_to_string(&p).unwrap();
    write(
        &p,
        &format!("-- SYNC-OK: PostgreSQL needs no lock hint\n{text}"),
    );
    write(
        &d.join("mssql/procs/header.sql"),
        "-- SYNC-OK: old reason\n-- generated by v2\n",
    );
    git(&d, &["commit", "-q", "-am", "work"]);
    let (o, r) = drift(&d, &["--base", "main"]);
    assert_eq!(
        o.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(
        items(&r),
        [
            (
                "procs/header.sql".into(),
                "tsql".into(),
                "drift.changed-alone".into()
            ),
            (
                "tables/entity.sql".into(),
                "tsql".into(),
                "drift.changed-alone".into()
            )
        ]
    );
    assert_eq!(r["waived"][0]["path"], "procs/touch.sql");
    assert_eq!(r["waived"][0]["reason"], "PostgreSQL needs no lock hint");
}

#[test]
fn checks_every_instance_against_the_reference() {
    let d = scratch("instances");
    trees(&d);
    std::fs::remove_dir_all(d.join("ibmi")).unwrap();
    write(
        &d.join("instances.toml"),
        "[params]\nSCHEMA = \"dbo\"\n[params_for.postgres]\nSCHEMA = \"public\"\n\n[[instance]]\nname = \"Customer\"\ntemplate = \"tables/entity.sql\"\nparams = { TABLE = \"Customer\" }\n\n[[instance]]\nname = \"TouchCustomer\"\ntemplate = \"procs/touch.sql\"\nparams = { TABLE = \"Customer\" }\n",
    );
    let m = d.join("instances.toml");
    let (o, r) = drift(&d, &["--manifest", m.to_str().unwrap()]);
    assert_eq!(r["instances"][0]["dialect"], "postgres");
    let status = |name: &str| {
        let r: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(d.join("out/instances/postgres/instance-report.json"))
                .unwrap(),
        )
        .unwrap();
        r["instances"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["name"] == name)
            .unwrap()["status"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(status("TouchCustomer"), "converted");
    // The table is the one the conversion rules write.
    assert_eq!(status("Customer"), "converted");
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(r["instances"][0]["ok"], true);

    // A table edited by hand is compared column by column: a narrower
    // column rejects values the original stores.
    let p = d.join("pg/tables/entity.sql");
    let table = std::fs::read_to_string(&p).unwrap();
    write(&p, &table.replace("Price integer", "Price smallint"));
    let (o, _) = drift(&d, &["--manifest", m.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(status("Customer"), "differs");
    // A wider one holds every value, under an assumption about the data.
    write(&p, &table.replace("Price integer", "Price bigint"));
    let (o, _) = drift(&d, &["--manifest", m.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(status("Customer"), "assumptions");
    write(&p, &table);

    // A hand edit that changes what a template means fails the check.
    let p = d.join("pg/procs/touch.sql");
    let text = std::fs::read_to_string(&p).unwrap();
    write(&p, &text.replace("SET Price = 0", "SET Price = 1"));
    let (o, _) = drift(&d, &["--manifest", m.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(status("TouchCustomer"), "differs");
}
