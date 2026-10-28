//! The done gate (R7): a complete review passes, and every injected change
//! fails it.

use std::path::PathBuf;

use sqldiff_convert::gate::{check, Gate};
use sqldiff_convert::queue::Instances;
use sqldiff_convert::review::{append, file_hash, By, Ledger, Record, State};
use sqldiff_convert::tree::{self, Tree};
use sqldiff_convert::Status;
use sqldiff_core::Dialect;

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str, files: &[(&str, &str)]) -> Fixture {
        let dir = std::env::temp_dir().join(format!("sqldiff-gate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (p, text) in files {
            let f = dir.join("src").join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        std::fs::create_dir_all(dir.join("out")).unwrap();
        let fx = Fixture { dir };
        fx.convert(true);
        fx
    }

    fn src(&self) -> PathBuf {
        self.dir.join("src")
    }

    fn out(&self) -> PathBuf {
        self.dir.join("out")
    }

    /// Converts with the ledger applied; `write` as `sqldiff convert`.
    fn args(&self, templates: bool) -> tree::Args {
        let mut a = self.plain_args();
        if templates {
            // Only `template.is_some()` matters to the gate's own checks.
            a.template = Some(template_mode());
        }
        a
    }

    fn plain_args(&self) -> tree::Args {
        tree::Args {
            src: self.src(),
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
        }
    }

    fn convert(&self, write: bool) -> (Tree, Vec<tree::Input>) {
        let a = self.plain_args();
        let (inputs, unreadable) = tree::read(&a).unwrap();
        let mut t = tree::convert(&a, &inputs, unreadable, Some(&self.out())).unwrap();
        tree::review_in(&mut t, &inputs, &self.out()).unwrap();
        if t.report.ledger.is_none() {
            tree::review(&mut t, &inputs, &Ledger::default(), "(none)");
        }
        if write {
            tree::write(&t, &self.out()).unwrap();
        }
        (t, inputs)
    }

    fn gate(&self, templates: bool, inst: Option<&Instances>) -> Gate {
        let (t, _) = self.convert(false);
        check(&t, &self.args(templates), &self.out(), inst).unwrap()
    }

    /// A person accepts every open assumption and picks every open decision.
    fn accept_all(&self) {
        let (t, inputs) = self.convert(false);
        for (k, f) in t.report.files.iter().enumerate() {
            for u in f.units.iter().chain(&f.converted_units) {
                let c = &t.conversions[k];
                for rule in &u.review.as_ref().unwrap().open {
                    let n = c
                        .units
                        .iter()
                        .flat_map(|x| &x.notes)
                        .find(|n| &n.rule == rule)
                        .unwrap();
                    let state = if n.status == Status::Decision {
                        State::Picked
                    } else {
                        State::Accepted
                    };
                    self.record(&t, &inputs, k, &u.id, rule, state, By::Human);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record(
        &self,
        t: &Tree,
        inputs: &[tree::Input],
        k: usize,
        unit: &str,
        rule: &str,
        state: State,
        by: By,
    ) {
        append(
            &self.out(),
            &Record {
                unit: unit.into(),
                path: t.report.files[k].path.clone(),
                rule: rule.into(),
                state,
                by,
                author: "Davy".into(),
                reason: "checked".into(),
                at: "2026-10-26T13:00:00Z".into(),
                input_hash: file_hash(&inputs[k].text),
                output_hash: file_hash(&t.conversions[k].output),
                text: None,
                base_catalogs: None,
            },
        )
        .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

const ASSUMING: &str = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  UPDATE dbo.t SET a = 1;\nEND\n";
const PLAIN: &str = "SELECT 1 AS x;\n";

fn kinds(g: &Gate) -> Vec<&str> {
    g.blockers.iter().map(|b| b.kind.as_str()).collect()
}

/// A tree whose review is complete.
fn complete(name: &str) -> Fixture {
    let fx = Fixture::new(name, &[("a.sql", ASSUMING), ("c.sql", PLAIN)]);
    let g = fx.gate(false, None);
    assert!(!g.complete);
    assert!(kinds(&g).iter().all(|k| *k == "open"), "{g:#?}");
    fx.accept_all();
    let g = fx.gate(false, None);
    assert!(g.complete, "{g:#?}");
    assert!(g
        .accepted
        .iter()
        .any(|a| a.rule == "ASM-DBO-SCHEMA" && a.authors == ["Davy"]));
    // The SELECT: nothing found, never reviewed, and it needs no decision.
    assert!(g.nothing_found >= 1);
    fx
}

#[test]
fn a_complete_review_passes() {
    complete("pass");
}

#[test]
fn an_edited_output_file_fails() {
    let fx = complete("edited");
    let f = fx.out().join("c.sql");
    let t = std::fs::read_to_string(&f).unwrap();
    std::fs::write(&f, t.replace("1", "2")).unwrap();
    assert_eq!(kinds(&fx.gate(false, None)), ["stale-output"]);
    std::fs::remove_file(&f).unwrap();
    assert_eq!(kinds(&fx.gate(false, None)), ["stale-output"]);
}

#[test]
fn a_changed_source_fails_and_outdates_its_decisions() {
    let fx = complete("source");
    std::fs::write(fx.src().join("a.sql"), format!("{ASSUMING}-- note\n")).unwrap();
    let g = fx.gate(false, None);
    let k = kinds(&g);
    assert!(
        k.contains(&"stale-output") && k.contains(&"outdated"),
        "{g:#?}"
    );
    // Converted again, the decisions are still made on other bytes.
    fx.convert(true);
    let g = fx.gate(false, None);
    assert!(kinds(&g).contains(&"outdated"), "{g:#?}");
    assert!(!kinds(&g).contains(&"stale-output"), "{g:#?}");
}

#[test]
fn a_rejection_fails() {
    let fx = complete("rejected");
    let (t, inputs) = fx.convert(false);
    let k = t
        .report
        .files
        .iter()
        .position(|f| f.path == "a.sql")
        .unwrap();
    let id = t.report.files[k].units[0].id.clone();
    fx.record(&t, &inputs, k, &id, "", State::Rejected, By::Human);
    assert_eq!(kinds(&fx.gate(false, None)), ["rejected"]);
}

#[test]
fn an_agents_text_fails_until_a_person_approves_it() {
    let fx = complete("agent");
    let (t, inputs) = fx.convert(false);
    let k = t
        .report
        .files
        .iter()
        .position(|f| f.path == "c.sql")
        .unwrap();
    let id = t.report.files[k].converted_units[0].id.clone();
    let r = tree::written_record(
        &t,
        &inputs,
        "c.sql",
        &id,
        "SELECT 1 AS x;",
        By::Agent,
        "agent",
        "same",
        "2026-10-26T13:10:00Z",
    )
    .unwrap();
    append(&fx.out(), &r).unwrap();
    fx.convert(true);
    let g = fx.gate(false, None);
    let k2 = kinds(&g);
    assert!(
        k2.contains(&"agent-written") && k2.contains(&"open"),
        "{g:#?}"
    );
    fx.accept_all();
    let (t, inputs) = fx.convert(false);
    let u = &t.report.files[k].units[0];
    fx.record(
        &t,
        &inputs,
        k,
        &u.id.clone(),
        "",
        State::Approved,
        By::Human,
    );
    let g = fx.gate(false, None);
    assert!(g.complete, "{g:#?}");
}

#[test]
fn an_unreadable_ledger_line_or_source_file_fails() {
    let fx = complete("ledger");
    let l = fx.out().join(sqldiff_convert::review::LEDGER_FILE);
    let text = std::fs::read_to_string(&l).unwrap();
    std::fs::write(&l, format!("{text}{{\"unit\": \"broken\"\n")).unwrap();
    assert_eq!(kinds(&fx.gate(false, None)), ["ledger"]);
    std::fs::write(&l, text).unwrap();
    std::fs::write(fx.src().join("bad.sql"), [0xC3u8, 0x28, 0xFF, 0x00]).unwrap();
    assert_eq!(kinds(&fx.gate(false, None)), ["unreadable"]);
}

#[test]
fn templates_need_a_complete_instance_check_of_these_trees() {
    let fx = complete("instances");
    assert_eq!(kinds(&fx.gate(true, None)), ["instances"]);
    let manifest = fx.dir.join("instances.toml");
    std::fs::write(&manifest, "[[instance]]\n").unwrap();
    let report = |complete: Option<bool>, target: &str| -> Instances {
        let mut v = serde_json::json!({
            "instances": [{"name": "I", "status": "converted", "templates": ["a.sql"], "done": true}],
            "manifest": manifest.to_str().unwrap(),
            "hashes": {
                "source": tree::dir_hash(&fx.src()).unwrap(),
                "target": target,
                "manifest": tree::dir_hash(&manifest).unwrap(),
            },
        });
        if let Some(c) = complete {
            v["complete"] = c.into();
        }
        serde_json::from_value(v).unwrap()
    };
    let target = tree::dir_hash(&fx.out()).unwrap();
    let g = fx.gate(true, Some(&report(Some(true), &target)));
    assert!(g.complete, "{g:#?}");
    assert_eq!(g.instances, Some(true));
    // Recording a decision does not change the target's hash.
    let (t, inputs) = fx.convert(false);
    let k = t
        .report
        .files
        .iter()
        .position(|f| f.path == "c.sql")
        .unwrap();
    let id = t.report.files[k].converted_units[0].id.clone();
    fx.record(&t, &inputs, k, &id, "", State::Commented, By::Human);
    assert_eq!(tree::dir_hash(&fx.out()).unwrap(), target);

    for (inst, what) in [
        (report(Some(false), &target), "not complete"),
        (report(None, &target), "predates"),
        (report(Some(true), "0000"), "target"),
    ] {
        let g = fx.gate(true, Some(&inst));
        assert_eq!(kinds(&g), ["instances"], "{g:#?}");
        assert!(g.blockers[0].what.contains(what), "{g:#?}");
    }
    let before = report(Some(true), &target);
    std::fs::write(&manifest, "[[instance]]\nname = \"x\"\n").unwrap();
    let g = fx.gate(true, Some(&before));
    assert!(
        g.blockers.iter().any(|b| b.what.contains("manifest")),
        "{g:#?}"
    );
}

fn template_mode() -> tree::TemplateMode {
    let def: sqldiff_source::pack::TemplateSyntax =
        serde_json::from_value(serde_json::json!({ "open": "<%", "close": "%>" })).unwrap();
    tree::TemplateMode::new(&def, None).unwrap()
}

#[test]
fn an_output_file_no_source_converts_to_fails() {
    let fx = complete("orphan");
    std::fs::remove_file(fx.src().join("c.sql")).unwrap();
    let g = fx.gate(false, None);
    assert!(
        g.blockers
            .iter()
            .any(|b| b.kind == "stale-output" && b.path == "c.sql"),
        "{g:#?}"
    );
    // Files the conversion does not read are not its business.
    std::fs::remove_file(fx.out().join("c.sql")).unwrap();
    std::fs::write(fx.out().join("README.txt"), "notes\n").unwrap();
    assert!(fx.gate(false, None).complete);
}
