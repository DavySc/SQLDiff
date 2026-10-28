use sqldiff_convert::review::{
    file_hash, review_file, unit_ids, By, Ledger, Record, State, UnitReview, LEDGER_FILE,
};
use sqldiff_convert::{convert, Conversion, Status};
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
        input_hash: file_hash(input),
        output_hash: file_hash(&c.output),
        text: None,
        base_catalogs: None,
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

#[test]
fn one_persons_rejection_is_never_replaced_by_anothers_approval() {
    let c = pg(ASSUMING);
    let (i, id, _) = at(ASSUMING, &c, "UPDATE");
    let accept_all = |author: &str| -> Vec<Record> {
        c.units[i]
            .notes
            .iter()
            .filter_map(|n| {
                match n.status {
                    Status::Assumptions => Some(State::Accepted),
                    Status::Decision => Some(State::Picked),
                    _ => None,
                }
                .map(|s| {
                    let mut r = record(ASSUMING, &c, &id, &n.rule, s, By::Human);
                    r.author = author.into();
                    r
                })
            })
            .collect()
    };
    let mut ann_ok = record(ASSUMING, &c, &id, "", State::Approved, By::Human);
    ann_ok.author = "Ann".into();
    let mut bob_no = record(ASSUMING, &c, &id, "", State::Rejected, By::Human);
    bob_no.author = "Bob".into();
    bob_no.at = "2026-10-26T12:00:00Z".into();

    // Bob's rejection, then Ann's approval appended later: still open.
    let mut recs = accept_all("Ann");
    recs.push(bob_no.clone());
    recs.push(ann_ok.clone());
    let r = review(ASSUMING, &c, recs);
    assert!(!r[i].reviewed, "{:?}", r[i]);
    assert!(r[i]
        .decisions
        .iter()
        .any(|a| a.current && a.record.author == "Bob" && a.record.state == State::Rejected));

    // Bob changing his own verdict clears his rejection.
    let mut bob_ok = ann_ok.clone();
    bob_ok.author = "Bob".into();
    let mut recs = accept_all("Ann");
    recs.extend([bob_no, ann_ok, bob_ok]);
    let r = review(ASSUMING, &c, recs);
    assert_eq!(r[i].reviewed, r[i].undischargeable.is_empty(), "{:?}", r[i]);
}

#[test]
fn trailing_blanks_outdate_decisions_and_unplaced_findings_hold_the_file() {
    assert_ne!(file_hash("SELECT 'a  ';\n"), file_hash("SELECT 'a';\n"));
    let src = "SELECT 1 AS x;\n";
    let c = pg(src);
    let f = review_file(PATH, src, &c, &Ledger::default());
    assert!(f.unplaced.is_empty());
    assert!(
        !f.reviewed(),
        "a converted unit nobody approved is not reviewed"
    );
}

mod tree_report {
    use super::*;
    use sqldiff_convert::tree;

    fn args(src: &std::path::Path) -> tree::Args {
        tree::Args {
            src: src.to_path_buf(),
            from: Dialect::Tsql,
            to: Dialect::Postgres,
            jobs: Some(1),
            no_catalog: false,
            template: None,
            extensions: tree::extensions(Vec::new()),
            helpers: Vec::new(),
            catalogs: Vec::new(),
            catalogs_b: Vec::new(),
        }
    }

