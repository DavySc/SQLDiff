//! The deployment profile in a converted tree's report (decision 14 in
//! docs/verified-conversion.md): accepted assumptions are listed per unit
//! and in the summary, with the layer, pack and file that accept them;
//! statuses never change.

use std::path::PathBuf;

use sqldiff_convert::tree::{self, Tree};
use sqldiff_convert::Status;
use sqldiff_core::model::DeploymentSource;
use sqldiff_core::Dialect;
use sqldiff_source::deployment::Profile;
use sqldiff_source::pack::Deployment;

const PROC: &str = "CREATE PROCEDURE dbo.label_of @id int AS\nBEGIN\n    SET NOCOUNT ON;\n    SELECT ISNULL(name, '') AS label FROM dbo.item WHERE id = @id;\nEND\n";
const SCHEMA: &str =
    "CREATE TABLE dbo.item (id int NOT NULL PRIMARY KEY, name nvarchar(50) NULL);\n";

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir =
            std::env::temp_dir().join(format!("sqldiff-deployment-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (p, text) in [("schema/item.sql", SCHEMA), ("procs/label_of.sql", PROC)] {
            let f = dir.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        Fixture { dir }
    }

    fn convert(&self, deployment: Profile) -> Tree {
        let a = tree::Args {
            src: self.dir.clone(),
            from: Dialect::Tsql,
            to: Dialect::Postgres,
            jobs: Some(1),
            no_catalog: false,
            template: None,
            extensions: tree::extensions(Vec::new()),
            helpers: Vec::new(),
            deployment,
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

fn source() -> DeploymentSource {
    DeploymentSource {
        layer: "shared_pack".into(),
        pack: Some("team".into()),
        file: Some("/packs/team/pack.toml".into()),
    }
}

fn profile(accept: &[&str]) -> Profile {
    let mut p = Profile::default();
    p.add(
        &Deployment {
            accept: accept.iter().map(|s| s.to_string()).collect(),
            facts: [(
                "collation".to_string(),
                "SQL_Latin1_General_CP1_CI_AS".to_string(),
            )]
            .into(),
        },
        source(),
    );
    p
}

fn proc_file(t: &Tree) -> &tree::FileReport {
    t.report
        .files
        .iter()
        .find(|f| f.path == "procs/label_of.sql")
        .unwrap()
}

#[test]
fn without_a_profile_the_report_has_no_deployment_section() {
    let f = Fixture::new("none");
    let t = f.convert(Profile::default());
    assert!(t.report.deployment.is_none());
    let json = serde_json::to_string(&t.report).unwrap();
    assert!(!json.contains("\"deployment\""), "{json}");
    assert!(!json.contains("\"accepted\""), "{json}");
}

#[test]
fn accepted_assumptions_are_listed_and_statuses_stay() {
    let f = Fixture::new("listed");
    let plain = f.convert(Profile::default());
    let unit = proc_file(&plain)
        .units
        .iter()
        .find(|u| u.rules.iter().any(|r| r == "ASM-DBO-SCHEMA"))
        .expect("dropping dbo is an assumption");
    assert_eq!(unit.status, Status::Assumptions);

    // Accept only the dbo schema's assumption.
    let t = f.convert(profile(&["ASM-DBO-SCHEMA"]));
    let u = proc_file(&t)
        .units
        .iter()
        .find(|u| u.id == unit.id)
        .unwrap();
    assert_eq!(
        u.status,
        Status::Assumptions,
        "acceptance changes no status"
    );
    assert_eq!(u.accepted, ["ASM-DBO-SCHEMA"]);
    for (a, b) in plain.report.files.iter().zip(&t.report.files) {
        assert_eq!(a.status, b.status, "{}", a.path);
    }
    assert_eq!(plain.report.ok(), t.report.ok());

    let d = t.report.deployment.as_ref().unwrap();
    let dbo = d.carried.iter().find(|x| x.id == "ASM-DBO-SCHEMA").unwrap();
    assert!(dbo.accepted);
    assert!(dbo.units >= 1);
    assert_eq!(
        d.assumptions_within_accept,
        d.carried.iter().all(|x| x.accepted)
    );
    assert_eq!(d.accept[0].by, [source()]);
    assert_eq!(d.facts[0].key, "collation");
    assert!(d.facts[0]
        .bears_on
        .iter()
        .any(|x| x == "ASM-TSQL-CI-COLLATION"));

    let json = serde_json::to_string(&t.report).unwrap();
    assert!(json.contains("\"assumptions_within_accept\""), "{json}");
    let md = tree::markdown(&t.report);
    assert!(md.contains("## Deployment profile"), "{md}");
    assert!(
        md.contains("| `ASM-DBO-SCHEMA` |")
            && md.contains("shared pack team (/packs/team/pack.toml)"),
        "{md}"
    );

    // Accepting every acceptable id the tree carries: within accept unless
    // an id cannot be accepted at all.
    let ids: Vec<String> = d.carried.iter().map(|x| x.id.clone()).collect();
    let acceptable: Vec<&str> = ids
        .iter()
        .map(String::as_str)
        .filter(|id| sqldiff_core::assumptions::check_accept(id).is_ok())
        .collect();
    let all = f.convert(profile(&acceptable));
    let d = all.report.deployment.as_ref().unwrap();
    assert_eq!(
        d.assumptions_within_accept,
        acceptable.len() == ids.len(),
        "{:?}",
        ids
    );
}

#[test]
fn an_assumption_not_accepted_keeps_the_tree_outside_accept() {
    let f = Fixture::new("outside");
    let t = f.convert(profile(&["ASM-OBSERVER-ROWCOUNT"]));
    let d = t.report.deployment.as_ref().unwrap();
    assert!(!d.assumptions_within_accept);
    let dbo = d.carried.iter().find(|x| x.id == "ASM-DBO-SCHEMA").unwrap();
    assert!(!dbo.accepted);
    let md = tree::markdown(&t.report);
    assert!(md.contains("**not accepted**"), "{md}");
    assert!(
        tree::summary(&t.report).contains("deployment profile: "),
        "{}",
        tree::summary(&t.report)
    );
}

/// A private pack loaded from disk: the converted tree's report names it by
/// its file only, never by its name.
#[test]
fn a_private_pack_is_named_by_its_file_only_in_the_tree_report() {
    const SECRET: &str = "zz-private-pack-name-must-not-leak";
    let f = Fixture::new("private-name");
    let pack = f.dir.join("private-pack");
    std::fs::create_dir_all(&pack).unwrap();
    std::fs::write(
        pack.join("pack.toml"),
        format!(
            "[pack]\nname = \"{SECRET}\"\nversion = \"1.0.0\"\n\n[deployment]\naccept = [\"ASM-DBO-SCHEMA\"]\n\n[deployment.facts]\ncollation = \"SQL_Latin1_General_CP1_CI_AS\"\n"
        ),
    )
    .unwrap();
    let cfg = sqldiff_source::Resolved::load(&sqldiff_source::config::Sources {
        isolated: true,
        packs: vec![pack.clone()],
        ..Default::default()
    })
    .unwrap();
    // `pack.toml` is not SQL, so the tree does not read it.
    let t = f.convert(cfg.deployment.clone());
    assert!(t.report.unreadable.is_empty());
    let file = pack.join("pack.toml").display().to_string();
    let d = t.report.deployment.as_ref().unwrap();
    let by = &d.accept[0].by;
    assert_eq!(by.len(), 1);
    assert_eq!(by[0].layer, "private_pack");
    assert_eq!(by[0].pack, None);
    assert_eq!(by[0].file.as_deref(), Some(file.as_str()));
    assert_eq!(d.facts[0].by, by[0]);

    let json = serde_json::to_string(&t.report).unwrap();
    let md = tree::markdown(&t.report);
    assert!(md.contains(&format!("private pack {file}")), "{md}");
    for out in [&json, &md] {
        assert!(!out.contains(SECRET), "{out}");
    }
}
