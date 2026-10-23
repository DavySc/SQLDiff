//! Packs, layered config, pairing, bundles and the fs and git adapters.

use std::path::{Path, PathBuf};
use std::process::Command;

use sqldiff_core::model::AssumptionSource;
use sqldiff_core::{ChangeClass, Severity, Verdict};
use sqldiff_source::bundle::CaptureOptions;
use sqldiff_source::config::{cli_layer, ConfigLayer, LayerKind, Sources};
use sqldiff_source::protocol::{ObjectFilter, PairScope};
use sqldiff_source::{
    pairing, policy, Bundle, BundleSource, FsSource, GitSource, Resolved, Source,
};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sqldiff-source-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

const TSQL: &str = "CREATE PROCEDURE dbo.usp_GetStock @id int AS\nBEGIN\n  SELECT qty FROM stock WHERE id = @id;\nEND\n";
const PG: &str = "CREATE OR REPLACE FUNCTION get_stock(p_id int) RETURNS int LANGUAGE sql AS $$\n  SELECT qty FROM stock WHERE id = p_id;\n$$;\n";

const PACK: &str = r#"
deny_list = ["example-secret-term"]

[pack]
name = "example-conventions"
version = "1.0.0"

[[pairing.rules]]
name = "mssql-prefix"
dialect = "tsql"
strip_prefix = ["usp_"]

[[pairing.pairs]]
a = "mssql/odd_name.sql"
b = "pg/renamed.sql"

[pairing]
shared = ["common/**"]

[[dialect_map]]
glob = "mssql/**"
dialect = "tsql"

[[dialect_map]]
glob = "pg/**"
dialect = "postgres"

[dialect_versions]
tsql = "2019"

[[limits]]
id = "no-cursor"
message = "Cursors are not allowed"
dialects = ["tsql"]
pattern = "(?i)\\bcursor\\b"
severity = "high"

[[overrides]]
rule = "limit.*"
severity = "critical"

[assumptions]
ASM-EXAMPLE = "Declared by the example pack"

[[change_patterns]]
name = "ticket"
pattern = "\\b(EX-[0-9]+)\\b"
"#;

fn tree(name: &str) -> PathBuf {
    let root = tmp(name);
    write(&root, "mssql/usp_GetStock.sql", TSQL);
    write(&root, "pg/get_stock.sql", PG);
    write(&root, "mssql/odd_name.sql", TSQL);
    write(&root, "pg/renamed.sql", PG);
    write(
        &root,
        "mssql/only_here.sql",
        "CREATE VIEW v AS SELECT 1 AS x\n",
    );
    write(&root, "common/shared.tsql.sql", "SELECT 1\n");
    write(&root, "notes/readme.sql", "SELECT 1\n");
    write(&root, "pack/pack.toml", PACK);
    root
}

fn config(root: &Path) -> Resolved {
    Resolved::load(&Sources {
        isolated: true,
        packs: vec![root.join("pack")],
        ..Sources::default()
    })
    .unwrap()
}

#[test]
fn fs_pairs_by_pack_rules() {
    let root = tree("fs_pairs_by_pack_rules");
    let cfg = config(&root);
    let src = FsSource::new(&root, &cfg).unwrap();
    let objects = src.list_objects(&ObjectFilter::default()).unwrap();
    let get = |id: &str| objects.iter().find(|o| o.id == id).unwrap();
    assert_eq!(
        get("mssql/usp_GetStock.sql").dialect.as_deref(),
        Some("tsql")
    );
    assert_eq!(
        get("mssql/usp_GetStock.sql").version.as_deref(),
        Some("2019")
    );
    assert_eq!(get("mssql/usp_GetStock.sql").kind, "procedure");
    assert_eq!(get("pg/get_stock.sql").version.as_deref(), Some("18"));
    assert_eq!(get("notes/readme.sql").dialect, None);

    let p = pairing::pair(&objects, &cfg, &PairScope::default()).unwrap();
    let pairs: Vec<(&str, &str, &str)> = p
        .pairs
        .iter()
        .map(|x| (x.a.as_str(), x.b.as_str(), x.rule.as_str()))
        .collect();
    assert!(pairs.contains(&("mssql/usp_GetStock.sql", "pg/get_stock.sql", "mssql-prefix")));
    assert!(pairs.contains(&("mssql/odd_name.sql", "pg/renamed.sql", "explicit")));
    assert_eq!(p.unpaired, vec!["mssql/only_here.sql"]);
    assert_eq!(p.shared, vec!["common/shared.tsql.sql"]);
    assert_eq!(p.no_dialect, vec!["notes/readme.sql"]);
}

