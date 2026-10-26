//! `sqldiff instances`: weave every instance from the source and the
//! converted templates and check each pair.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sqldiff-instances-{name}-{}", std::process::id()));
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

fn pack() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/examples/pack")
        .to_str()
        .unwrap()
        .to_string()
}

const TABLE: &str =
    "CREATE TABLE <%SCHEMA%>.<%TABLE%> (Id int NOT NULL, Price decimal(12, 2) NOT NULL);\n";
const PROC: &str = "<%@include header.sql %>CREATE PROCEDURE <%SCHEMA%>.Touch<%TABLE%> @Id int AS\nBEGIN\n  UPDATE <%SCHEMA%>.<%TABLE%> SET Price = 0 WHERE Id = @Id;\nEND\n";

const MANIFEST: &str = r#"
[params]
SCHEMA = "dbo"
[params_for.postgres]
SCHEMA = "public"

[[instance]]
name = "tables/Customer"
template = "tables/entity.sql"
params = { TABLE = "Customer" }

[[instance]]
name = "procs/TouchCustomer"
template = "procs/touch.sql"
params = { TABLE = "Customer" }

[[instance]]
name = "tables/Order"
template = "tables/entity.sql"
params = { TABLE = "Order" }
"#;

/// Source templates, converted with `sqldiff convert --template`.
fn setup(d: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let (src, conv, manifest) = (d.join("src"), d.join("conv"), d.join("instances.toml"));
    write(&src.join("tables/entity.sql"), TABLE);
    write(&src.join("procs/touch.sql"), PROC);
    write(&src.join("procs/header.sql"), "-- generated\n");
    write(&manifest, MANIFEST);
    let o = run(&[
        "convert",
        src.to_str().unwrap(),
        "-o",
        conv.to_str().unwrap(),
        "--to",
        "postgres",
        "--template",
        "--pack",
        &pack(),
        "--isolated",
    ]);
    assert!(
        o.status.code().is_some(),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    (src, conv, manifest)
}

fn instances(d: &Path, extra: &[&str]) -> (std::process::Output, serde_json::Value) {
    let (src, conv, manifest) = (d.join("src"), d.join("conv"), d.join("instances.toml"));
    let out = d.join("out");
    let pack = pack();
    let mut args = vec![
        "instances",
        manifest.to_str().unwrap(),
        "--source",
        src.to_str().unwrap(),
        "--target",
        conv.to_str().unwrap(),
        "--to",
        "postgres",
        "-o",
        out.to_str().unwrap(),
        "--overwrite",
        "--pack",
        &pack,
        "--isolated",
    ];
    args.extend_from_slice(extra);
    let o = run(&args);
    let r = std::fs::read_to_string(out.join("instance-report.json"))
        .map(|t| serde_json::from_str(&t).unwrap())
        .unwrap_or(serde_json::Value::Null);
    (o, r)
}

fn instance<'a>(r: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    r["instances"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["name"] == name)
        .unwrap()
}

