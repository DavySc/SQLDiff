//! The per-unit `proof` field and the proof totals of
//! `conversion-report.json` (`tree::annotate_proof`): every unit inside a
//! certified routine carries the checker's answer, units outside any
//! routine carry none, the totals count routines, units and refusal codes,
//! and the report on disk has it all, since `job::convert_into` computes
//! the certificates before it writes the report.

use std::path::PathBuf;
use std::sync::Arc;

use sqldiff_convert::job::{convert_into, Job, Written};
use sqldiff_convert::tree::{self, Hooks};
use sqldiff_core::proof::{Answer, Checker, MirrorAnswer, MirrorRequest, Mode, Request, Settings};
use sqldiff_core::Dialect;

/// Two routines and a statement outside both: `p_ok` is what a checker
/// may confirm, `p_no` is refused on the Rust side (dynamic SQL), and the
/// `SELECT` lies in no routine.
const SRC: &str = "CREATE PROCEDURE dbo.p_ok @a int\nAS\nBEGIN\n  SET @a = @a + 1;\nEND\nGO\nCREATE PROCEDURE dbo.p_no @s nvarchar(100)\nAS\nBEGIN\n  EXEC (@s);\nEND\nGO\nSELECT 1;\n";

/// A checker that answers every mirror request the same way.
#[derive(Debug)]
struct Fake(fn() -> MirrorAnswer);

impl Checker for Fake {
    fn name(&self) -> String {
        "fake sqldiff-check/3".into()
    }
    fn check(&self, _: &Request) -> Answer {
        panic!("the mirror check is what convert uses")
    }
    fn check_mirror(&self, _: &MirrorRequest) -> MirrorAnswer {
        (self.0)()
    }
}

fn confirmed() -> MirrorAnswer {
    MirrorAnswer::from(Answer::Confirmed {
        theorem: "Sqldiff.Mirror.check_sound".into(),
    })
}

fn with_codes() -> MirrorAnswer {
    let mut m = MirrorAnswer::from(Answer::Unconfirmed {
        reason: "the lowered routines differ beyond the renaming".into(),
    });
    m.why = vec![
        "check:differ".into(),
        "check:kind".into(),
        "check:differ".into(),
    ];
    m
}

