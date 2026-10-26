use sqldiff_convert::review::{
    review_file, unit_ids, By, Ledger, Record, State, UnitReview, LEDGER_FILE,
};
use sqldiff_convert::{convert, Conversion, Status};
use sqldiff_core::artifact::content_hash;
use sqldiff_core::Dialect;

const PATH: &str = "procs/p.sql";

fn pg(text: &str) -> Conversion {
    convert(text, Dialect::Tsql, Dialect::Postgres).unwrap()
}

/// The first unit with this status, and its id.
fn first(input: &str, c: &Conversion, s: Status) -> (usize, String, String) {
    let ids = unit_ids(PATH, input, c);
    let i = c
        .units
        .iter()
        .position(|u| u.status == s)
        .unwrap_or_else(|| panic!("no {} unit in\n{}", s.label(), c.output));
    let rule = c.units[i]
        .notes
        .iter()
        .find(|n| n.status == s)
        .map(|n| n.rule.clone())
        .unwrap_or_default();
    (i, ids[i].clone(), rule)
}

/// The unit whose input holds `needle`, its id and its first assumption.
fn at(input: &str, c: &Conversion, needle: &str) -> (usize, String, String) {
    let p = input.find(needle).unwrap();
    let i = c
        .units
        .iter()
        .position(|u| u.src.start <= p && p < u.src.end)
        .unwrap();
    let rule = c.units[i]
        .notes
        .iter()
        .find(|n| n.status == Status::Assumptions)
        .map(|n| n.rule.clone())
        .unwrap_or_else(|| panic!("no assumption on {needle}: {:?}", c.units[i].notes));
    (i, unit_ids(PATH, input, c)[i].clone(), rule)
}

fn record(input: &str, c: &Conversion, unit: &str, rule: &str, state: State, by: By) -> Record {
    Record {
        unit: unit.to_string(),
        path: PATH.to_string(),
        rule: rule.to_string(),
        state,
        by,
        author: "Davy".into(),
        reason: "checked against the schema".into(),
        at: "2026-10-26T10:00:00Z".into(),
        input_hash: content_hash(input),
        output_hash: content_hash(&c.output),
    }
}

fn review(input: &str, c: &Conversion, records: Vec<Record>) -> Vec<UnitReview> {
    let mut l = Ledger::default();
    for r in records {
        l.insert(r).unwrap();
    }
    review_file(PATH, input, c, &l).units
}

const ASSUMING: &str = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  UPDATE dbo.t SET a = 1;\nEND\n";

#[test]
fn accepting_every_assumption_reviews_the_unit_without_changing_its_status() {
    let c = pg(ASSUMING);
    let (i, id, rule) = at(ASSUMING, &c, "UPDATE");
    let before = review(ASSUMING, &c, vec![]);
    assert!(!before[i].reviewed);
    assert!(before[i].open.contains(&rule));

    let recs = c.units[i]
        .notes
        .iter()
        .filter_map(|n| {
            match n.status {
                Status::Assumptions => Some(State::Accepted),
                Status::Decision => Some(State::Picked),
                _ => None,
            }
            .map(|s| record(ASSUMING, &c, &id, &n.rule, s, By::Human))
        })
        .collect();
    let after = review(ASSUMING, &c, recs);
    assert!(after[i].open.is_empty(), "{:?}", after[i]);
    assert_eq!(
        after[i].reviewed,
        after[i].undischargeable.is_empty(),
        "{:?}",
        after[i]
    );
    assert_eq!(
        after[i].status, c.units[i].status,
        "a decision never changes a status"
    );
}

#[test]
fn one_changed_byte_anywhere_in_the_file_outdates_a_decision() {
    let c = pg(ASSUMING);
    let (_, id, rule) = at(ASSUMING, &c, "UPDATE");
    let rec = record(ASSUMING, &c, &id, &rule, State::Accepted, By::Human);

    // Same unit text, but the file around it changed: outdated.
    let edited = format!("{ASSUMING}-- a later note\n");
    let c2 = pg(&edited);
    let r2 = review(&edited, &c2, vec![rec.clone()]);
    let u = r2
        .iter()
        .find(|u| u.id == id)
        .expect("unit text unchanged, same id");
    assert!(!u.decisions[0].current);
    assert!(u.open.contains(&rule));

    // The unit's own text changed: a new id, and the record is orphaned.
    let changed = ASSUMING.replace("a = 1", "a = 2");
    let c3 = pg(&changed);
    let mut l = Ledger::default();
    l.insert(rec).unwrap();
    let f = review_file(PATH, &changed, &c3, &l);
    assert!(f.units.iter().all(|u| u.id != id));
    assert_eq!(f.orphaned.len(), 1);
}

