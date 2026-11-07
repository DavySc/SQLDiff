//! A conversion for review (`tree::convert_for_review`) is the conversion:
//! the same report, outputs, units and findings, with the engine's reports
//! kept to their findings and assumptions; and equal notes share one text.

use std::path::PathBuf;

use sqldiff_convert::tree;
use sqldiff_core::Dialect;

const FILES: &[(&str, &str)] = &[
    (
        "a.sql",
        "CREATE PROCEDURE dbo.p AS\nBEGIN\n  UPDATE dbo.t SET a = 1;\nEND\n",
    ),
    (
        "b.sql",
        "CREATE PROCEDURE dbo.p2 AS\nBEGIN\n  UPDATE dbo.t SET a = 2;\nEND\n",
    ),
    (
        "r.sql",
        "CREATE PROCEDURE dbo.r AS\nBEGIN\n  SELECT @@ERROR;\nEND\n",
    ),
    ("t.sql", "CREATE TABLE t (a int NOT NULL);\n"),
];

fn tree_dir() -> PathBuf {
    // One directory per test: the tests of this file run on threads of one
    // process, and one must not delete the files of the other.
    let d = std::env::temp_dir().join(format!(
        "sqldiff-for-review-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    for (p, text) in FILES {
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src").join(p), text).unwrap();
    }
    d
}

fn args(d: &std::path::Path) -> tree::Args {
    tree::Args {
        rules: Default::default(),
        src: d.join("src"),
        from: Dialect::Tsql,
        to: Dialect::Postgres,
        jobs: Some(2),
        no_catalog: false,
        recheck: Default::default(),
        template: None,
        extensions: tree::extensions(Vec::new()),
        helpers: Vec::new(),
        deployment: Default::default(),
        catalogs: Vec::new(),
        catalogs_b: Vec::new(),
    }
}

fn report(t: &tree::Tree) -> serde_json::Value {
    let mut v = serde_json::to_value(&t.report).unwrap();
    v.as_object_mut().unwrap().remove("run");
    v
}

#[test]
fn a_conversion_for_review_is_the_conversion_with_its_reports_trimmed() {
    let d = tree_dir();
    let a = args(&d);
    let (inputs, unreadable) = tree::read(&a).unwrap();
    let whole = tree::convert(&a, &inputs, unreadable.clone(), None).unwrap();
    let hooks = tree::Hooks::default();
    let kept = tree::convert_for_review(&a, &inputs, unreadable, None, &hooks).unwrap();
    let _ = std::fs::remove_dir_all(&d);
    assert_eq!(report(&kept), report(&whole));
    assert_eq!(kept.outputs, whole.outputs);
    assert_eq!(kept.conversions.len(), whole.conversions.len());
    let mut assumed = 0;
    for (k, w) in kept.conversions.iter().zip(&whole.conversions) {
        assert_eq!(format!("{:?}", k.units), format!("{:?}", w.units));
        assert_eq!(k.output, w.output);
        assert_eq!((&k.unplaced, &k.unproven), (&w.unplaced, &w.unproven));
        for (kr, wr) in [(&k.report, &w.report), (&k.output_check, &w.output_check)] {
            assert_eq!(kr.findings, wr.findings);
            assert_eq!(kr.assumptions, wr.assumptions);
            assumed += wr.assumptions.len();
            assert!(kr.witnesses.is_empty() && kr.diagnostics.is_empty());
            assert!(kr.unknown_reasons.is_empty() && kr.normalizations.is_empty());
        }
    }
    // Something to keep was there to be kept.
    assert!(assumed > 0);
}

#[test]
fn equal_notes_share_their_text() {
    let d = tree_dir();
    let a = args(&d);
    let (inputs, unreadable) = tree::read(&a).unwrap();
    let t = tree::convert(&a, &inputs, unreadable, None).unwrap();
    let _ = std::fs::remove_dir_all(&d);
    let notes: Vec<&sqldiff_convert::Note> = t
        .conversions
        .iter()
        .flat_map(|c| &c.units)
        .flat_map(|u| &u.notes)
        .collect();
    let mut pairs = 0;
    for (i, x) in notes.iter().enumerate() {
        for y in &notes[i + 1..] {
            if x.text == y.text {
                pairs += 1;
                assert!(
                    std::ptr::eq(x.text.as_ptr(), y.text.as_ptr()),
                    "{:?}",
                    x.text
                );
            }
            if x.rule == y.rule {
                assert!(
                    std::ptr::eq(x.rule.as_ptr(), y.rule.as_ptr()),
                    "{:?}",
                    x.rule
                );
            }
        }
    }
    // a.sql and b.sql rest on the same assumptions.
    assert!(pairs > 0);
}

#[test]
fn a_stored_review_conversion_reuses_results_and_equals_the_whole_one() {
    let d = tree_dir();
    let out = d.join("out-for-review");
    let mut a = args(&d);
    a.recheck = tree::Recheck::None;
    let (inputs, unreadable) = tree::read(&a).unwrap();
    let whole = tree::convert(&a, &inputs, unreadable.clone(), None).unwrap();
    let hooks = tree::Hooks::default();
    let first =
        tree::convert_for_review_stored(&a, &inputs, unreadable.clone(), &out, &hooks).unwrap();
    let second = tree::convert_for_review_stored(&a, &inputs, unreadable, &out, &hooks).unwrap();
    let reused = second.report.run.files.iter().filter(|f| f.reused).count();
    let _ = std::fs::remove_dir_all(&d);
    // Only the output folder differs: the whole one wrote nothing.
    let mut r = report(&first);
    r.as_object_mut().unwrap().remove("out");
    assert_eq!(r, report(&whole));
    assert_eq!(first.outputs, whole.outputs);
    assert!(
        !first.report.run.files.iter().any(|f| f.reused),
        "the first run checks every file"
    );
    assert_eq!(
        reused,
        second.report.run.files.len(),
        "the second run reuses every file"
    );
    assert_eq!(second.outputs, whole.outputs);
}
