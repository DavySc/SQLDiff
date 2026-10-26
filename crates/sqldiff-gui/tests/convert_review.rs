//! Review mode (`sqldiff-gui review`): what the page's actions write to the
//! ledger, what they refuse, and how the queue follows.

use std::path::PathBuf;

use sqldiff_convert::review::{file_hash, By, Ledger, State, LEDGER_FILE};
use sqldiff_convert::tree;
use sqldiff_convert::Status;
use sqldiff_core::Dialect;
use sqldiff_gui::convert_review::{Action, RecordRequest, Review, Setup, Target, UnitView};

const ASSUMING: &str = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  UPDATE dbo.t SET a = 1;\nEND\n";
const ASSUMING_2: &str = "CREATE PROCEDURE dbo.p2 AS\nBEGIN\n  UPDATE dbo.t SET a = 2;\nEND\n";
const DECIDING: &str =
    "CREATE PROCEDURE dbo.q AS\nBEGIN\n  BEGIN TRAN;\n  UPDATE t SET a = 1;\n  COMMIT;\nEND\n";
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
        }
    }

    fn open(&self) -> Review {
        Review::open(Setup {
            tree: self.args(),
            out: self.out(),
            instances: None,
        })
        .unwrap()
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
    r.state(all)
        .items
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
    let st = r.state(false);
    let item = st
        .items
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
    assert!(r.state(false).items.iter().all(|i| !i.item.outdated));

    // The unit's own text is the same; the file around it is not.
    std::fs::write(f.dir.join("src/a.sql"), format!("{body}-- changed\n")).unwrap();
    r.refresh().unwrap();
    assert!(r.converted_last());
    let st = r.state(false);
    let item = st
        .items
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
    });
    assert!(r.is_err());
}