    #[test]
    fn the_tree_report_lists_every_unit_by_id_and_applies_the_committed_ledger() {
        let dir = std::env::temp_dir().join(format!("sqldiff-review-tree-{}", std::process::id()));
        let (src, out) = (dir.join("src"), dir.join("out"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(src.join("procs")).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        let text = format!("SELECT 1 AS x;\n{ASSUMING}");
        std::fs::write(src.join("procs/p.sql"), &text).unwrap();

        let a = args(&src);
        let (inputs, unreadable) = tree::read(&a).unwrap();
        let t = tree::convert(&a, &inputs, unreadable, Some(&out)).unwrap();
        let f = &t.report.files[0];
        assert_eq!(f.path, PATH);
        let c = &t.conversions[0];
        assert_eq!(f.units.len() + f.converted_units.len(), c.units.len());
        assert!(f.converted_units.iter().all(|u| u.rules.is_empty()));
        let ids = unit_ids(PATH, &text, c);
        let mut listed: Vec<&str> = f
            .units
            .iter()
            .chain(&f.converted_units)
            .map(|u| u.id.as_str())
            .collect();
        listed.sort();
        let mut want: Vec<&str> = ids.iter().map(String::as_str).collect();
        want.sort();
        assert_eq!(listed, want);
        assert!(t.report.ledger.is_none());

        // A ledger next to the converted tree: accept everything on the UPDATE.
        let (i, id, _) = at(&text, c, "UPDATE");
        let lines: Vec<String> = c.units[i]
            .notes
            .iter()
            .filter_map(|n| {
                match n.status {
                    Status::Assumptions => Some(State::Accepted),
                    Status::Decision => Some(State::Picked),
                    _ => None,
                }
                .map(|s| record(&text, c, &id, &n.rule, s, By::Human))
            })
            .map(|r| serde_json::to_string(&r).unwrap())
            .chain(["not json".to_string()])
            .collect();
        std::fs::write(out.join(LEDGER_FILE), lines.join("\n")).unwrap();
        let mut t = tree::convert(&a, &inputs, Vec::new(), Some(&out)).unwrap();
        let bad = tree::review_in(&mut t, &inputs, &out).unwrap();
        assert_eq!(bad.len(), 1);
        let f = &t.report.files[0];
        let u = f.units.iter().find(|u| u.id == id).unwrap();
        let r = u.review.as_ref().unwrap();
        assert!(r.open.is_empty(), "{:?}", r.open);
        assert_eq!(r.reviewed, r.undischargeable.is_empty());
        assert_eq!(u.status, c.units[i].status, "statuses never change");
        // The plainly converted SELECT nobody approved keeps the file open.
        let fr = f.review.as_ref().unwrap();
        assert!(!fr.reviewed);
        let l = t.report.ledger.as_ref().unwrap();
        assert_eq!(l.rejected_lines.len(), 1);
        assert!(tree::summary(&t.report).contains("units reviewed"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_file_without_units_is_never_reviewed() {
    for text in ["", "-- only a comment\n", "GO\n", "/* a block */\nGO\n"] {
        let c = pg(text);
        assert!(c.units.is_empty(), "{text:?} has units");
        let f = review_file(PATH, text, &c, &Ledger::default());
        assert!(!f.reviewed(), "{text:?}");
    }
}

mod empty_files {
    use super::*;
    use sqldiff_convert::tree;

    #[test]
    fn the_tree_report_never_counts_a_file_without_units_as_reviewed() {
        let dir = std::env::temp_dir().join(format!("sqldiff-review-empty-{}", std::process::id()));
        let (src, out) = (dir.join("src"), dir.join("out"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        for (p, t) in [
            ("empty.sql", ""),
            ("comment.sql", "-- nothing\n"),
            ("go.sql", "GO\n"),
        ] {
            std::fs::write(src.join(p), t).unwrap();
        }
        // An empty ledger is still a ledger.
        std::fs::write(out.join(LEDGER_FILE), "").unwrap();
        let a = tree::Args {
            src: src.clone(),
            from: Dialect::Tsql,
            to: Dialect::Postgres,
            jobs: Some(1),
            no_catalog: false,
            template: None,
            extensions: tree::extensions(Vec::new()),
            helpers: Vec::new(),
            catalogs: Vec::new(),
            catalogs_b: Vec::new(),
        };
        let (inputs, unreadable) = tree::read(&a).unwrap();
        let mut t = tree::convert(&a, &inputs, unreadable, Some(&out)).unwrap();
        tree::review_in(&mut t, &inputs, &out).unwrap();
        for f in &t.report.files {
            assert!(!f.review.as_ref().unwrap().reviewed, "{}", f.path);
        }
        assert_eq!(t.report.ledger.as_ref().unwrap().files_reviewed, 0);
        assert!(!tree::markdown(&t.report).contains("accepted in review"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Appending to a ledger that is a symbolic link would write the decisions
/// wherever it points.
#[cfg(unix)]
#[test]
fn the_ledger_is_not_appended_through_a_link() {
    let dir = std::env::temp_dir().join(format!("sqldiff-ledger-link-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("out")).unwrap();
    std::fs::write(dir.join("elsewhere.txt"), "keep\n").unwrap();
    std::os::unix::fs::symlink(
        dir.join("elsewhere.txt"),
        dir.join("out").join(sqldiff_convert::review::LEDGER_FILE),
    )
    .unwrap();
    let input = "SELECT 1;\n";
    let c = pg(input);
    let (_, id, _) = first(input, &c, c.units[0].status);
    let r = record(input, &c, &id, "", State::Commented, By::Human);
    let e = sqldiff_convert::review::append(&dir.join("out"), &r).unwrap_err();
    assert!(e.contains("symbolic link"), "{e}");
    assert_eq!(
        std::fs::read_to_string(dir.join("elsewhere.txt")).unwrap(),
        "keep\n"
    );
}