#[test]
fn differences_refusals_and_unproven_are_never_discharged() {
    let src = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  SELECT @@ERROR;\nEND\n";
    let c = pg(src);
    let (i, id, rule) = first(src, &c, Status::NotConverted);
    let recs = vec![
        record(src, &c, &id, &rule, State::Accepted, By::Human),
        record(src, &c, &id, "", State::Approved, By::Human),
    ];
    let r = review(src, &c, recs);
    assert!(!r[i].reviewed);
    assert!(r[i].undischargeable.contains(&rule));
    let acc = r[i]
        .decisions
        .iter()
        .find(|a| a.record.state == State::Accepted)
        .unwrap();
    assert!(!acc.current, "{:?}", acc.why_not);
}

#[test]
fn agents_only_write_and_comment_and_their_output_needs_a_person() {
    let c = pg(ASSUMING);
    let (_, id, rule) = first(ASSUMING, &c, Status::Assumptions);
    let mut l = Ledger::default();
    for s in [
        State::Accepted,
        State::Picked,
        State::Approved,
        State::Rejected,
    ] {
        let mut r = record(ASSUMING, &c, &id, &rule, s, By::Agent);
        if s == State::Approved {
            r.rule.clear();
        }
        assert!(l.insert(r).is_err(), "agent {}", s.label());
    }

    // A plainly converted unit an agent wrote is open until a person approves it.
    let src = "SELECT 1 AS x;\n";
    let c = pg(src);
    let (i, id, _) = first(src, &c, Status::Converted);
    let wrote = record(src, &c, &id, "", State::Written, By::Agent);
    let r = review(src, &c, vec![wrote.clone()]);
    assert_eq!(r[i].written_by, Some(By::Agent));
    assert!(!r[i].reviewed);
    let ok = record(src, &c, &id, "", State::Approved, By::Human);
    let r = review(src, &c, vec![wrote, ok]);
    assert!(r[i].reviewed);
}

#[test]
fn comments_discharge_nothing_and_plain_converted_is_not_reviewed() {
    let c = pg(ASSUMING);
    let (i, id, rule) = first(ASSUMING, &c, Status::Assumptions);
    let mut com = record(ASSUMING, &c, &id, &rule, State::Commented, By::Human);
    com.reason.clear();
    let r = review(ASSUMING, &c, vec![com]);
    assert!(!r[i].reviewed);
    assert!(r[i].open.contains(&rule));

    let src = "SELECT 1 AS x;\n";
    let c = pg(src);
    let (i, _, _) = first(src, &c, Status::Converted);
    assert!(!review(src, &c, vec![])[i].reviewed);
}

#[test]
fn rejected_keeps_the_unit_open_and_bad_lines_are_listed_not_applied() {
    let c = pg(ASSUMING);
    let (i, id, rule) = first(ASSUMING, &c, Status::Assumptions);
    let acc = record(ASSUMING, &c, &id, &rule, State::Accepted, By::Human);
    let rej = record(ASSUMING, &c, &id, "", State::Rejected, By::Human);
    let r = review(ASSUMING, &c, vec![acc.clone(), rej]);
    assert!(!r[i].reviewed);

    let mut no_reason = acc.clone();
    no_reason.reason.clear();
    let text = format!(
        "{}\nnot json\n{}\n",
        serde_json::to_string(&acc).unwrap(),
        serde_json::to_string(&no_reason).unwrap()
    );
    let l = Ledger::parse(&text);
    assert_eq!(l.records().count(), 1);
    assert_eq!(l.rejected_lines.len(), 2, "{:?}", l.rejected_lines);
    assert_eq!(LEDGER_FILE, "review-decisions.jsonl");
}

#[test]
fn identical_units_in_one_file_get_distinct_ids() {
    let src = "UPDATE dbo.t SET a = 1;\nUPDATE dbo.t SET a = 1;\n";
    let c = pg(src);
    let ids = unit_ids(PATH, src, &c);
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    assert_eq!(ids[1], format!("{}~2", ids[0]));
}