fn without_codes() -> MirrorAnswer {
    MirrorAnswer::from(Answer::Unconfirmed {
        reason: "a procedure and a function".into(),
    })
}

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!(
            "sqldiff-proof-report-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let f = dir.join("src/p.sql");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, SRC).unwrap();
        Fixture { dir }
    }

    fn run(&self, proof: Settings) -> Written {
        let job = Job {
            tree: tree::Args {
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
            },
            out: self.dir.join("out"),
            overwrite: true,
            discard_edits: false,
            instances: None,
            queue_all: false,
            proof,
            row_counts: false,
        };
        convert_into(&job, &Hooks::default()).unwrap()
    }

    fn report(&self) -> serde_json::Value {
        serde_json::from_str(
            &std::fs::read_to_string(self.dir.join("out/conversion-report.json")).unwrap(),
        )
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn report(checker: fn() -> MirrorAnswer) -> Settings {
    Settings {
        mode: Mode::Report,
        checker: Some(Arc::new(Fake(checker))),
    }
}

/// Every unit of the one file, in the report's order, with its source line.
fn units(r: &serde_json::Value) -> Vec<(usize, serde_json::Value)> {
    let f = &r["files"][0];
    f["units"]
        .as_array()
        .unwrap()
        .iter()
        .chain(f["converted_units"].as_array().unwrap())
        .map(|u| (u["src_line"].as_u64().unwrap() as usize, u["proof"].clone()))
        .collect()
}

/// `SRC`'s units by routine: lines 1 to 5 are `p_ok`, 7 to 11 `p_no`, 13
/// the statement outside both.
fn in_ok(line: usize) -> bool {
    line <= 5
}
fn in_no(line: usize) -> bool {
    (7..=11).contains(&line)
}

#[test]
fn units_carry_the_answer_of_their_routine_and_the_totals_count_it() {
    let fx = Fixture::new("confirmed");
    let w = fx.run(report(confirmed));
    let r = fx.report();
    let us = units(&r);
    assert!(us.iter().any(|(l, _)| in_ok(*l)), "{r}");
    assert!(us.iter().any(|(l, _)| in_no(*l)), "{r}");
    assert!(us.iter().any(|(l, _)| *l == 13), "{r}");
    for (line, p) in &us {
        if in_ok(*line) {
            assert_eq!(p["answer"], "confirmed", "line {line}: {p}");
            assert_eq!(
                p["theorem"], "Sqldiff.Mirror.check_sound",
                "line {line}: {p}"
            );
            assert!(p.get("reason").is_none() && p.get("why").is_none(), "{p}");
        } else if in_no(*line) {
            assert_eq!(p["answer"], "unconfirmed", "line {line}: {p}");
            assert_eq!(
                p["reason"],
                "the source routine is outside the verified shape: dynamic SQL is not read"
            );
            assert_eq!(p["why"], serde_json::json!(["read:dynamic-sql"]));
            assert!(p.get("theorem").is_none(), "{p}");
        } else {
            assert!(p.is_null(), "line {line}: {p}");
        }
    }
    let in_routines = us.iter().filter(|(l, _)| in_ok(*l) || in_no(*l)).count();
    let confirmed = us.iter().filter(|(l, _)| in_ok(*l)).count();
    let t = &r["totals"]["proof"];
    assert_eq!(t["routines"], 2, "{t}");
    assert_eq!(t["routines_confirmed"], 1);
    assert_eq!(t["units_in_routines"], in_routines);
    assert_eq!(t["units_confirmed"], confirmed);
    assert_eq!(t["reasons"], serde_json::json!({ "read:dynamic-sql": 1 }));
    // The same object at the top of certificates.json, and in memory.
    let c: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fx.dir.join("out/certificates.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(&c["summary"], t);
    assert_eq!(
        c["files"][0]["routines"][1]["why"],
        serde_json::json!(["read:dynamic-sql"])
    );
    assert_eq!(
        serde_json::to_value(&w.tree.report.totals.proof).unwrap(),
        *t
    );
    // The summary and the markdown.
    let s = tree::summary(&w.tree.report);
    assert!(
        s.contains(&format!(
            "proof: 1 of 2 routines confirmed ({confirmed} of {in_routines} units)\n"
        )),
        "{s}"
    );
    assert!(s.contains("proof blockers: read:dynamic-sql (1)\n"), "{s}");
    let md = std::fs::read_to_string(fx.dir.join("out/conversion-report.md")).unwrap();
    assert!(md.contains("## Proof\n"), "{md}");
    assert!(md.contains("| `read:dynamic-sql` | 1 |"), "{md}");
    // Additive: the statuses are those of a run without a proof mode.
    let plain = Fixture::new("plain").run(Settings::default());
    assert_eq!(plain.tree.report.totals.units, w.tree.report.totals.units);
}

#[test]
fn the_checkers_codes_pass_through_and_count_once_per_routine() {
    let fx = Fixture::new("codes");
    let w = fx.run(report(with_codes));
    let r = fx.report();
    for (line, p) in units(&r) {
        if in_ok(line) {
            assert_eq!(p["answer"], "unconfirmed", "{p}");
            assert_eq!(
                p["reason"],
                "the lowered routines differ beyond the renaming"
            );
            assert_eq!(
                p["why"],
                serde_json::json!(["check:differ", "check:kind", "check:differ"])
            );
        }
    }
    let t = &r["totals"]["proof"];
    assert_eq!(t["routines_confirmed"], 0);
    assert_eq!(t["units_confirmed"], 0);
    assert_eq!(
        t["reasons"],
        serde_json::json!({ "check:differ": 1, "check:kind": 1, "read:dynamic-sql": 1 })
    );
    let s = tree::summary(&w.tree.report);
    assert!(
        s.contains("proof blockers: check:differ (1), check:kind (1), read:dynamic-sql (1)\n"),
        "{s}"
    );
}

#[test]
fn a_checker_reason_without_codes_gets_one() {
    let fx = Fixture::new("fallback");
    fx.run(report(without_codes));
    let r = fx.report();
    for (line, p) in units(&r) {
        if in_ok(line) {
            assert_eq!(p["reason"], "a procedure and a function", "{p}");
            assert_eq!(p["why"], serde_json::json!(["check:kind"]));
        }
    }
    assert_eq!(
        r["totals"]["proof"]["reasons"],
        serde_json::json!({ "check:kind": 1, "read:dynamic-sql": 1 })
    );
}

#[test]
fn without_a_proof_mode_nothing_is_added() {
    let fx = Fixture::new("off");
    let w = fx.run(Settings::default());
    assert!(w.certificates.is_none());
    assert!(!fx.dir.join("out/certificates.json").exists());
    let text = std::fs::read_to_string(fx.dir.join("out/conversion-report.json")).unwrap();
    assert!(!text.contains("\"proof\""), "{text}");
    assert!(w.tree.report.totals.proof.is_none());
    let s = tree::summary(&w.tree.report);
    assert!(!s.contains("proof"), "{s}");
    let md = std::fs::read_to_string(fx.dir.join("out/conversion-report.md")).unwrap();
    assert!(!md.contains("## Proof"), "{md}");
}
