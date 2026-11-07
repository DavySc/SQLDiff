//! The key of a stored check is made from the `Debug` text of a `Draft`
//! (`Draft::canonical`). A hand-written `Debug` could leave a field out, and
//! two drafts that differ there would share a key. Every type reachable from
//! `Draft` derives `Debug`, with one exception that is checked here:
//! `Shared` prints as the string it holds, which is everything it is.

use std::path::PathBuf;

fn sources() -> Vec<(String, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "rs"))
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read_to_string(e.path()).unwrap(),
            )
        })
        .collect();
    v.sort();
    v
}

#[test]
fn only_shared_has_a_hand_written_debug() {
    let mut found = Vec::new();
    for (file, text) in sources() {
        for line in text.lines() {
            let l = line.trim();
            if l.starts_with("impl")
                && (l.contains("fmt::Debug for") || l.contains("Debug for"))
                && !l.contains("derive")
            {
                found.push(format!("{file}: {l}"));
            }
        }
    }
    assert_eq!(
        found,
        vec!["shared.rs: impl fmt::Debug for Shared {".to_string()],
        "a type reachable from Draft has a hand-written Debug: check that it prints every field, or derive it"
    );
}

#[test]
fn shared_prints_as_its_string() {
    let s = sqldiff_units::Shared::from("a \"quoted\" text");
    assert_eq!(format!("{s:?}"), format!("{:?}", "a \"quoted\" text"));
}

#[test]
fn a_draft_prints_every_field() {
    // Two drafts that differ in any one field print differently.
    use sqldiff_syntax::Dialect;
    use sqldiff_units::out::{Out, RawUnit};
    use sqldiff_units::{Draft, Note, RuleOptions, Status, Step, Written};
    let base = || Draft {
        from: Dialect::Tsql,
        to: Dialect::Postgres,
        options: RuleOptions::default(),
        raw: Out {
            text: "x".into(),
            units: vec![RawUnit {
                src: (0, 1),
                out: (0, 1),
                notes: vec![Note {
                    status: Status::Assumptions,
                    rule: "r".into(),
                    text: "t".into(),
                }],
                trace: vec![Step {
                    rule: "tsql.a".into(),
                    start: 0,
                    end: 1,
                    inst: None,
                }],
                written: None,
            }],
        },
        plain: None,
    };
    let key = |d: &Draft| format!("{d:?}");
    let k = key(&base());
    let mut variants: Vec<Draft> = Vec::new();
    let mut d = base();
    d.to = Dialect::Db2i;
    variants.push(d);
    let mut d = base();
    d.options.temporal_historyless = true;
    variants.push(d);
    let mut d = base();
    d.raw.text = "y".into();
    variants.push(d);
    let mut d = base();
    d.raw.units[0].out = (0, 0);
    variants.push(d);
    let mut d = base();
    d.raw.units[0].notes[0].text = "u".into();
    variants.push(d);
    let mut d = base();
    d.raw.units[0].trace[0].inst = Some("i".into());
    variants.push(d);
    let mut d = base();
    d.raw.units[0].written = Some(Written {
        by: sqldiff_units::By::Agent,
        author: "a".into(),
        origin: "o".into(),
    });
    variants.push(d);
    variants.push(Draft {
        plain: Some(Box::new(base())),
        ..base()
    });
    for (i, v) in variants.iter().enumerate() {
        assert_ne!(key(v), k, "variant {i} prints like the base draft");
    }
}
