use std::path::{Path, PathBuf};

use sqldiff_convert::queue::{build, Instances, Kind, Queue};
use sqldiff_convert::review::{file_hash, unit_ids, By, Record, State, LEDGER_FILE};
use sqldiff_convert::tree::{self, Tree};
use sqldiff_convert::Status;
use sqldiff_core::Dialect;

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str, files: &[(&str, &str)]) -> Fixture {
        let dir = std::env::temp_dir().join(format!("sqldiff-queue-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (p, text) in files {
            let f = dir.join("src").join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        std::fs::create_dir_all(dir.join("out")).unwrap();
        Fixture { dir }
    }

    fn out(&self) -> PathBuf {
        self.dir.join("out")
    }

    fn convert(&self) -> (Tree, Vec<tree::Input>) {
        let a = tree::Args {
            src: self.dir.join("src"),
            from: Dialect::Tsql,
            to: Dialect::Postgres,
            jobs: Some(1),
            no_catalog: false,
            template: None,
            extensions: tree::extensions(Vec::new()),
            helpers: Vec::new(),
            deployment: Default::default(),
            catalogs: Vec::new(),
            catalogs_b: Vec::new(),
        };
        let (inputs, unreadable) = tree::read(&a).unwrap();
        let mut t = tree::convert(&a, &inputs, unreadable, Some(&self.out())).unwrap();
        tree::review_in(&mut t, &inputs, &self.out()).unwrap();
        (t, inputs)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

const ASSUMING: &str = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  UPDATE dbo.t SET a = 1;\nEND\n";
const DECIDING: &str =
    "CREATE PROCEDURE dbo.q AS\nBEGIN\n  DECLARE @f bit = 1;\n  UPDATE t SET a = 1;\nEND\n";
const REFUSED: &str = "CREATE PROCEDURE dbo.r AS\nBEGIN\n  SELECT @@ERROR;\nEND\n";

fn tiers_of(q: &Queue, path: &str) -> Vec<u8> {
    q.items
        .iter()
        .filter(|i| i.path == path && i.kind == Kind::Unit)
        .map(|i| i.tier)
        .collect()
}

#[test]
fn units_are_tiered_by_status_and_nothing_is_ranked_as_safe() {
    let fx = Fixture::new(
        "tiers",
        &[
            ("a.sql", ASSUMING),
            ("d.sql", DECIDING),
            ("r.sql", REFUSED),
            ("c.sql", "SELECT 1 AS x;\n"),
        ],
    );
    let (t, _) = fx.convert();
    let q = build(&t.report, None, false);
    // Sorted by tier, worst first.
    assert!(q.items.windows(2).all(|w| w[0].tier <= w[1].tier));
    assert!(tiers_of(&q, "r.sql").contains(&1), "{:#?}", q.items);
    assert!(tiers_of(&q, "d.sql").contains(&4), "{:#?}", q.items);
    assert!(tiers_of(&q, "a.sql").contains(&5), "{:#?}", q.items);
    // A plainly converted unit is not listed unless asked, and then only at 6.
    assert!(tiers_of(&q, "c.sql").is_empty());
    let all = build(&t.report, None, true);
    assert_eq!(tiers_of(&all, "c.sql"), vec![6]);
    // Every unit that is not plainly converted is somewhere in the queue.
    for f in &t.report.files {
        for u in &f.units {
            assert!(
                q.items
                    .iter()
                    .any(|i| i.unit.as_deref() == Some(u.id.as_str()) || i.units.contains(&u.id)),
                "{} {} missing from the queue",
                f.path,
                u.id
            );
            if u.status >= Status::Unproven {
                let i = q
                    .items
                    .iter()
                    .find(|i| i.unit.as_deref() == Some(u.id.as_str()) || i.units.contains(&u.id))
                    .unwrap();
                assert!(i.tier <= 2, "{} at tier {}", u.status.label(), i.tier);
            }
        }
    }
}

#[test]
fn units_failing_on_one_missing_table_become_one_item() {
    let fx = Fixture::new(
        "missing",
        &[
            (
                "x.sql",
                "CREATE PROCEDURE x AS\nBEGIN\n  UPDATE stock SET q = 1;\nEND\n",
            ),
            (
                "y.sql",
                "CREATE PROCEDURE y AS\nBEGIN\n  DELETE FROM stock WHERE q = 0;\nEND\n",
            ),
            // A table in the tree makes the catalog authoritative for tables.
            ("t.sql", "CREATE TABLE other (id int);\n"),
        ],
    );
    let (t, _) = fx.convert();
    let q = build(&t.report, None, false);
    let m: Vec<_> = q
        .items
        .iter()
        .filter(|i| i.kind == Kind::MissingObject)
        .collect();
    if t.report
        .files
        .iter()
        .filter(|f| f.path != "t.sql")
        .all(|f| {
            f.units
                .iter()
                .any(|u| u.rules == vec!["bind.unknown-table".to_string()])
        })
    {
        assert_eq!(m.len(), 1, "{:#?}", q.items);
        assert_eq!(m[0].tier, 1);
        assert_eq!(m[0].units.len(), 2);
        assert!(m[0].reason.contains("table `stock`"), "{}", m[0].reason);
    } else {
        panic!(
            "expected both units to fail binding on stock: {:#?}",
            t.report.files.iter().map(|f| &f.units).collect::<Vec<_>>()
        );
    }
}

fn record(input: &str, out: &str, unit: &str, rule: &str, state: State, by: By) -> Record {
    Record {
        unit: unit.into(),
        path: "a.sql".into(),
        rule: rule.into(),
        state,
        by,
        author: "Davy".into(),
        reason: "checked".into(),
        at: "2026-10-26T11:00:00Z".into(),
        input_hash: file_hash(input),
        output_hash: file_hash(out),
        text: None,
        base_catalogs: None,
    }
}

fn write_ledger(out: &Path, recs: &[Record]) {
    let text: Vec<String> = recs
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect();
    std::fs::write(out.join(LEDGER_FILE), text.join("\n")).unwrap();
}

#[test]
fn reviewed_units_leave_the_queue_and_agent_output_waits_for_a_person() {
    let fx = Fixture::new("reviewed", &[("a.sql", ASSUMING)]);
    let (t, inputs) = fx.convert();
    let c = &t.conversions[0];
    let ids = unit_ids("a.sql", &inputs[0].text, c);
    let mut recs = Vec::new();
    for (u, id) in c.units.iter().zip(&ids) {
        for n in &u.notes {
            let s = match n.status {
                Status::Assumptions => State::Accepted,
                Status::Decision => State::Picked,
                _ => continue,
            };
            recs.push(record(
                &inputs[0].text,
                &c.output,
                id,
                &n.rule,
                s,
                By::Human,
            ));
        }
    }
    write_ledger(&fx.out(), &recs);
    let (t2, _) = fx.convert();
    let q = build(&t2.report, None, false);
    let left: Vec<_> = q.items.iter().filter(|i| i.tier >= 4).collect();
    assert!(left.is_empty(), "{left:#?}");
    assert!(q.reviewed > 0);

    // An agent writes the converted units: they come back, at tier 3.
    for (u, id) in c.units.iter().zip(&ids) {
        if u.status <= Status::Assumptions {
            recs.push(record(
                &inputs[0].text,
                &c.output,
                id,
                "",
                State::Written,
                By::Agent,
            ));
        }
    }
    write_ledger(&fx.out(), &recs);
    let (t3, _) = fx.convert();
    let q = build(&t3.report, None, false);
    assert!(
        q.items
            .iter()
            .any(|i| i.tier == 3 && i.written_by == Some(By::Agent)),
        "{:#?}",
        q.items
    );
}

#[test]
fn templates_are_judged_by_their_instances() {
    let fx = Fixture::new(
        "instances",
        &[("a.sql", ASSUMING), ("c.sql", "SELECT 1 AS x;\n")],
    );
    let (t, _) = fx.convert();
    let inst: Instances = serde_json::from_str(
        r#"{
          "instances": [
            {"name": "I1", "status": "differs", "templates": ["c.sql"]},
            {"name": "I2", "status": "converted", "templates": ["c.sql"], "done": true},
            {"name": "I3", "status": "something-new", "templates": ["a.sql"]}
          ],
          "templates": [
            {"path": "c.sql", "status": "differs", "instances": 2},
            {"path": "a.sql", "status": "something-new", "instances": 1}
          ],
          "not_exercised": ["unused.sql"],
          "not_in_manifest": ["gen/extra.sql"]
        }"#,
    )
    .unwrap();
    let q = build(&t.report, Some(&inst), false);
    let c = q.items.iter().find(|i| i.key == "template:c.sql").unwrap();
    assert_eq!(c.tier, 1);
    assert_eq!(
        c.failing_instances,
        vec![("I1".to_string(), "differs".to_string())]
    );
    // A status the queue does not know is never safe.
    assert_eq!(
        q.items
            .iter()
            .find(|i| i.key == "template:a.sql")
            .unwrap()
            .tier,
        2
    );
    assert!(q
        .items
        .iter()
        .any(|i| i.kind == Kind::NotExercised && i.tier == 2));
    assert!(q
        .items
        .iter()
        .any(|i| i.kind == Kind::NotInManifest && i.tier == 2));
    // Blast radius reaches the units of a template.
    assert!(q
        .items
        .iter()
        .any(|i| i.path == "a.sql" && i.kind == Kind::Unit && i.instances == 1));
}

#[test]
fn templates_checked_on_sqldiffs_weave_only_are_never_better_than_nothing_claimed() {
    let fx = Fixture::new(
        "weave-only",
        &[
            ("a.sql", ASSUMING),
            ("c.sql", "SELECT 1 AS x;\n"),
            ("d.sql", "SELECT 2 AS y;\n"),
        ],
    );
    let (t, _) = fx.convert();
    let inst: Instances = serde_json::from_str(
        r#"{
          "instances": [
            {"name": "W", "status": "converted", "templates": ["c.sql"], "done": false, "weave_only": ["target"]},
            {"name": "A", "status": "assumptions", "templates": ["a.sql"], "done": false, "weave_only": ["source", "target"]},
            {"name": "D", "status": "converted", "templates": ["d.sql"], "done": true, "weave_only": []},
            {"name": "Old", "status": "converted", "templates": ["e.sql"]}
          ],
          "templates": [
            {"path": "c.sql", "status": "converted", "done": false, "instances": 1, "weave_only": 1},
            {"path": "a.sql", "status": "assumptions", "done": false, "instances": 1, "weave_only": 1},
            {"path": "d.sql", "status": "converted", "done": true, "instances": 1, "weave_only": 0},
            {"path": "e.sql", "status": "converted", "instances": 1}
          ],
          "not_in_manifest_target": ["gen/pg/extra.sql"]
        }"#,
    )
    .unwrap();
    let q = build(&t.report, Some(&inst), false);
    let item = |k: &str| q.items.iter().find(|i| i.key == k);
    let c = item("template:c.sql").expect("a converted template checked on the weave only");
    assert_eq!(c.tier, 2);
    assert!(c.failing_instances[0].1.contains("weave only"), "{c:#?}");
    assert_eq!(item("template:a.sql").unwrap().tier, 2);
    // Done: checked on the generator's output.
    assert!(item("template:d.sql").is_none());
    // A report from before the generator check claims nothing about it.
    assert_eq!(item("template:e.sql").unwrap().tier, 2);
    let n = item("not-in-manifest-target:gen/pg/extra.sql").unwrap();
    assert_eq!((n.tier, n.kind), (2, Kind::NotInManifest));
}

#[test]
fn unreadable_files_are_wrong_or_missing() {
    let fx = Fixture::new("unreadable", &[("c.sql", "SELECT 1 AS x;\n")]);
    std::fs::write(
        fx.dir.join("src").join("bad.sql"),
        [0xC3u8, 0x28, 0xFF, 0x00],
    )
    .unwrap();
    let (t, _) = fx.convert();
    assert_eq!(t.report.unreadable, vec!["bad.sql".to_string()]);
    let q = build(&t.report, None, false);
    let i = q
        .items
        .iter()
        .find(|i| i.key == "unreadable:bad.sql")
        .unwrap();
    assert_eq!((i.tier, i.kind), (1, Kind::Unreadable));
    assert_eq!(q.items[0].tier, 1);
}
