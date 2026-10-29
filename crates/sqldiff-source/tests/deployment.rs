//! The deployment profile (`[deployment]`, decision 14 in
//! docs/verified-conversion.md): accepted assumption ids are validated
//! against the registry, merged across packs and config with where each
//! comes from, and marked on reports without changing the verdict.

use std::path::{Path, PathBuf};

use sqldiff_core::{diff, Artifact, Dialect, Verdict};
use sqldiff_source::config::Sources;
use sqldiff_source::{policy, Resolved};

const COUNTS: &str =
    "CREATE PROCEDURE dbo.touch @id int AS\nBEGIN\n  UPDATE dbo.t SET x = 1 WHERE id = @id;\nEND\n";
const PG: &str = "CREATE PROCEDURE touch(p_id int) LANGUAGE plpgsql AS $$\nBEGIN\n  UPDATE t SET x = 1 WHERE id = p_id;\nEND $$;\n";

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sqldiff-deployment-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("pack")).unwrap();
    d
}

fn load(root: &Path, project: &str) -> sqldiff_source::Result<Resolved> {
    std::fs::write(root.join("sqldiff.toml"), project).unwrap();
    Resolved::load(&Sources {
        isolated: true,
        project: Some(root.join("sqldiff.toml")),
        packs: vec![root.join("pack")],
        ..Sources::default()
    })
}

const PACK: &str = r#"
[pack]
name = "team"
version = "1.0.0"

[deployment]
accept = ["ASM-OBSERVER-ROWCOUNT"]

[deployment.facts]
collation = "Latin1_General_CS_AS"
"#;