fn rules(i: &serde_json::Value) -> Vec<String> {
    i["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["rule"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn checks_every_instance_and_rolls_up_to_templates() {
    let d = scratch("rollup");
    setup(&d);
    let (o, r) = instances(&d, &[]);
    assert_eq!(
        o.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );

    // Woven with the dialect's own values, includes expanded.
    let woven = std::fs::read_to_string(d.join("out/target/procs/TouchCustomer.sql")).unwrap();
    assert!(woven.contains("-- generated\n"), "{woven}");
    assert!(
        woven.contains("UPDATE public.Customer SET Price = 0"),
        "{woven}"
    );
    let src = std::fs::read_to_string(d.join("out/source/procs/TouchCustomer.sql")).unwrap();
    assert!(src.contains("UPDATE dbo.Customer SET Price = 0"), "{src}");

    // The template assumptions are discharged by the instances.
    let t = instance(&r, "procs/TouchCustomer");
    assert_eq!(t["status"], "converted", "{t}");
    for rule in rules(t) {
        assert!(!rule.starts_with("ASM-TEMPLATE"), "{rule}");
    }
    // A value that is a reserved word in PostgreSQL.
    let order = instance(&r, "tables/Order");
    assert_eq!(order["status"], "differs");
    assert!(
        rules(order).contains(&"pg.reserved-name".to_string()),
        "{order}"
    );

    // The table template is as good as its worst instance.
    let t = r["templates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["path"] == "tables/entity.sql")
        .unwrap();
    assert_eq!(t["status"], "differs");
    assert_eq!(t["instances"], 2);
    assert!(
        r["not_exercised"].as_array().unwrap().is_empty(),
        "{}",
        r["not_exercised"]
    );
}

#[test]
fn the_converted_templates_status_lines_stand_until_resolved() {
    let d = scratch("markers");
    setup(&d);
    // A status line in a converted template that the instance cannot check.
    let conv = d.join("conv/procs/touch.sql");
    let text = std::fs::read_to_string(&conv).unwrap();
    let text = text.replacen(
        "  UPDATE",
        "  -- [sqldiff: under assumptions] ASM-CI-COLLATION, ASM-TEMPLATE-PLACEHOLDER\n  UPDATE",
        1,
    );
    std::fs::write(&conv, text).unwrap();
    let (_, r) = instances(&d, &[]);
    let t = instance(&r, "procs/TouchCustomer");
    assert_eq!(t["status"], "assumptions", "{t}");
    assert_eq!(rules(t), ["ASM-CI-COLLATION"]);
    // Accepted: nothing holds the instance back any more.
    let (_, r) = instances(&d, &["--accept", "ASM-CI-COLLATION"]);
    let t = instance(&r, "procs/TouchCustomer");
    assert_eq!(t["status"], "converted", "{t}");
    assert_eq!(r["accepted"][0], "ASM-CI-COLLATION");
}

#[test]
fn the_generator_output_proves_the_manifest() {
    let d = scratch("generator");
    setup(&d);
    let gen = d.join("gen");
    write(
        &gen.join("tables/Customer.sql"),
        "CREATE TABLE dbo.Customer (Id int NOT NULL, Price decimal(12, 2) NOT NULL);  \n\n",
    );
    // Generated by something the manifest does not describe.
    write(
        &gen.join("procs/TouchCustomer.sql"),
        "-- generated\nCREATE PROCEDURE dbo.TouchCustomer @Id int AS\nBEGIN\n  UPDATE dbo.Customer SET Price = 1 WHERE Id = @Id;\nEND\n",
    );
    write(&gen.join("extra/Unlisted.sql"), "SELECT 1;\n");
    let g = gen.to_str().unwrap().to_string();
    let (o, r) = instances(&d, &["--woven", &g]);
    assert_eq!(o.status.code(), Some(1));
    // Same text apart from trailing white space.
    assert_ne!(instance(&r, "tables/Customer")["status"], "not_woven");
    let t = instance(&r, "procs/TouchCustomer");
    assert_eq!(t["status"], "not_woven", "{t}");
    assert_eq!(t["items"][0]["rule"], "weave.generator-mismatch");
    assert_eq!(t["items"][0]["line"], 4);
    // No output file for Order; and output nobody accounts for.
    assert_eq!(instance(&r, "tables/Order")["status"], "not_woven");
    assert_eq!(r["not_in_manifest"][0], "extra/Unlisted.sql");
}

#[test]
fn generator_logic_is_never_guessed() {
    let d = scratch("logic");
    setup(&d);
    // A directive the pack does not name as its include form.
    let p = pack();
    let custom = d.join("pack.toml");
    let text = std::fs::read_to_string(Path::new(&p).join("pack.toml"))
        .unwrap()
        .replace("include = \"include\"\n", "");
    std::fs::write(&custom, text).unwrap();
    let (src, conv, manifest) = (d.join("src"), d.join("conv"), d.join("instances.toml"));
    let out = d.join("out");
    let o = run(&[
        "instances",
        manifest.to_str().unwrap(),
        "--source",
        src.to_str().unwrap(),
        "--target",
        conv.to_str().unwrap(),
        "--to",
        "postgres",
        "-o",
        out.to_str().unwrap(),
        "--pack",
        custom.to_str().unwrap(),
        "--isolated",
    ]);
    assert_eq!(
        o.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let r: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("instance-report.json")).unwrap())
            .unwrap();
    let t = instance(&r, "procs/TouchCustomer");
    assert_eq!(t["status"], "not_woven");
    assert!(
        t["items"][0]["text"]
            .as_str()
            .unwrap()
            .contains("directive `include`"),
        "{t}"
    );
}

#[test]
fn two_values_postgres_truncates_to_one_name_differ() {
    let d = scratch("truncate");
    setup(&d);
    let stem = "x".repeat(63);
    let manifest = format!(
        "[params]\nSCHEMA = \"s\"\n[[instance]]\nname = \"a\"\ntemplate = \"tables/entity.sql\"\nparams = {{ TABLE = \"{stem}_one\" }}\n[[instance]]\nname = \"b\"\ntemplate = \"tables/entity.sql\"\nparams = {{ TABLE = \"{stem}_two\" }}\n[[instance]]\nname = \"c\"\ntemplate = \"tables/entity.sql\"\nparams = {{ TABLE = \"{}\" }}\n",
        "y".repeat(70)
    );
    write(&d.join("instances.toml"), &manifest);
    let (_, r) = instances(&d, &[]);
    for n in ["a", "b"] {
        let i = instance(&r, n);
        assert_eq!(i["status"], "differs", "{i}");
        assert!(rules(i).contains(&"pg.name-collision".to_string()), "{i}");
    }
    // Truncated, but no other name becomes the same.
    let c = instance(&r, "c");
    assert_eq!(c["status"], "assumptions", "{c}");
    assert_eq!(rules(c), ["ASM-PG-NAME-TRUNCATED"]);
}

#[test]
fn what_the_diff_cannot_decide_holds_an_instance_back() {
    let d = scratch("unknown");
    setup(&d);
    // A hand edit that changes what the procedure updates.
    let conv = d.join("conv/procs/touch.sql");
    let text = std::fs::read_to_string(&conv).unwrap();
    assert!(text.contains("WHERE Id = p_id"), "{text}");
    std::fs::write(&conv, text.replace("WHERE Id = p_id", "WHERE Id <> p_id")).unwrap();
    let (o, r) = instances(&d, &[]);
    assert_eq!(o.status.code(), Some(1));
    let t = instance(&r, "procs/TouchCustomer");
    assert_ne!(t["status"], "converted", "{t}");
    assert_ne!(t["status"], "assumptions", "{t}");
}
