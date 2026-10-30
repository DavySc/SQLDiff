//! Review mode (`sqldiff-gui review`): what the page's actions write to the
//! ledger, what they refuse, and how the queue follows.

use std::path::PathBuf;

use sqldiff_convert::review::{file_hash, By, Ledger, State, LEDGER_FILE};
use sqldiff_convert::tree;
use sqldiff_convert::Status;
use sqldiff_core::Dialect;
use sqldiff_gui::convert_review::{
    stored_key, Action, Near, RecordRequest, Review, Setup, Target, UnitView, GATE_BLOCKERS_SHOWN,
    GATE_STALE,
};
use sqldiff_gui::review_list::{Group, Query};

const ASSUMING: &str = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  UPDATE dbo.t SET a = 1;\nEND\n";
const ASSUMING_2: &str = "CREATE PROCEDURE dbo.p2 AS\nBEGIN\n  UPDATE dbo.t SET a = 2;\nEND\n";
// A parameter without a default after one with a default: PostgreSQL
// needs them reordered, a decision.
const DECIDING: &str =
    "CREATE PROCEDURE dbo.q @x int = 1, @y int AS\nBEGIN\n  UPDATE t SET a = 1;\nEND\n";
const REFUSED: &str = "CREATE PROCEDURE dbo.r AS\nBEGIN\n  SELECT @@ERROR;\nEND\n";
const PLAIN: &str = "CREATE TABLE t (a int NOT NULL);\n";
const AT: &str = "2026-10-26T12:00:00Z";

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    /// Converts `files` into `out` as `sqldiff convert` does.
    fn new(name: &str, files: &[(&str, &str)]) -> Fixture {
        let dir =
            std::env::temp_dir().join(format!("sqldiff-gui-review-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (p, text) in files {
            let f = dir.join("src").join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        let f = Fixture { dir };
        let a = f.args();
        let (inputs, unreadable) = tree::read(&a).unwrap();
        tree::prepare_out(&a.src, &f.out(), false).unwrap();
        let t = tree::convert(&a, &inputs, unreadable, Some(&f.out())).unwrap();
        tree::write(&t, &f.out()).unwrap();
        f
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

    fn setup(&self) -> Setup {
        Setup {
            tree: self.args(),
            out: self.out(),
            instances: None,
            packs: Vec::new(),
        }
    }

    fn open(&self) -> Review {
        Review::open(self.setup()).unwrap()
    }

    fn ledger(&self) -> String {
        std::fs::read_to_string(self.out().join(LEDGER_FILE)).unwrap_or_default()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Units of `path` in the review state's queue, by id.
fn unit_items(r: &Review, path: &str, all: bool) -> Vec<String> {
    r.items(all)
        .iter()
        .filter(|i| i.item.path == path)
        .filter_map(|i| i.item.unit.clone())
        .collect()
}

/// The first unit of `path` with a note `rule` of `status`.
fn unit_with(r: &Review, path: &str, status: Status, all: bool) -> UnitView {
    unit_items(r, path, all)
        .iter()
        .filter_map(|id| r.unit(path, id))
        .find(|u| u.notes.iter().any(|n| n.status == status) || u.status == status)
        .unwrap_or_else(|| panic!("no {status:?} unit in {path}"))
}

fn target(u: &UnitView) -> Target {
    Target {
        path: u.path.clone(),
        unit: u.unit.clone(),
        input_hash: u.input_hash.clone(),
        output_hash: u.output_hash.clone(),
    }
}

fn req(action: Action, rule: &str, targets: Vec<Target>) -> RecordRequest {
    RecordRequest {
        action,
        rule: rule.into(),
        reason: "checked against the schema".into(),
        author: "Davy".into(),
        targets,
    }
}

#[test]
fn accepting_an_assumption_writes_one_human_record_on_both_file_hashes() {
    let f = Fixture::new("accept", &[("a.sql", ASSUMING)]);
    let mut r = f.open();
    let u = unit_with(&r, "a.sql", Status::Assumptions, false);
    assert_eq!(u.input_hash, file_hash(ASSUMING));
    let before = r.state(false);
    assert!(before.reviewed.is_empty());
    let rules = u.assumptions.clone();
    assert!(!rules.is_empty());
    for rule in &rules {
        r.record(req(Action::Accept, rule, vec![target(&u)]), AT)
            .unwrap();
    }
    let ledger = Ledger::parse(&f.ledger());
    assert!(
        ledger.rejected_lines.is_empty(),
        "{:?}",
        ledger.rejected_lines
    );
    let recs: Vec<_> = ledger.records().collect();
    assert_eq!(recs.len(), rules.len());
    for rec in recs {
        assert_eq!(rec.by, By::Human);
        assert_eq!(rec.state, State::Accepted);
        assert_eq!(rec.author, "Davy");
        assert_eq!(rec.unit, u.unit);
        assert_eq!(rec.input_hash, file_hash(ASSUMING));
        assert_eq!(rec.output_hash, u.output_hash);
        assert_eq!(rec.at, AT);
    }
    // Every assumption accepted: the unit leaves the queue and is accepted in
    // review, with its status unchanged (never shown as converted).
    let after = r.state(false);
    assert!(!unit_items(&r, "a.sql", true).contains(&u.unit));
    let rev = after.reviewed.iter().find(|x| x.unit == u.unit).unwrap();
    assert_eq!(rev.status, Status::Assumptions);
    assert_eq!(rev.by, vec!["Davy".to_string()]);
    assert_eq!(
        r.unit("a.sql", &u.unit).unwrap().status,
        Status::Assumptions
    );
}

#[test]
fn the_page_cannot_say_who_made_a_decision() {
    let body = serde_json::json!({
        "action": "approve", "reason": "x", "author": "bot", "by": "agent",
        "targets": [],
    });
    assert!(serde_json::from_value::<RecordRequest>(body).is_err());
    let body = serde_json::json!({
        "action": "approve", "reason": "x", "author": "bot",
        "targets": [{"path": "a", "unit": "u", "input_hash": "", "output_hash": "", "by": "agent"}],
    });
    assert!(serde_json::from_value::<RecordRequest>(body).is_err());
    // `written` is not an action the page has.
    let body = serde_json::json!({
        "action": "written", "reason": "x", "author": "bot", "targets": [],
    });
    assert!(serde_json::from_value::<RecordRequest>(body).is_err());
}

#[test]
fn records_that_would_not_apply_are_refused_and_nothing_is_written() {
    let f = Fixture::new(
        "refuse",
        &[("a.sql", ASSUMING), ("q.sql", DECIDING), ("r.sql", REFUSED)],
    );
    let mut r = f.open();
    let a = unit_with(&r, "a.sql", Status::Assumptions, false);
    let q = unit_with(&r, "q.sql", Status::Decision, false);
    let x = unit_with(&r, "r.sql", Status::NotConverted, false);
    let decision = q.decisions[0].clone();
    let cases = vec![
        // A decision is picked, not accepted; an assumption accepted, not picked.
        req(Action::Accept, &decision, vec![target(&q)]),
        req(Action::Pick, &a.assumptions[0], vec![target(&a)]),
        req(Action::Accept, "ASM-NOT-THERE", vec![target(&a)]),
        // An approval never stands in for fixing a refusal.
        req(Action::Approve, "", vec![target(&x)]),
        // An approval names no rule.
        req(Action::Approve, &a.assumptions[0], vec![target(&a)]),
        // No reason, no author.
        RecordRequest {
            reason: "  ".into(),
            ..req(Action::Accept, &a.assumptions[0], vec![target(&a)])
        },
        RecordRequest {
            author: " ".into(),
            ..req(Action::Accept, &a.assumptions[0], vec![target(&a)])
        },
        RecordRequest {
            reason: String::new(),
            ..req(Action::Comment, "", vec![target(&a)])
        },
        // Hashes the page did not see: the file changed since.
        req(
            Action::Accept,
            &a.assumptions[0],
            vec![Target {
                input_hash: file_hash("other text"),
                ..target(&a)
            }],
        ),
        req(
            Action::Accept,
            &a.assumptions[0],
            vec![Target {
                unit: "u0000000000000000".into(),
                ..target(&a)
            }],
        ),
        // Only accepting is done for several units at once.
        req(Action::Reject, "", vec![target(&a), target(&q)]),
        // The same unit twice.
        req(
            Action::Accept,
            &a.assumptions[0],
            vec![target(&a), target(&a)],
        ),
        req(Action::Comment, "", Vec::new()),
    ];
    assert!(x.approve_blocked.is_some());
    for c in cases {
        let what = format!("{c:?}");
        assert!(r.record(c, AT).is_err(), "accepted: {what}");
    }
    assert_eq!(f.ledger(), "", "a refused request wrote something");
}

#[test]
fn bulk_accept_writes_one_record_per_unit_or_none() {
    let f = Fixture::new(
        "bulk",
        &[
            ("a.sql", ASSUMING),
            ("b.sql", ASSUMING_2),
            ("q.sql", DECIDING),
        ],
    );
    let mut r = f.open();
    let a = unit_with(&r, "a.sql", Status::Assumptions, false);
    let b = unit_with(&r, "b.sql", Status::Assumptions, false);
    let q = unit_with(&r, "q.sql", Status::Decision, false);
    let rule = a
        .assumptions
        .iter()
        .find(|x| b.assumptions.contains(x) && !q.assumptions.contains(x))
        .expect("an assumption a and b share and q lacks")
        .clone();
    // One unit without it: the whole batch is refused.
    let bad = req(
        Action::Accept,
        &rule,
        vec![target(&a), target(&b), target(&q)],
    );
    assert!(r.record(bad, AT).is_err());
    assert_eq!(f.ledger(), "");
    let n = r
        .record(req(Action::Accept, &rule, vec![target(&a), target(&b)]), AT)
        .unwrap();
    assert_eq!(n, 2);
    let ledger = Ledger::parse(&f.ledger());
    let mut units: Vec<_> = ledger
        .records()
        .map(|x| (x.path.clone(), x.unit.clone(), x.rule.clone()))
        .collect();
    units.sort();
    assert_eq!(
        units,
        vec![
            ("a.sql".to_string(), a.unit.clone(), rule.clone()),
            ("b.sql".to_string(), b.unit.clone(), rule.clone())
        ]
    );
    // The accepted rule is no longer open on either.
    for u in [&a, &b] {
        let open = &r.unit(&u.path, &u.unit).unwrap().review.unwrap().open;
        assert!(!open.contains(&rule), "{open:?}");
    }
}

#[test]
fn a_comment_discharges_nothing_and_a_rejection_keeps_the_unit_open() {
    let f = Fixture::new("comment", &[("p.sql", PLAIN), ("a.sql", ASSUMING)]);
    let mut r = f.open();
    let p = unit_with(&r, "p.sql", Status::Converted, true);
    assert!(p.approve_blocked.is_none());
    // Plainly converted and nobody approved it: tier 6 only, never reviewed.
    assert!(unit_items(&r, "p.sql", false).is_empty());
    assert!(unit_items(&r, "p.sql", true).contains(&p.unit));
    r.record(req(Action::Comment, "", vec![target(&p)]), AT)
        .unwrap();
    assert!(unit_items(&r, "p.sql", true).contains(&p.unit));
    assert!(r.state(true).reviewed.is_empty());
    // An approval reviews it, and the same author's rejection replaces it.
    r.record(req(Action::Approve, "", vec![target(&p)]), AT)
        .unwrap();
    assert!(!unit_items(&r, "p.sql", true).contains(&p.unit));
    let rev = r.state(true).reviewed;
    assert_eq!(rev.len(), 1);
    assert_eq!(rev[0].status, Status::Converted);
    r.record(
        req(Action::Reject, "", vec![target(&p)]),
        "2026-10-26T12:01:00Z",
    )
    .unwrap();
    let items = r.items(false);
    let item = items
        .iter()
        .find(|i| i.item.unit.as_deref() == Some(p.unit.as_str()))
        .unwrap();
    assert_eq!(item.item.tier, 1, "a rejected unit is wrong or missing");
    // Another person's approval does not lift it.
    let other = RecordRequest {
        author: "Someone else".into(),
        ..req(Action::Approve, "", vec![target(&p)])
    };
    r.record(other, "2026-10-26T12:02:00Z").unwrap();
    assert!(unit_items(&r, "p.sql", false).contains(&p.unit));
    // Approving a unit with open assumptions does not accept them.
    let a = unit_with(&r, "a.sql", Status::Assumptions, false);
    r.record(req(Action::Approve, "", vec![target(&a)]), AT)
        .unwrap();
    assert!(unit_items(&r, "a.sql", false).contains(&a.unit));
}

#[test]
fn after_a_source_changes_the_tree_is_converted_again_and_decisions_are_outdated() {
    let body = format!("{ASSUMING}GO\n{PLAIN}");
    let f = Fixture::new("outdated", &[("a.sql", &body)]);
    let mut r = f.open();
    let u = unit_with(&r, "a.sql", Status::Assumptions, false);
    for rule in &u.assumptions {
        r.record(req(Action::Accept, rule, vec![target(&u)]), AT)
            .unwrap();
    }
    assert!(
        !r.converted_last(),
        "nothing changed: the conversion is kept"
    );
    // Kept conversion = a fresh one.
    assert_eq!(r.report_json(), f.open().report_json());
    assert!(r.items(false).iter().all(|i| !i.item.outdated));

    // The unit's own text is the same; the file around it is not.
    std::fs::write(f.dir.join("src/a.sql"), format!("{body}-- changed\n")).unwrap();
    r.refresh().unwrap();
    assert!(r.converted_last());
    let st = r.state(false);
    let items = r.items(false);
    let item = items
        .iter()
        .find(|i| i.item.unit.as_deref() == Some(u.unit.as_str()))
        .expect("the unit is open again");
    assert!(item.item.outdated);
    assert_eq!(st.outdated, 1);
    assert!(st.out_differs.contains(&"a.sql".to_string()));
    // A decision made on what the page showed before is refused now.
    let stale = req(Action::Accept, &u.assumptions[0], vec![target(&u)]);
    assert!(r.record(stale, AT).is_err());
    assert_eq!(r.report_json(), f.open().report_json());
}

#[test]
fn bad_ledger_lines_are_reported_not_applied() {
    let f = Fixture::new("badline", &[("a.sql", ASSUMING)]);
    std::fs::write(f.out().join(LEDGER_FILE), "{not json").unwrap();
    let mut r = f.open();
    assert_eq!(r.state(false).not_applied.len(), 1);
    // Appending after a line without its newline starts a new line.
    let u = unit_with(&r, "a.sql", Status::Assumptions, false);
    r.record(req(Action::Comment, "", vec![target(&u)]), AT)
        .unwrap();
    let text = f.ledger();
    assert!(text.starts_with("{not json\n{"), "{text}");
    assert_eq!(r.state(false).not_applied.len(), 1);
    assert_eq!(r.state(false).records, 1);
}

#[test]
fn the_page_carries_the_shared_highlighter_and_the_state() {
    let f = Fixture::new("page", &[("a.sql", ASSUMING)]);
    let r = f.open();
    let html = sqldiff_gui::convert_review::page(&r.state(false));
    assert!(html.contains("function lexSql("));
    assert!(html.contains("function hlLines("));
    assert!(!html.contains("/*__SQLDIFF_LEXER__*/"));
    assert!(!html.contains("/*__SQLDIFF_PAYLOAD__*/null"));
}

#[test]
fn out_must_be_the_converted_tree() {
    let f = Fixture::new("noout", &[("a.sql", ASSUMING)]);
    let r = Review::open(Setup {
        tree: f.args(),
        out: f.dir.join("missing"),
        instances: None,
        packs: Vec::new(),
    });
    assert!(r.is_err());
}

#[test]
fn a_text_written_into_the_ledger_is_converted_in_on_refresh() {
    let f = Fixture::new("written", &[("p.sql", PLAIN)]);
    let mut r = f.open();
    let p = unit_with(&r, "p.sql", Status::Converted, true);
    // An agent writes the unit by hand (as MCP `write_unit` does) while
    // the page is open; no source file changes.
    let rec = sqldiff_convert::review::Record {
        unit: p.unit.clone(),
        path: "p.sql".into(),
        rule: String::new(),
        state: State::Written,
        by: By::Agent,
        author: "bot".into(),
        reason: String::new(),
        at: AT.into(),
        input_hash: String::new(),
        output_hash: String::new(),
        text: Some("CREATE TABLE t (a integer NOT NULL);".into()),
        base_catalogs: None,
    };
    let mut ledger = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(f.out().join(LEDGER_FILE))
        .unwrap();
    std::io::Write::write_all(
        &mut ledger,
        (serde_json::to_string(&rec).unwrap() + "\n").as_bytes(),
    )
    .unwrap();
    r.refresh().unwrap();
    assert!(r.converted_last(), "the ledger's texts changed");
    assert_eq!(r.report_json(), f.open().report_json());
}

#[test]
fn the_done_gate_follows_the_decisions_and_the_files() {
    let f = Fixture::new("gate", &[("a.sql", ASSUMING), ("t.sql", PLAIN)]);
    let mut r = f.open();
    let g = r.state(false).gate.expect("judged");
    assert!(!g.complete);
    assert!(g.counts.keys().all(|k| k == "open"), "{:?}", g.counts);
    // Every assumption accepted by a person: done, and still so after a refresh.
    for id in unit_items(&r, "a.sql", false) {
        let u = r.unit("a.sql", &id).unwrap();
        for rule in u.assumptions.clone() {
            r.record(req(Action::Accept, &rule, vec![target(&u)]), AT)
                .unwrap();
        }
    }
    let g = r.state(false).gate.expect("judged");
    assert!(g.complete, "{:?}", g.blockers);
    assert_eq!(g.nothing_found, 1, "the table: nothing found, not reviewed");
    // Accepted by name, as `sqldiff review-status` lists it.
    assert!(!g.accepted.is_empty());
    assert!(
        g.accepted.iter().all(|a| a.authors == ["Davy"]),
        "{:?}",
        g.accepted
    );
    r.refresh().unwrap();
    assert!(r.state(false).gate.unwrap().complete);
    // The converted tree no longer what converting gives. Before anything
    // reads it again, the verdict on the old files no longer stands.
    std::fs::write(f.out().join("t.sql"), "-- edited\n").unwrap();
    let st = r.state(false);
    assert!(st.gate.is_none(), "a Done on files that changed since");
    assert_eq!(st.gate_error.as_deref(), Some(GATE_STALE));
    assert_eq!(r.gate().unwrap_err(), GATE_STALE);
    // A new source file makes it stale too.
    r.refresh().unwrap();
    assert!(r.state(false).gate.is_some());
    std::fs::write(f.dir.join("src").join("new.sql"), PLAIN).unwrap();
    assert_eq!(r.gate().unwrap_err(), GATE_STALE);
    std::fs::remove_file(f.dir.join("src").join("new.sql")).unwrap();
    // Read again: judged on the files as they are.
    r.refresh().unwrap();
    let st = r.state(false);
    let g = st.gate.unwrap();
    assert!(!g.complete);
    assert!(g.counts.contains_key("stale-output"), "{:?}", g.counts);
    // Judging never leaves a ledger behind on the page's state.
    assert_eq!(st.gate_error, None);
}

#[test]
fn without_a_ledger_the_gate_is_judged_and_the_state_keeps_no_ledger() {
    let f = Fixture::new("gate-none", &[("t.sql", PLAIN)]);
    let r = f.open();
    assert!(!f.out().join(LEDGER_FILE).exists());
    let st = r.state(false);
    assert_eq!(st.records, 0);
    let g = st.gate.unwrap();
    // A plainly converted table needs no decision.
    assert!(g.complete, "{:?}", g.blockers);
    assert!(r.state(false).reviewed.is_empty());
}

#[test]
fn a_decision_follows_the_base_catalogs_contents() {
    let dir = std::env::temp_dir().join(format!("sqldiff-gui-review-base-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (p, text) in [("src/a.sql", ASSUMING), ("base/t.sql", PLAIN)] {
        let f = dir.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, text).unwrap();
    }
    let a = || tree::Args {
        src: dir.join("src"),
        from: Dialect::Tsql,
        to: Dialect::Postgres,
        jobs: Some(1),
        no_catalog: false,
        template: None,
        extensions: tree::extensions(Vec::new()),
        helpers: Vec::new(),
        catalogs: vec![dir.join("base/t.sql")],
        catalogs_b: Vec::new(),
        deployment: Default::default(),
    };
    let out = dir.join("out");
    let (inputs, unreadable) = tree::read(&a()).unwrap();
    tree::prepare_out(&a().src, &out, false).unwrap();
    let t = tree::convert(&a(), &inputs, unreadable, Some(&out)).unwrap();
    tree::write(&t, &out).unwrap();
    let mut r = Review::open(Setup {
        tree: a(),
        out: out.clone(),
        instances: None,
        packs: Vec::new(),
    })
    .unwrap();
    let base = r.report_json()["base_hash"].as_str().unwrap().to_string();
    let u = unit_with(&r, "a.sql", Status::Assumptions, false);
    for rule in &u.assumptions {
        r.record(req(Action::Accept, rule, vec![target(&u)]), AT)
            .unwrap();
    }
    let text = std::fs::read_to_string(out.join(LEDGER_FILE)).unwrap();
    let ledger = Ledger::parse(&text);
    assert!(ledger
        .records()
        .all(|x| x.base_catalogs.as_deref() == Some(base.as_str())));
    let open = |r: &Review| {
        r.unit("a.sql", &u.unit)
            .and_then(|v| v.review)
            .map(|v| v.open)
            .unwrap()
    };
    r.refresh().unwrap();
    assert!(!r.converted_last());
    assert!(open(&r).is_empty());
    // Other contents: converted again, and the acceptances no longer apply.
    std::fs::write(
        dir.join("base/t.sql"),
        "CREATE TABLE t (a int NOT NULL, b int NULL);\n",
    )
    .unwrap();
    r.refresh().unwrap();
    assert!(r.converted_last());
    assert_ne!(r.report_json()["base_hash"].as_str().unwrap(), base);
    assert_eq!(open(&r), u.assumptions);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn listed_units_carry_their_hashes_and_what_no_decision_discharges() {
    let f = Fixture::new(
        "listed",
        &[
            ("a.sql", ASSUMING),
            ("b.sql", ASSUMING_2),
            ("r.sql", REFUSED),
        ],
    );
    let mut r = f.open();
    let items = r.items(false);
    // The page groups and bulk-accepts from the list alone: each unit item
    // carries the hashes /api/unit would give, and what blocks it.
    for i in items.iter().filter(|i| i.item.unit.is_some()) {
        let u = r.unit(&i.item.path, i.item.unit.as_ref().unwrap()).unwrap();
        assert_eq!(
            i.hashes,
            Some((u.input_hash.clone(), u.output_hash.clone())),
            "{}",
            i.item.path
        );
        if i.item.tier <= 2 {
            assert!(!i.blocking.is_empty(), "{} lists no blocker", i.item.path);
        } else {
            assert!(i.blocking.is_empty(), "{}: {:?}", i.item.path, i.blocking);
        }
    }
    let refused = items
        .iter()
        .find(|i| i.item.path == "r.sql" && i.item.unit.is_some())
        .expect("the refused unit is listed");
    assert_eq!(refused.item.tier, 1);
    // A group's bulk accept sends the listed hashes; the server takes them.
    let a = unit_with(&r, "a.sql", Status::Assumptions, false);
    let b = unit_with(&r, "b.sql", Status::Assumptions, false);
    let rule = a
        .assumptions
        .iter()
        .find(|x| b.assumptions.contains(x))
        .expect("a shared assumption")
        .clone();
    let targets: Vec<Target> = items
        .iter()
        .filter(|i| i.item.path != "r.sql" && i.open_assumptions.contains(&rule))
        .map(|i| {
            let (input_hash, output_hash) = i.hashes.clone().unwrap();
            Target {
                path: i.item.path.clone(),
                unit: i.item.unit.clone().unwrap(),
                input_hash,
                output_hash,
            }
        })
        .collect();
    let n = targets.len();
    assert!(n >= 2, "{n}");
    assert_eq!(
        r.record(req(Action::Accept, &rule, targets), AT).unwrap(),
        n
    );
    assert!(!r
        .items(false)
        .iter()
        .any(|i| i.item.path != "r.sql" && i.open_assumptions.contains(&rule)));
}

#[test]
fn the_page_gets_the_gates_first_blockers_and_how_many_there_are() {
    // A blocker or more per file: more than the review keeps.
    let names: Vec<String> = (0..GATE_BLOCKERS_SHOWN + 5)
        .map(|i| format!("p{i}.sql"))
        .collect();
    let files: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), ASSUMING)).collect();
    let f = Fixture::new("gate-shown", &files);
    let r = f.open();
    // The gate as `sqldiff review-status` judges it, every blocker listed.
    let a = f.args();
    let (inputs, unreadable) = tree::read(&a).unwrap();
    let mut t = tree::convert(&a, &inputs, unreadable, Some(&f.out())).unwrap();
    tree::review(&mut t, &inputs, &Ledger::default(), "(none)");
    let full = sqldiff_convert::gate::check(&t, &a, &f.out(), None).unwrap();
    assert!(
        full.blockers.len() > GATE_BLOCKERS_SHOWN,
        "{}",
        full.blockers.len()
    );
    let g = r.gate().unwrap();
    assert_eq!(g.blockers.len(), GATE_BLOCKERS_SHOWN);
    assert_eq!(g.blockers_total, full.blockers.len());
    // The first blockers, and everything else as the gate has it.
    let mut expect = serde_json::to_value(&full).unwrap();
    expect["blockers"] = serde_json::to_value(&full.blockers[..GATE_BLOCKERS_SHOWN]).unwrap();
    expect["blockers_total"] = full.blockers.len().into();
    let st = serde_json::to_value(r.state(false)).unwrap();
    assert_eq!(st["gate"], expect);
    // `/api/gate` gives the state's gate, error and time.
    let gate = serde_json::to_value(r.gate_state()).unwrap();
    for k in ["gate", "gate_error", "gate_judged"] {
        assert_eq!(gate[k], st[k], "{k}");
    }
}

#[test]
fn the_gate_banner_counts_the_blockers_it_does_not_list() {
    use std::process::Command;
    if Command::new("node").arg("--version").output().is_err() {
        assert!(
            std::env::var_os("CI").is_none(),
            "node is needed for this test on CI"
        );
        eprintln!("skipped: node not found");
        return;
    }
    // CRLF where Windows CI checks the page out so.
    let page = sqldiff_gui::convert_review::REVIEW_HTML.replace("\r\n", "\n");
    let esc = &page[page.find("const esc = ").expect("esc")..];
    let esc = &esc[..esc.find('\n').unwrap()];
    let list = &page[page.find("function blockerList(g)").expect("blockerList")..];
    let list = &list[..list.find("\n}\n").unwrap() + 2];
    let blocker =
        serde_json::json!({ "kind": "open", "path": "a.sql", "out_line": 3, "what": "x" });
    let gate = |shown: usize, total: usize| serde_json::json!({ "blockers": vec![blocker.clone(); shown], "blockers_total": total });
    let script = format!(
        "{esc}\n{list}\nfor (const g of [{}, {}]) console.log(blockerList(g));\n",
        gate(GATE_BLOCKERS_SHOWN, GATE_BLOCKERS_SHOWN + 5),
        gate(2, 2),
    );
    let dir = std::env::temp_dir().join(format!("sqldiff-banner-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("banner.js"), script).unwrap();
    let out = Command::new("node")
        .arg(dir.join("banner.js"))
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0].matches("<li>a.sql:3").count(), GATE_BLOCKERS_SHOWN);
    assert!(lines[0].contains("<li>… and 5 more</li>"), "{}", lines[0]);
    assert_eq!(lines[1].matches("<li>a.sql:3").count(), 2);
    assert!(!lines[1].contains("more"), "{}", lines[1]);
}

/// The state as the page gets it, without what differs between two reads
/// of the same tree (when, how long).
fn state_json(r: &Review) -> serde_json::Value {
    let mut v = serde_json::to_value(r.state(true)).unwrap();
    for k in ["convert_ms", "gate_judged"] {
        v.as_object_mut().unwrap().remove(k);
    }
    v
}

#[test]
fn a_stored_conversion_opens_the_review_a_fresh_one_would() {
    let f = Fixture::new("stored", &[("a.sql", ASSUMING), ("t.sql", PLAIN)]);
    let fresh = f.open();
    let tree: tree::Tree = serde_json::from_slice(&fresh.tree_json()).unwrap();
    // Read back byte for byte.
    assert_eq!(serde_json::to_vec(&tree).unwrap(), fresh.tree_json());
    let (key, now) = stored_key(&f.setup(), b"extra").unwrap();
    assert_eq!(key, fresh.stored_key(b"extra"));
    assert_ne!(key, fresh.stored_key(b"other build or config"));
    let mut r = Review::open_stored(f.setup(), tree, now).unwrap();
    assert!(!r.converted_last());
    // Queue, gate and all, as converting gives.
    assert_eq!(state_json(&r), state_json(&fresh));
    assert!(r.gate().is_ok());
    // Decisions apply to it as to a fresh one.
    let u = unit_with(&r, "a.sql", Status::Assumptions, false);
    for rule in u.assumptions.clone() {
        r.record(req(Action::Accept, &rule, vec![target(&u)]), AT)
            .unwrap();
    }
    let mut again = f.open();
    again.refresh().unwrap();
    assert_eq!(state_json(&r), state_json(&again));
    // Files in OUT edited by hand still make it stale.
    std::fs::write(f.out().join("t.sql"), "-- edited\n").unwrap();
    assert_eq!(r.gate().unwrap_err(), GATE_STALE);
    // A changed source, or a hand-written text, changes the key.
    std::fs::write(
        f.dir.join("src").join("t.sql"),
        "CREATE TABLE t (b int NOT NULL);\n",
    )
    .unwrap();
    assert_ne!(stored_key(&f.setup(), b"extra").unwrap().0, key);
}

#[test]
fn the_list_pages_through_the_queue_as_the_queue_has_it() {
    let f = Fixture::new(
        "list-pages",
        &[
            ("a.sql", ASSUMING),
            ("b.sql", ASSUMING_2),
            ("c.sql", DECIDING),
            ("d.sql", REFUSED),
            ("t.sql", PLAIN),
        ],
    );
    let r = f.open();
    let items = r.items(false);
    assert!(items.len() > 2, "{}", items.len());
    // By tier, every tier: the rows, a page of one at a time, are the queue.
    let q = Query {
        tiers: vec![1, 2, 3, 4, 5],
        group: Group::Tier,
        filter: String::new(),
    };
    let l = r.list(&q, 0, 1000);
    assert_eq!((l.total, l.shown), (items.len(), items.len()));
    let mut paged = Vec::new();
    for g in &l.groups {
        let mut from = 0;
        while let Some(page) = r.rows(&q, &g.key, from, 1).filter(|p| from < p.n) {
            paged.extend(page.rows);
            from += 1;
        }
    }
    let k = |v: &[sqldiff_gui::convert_review::ListedItem]| -> Vec<String> {
        v.iter().map(|i| i.item.key.clone()).collect()
    };
    assert_eq!(k(&paged), k(&items));
    // Rows carry the hashes the unit view gives.
    for i in paged.iter().filter(|i| i.item.unit.is_some()) {
        let u = r.unit(&i.item.path, i.item.unit.as_ref().unwrap()).unwrap();
        assert_eq!(i.hashes, Some((u.input_hash, u.output_hash)));
    }
    // The walk starts with the first, and a page of groups is a page.
    let first = r.near(&q, Near::Index(0)).unwrap();
    assert_eq!(first.key, items[0].item.key);
    assert_eq!(r.list(&q, 1, 1).groups.len(), 1.min(l.groups_total - 1));
    assert!(r.rows(&q, "no such group", 0, 10).is_none());
    // The state counts, the list lists.
    let st = r.state(false);
    assert_eq!(st.open, items.len());
    assert_eq!(st.reviewed_total, st.reviewed.len());
}