#[test]
fn layering_and_provenance() {
    let root = tree("layering_and_provenance");
    write(
        &root,
        "sqldiff.toml",
        "packs = [\"pack\"]\nexcerpt_lines = 5\n[dialect_versions]\ntsql = \"2022\"\n[source]\nkind = \"fs\"\nroot = \".\"\n",
    );
    let cfg = Resolved::load(&Sources {
        project: Some(root.join("sqldiff.toml")),
        isolated: true,
        cli: cli_layer(&[("ASM-X".into(), "from cli".into())], &[], None).unwrap(),
        ..Sources::default()
    })
    .unwrap();
    let kinds: Vec<LayerKind> = cfg.layers.iter().map(|l| l.kind).collect();
    assert_eq!(
        kinds,
        [
            LayerKind::Builtin,
            LayerKind::SharedPack,
            LayerKind::Project,
            LayerKind::CommandLine
        ]
    );
    assert_eq!(cfg.layers[1].name, "example-conventions");
    // Project config overrides the pack.
    assert_eq!(cfg.dialect_version(sqldiff_core::Dialect::Tsql), "2022");
    assert_eq!(cfg.excerpt_lines, 5);
    assert_eq!(cfg.policy.assumptions["ASM-X"], "from cli");
    assert_eq!(
        cfg.policy.assumptions["ASM-EXAMPLE"],
        "Declared by the example pack"
    );
    assert_eq!(
        cfg.source,
        Some(sqldiff_source::SourceSpec::Fs {
            root: root.join(".")
        })
    );
    assert_eq!(cfg.work_items("Fix EX-12 and EX-7"), ["EX-12", "EX-7"]);
}

#[test]
fn builtin_runs_without_packs() {
    let cfg = Resolved::builtin();
    assert_eq!(cfg.layers.len(), 1);
    assert_eq!(cfg.dialect_version(sqldiff_core::Dialect::Postgres), "18");
    assert_eq!(cfg.dialect_version(sqldiff_core::Dialect::Tsql), "2022");
}

#[test]
fn config_rejects_secrets() {
    let e = ConfigLayer::parse("[source]\nkind = \"adapter\"\nname = \"x\"\npassword = \"p\"\n")
        .unwrap_err();
    assert!(e.0.contains("secret"), "{e}");
    let e = ConfigLayer::parse("[source]\nkind = \"adapter\"\nname = \"x\"\ncredential = \"p\"\n")
        .unwrap_err();
    assert!(e.0.contains("credential"), "{e}");
    ConfigLayer::parse(
        "[source]\nkind = \"adapter\"\nname = \"x\"\ncredential = \"env:DB_CRED\"\n",
    )
    .unwrap();
}

#[test]
fn bundle_round_trip_and_tamper_detection() {
    let root = tree("bundle_round_trip_and_tamper_detection");
    let cfg = config(&root);
    let src = FsSource::new(&root, &cfg).unwrap();
    let b = Bundle::capture(&src, &cfg, &CaptureOptions::default()).unwrap();
    assert_eq!(b.objects.len(), 7);
    assert_eq!(b.pairs.len(), 2);
    assert_eq!(b.provenance.len(), 2);
    let text = b.to_canonical_json();
    let again = Bundle::parse(&text).unwrap();
    assert_eq!(again, b);
    assert_eq!(again.to_canonical_json(), text, "canonical form is stable");

    // taken_at does not change the snapshot id.
    let mut c = b.clone();
    c.taken_at = Some("2026-01-01T00:00:00Z".into());
    Bundle::parse(&c.to_canonical_json()).unwrap();

    let tampered = text.replacen("SELECT qty", "SELECT qtx", 1);
    assert!(Bundle::parse(&tampered).unwrap_err().0.contains("hash"));

    // The bundle adapter serves the same objects and pairs.
    let bs = BundleSource::new(again, &cfg);
    assert_eq!(
        bs.list_objects(&ObjectFilter::default()).unwrap(),
        src.list_objects(&ObjectFilter::default())
            .unwrap()
            .into_iter()
            .map(|mut o| {
                o.last_changed = b
                    .objects
                    .iter()
                    .find(|x| x.info.id == o.id)
                    .unwrap()
                    .info
                    .last_changed
                    .clone();
                o
            })
            .collect::<Vec<_>>()
    );
    let only_tsql_a = bs
        .get_pairs(&PairScope {
            a_dialect: Some("tsql".into()),
            b_dialect: Some("pg".into()),
        })
        .unwrap();
    assert_eq!(only_tsql_a.len(), 2);
    let art = bs.load_artifact("pg/get_stock.sql").unwrap();
    assert_eq!(art.text, PG);
}

