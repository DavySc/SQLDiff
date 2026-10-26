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
    assert!(
        rules(c).contains(&"ASM-PG-NAME-TRUNCATED".to_string()),
        "{c}"
    );
    assert!(!rules(c).contains(&"pg.name-collision".to_string()), "{c}");
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

/// A pack that declares value kinds, fragment kinds and a head/foot pair.
const KINDS_PACK: &str = r#"
[pack]
name = "kinds"
version = "0.1.0"
schema = 1

[template]
open = "<%"
close = "%>"
name_pattern = "[A-Z_][A-Z0-9_]*"
special_forms = [{ name = "include", pattern = "<%@include\\s+(\\S+)\\s*%>" }]
include = "include"
values = { KEYTYPE = "type" }
fragments = [{ glob = "where/**", kind = "predicate" }, { glob = "parts/body*.sql", kind = "routine-body" }]
pairs = [{ head = "parts/head.sql", foot = "parts/foot.sql" }]
"#;

const KINDS_MANIFEST: &str = r#"
[params]
SCHEMA = "dbo"
TABLE = "Item"
[params_for.postgres]
SCHEMA = "public"

[[instance]]
name = "table"
template = "q/table.sql"

[[instance]]
name = "list"
template = "q/list.sql"

[[instance]]
name = "save"
parts = ["parts/head.sql", "parts/body.sql", "parts/foot.sql"]
params = { KEYTYPE = "int" }
params_for.postgres = { KEYTYPE = "integer" }

[[instance]]
name = "table-other"
template = "q/table.sql"
params = { TABLE = "Other" }

[[instance]]
name = "save-same"
parts = ["parts/head.sql", "parts/body.sql", "parts/foot.sql"]
params = { KEYTYPE = "int", TABLE = "Other" }
"#;

#[test]
fn fragments_pairs_and_value_kinds() {
    let d = scratch("kinds");
    let (src, conv) = (d.join("src"), d.join("conv"));
    let pack = d.join("pack.toml");
    write(&pack, KINDS_PACK);
    write(&d.join("instances.toml"), KINDS_MANIFEST);
    write(
        &src.join("q/table.sql"),
        "CREATE TABLE <%SCHEMA%>.<%TABLE%> (Id int NOT NULL, Price int NOT NULL);\n",
    );
    write(
        &src.join("q/list.sql"),
        "SELECT Id FROM <%SCHEMA%>.<%TABLE%> WHERE Id > 0 AND <%@include where/positive.sql %>;\n",
    );
    write(&src.join("where/positive.sql"), "Price > 0\n");
    write(
        &src.join("parts/head.sql"),
        "CREATE PROCEDURE <%SCHEMA%>.Save<%TABLE%> @Key <%KEYTYPE%> AS\nBEGIN\n",
    );
    write(
        &src.join("parts/body.sql"),
        "  UPDATE <%SCHEMA%>.<%TABLE%> SET Price = 0 WHERE Price < 0;\n",
    );
    write(&src.join("parts/foot.sql"), "END\n");
    let o = run(&[
        "convert",
        src.to_str().unwrap(),
        "-o",
        conv.to_str().unwrap(),
        "--to",
        "postgres",
        "--template",
        "--pack",
        pack.to_str().unwrap(),
        "--isolated",
    ]);
    assert!(
        o.status.code().is_some(),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let read = |p: &str| std::fs::read_to_string(conv.join(p)).unwrap();
    // A predicate converts as a predicate, with the template's own edges.
    assert_eq!(read("where/positive.sql").lines().last(), Some("Price > 0"));
    // A routine body converts inside a stand-in routine.
    let body = read("parts/body.sql");
    assert!(
        body.ends_with("  UPDATE <%SCHEMA%>.<%TABLE%> SET Price = 0 WHERE Price < 0;\n"),
        "{body}"
    );
    // Head and foot convert as one routine, split again.
    let head = read("parts/head.sql");
    assert!(
        head.contains("PROCEDURE <%SCHEMA%>.Save<%TABLE%>(p_key <%KEYTYPE%>)"),
        "{head}"
    );
    // A type value cannot be converted by the rules: a decision.
    assert!(head.contains("template.value-per-dialect"), "{head}");
    assert!(
        read("parts/foot.sql").contains("$$"),
        "{}",
        read("parts/foot.sql")
    );

    let (manifest, out) = (d.join("instances.toml"), d.join("out"));
    let args = [
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
        pack.to_str().unwrap(),
        "--isolated",
    ];
    let o = run(&args);
    let r: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(d.join("out/instance-report.json")).unwrap())
            .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&o.stderr)));
    // The woven instances discharge the fragment and decision notes.
    for name in ["list", "save"] {
        let i = instance(&r, name);
        assert_eq!(i["status"], "converted", "{name}: {i}");
    }
    // The engine does not compare DDL: not decided, never converted.
    let table = instance(&r, "table");
    assert_eq!(table["status"], "unknown", "{table}");
    assert!(
        rules(table).iter().all(|r| r == "equiv.not-covered"),
        "{table}"
    );
    let woven = std::fs::read_to_string(d.join("out/target/save.sql")).unwrap();
    assert!(woven.contains("(p_key integer)"), "{woven}");
    // A type given once for every dialect is an assumption.
    let same = instance(&r, "save-same");
    assert_eq!(same["status"], "assumptions", "{same}");
    assert_eq!(rules(same), ["ASM-TEMPLATE-VALUE-SAME"]);
}