#[test]
fn accepted_assumptions_are_marked_with_their_source_and_the_verdict_stays() {
    let root = dir("marked");
    std::fs::write(root.join("pack/pack.toml"), PACK).unwrap();
    let cfg = load(
        &root,
        "[deployment]\naccept = [\"ASM-OBSERVER-ROWCOUNT\", \"ASM-DBO-SCHEMA\"]\n[deployment.facts]\ncollation = \"SQL_Latin1_General_CP1_CI_AS\"\n",
    )
    .unwrap();

    let p = &cfg.deployment;
    assert!(p.declared);
    let rc = p
        .accept
        .iter()
        .find(|a| a.id == "ASM-OBSERVER-ROWCOUNT")
        .unwrap();
    // Lowest layer first: the pack, then the project file.
    assert_eq!(rc.by.len(), 2);
    assert_eq!(rc.by[0].layer, "private_pack");
    // A private pack is named by its file only.
    assert_eq!(rc.by[0].pack, None);
    let pack_file = root.join("pack").join("pack.toml").display().to_string();
    assert_eq!(rc.by[0].file.as_deref(), Some(pack_file.as_str()));
    assert_eq!(rc.by[1].layer, "project");
    assert_eq!(rc.by[1].pack, None);
    assert!(rc.by[1].file.as_deref().unwrap().ends_with("sqldiff.toml"));
    // The project's fact replaces the pack's.
    assert_eq!(p.facts.len(), 1);
    assert_eq!(p.facts[0].value, "SQL_Latin1_General_CP1_CI_AS");
    assert_eq!(p.facts[0].by.layer, "project");
    // The merged policy holds the joined table too (`sqldiff config`).
    let d = cfg.policy.deployment.as_ref().unwrap();
    assert_eq!(d.accept, ["ASM-OBSERVER-ROWCOUNT", "ASM-DBO-SCHEMA"]);

    let a = Artifact::new("a.sql", Dialect::Tsql, COUNTS);
    let b = Artifact::new("b.sql", Dialect::Postgres, PG);
    let mut r = diff(&a, &b, &cfg.options());
    let before = r.verdict();
    assert_eq!(before, Verdict::EqualUnderAssumptions, "{:?}", r.findings);
    policy::apply(&mut r, &cfg);
    assert_eq!(r.verdict(), before, "acceptance never changes the verdict");
    r.settle();
    assert_eq!(r.verdict(), Verdict::EqualUnderAssumptions);
    let rc = r
        .assumptions
        .iter()
        .find(|x| x.id == "ASM-OBSERVER-ROWCOUNT")
        .unwrap();
    assert_eq!(rc.accepted_by.len(), 2);
    for x in r
        .assumptions
        .iter()
        .filter(|x| x.id != "ASM-OBSERVER-ROWCOUNT")
    {
        let accepted = p.accepting(&x.id);
        assert_eq!(x.accepted_by, accepted, "{}", x.id);
    }
    let text = sqldiff_core::render::text(&r, false);
    assert!(
        text.contains(&format!(
            "accepted by the deployment profile: private pack {pack_file}; project "
        )),
        "{text}"
    );
    let json = sqldiff_core::render::json(&r);
    assert!(json.contains("\"accepted_by\""), "{json}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn without_a_profile_nothing_is_marked() {
    let root = dir("none");
    std::fs::write(
        root.join("pack/pack.toml"),
        "[pack]\nname = \"team\"\nversion = \"1\"\n",
    )
    .unwrap();
    let cfg = load(&root, "").unwrap();
    assert!(!cfg.deployment.declared);
    let a = Artifact::new("a.sql", Dialect::Tsql, COUNTS);
    let b = Artifact::new("b.sql", Dialect::Postgres, PG);
    let mut r = diff(&a, &b, &cfg.options());
    policy::apply(&mut r, &cfg);
    assert!(r.assumptions.iter().all(|x| x.accepted_by.is_empty()));
    assert!(!sqldiff_core::render::json(&r).contains("accepted_by"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unknown_and_undischargeable_ids_are_config_errors() {
    let root = dir("errors");
    std::fs::write(
        root.join("pack/pack.toml"),
        "[pack]\nname = \"team\"\nversion = \"1\"\n",
    )
    .unwrap();
    let e = load(
        &root,
        "[deployment]\naccept = [\"ASM-OBSERVER-ROWCOUNTS\"]\n",
    )
    .unwrap_err();
    assert!(e.0.contains("not a registered assumption"), "{e}");
    let e = load(&root, "[deployment]\naccept = [\"ASM-WITNESS-PATH\"]\n").unwrap_err();
    assert!(e.0.contains("cannot be accepted"), "{e}");
    let e = load(&root, "[deployment.facts]\nsort_order = \"x\"\n").unwrap_err();
    assert!(e.0.contains("unknown fact"), "{e}");
    let e = load(&root, "[deployment]\nobserver = 1\n").unwrap_err();
    assert!(e.0.contains("observer"), "{e}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A private pack's name never reaches a report: `accepted_by` and the
/// profile name it by its file only. A shared pack keeps its name.
#[test]
fn a_private_pack_is_named_by_its_file_only() {
    const SECRET: &str = "zz-private-pack-name-must-not-leak";
    let root = dir("private-name");
    std::fs::write(
        root.join("pack").join("pack.toml"),
        format!(
            "[pack]\nname = \"{SECRET}\"\nversion = \"1.0.0\"\n\n[deployment]\naccept = [\"ASM-OBSERVER-ROWCOUNT\"]\n"
        ),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("shared")).unwrap();
    std::fs::write(
        root.join("shared").join("pack.toml"),
        "[pack]\nname = \"shared-team\"\nversion = \"1.0.0\"\n\n[deployment]\naccept = [\"ASM-OBSERVER-ROWCOUNT\"]\n",
    )
    .unwrap();
    let cfg = load(&root, "packs = [\"shared\"]\n").unwrap();
    let private_file = root.join("pack").join("pack.toml").display().to_string();
    let shared_file = root.join("shared").join("pack.toml").display().to_string();

    let rc = cfg
        .deployment
        .accept
        .iter()
        .find(|a| a.id == "ASM-OBSERVER-ROWCOUNT")
        .unwrap();
    assert_eq!(rc.by.len(), 2);
    assert_eq!(rc.by[0].layer, "shared_pack");
    assert_eq!(rc.by[0].pack.as_deref(), Some("shared-team"));
    assert_eq!(rc.by[0].file.as_deref(), Some(shared_file.as_str()));
    assert_eq!(rc.by[1].layer, "private_pack");
    assert_eq!(rc.by[1].pack, None);
    assert_eq!(rc.by[1].file.as_deref(), Some(private_file.as_str()));

    let a = Artifact::new("a.sql", Dialect::Tsql, COUNTS);
    let b = Artifact::new("b.sql", Dialect::Postgres, PG);
    let mut r = diff(&a, &b, &cfg.options());
    policy::apply(&mut r, &cfg);
    let x = r
        .assumptions
        .iter()
        .find(|x| x.id == "ASM-OBSERVER-ROWCOUNT")
        .unwrap();
    assert_eq!(x.accepted_by, rc.by);
    let json = sqldiff_core::render::json(&r);
    assert!(json.contains("\"private_pack\""), "{json}");
    assert!(json.contains("shared-team"), "{json}");
    let text = sqldiff_core::render::text(&r, true);
    assert!(
        text.contains(&format!(
            "accepted by the deployment profile: shared pack shared-team ({shared_file}); private pack {private_file}"
        )),
        "{text}"
    );
    let profile = serde_json::to_string(&cfg.deployment).unwrap();
    for out in [&json, &text, &profile] {
        assert!(!out.contains(SECRET), "{out}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