#[test]
fn policy_applies_limits_overrides_and_assumptions() {
    let root = tree("policy_applies_limits_overrides_and_assumptions");
    let cfg = config(&root);
    let src = FsSource::new(&root, &cfg).unwrap();
    write(
        &root,
        "mssql/curs.sql",
        "CREATE PROCEDURE c AS\nDECLARE c CURSOR FOR SELECT 1;\n",
    );
    let a = src.load_artifact("mssql/curs.sql").unwrap();
    let b = src.load_artifact("pg/get_stock.sql").unwrap();
    let mut r = sqldiff_core::diff(&a, &b, &sqldiff_core::Options::default());
    r.assumptions.push(sqldiff_core::model::Assumption::default(
        "ASM-EXAMPLE",
        "default",
    ));
    policy::add_limits(&mut r, &a, Some(&b), &cfg);
    policy::apply(&mut r, &cfg);
    let lim: Vec<_> = r
        .findings
        .iter()
        .filter(|f| f.class == ChangeClass::Limit)
        .collect();
    assert_eq!(lim.len(), 1);
    assert_eq!(lim[0].rule, "limit.no-cursor");
    assert_eq!(
        lim[0].severity,
        Severity::Critical,
        "override wins over the rule's own severity"
    );
    assert_eq!(lim[0].a.as_ref().unwrap().start_line, 2);
    let asm = r
        .assumptions
        .iter()
        .find(|a| a.id == "ASM-EXAMPLE")
        .unwrap();
    assert_eq!(asm.source, AssumptionSource::Declared);

    let mut eq = sqldiff_core::diff(&b, &b, &sqldiff_core::Options::default());
    assert_eq!(eq.verdict, Verdict::Equal);
    policy::cap_stale(&mut eq, false, true);
    assert_eq!(eq.verdict, Verdict::Unknown);
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
}

#[test]
fn git_adapter_reads_revisions_and_history() {
    let root = tmp("git");
    git(&root, &["init", "-q"]);
    write(
        &root,
        "db/tsql/get_stock.sql",
        "CREATE PROCEDURE get_stock AS SELECT 1;\n",
    );
    write(&root, "db/pg/get_stock.sql", PG);
    write(&root, "other.sql", "SELECT 1\n");
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "EX-1 first version"]);
    write(
        &root,
        "db/tsql/get_stock.sql",
        "CREATE PROCEDURE get_stock AS SELECT 2;\n",
    );
    write(&root, "db/tsql/uncommitted.sql", "SELECT 3\n");
    git(&root, &["commit", "-q", "-am", "EX-2 second version"]);

    let pack = root.join("pack.toml");
    std::fs::write(&pack, "[pack]\nname=\"p\"\nversion=\"1\"\n[[change_patterns]]\nname=\"t\"\npattern=\"EX-[0-9]+\"\n").unwrap();
    let cfg = Resolved::load(&Sources {
        isolated: true,
        packs: vec![pack],
        ..Sources::default()
    })
    .unwrap();

    let src = GitSource::new(&root, "HEAD", Some("db"), &cfg).unwrap();
    let ids: Vec<String> = src
        .list_objects(&ObjectFilter::default())
        .unwrap()
        .into_iter()
        .map(|o| o.id)
        .collect();
    assert_eq!(
        ids,
        ["pg/get_stock.sql", "tsql/get_stock.sql"],
        "working tree and other dirs are not read"
    );
    assert_eq!(
        src.load_artifact("tsql/get_stock.sql").unwrap().text,
        "CREATE PROCEDURE get_stock AS SELECT 2;\n"
    );
    assert_eq!(src.get_pairs(&PairScope::default()).unwrap().len(), 1);

    let h = src.get_history("tsql/get_stock.sql").unwrap();
    assert_eq!(h.len(), 2);
    assert_eq!(h[0].work_items, ["EX-2"]);
    assert_eq!(h[1].description, "EX-1 first version");
    let old = src
        .get_artifact(h[1].artifact_id.as_ref().unwrap())
        .unwrap();
    assert_eq!(old.text, "CREATE PROCEDURE get_stock AS SELECT 1;\n");
    assert_eq!(old.provenance.author.as_deref(), Some("Test"));

    let prev = GitSource::new(&root, "HEAD~1", Some("db"), &cfg).unwrap();
    let (pairs, only_a, only_b) = pairing::pair_versions(
        &prev.list_objects(&ObjectFilter::default()).unwrap(),
        &src.list_objects(&ObjectFilter::default()).unwrap(),
    );
    assert_eq!(pairs.len(), 2);
    assert!(only_a.is_empty() && only_b.is_empty());
}

#[test]
fn example_pack_is_valid() {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/pack");
    let pack = sqldiff_source::Pack::load(&p).unwrap();
    assert_eq!(pack.meta.name, "example");
    let t = pack.policy.template.as_ref().unwrap();
    let ph = policy::placeholders("SELECT <% TABLE_NAME %> FROM x", t);
    assert_eq!(ph[0].name, "TABLE_NAME");
    let cfg = Resolved::load(&Sources {
        isolated: true,
        packs: vec![p],
        ..Sources::default()
    })
    .unwrap();
    let m = policy::markers("SELECT 1; -- SYNC-OK: rounding differs on purpose\n", &cfg);
    assert_eq!(m[0].value, "rounding differs on purpose");
}
