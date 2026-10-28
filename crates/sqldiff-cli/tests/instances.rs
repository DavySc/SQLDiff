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

/// Converted, except that the T-SQL procedure sends rows-affected counts and
/// PostgreSQL does not: decision 1's observer profile lists that as the
/// one open assumption.
fn converted_but_row_counts(i: &serde_json::Value) {
    assert_eq!(i["status"], "assumptions", "{i}");
    // Once for the pair, and on each unit's status line in the converted
    // template, which carries the conversion's own pair assumptions.
    assert_eq!(rule_set(i), ["ASM-OBSERVER-ROWCOUNT"], "{i}");
}

fn rules(i: &serde_json::Value) -> Vec<String> {
    i["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["rule"].as_str().unwrap().to_string())
        .collect()
}

/// The distinct rules of an instance's items, sorted.
fn rule_set(i: &serde_json::Value) -> Vec<String> {
    let set: std::collections::BTreeSet<String> = rules(i).into_iter().collect();
    set.into_iter().collect()
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
    converted_but_row_counts(t);
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
    // The rows-affected counts the T-SQL procedure sends (decision 1).
    assert_eq!(rule_set(t), ["ASM-CI-COLLATION", "ASM-OBSERVER-ROWCOUNT"]);
    // Accepted: nothing holds the instance back any more.
    let (_, r) = instances(
        &d,
        &[
            "--accept",
            "ASM-CI-COLLATION",
            "--accept",
            "ASM-OBSERVER-ROWCOUNT",
        ],
    );
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
    let text = std::fs::read_to_string(Path::new(&p).join("pack.toml")).unwrap();
    // Line by line, so a CRLF checkout drops the line too.
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| l.trim() != "include = \"include\"")
        .collect();
    assert_eq!(kept.len() + 1, text.lines().count(), "one include line");
    std::fs::write(&custom, kept.join("\n") + "\n").unwrap();
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
    // The table is the one the conversion rules write from the source.
    let i = instance(&r, "table");
    assert_eq!(i["status"], "converted", "table: {i}");
    converted_but_row_counts(instance(&r, "save"));
    // `SELECT Id` returns `Id` in T-SQL and `id` in PostgreSQL (decision 1:
    // result column names are observed), and nothing else is open.
    let i = instance(&r, "list");
    assert_eq!(i["status"], "assumptions", "{i}");
    let items: Vec<&str> = i["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x["rule"].as_str())
        .collect();
    assert_eq!(items, ["ASM-RESULT-NAMES-CI"], "{i}");
    let woven = std::fs::read_to_string(d.join("out/target/save.sql")).unwrap();
    assert!(woven.contains("(p_key integer)"), "{woven}");
    // A type given once for every dialect is an assumption.
    let same = instance(&r, "save-same");
    assert_eq!(same["status"], "assumptions", "{same}");
    assert_eq!(
        rule_set(same),
        ["ASM-OBSERVER-ROWCOUNT", "ASM-TEMPLATE-VALUE-SAME"]
    );
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
    converted_but_row_counts(instance(&r, "procs/TouchCustomer"));
}

#[test]
fn a_table_counts_only_as_the_rules_write_it() {
    // The diff does not compare tables: a table counts only when it is the
    // one the conversion rules write, so a wrong per-dialect type is never
    // converted (cross-review on #66).
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
    // The engine compares the tables column by column: another type is a
    // difference.
    for (value, want) in [
        ("boolean", "differs"),
        ("varchar(5)", "differs"),
        ("smallint", "differs"),
        // What the rules write for INT.
        ("integer", "converted"),
    ] {
        write(
            &d.join("instances.toml"),
            &format!(
                "[params]\nSCHEMA = \"dbo\"\nTABLE = \"Item\"\nKEYTYPE = \"INT\"\n[params_for.postgres]\nSCHEMA = \"public\"\nKEYTYPE = \"{value}\"\n\n[[instance]]\nname = \"t\"\ntemplate = \"q/table.sql\"\n"
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
        assert_eq!(
            o.status.code(),
            Some(if want == "converted" { 0 } else { 1 }),
            "{value}"
        );
        let r: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(out.join("instance-report.json")).unwrap(),
        )
        .unwrap();
        let t = instance(&r, "t");
        assert_eq!(t["status"], want, "{value}: {t}");
        if want == "differs" {
            assert!(rules(t).contains(&"equiv.column-type".to_string()), "{t}");
        }
    }
}

#[test]
fn a_table_in_another_schema_differs() {
    let d = scratch("ddl-schema");
    setup(&d);
    // The target names a schema the source table is not in.
    let m = d.join("instances.toml");
    let text = std::fs::read_to_string(&m).unwrap();
    std::fs::write(
        &m,
        text.replace("SCHEMA = \"public\"", "SCHEMA = \"sales\""),
    )
    .unwrap();
    let (_, r) = instances(&d, &[]);
    let t = instance(&r, "tables/Customer");
    assert_eq!(t["status"], "differs", "{t}");
    assert!(rules(t).contains(&"equiv.table-name".to_string()), "{t}");
}

/// Customer and TouchCustomer only: both convert.
const TWO: &str = r#"
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
"#;

/// Generator output equal to SQLDiff's weave on both sides, in `gens/` and
/// `gent/`, as a generator weaving the converted templates unchanged writes.
fn generator_output(d: &Path) -> (String, String) {
    setup(d);
    write(&d.join("instances.toml"), TWO);
    let (o, _) = instances(d, &[]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let (gens, gent) = (d.join("gens"), d.join("gent"));
    for (from, to) in [("source", &gens), ("target", &gent)] {
        for f in ["tables/Customer.sql", "procs/TouchCustomer.sql"] {
            let text = std::fs::read_to_string(d.join("out").join(from).join(f)).unwrap();
            write(&to.join(f), &text);
        }
    }
    (
        gens.to_str().unwrap().to_string(),
        gent.to_str().unwrap().to_string(),
    )
}

/// Mutations of the generator's target output: (file, old, new).
const MUTATIONS: [(&str, &str, &str); 11] = [
    ("procs/TouchCustomer.sql", "SET Price = 0", "SET Price = 1"),
    (
        "procs/TouchCustomer.sql",
        "WHERE Id = p_id",
        "WHERE Id <> p_id",
    ),
    ("procs/TouchCustomer.sql", " WHERE Id = p_id", ""),
    (
        "procs/TouchCustomer.sql",
        "WHERE Id = p_id;",
        "WHERE Id = p_id;\n  UPDATE public.Customer SET Price = 0;",
    ),
    ("procs/TouchCustomer.sql", "p_id integer", "p_id text"),
    ("procs/TouchCustomer.sql", "END;", "EN;"),
    (
        "procs/TouchCustomer.sql",
        "UPDATE public.Customer",
        "UPDATE public.Client",
    ),
    ("tables/Customer.sql", "numeric(12, 2)", "numeric(12, 3)"),
    (
        "tables/Customer.sql",
        "Price numeric(12, 2) NOT NULL",
        "Price numeric(12, 2)",
    ),
    (
        "tables/Customer.sql",
        ",\n    Price numeric(12, 2) NOT NULL",
        "",
    ),
    ("tables/Customer.sql", "public.Customer", "public.Client"),
];

fn mutate(dir: &Path, file: &str, old: &str, new: &str) -> String {
    let p = dir.join(file);
    let text = std::fs::read_to_string(&p).unwrap();
    assert!(text.contains(old), "{file}: no `{old}` in {text}");
    std::fs::write(&p, text.replacen(old, new, 1)).unwrap();
    text
}

fn name_of(file: &str) -> &str {
    file.trim_end_matches(".sql")
}

#[test]
fn the_generators_target_output_must_be_the_target_weave() {
    let d = scratch("woven-target");
    let (gens, gent) = generator_output(&d);
    let (o, r) = instances(&d, &["--woven", &gens, "--woven-target", &gent]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    for i in r["instances"].as_array().unwrap() {
        assert_eq!(i["done"], true, "{i}");
        assert!(i["weave_only"].as_array().unwrap().is_empty(), "{i}");
    }
    assert_eq!(r["done"], 2);
    assert_eq!(r["complete"], true, "{r}");

    // Any change to the generator's target output is not the weave.
    let gent = PathBuf::from(gent);
    for (file, old, new) in MUTATIONS {
        let text = mutate(&gent, file, old, new);
        let (o, r) = instances(
            &d,
            &["--woven", &gens, "--woven-target", gent.to_str().unwrap()],
        );
        assert_eq!(o.status.code(), Some(1), "{file}: {new}");
        let i = instance(&r, name_of(file));
        assert_eq!(i["status"], "not_woven", "{file}: {new}: {i}");
        assert_eq!(i["done"], false);
        assert_eq!(
            i["items"][0]["rule"], "weave.generator-target-mismatch",
            "{i}"
        );
        assert_eq!(r["complete"], false);
        std::fs::write(gent.join(file), text).unwrap();
    }

    // Trailing white space is not a difference.
    let p = gent.join("tables/Customer.sql");
    let text = std::fs::read_to_string(&p).unwrap();
    std::fs::write(&p, text.replace(");", ");   ") + "\n\n").unwrap();
    let (o, r) = instances(
        &d,
        &["--woven", &gens, "--woven-target", gent.to_str().unwrap()],
    );
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(instance(&r, "tables/Customer")["done"], true);

    // No target output: checked on SQLDiff's weave only, never done.
    std::fs::remove_file(&p).unwrap();
    write(&gent.join("extra/Unlisted.sql"), "SELECT 1;\n");
    let (o, r) = instances(
        &d,
        &["--woven", &gens, "--woven-target", gent.to_str().unwrap()],
    );
    assert_eq!(o.status.code(), Some(1));
    let c = instance(&r, "tables/Customer");
    assert_eq!(c["status"], "converted", "{c}");
    assert_eq!(c["done"], false);
    assert_eq!(c["weave_only"], serde_json::json!(["target"]));
    assert_eq!(r["not_in_manifest_target"][0], "extra/Unlisted.sql");
    let t = r["templates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["path"] == "tables/entity.sql")
        .unwrap();
    assert_eq!(t["weave_only"], 1);
    assert_eq!(t["done"], false);
    let md = std::fs::read_to_string(d.join("out/instance-report.md")).unwrap();
    assert!(
        md.contains("`tables/Customer`: converted, checked on SQLDiff's weave only (no generator target output)"),
        "{md}"
    );

    // Without generator output nothing is done, and the exit code is as
    // before.
    let (o, r) = instances(&d, &[]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(r["done"], 0);
    assert_eq!(r["weave_only"], 2);
    assert_eq!(r["complete"], false);
    assert_eq!(
        instance(&r, "procs/TouchCustomer")["weave_only"],
        serde_json::json!(["source", "target"])
    );
}

fn generator_only(d: &Path, gens: &str, gent: &str) -> (std::process::Output, serde_json::Value) {
    let (manifest, out) = (d.join("instances.toml"), d.join("gout"));
    let pack = pack();
    let o = run(&[
        "instances",
        manifest.to_str().unwrap(),
        "--generator-only",
        "--woven",
        gens,
        "--woven-target",
        gent,
        "--to",
        "postgres",
        "-o",
        out.to_str().unwrap(),
        "--overwrite",
        "--pack",
        &pack,
        "--isolated",
    ]);
    let r = std::fs::read_to_string(out.join("instance-report.json"))
        .map(|t| serde_json::from_str(&t).unwrap())
        .unwrap_or(serde_json::Value::Null);
    (o, r)
}

#[test]
fn the_generators_own_output_is_checked_alone() {
    let d = scratch("generator-only");
    let (gens, gent) = generator_output(&d);
    let (o, r) = generator_only(&d, &gens, &gent);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(r["generator_only"], true);
    for i in r["instances"].as_array().unwrap() {
        // The one procedure sends rows-affected counts; everything else is
        // plainly converted.
        if i["name"] == "procs/TouchCustomer" {
            converted_but_row_counts(i);
        } else {
            assert_eq!(i["status"], "converted", "{i}");
        }
        assert_eq!(i["done"], true, "{i}");
    }
    // Mapped to the templates the manifest names.
    let paths: Vec<&str> = r["templates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["procs/touch.sql", "tables/entity.sql"]);

    // Every mutation of the target output is caught by the checks.
    let gent_dir = PathBuf::from(&gent);
    for (file, old, new) in MUTATIONS {
        let text = mutate(&gent_dir, file, old, new);
        let (o, r) = generator_only(&d, &gens, &gent);
        assert_eq!(o.status.code(), Some(1), "{file}: {new}");
        let i = instance(&r, name_of(file));
        let status = i["status"].as_str().unwrap();
        assert!(
            ["differs", "unknown"].contains(&status),
            "{file}: {new}: {i}"
        );
        assert_eq!(i["done"], false);
        // The template inherits the worst instance.
        let t = r["templates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| i["templates"][0] == t["path"])
            .unwrap();
        assert_eq!(t["status"], status);
        std::fs::write(gent_dir.join(file), text).unwrap();
    }

    // A wider parameter type is equal only under an assumption, which counts.
    let text = mutate(
        &gent_dir,
        "procs/TouchCustomer.sql",
        "p_id integer",
        "p_id bigint",
    );
    let (_, r) = generator_only(&d, &gens, &gent);
    let i = instance(&r, "procs/TouchCustomer");
    assert_eq!(i["status"], "assumptions", "{i}");
    assert!(
        rules(i).contains(&"ASM-ARGUMENT-LENGTHS".to_string()),
        "{i}"
    );
    std::fs::write(gent_dir.join("procs/TouchCustomer.sql"), text).unwrap();

    // No target output: nothing to check.
    std::fs::remove_file(gent_dir.join("tables/Customer.sql")).unwrap();
    let (o, r) = generator_only(&d, &gens, &gent);
    assert_eq!(o.status.code(), Some(1));
    let c = instance(&r, "tables/Customer");
    assert_eq!(c["status"], "not_woven", "{c}");
    assert_eq!(c["done"], false);

    // The mode takes no templates and needs both outputs.
    let o = run(&[
        "instances",
        d.join("instances.toml").to_str().unwrap(),
        "--generator-only",
        "--woven",
        &gens,
        "--to",
        "postgres",
        "-o",
        d.join("x").to_str().unwrap(),
    ]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn the_generators_names_are_checked_where_values_are() {
    let d = scratch("generator-names");
    let (gens, gent) = (d.join("gens"), d.join("gent"));
    let long = format!("Customer{}", "X".repeat(60));
    write(
        &d.join("instances.toml"),
        &format!(
            r#"
[[instance]]
name = "tables/Order"
template = "tables/entity.sql"
params = {{ TABLE = "Order" }}

[[instance]]
name = "tables/Quoted"
template = "tables/entity.sql"
params = {{ TABLE = "Order" }}

[[instance]]
name = "tables/Long"
template = "tables/entity.sql"
params = {{ TABLE = "{long}" }}
"#
        ),
    );
    let table = |name: &str| format!("CREATE TABLE {name} (Id int NOT NULL);\n");
    let pg = |name: &str| format!("CREATE TABLE {name} (\n    Id integer NOT NULL\n);\n");
    write(&gens.join("tables/Order.sql"), &table("[Order]"));
    write(&gent.join("tables/Order.sql"), &pg("Order"));
    write(&gens.join("tables/Quoted.sql"), &table("[Order]"));
    write(&gent.join("tables/Quoted.sql"), &pg("\"Order\""));
    write(&gens.join("tables/Long.sql"), &table(&long));
    write(&gent.join("tables/Long.sql"), &pg(&long));
    let (_, r) = generator_only(&d, gens.to_str().unwrap(), gent.to_str().unwrap());
    let o = instance(&r, "tables/Order");
    assert_eq!(o["status"], "differs", "{o}");
    assert!(rules(o).contains(&"pg.reserved-name".to_string()), "{o}");
    // A keyword is not a name, a quoted name is not reserved.
    let q = instance(&r, "tables/Quoted");
    assert!(!rules(q).contains(&"pg.reserved-name".to_string()), "{q}");
    let l = instance(&r, "tables/Long");
    assert!(
        rules(l).contains(&"ASM-PG-NAME-TRUNCATED".to_string()),
        "{l}"
    );
}

fn instances_to(d: &Path, out: &str, extra: &[&str]) -> serde_json::Value {
    let (src, conv, manifest) = (d.join("src"), d.join("conv"), d.join("instances.toml"));
    let out = d.join(out);
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
    assert!(
        o.status.code().is_some_and(|c| c < 2),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    serde_json::from_str(&std::fs::read_to_string(out.join("instance-report.json")).unwrap())
        .unwrap()
}

/// A report without the count of reused results.
fn results(mut r: serde_json::Value) -> serde_json::Value {
    r.as_object_mut().unwrap().remove("reused");
    r
}

#[test]
fn a_rerun_since_a_report_checks_only_what_changed_and_agrees_with_a_full_run() {
    let d = scratch("since");
    setup(&d);
    // Two long values PostgreSQL truncates to one name: the collision is
    // worked out across instances, reused or not.
    let long = "X".repeat(64);
    let mut m = MANIFEST.to_string();
    for n in ["A", "B"] {
        m.push_str(&format!(
            "\n[[instance]]\nname = \"tables/Long{n}\"\ntemplate = \"tables/entity.sql\"\nparams = {{ TABLE = \"{long}{n}\" }}\n"
        ));
    }
    write(&d.join("instances.toml"), &m);
    let first = instances_to(&d, "run0", &[]);
    assert_eq!(first["reused"], 0);
    assert!(
        rules(instance(&first, "tables/LongA")).contains(&"pg.name-collision".to_string()),
        "{first}"
    );

    // Nothing changed: everything reused, the same results.
    let again = instances_to(&d, "run1", &["--since", d.join("run0").to_str().unwrap()]);
    assert_eq!(again["reused"], 5);
    assert_eq!(results(again), results(first.clone()));

    let since = |prev: &str| d.join(prev).to_str().unwrap().to_string();
    let edit = |file: &str, old: &str, new: &str| {
        let p = d.join(file);
        let t = std::fs::read_to_string(&p).unwrap();
        assert!(t.contains(old), "{file}: {t}");
        std::fs::write(&p, t.replacen(old, new, 1)).unwrap();
    };
    // Each step: a full run and a run since the previous step's report
    // agree, and only the instances named were checked again.
    let steps: [(&str, &str, &str, &[&str], usize); 4] = [
        // A converted template.
        (
            "conv/procs/touch.sql",
            "SET Price = 0",
            "SET Price = 1",
            &[],
            4,
        ),
        // An include of the source templates.
        (
            "src/procs/header.sql",
            "-- generated",
            "SET NOCOUNT ON;",
            &[],
            4,
        ),
        // A table: the catalog changes, so every instance is checked.
        (
            "conv/tables/entity.sql",
            "Price numeric(12, 2)",
            "Price numeric(12, 3)",
            &[],
            0,
        ),
        // An accepted assumption changes what every check counts.
        (
            "conv/procs/touch.sql",
            "SET Price = 1",
            "SET Price = 0",
            &["--accept", "ASM-X"],
            0,
        ),
    ];
    let mut prev = "run1".to_string();
    for (n, (file, old, new, extra, reused)) in steps.into_iter().enumerate() {
        edit(file, old, new);
        let full = instances_to(&d, &format!("full{n}"), extra);
        let mut args = extra.to_vec();
        let p = since(&prev);
        args.extend(["--since", p.as_str()]);
        let inc = instances_to(&d, &format!("inc{n}"), &args);
        assert_eq!(inc["reused"], reused, "step {n}: {file}");
        assert_eq!(results(inc), results(full), "step {n}: {file}");
        prev = format!("inc{n}");
    }

    // In place: the output directory is its own previous run.
    let p = since(&prev);
    let inc = instances_to(&d, &prev, &["--accept", "ASM-X", "--since", &p]);
    assert_eq!(inc["reused"], 5);
}

#[test]
fn the_done_gate_reads_the_instance_check_of_these_trees() {
    let d = scratch("gate");
    let (src, conv, _) = setup(&d);
    let (o, r) = instances(&d, &[]);
    assert!(
        o.status.code().is_some(),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    // No generator output given: checked on SQLDiff's weave only.
    assert_eq!(r["complete"], false, "{r:#}");
    let pack = pack();
    let gate = |extra: &[&str]| {
        let mut args = vec![
            "review-status",
            src.to_str().unwrap(),
            conv.to_str().unwrap(),
            "--to",
            "postgres",
            "--template",
            "--pack",
            &pack,
            "--isolated",
        ];
        args.extend_from_slice(extra);
        run(&args)
    };
    let report = d.join("out");
    let o = gate(&[
        "--instances",
        report.to_str().unwrap(),
        "--require-complete",
    ]);
    let text = String::from_utf8_lossy(&o.stdout);
    assert_eq!(
        o.status.code(),
        Some(1),
        "{text}{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(text.contains("Review not complete"), "{text}");
    assert!(
        text.contains("the instance check is not complete"),
        "{text}"
    );
    // The report was made on these very trees.
    assert!(!text.contains("changed since the instance check"), "{text}");
    assert!(!text.contains("stale-output"), "{text}");
    // Templates without the instance check are never complete.
    let o = gate(&["--json"]);
    assert_eq!(o.status.code(), Some(0));
    let g: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(g["complete"], false);
    assert!(g["counts"]["instances"].as_u64().unwrap() >= 1, "{g:#}");
    // A converted template changed after the check: the report is stale.
    let f = conv.join("tables/entity.sql");
    let t = std::fs::read_to_string(&f).unwrap();
    std::fs::write(&f, t + "\n").unwrap();
    let o = gate(&["--instances", report.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&o.stdout);
    assert!(text.contains("changed since the instance check"), "{text}");
    assert!(text.contains("stale-output"), "{text}");
}

#[test]
fn a_template_unit_written_by_hand_passes_the_gate_only_on_a_complete_instance_check() {
    use sqldiff_convert::review::{By, Record, State, LEDGER_FILE};
    let d = scratch("gate-hand");
    generator_output(&d);
    let (src, conv) = (d.join("src"), d.join("conv"));
    let pack = pack();
    let report: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(conv.join("conversion-report.json")).unwrap(),
    )
    .unwrap();
    let f = report["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "procs/touch.sql")
        .unwrap();
    let u = f["units"]
        .as_array()
        .unwrap()
        .iter()
        .chain(f["converted_units"].as_array().unwrap())
        .find(|u| u["src_line"] == 3)
        .unwrap_or_else(|| panic!("{f:#}"));
    let out = std::fs::read_to_string(conv.join("procs/touch.sql")).unwrap();
    let line = out.lines().find(|l| l.contains("SET Price = 0")).unwrap();
    let text = line.replace("WHERE Id = p_id", "WHERE p_id = Id");
    let r = Record {
        unit: u["id"].as_str().unwrap().into(),
        path: "procs/touch.sql".into(),
        rule: String::new(),
        state: State::Written,
        by: By::Human,
        author: "Davy".into(),
        reason: "operands swapped".into(),
        at: "2026-10-26T12:00:00Z".into(),
        input_hash: String::new(),
        output_hash: String::new(),
        text: Some(text.clone()),
        base_catalogs: None,
    };
    std::fs::write(
        conv.join(LEDGER_FILE),
        serde_json::to_string(&r).unwrap() + "\n",
    )
    .unwrap();
    let o = run(&[
        "convert",
        src.to_str().unwrap(),
        "-o",
        conv.to_str().unwrap(),
        "--to",
        "postgres",
        "--template",
        "--overwrite",
        "--pack",
        &pack,
        "--isolated",
    ]);
    assert!(
        o.status.code().is_some(),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(std::fs::read_to_string(conv.join("procs/touch.sql"))
        .unwrap()
        .contains(&text));
    // The generator weaves the new template: its target output follows.
    let (o, _) = instances(&d, &[]);
    assert!(o.status.code().is_some());
    let gent = d.join("gent/procs/TouchCustomer.sql");
    std::fs::copy(d.join("out/target/procs/TouchCustomer.sql"), &gent).unwrap();
    assert!(std::fs::read_to_string(&gent)
        .unwrap()
        .contains("WHERE p_id = Id"));
    let gens = d.join("gens");
    let (o, r) = instances(
        &d,
        &[
            "--woven",
            gens.to_str().unwrap(),
            "--woven-target",
            d.join("gent").to_str().unwrap(),
        ],
    );
    assert_eq!(
        r["complete"],
        true,
        "{r:#}{}",
        String::from_utf8_lossy(&o.stderr)
    );

    let gate = |extra: &[&str]| -> serde_json::Value {
        let mut args = vec![
            "review-status",
            src.to_str().unwrap(),
            conv.to_str().unwrap(),
            "--to",
            "postgres",
            "--template",
            "--pack",
            &pack,
            "--isolated",
            "--json",
        ];
        args.extend_from_slice(extra);
        let o = run(&args);
        assert_eq!(
            o.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&o.stderr)
        );
        serde_json::from_slice(&o.stdout).unwrap()
    };
    let unchecked = |g: &serde_json::Value| {
        g["blockers"].as_array().unwrap().iter().any(|b| {
            b["what"]
                .as_str()
                .unwrap()
                .contains("hand-written.unchecked")
        })
    };
    let instances_dir = d.join("out");
    let with = gate(&["--instances", instances_dir.to_str().unwrap()]);
    assert_eq!(with["instances"], true, "{with:#}");
    assert!(!unchecked(&with), "{with:#}");
    let without = gate(&[]);
    assert!(unchecked(&without), "{without:#}");
}
