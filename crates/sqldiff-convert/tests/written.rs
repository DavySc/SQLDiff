//! Output written by hand (R6): `written` records with a text replace a
//! unit's output on every later conversion, and are checked like the
//! rules' output.

use std::path::PathBuf;

use sqldiff_convert::queue::build;
use sqldiff_convert::review::{append, file_hash, By, Record, State, LEDGER_FILE};
use sqldiff_convert::tree::{self, Tree};
use sqldiff_convert::Status;
use sqldiff_core::Dialect;

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str, files: &[(&str, &str)]) -> Fixture {
        let dir =
            std::env::temp_dir().join(format!("sqldiff-written-{name}-{}", std::process::id()));
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

    fn args(&self) -> tree::Args {
        tree::Args {
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
        }
    }

    /// Converts, applies the ledger and writes the tree, as `sqldiff convert`.
    fn convert(&self) -> (Tree, Vec<tree::Input>) {
        let a = self.args();
        let (inputs, unreadable) = tree::read(&a).unwrap();
        let mut t = tree::convert(&a, &inputs, unreadable, Some(&self.out())).unwrap();
        tree::review_in(&mut t, &inputs, &self.out()).unwrap();
        tree::write(&t, &self.out()).unwrap();
        (t, inputs)
    }

    /// Records `text` for the unit of `path` whose output holds `needle`.
    fn write_unit(&self, path: &str, needle: &str, text: &str, by: By, author: &str) {
        let (t, inputs) = self.convert();
        let f = t.report.files.iter().find(|f| f.path == path).unwrap();
        let u = unit_at(&t, path, needle);
        let r = tree::written_record(
            &t,
            &inputs,
            &f.path,
            &u,
            text,
            by,
            author,
            "same rows as the source",
            "2026-10-26T12:00:00Z",
        )
        .unwrap();
        append(&self.out(), &r).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The id of the unit of `path` whose output holds `needle`.
fn unit_at(t: &Tree, path: &str, needle: &str) -> String {
    let k = t.report.files.iter().position(|f| f.path == path).unwrap();
    let c = &t.conversions[k];
    let p = c
        .output
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} not in\n{}", c.output));
    let i = c
        .units
        .iter()
        .position(|u| u.out.start <= p && p < u.out.end.max(u.out.start + 1))
        .unwrap();
    t.report.files[k].units[i].id.clone()
}

fn unit<'a>(t: &'a Tree, path: &str, needle: &str) -> &'a sqldiff_convert::Unit {
    let k = t.report.files.iter().position(|f| f.path == path).unwrap();
    let c = &t.conversions[k];
    let p = c
        .output
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} not in\n{}", c.output));
    c.units
        .iter()
        .find(|u| u.out.start <= p && p < u.out.end.max(u.out.start + 1))
        .unwrap()
}

const PROC: &str = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  UPDATE dbo.t SET a = 1 WHERE b = 2;\nEND\n";

fn rules(u: &sqldiff_convert::Unit) -> Vec<&str> {
    u.notes.iter().map(|n| n.rule.as_str()).collect()
}

#[test]
fn a_hand_written_text_is_checked_like_rule_output() {
    let fx = Fixture::new("checked", &[("a.sql", PROC)]);
    let text = "  UPDATE t SET a = 1 WHERE 2 = b;";
    fx.write_unit("a.sql", "UPDATE", text, By::Agent, "agent");
    let (t, _) = fx.convert();
    let u = unit(&t, "a.sql", "2 = b");
    assert!(u.written.as_ref().is_some_and(|w| w.by == By::Agent));
    // Equivalent, but never better than the rules: their assumption stays,
    // and the text itself is an assumption to accept.
    assert_eq!(u.status, Status::Assumptions, "{u:#?}");
    assert!(rules(u).contains(&"ASM-HAND-WRITTEN"), "{u:#?}");
    assert!(rules(u).contains(&"ASM-DBO-SCHEMA"), "{u:#?}");
    let out = std::fs::read_to_string(fx.out().join("a.sql")).unwrap();
    assert!(out.contains(text), "{out}");
    // An agent's text waits for a person, whatever its status.
    let q = build(&t.report, None, false);
    let id = unit_at(&t, "a.sql", "2 = b");
    let item = q
        .items
        .iter()
        .find(|i| i.unit.as_deref() == Some(id.as_str()))
        .unwrap();
    assert_eq!(item.tier, 3, "{item:#?}");

    let fx = Fixture::new("wrong", &[("a.sql", PROC)]);
    fx.write_unit(
        "a.sql",
        "UPDATE",
        "  UPDATE t SET a = 2 WHERE b = 2;",
        By::Agent,
        "agent",
    );
    let (t, _) = fx.convert();
    assert_eq!(unit(&t, "a.sql", "a = 2").status, Status::Differs);

    let fx = Fixture::new("schema", &[("a.sql", PROC)]);
    fx.write_unit(
        "a.sql",
        "UPDATE",
        "  UPDATE other.t SET a = 1 WHERE b = 2;",
        By::Agent,
        "agent",
    );
    let (t, _) = fx.convert();
    assert!(unit(&t, "a.sql", "other.t").status >= Status::Unproven);
}