#[test]
fn an_object_two_instances_create_is_not_decided() {
    let d = scratch("twice");
    setup(&d);
    let m = d.join("instances.toml");
    let text = std::fs::read_to_string(&m).unwrap();
    std::fs::write(
        &m,
        format!("{text}\n[[instance]]\nname = \"tables/Customer2\"\ntemplate = \"tables/entity.sql\"\nparams = {{ TABLE = \"Customer\" }}\n"),
    )
    .unwrap();
    let (_, r) = instances(&d, &[]);
    for name in ["tables/Customer", "tables/Customer2"] {
        let i = instance(&r, name);
        assert_eq!(i["status"], "unknown", "{i}");
        assert!(rules(i).contains(&"weave.object-twice".to_string()), "{i}");
    }
    assert_eq!(instance(&r, "procs/TouchCustomer")["status"], "converted");
}

#[test]
fn a_type_value_per_dialect_in_ddl_is_not_decided() {
    // A wrong per-dialect type is never checked by the diff: the table
    // instance must not come out converted (cross-review on #66).
    let d = scratch("ddl-type");
    let (src, conv) = (d.join("src"), d.join("conv"));
    // No pairs: this tree has no routine halves.
    let pack_text: String = KINDS_PACK
        .lines()
        .filter(|l| !l.starts_with("pairs"))
        .map(|l| format!("{l}\n"))
        .collect();
    write(&d.join("pack.toml"), &pack_text);
    write(
        &src.join("q/table.sql"),
        "CREATE TABLE <%SCHEMA%>.<%TABLE%> (Id <%KEYTYPE%> NOT NULL, Price int NOT NULL);\n",
    );
    let pack = d.join("pack.toml");
    let o = run(&[
        "convert",
        src.to_str().unwrap(),
        "-o",
        conv.to_str().unwrap(),
        "--to",
        "postgres",
        "--template",
        "--pack",
        pack.to_str().unwrap(),
        "--isolated",
    ]);
    assert!(
        conv.join("q/table.sql").is_file(),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    for wrong in ["boolean", "varchar(5)", "smallint"] {
        write(
            &d.join("instances.toml"),
            &format!(
                "[params]\nSCHEMA = \"dbo\"\nTABLE = \"Item\"\nKEYTYPE = \"INT\"\n[params_for.postgres]\nSCHEMA = \"public\"\nKEYTYPE = \"{wrong}\"\n\n[[instance]]\nname = \"t\"\ntemplate = \"q/table.sql\"\n"
            ),
        );
        let (manifest, out) = (d.join("instances.toml"), d.join("out"));
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
            "--overwrite",
            "--pack",
            pack.to_str().unwrap(),
            "--isolated",
        ]);
        assert_eq!(o.status.code(), Some(1), "{wrong}");
        let r: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(out.join("instance-report.json")).unwrap(),
        )
        .unwrap();
        let t = instance(&r, "t");
        assert_eq!(t["status"], "unknown", "{wrong}: {t}");
        assert!(rules(t).contains(&"equiv.not-covered".to_string()), "{t}");
    }
}