#[test]
fn a_new_draft_by_the_same_author_replaces_the_last() {
    let fx = Fixture::new("redraft", &[("a.sql", PROC)]);
    fx.write_unit(
        "a.sql",
        "UPDATE",
        "  UPDATE t SET a = 2 WHERE b = 2;",
        By::Agent,
        "agent",
    );
    // The id is now the hand-written unit's; the record still keys to the
    // rules' unit, so the second draft replaces the first.
    fx.write_unit(
        "a.sql",
        "a = 2",
        "  UPDATE t SET a = 1 WHERE b = 2;",
        By::Agent,
        "agent",
    );
    let (t, _) = fx.convert();
    let u = unit(&t, "a.sql", "a = 1");
    assert_eq!(u.status, Status::Assumptions, "{u:#?}");
    let ledger = std::fs::read_to_string(fx.out().join(LEDGER_FILE)).unwrap();
    let units: Vec<String> = ledger
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["unit"].to_string())
        .collect();
    assert_eq!(units.len(), 2);
    assert_eq!(units[0], units[1]);
}

#[test]
fn a_persons_text_replaces_an_agents_and_two_people_conflict() {
    let fx = Fixture::new("people", &[("a.sql", PROC)]);
    fx.write_unit(
        "a.sql",
        "UPDATE",
        "  UPDATE t SET a = 2 WHERE b = 2;",
        By::Agent,
        "agent",
    );
    fx.write_unit(
        "a.sql",
        "a = 2",
        "  UPDATE t SET a = 1 WHERE 2 = b;",
        By::Human,
        "Davy",
    );
    let (t, _) = fx.convert();
    let u = unit(&t, "a.sql", "2 = b");
    assert_eq!(u.written.as_ref().unwrap().by, By::Human);
    assert_eq!(u.status, Status::Assumptions, "{u:#?}");

    fx.write_unit(
        "a.sql",
        "2 = b",
        "  UPDATE t SET a = 1 WHERE b = 2 AND 2 = 2;",
        By::Human,
        "Ann",
    );
    let (t, _) = fx.convert();
    // Neither text applies: the rules' output stays, marked differs.
    let u = unit(&t, "a.sql", "UPDATE t SET a = 1 WHERE b = 2;");
    assert!(u.written.is_none());
    assert_eq!(u.status, Status::Differs);
    assert!(rules(u).contains(&"review.conflicting-texts"), "{u:#?}");
}

#[test]
fn files_edited_by_hand_are_not_overwritten() {
    let fx = Fixture::new("guard", &[("a.sql", PROC)]);
    fx.convert();
    tree::guard_hand_edits(&fx.out(), false).unwrap();
    let f = fx.out().join("a.sql");
    let text = std::fs::read_to_string(&f).unwrap();
    std::fs::write(&f, text.replace("a = 1", "a = 3")).unwrap();
    assert_eq!(
        tree::hand_edits(&fx.out()).unwrap(),
        vec!["a.sql".to_string()]
    );
    let e = tree::guard_hand_edits(&fx.out(), false).unwrap_err();
    assert!(e.contains("a.sql") && e.contains("--discard-edits"), "{e}");
    tree::guard_hand_edits(&fx.out(), true).unwrap();
    // A fresh conversion records the new output, so nothing is edited.
    fx.convert();
    assert!(tree::hand_edits(&fx.out()).unwrap().is_empty());
}

/// A person's verdict on the unit of `path` whose output holds `needle`,
/// made on the files as they are now.
fn verdict(fx: &Fixture, path: &str, needle: &str, rule: &str, state: State) {
    let (t, inputs) = fx.convert();
    let k = t.report.files.iter().position(|f| f.path == path).unwrap();
    let r = Record {
        unit: unit_at(&t, path, needle),
        path: path.into(),
        rule: rule.into(),
        state,
        by: By::Human,
        author: "Davy".into(),
        reason: "read against the source".into(),
        at: "2026-10-26T12:30:00Z".into(),
        input_hash: file_hash(&inputs[k].text),
        output_hash: file_hash(&t.conversions[k].output),
        text: None,
        base_catalogs: None,
    };
    append(&fx.out(), &r).unwrap();
}

#[test]
fn a_hand_written_unit_is_reviewed_only_once_a_person_has_read_it() {
    let fx = Fixture::new("reviewed", &[("a.sql", PROC)]);
    fx.write_unit(
        "a.sql",
        "UPDATE",
        "  UPDATE t SET a = 1 WHERE 2 = b;",
        By::Agent,
        "agent",
    );
    let review = |fx: &Fixture| {
        let (t, _) = fx.convert();
        let f = t.report.files.iter().find(|f| f.path == "a.sql").unwrap();
        let k = f
            .units
            .iter()
            .position(|u| u.id == unit_at(&t, "a.sql", "2 = b"))
            .unwrap();
        let u = &f.units[k];
        let r = u.review.as_ref().unwrap();
        // The applied text is the unit's, never an orphan.
        assert!(
            f.review.as_ref().unwrap().orphaned.is_empty(),
            "{:?}",
            f.review.as_ref().unwrap().orphaned
        );
        assert!(
            r.decisions
                .iter()
                .any(|d| d.current && d.record.text.is_some()),
            "{r:#?}"
        );
        (r.reviewed, r.open.clone(), r.written_by)
    };
    let (reviewed, open, by) = review(&fx);
    assert!(!reviewed);
    assert_eq!(by, Some(By::Agent));
    assert!(open.contains(&"ASM-HAND-WRITTEN".to_string()), "{open:?}");
    // The engine's assumptions about the file (no catalog: tables and
    // column types) are open on the unit too.
    for rule in open {
        verdict(&fx, "a.sql", "2 = b", &rule, State::Accepted);
    }
    verdict(&fx, "a.sql", "2 = b", "ASM-DBO-SCHEMA", State::Accepted);
    // Every assumption accepted, but an agent wrote it: a person approves.
    let (reviewed, open, _) = review(&fx);
    assert!(open.is_empty(), "{open:?}");
    assert!(!reviewed);
    verdict(&fx, "a.sql", "2 = b", "", State::Approved);
    assert!(review(&fx).0);
}
