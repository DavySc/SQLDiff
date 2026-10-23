//! The corpus gate: every expectation passes or is listed in known-failing.toml.

use sqldiff_corpus::{
    corpus_root, gate, known_failing_path, load_cases, load_known_failing, run_all,
};

#[test]
fn corpus_expectations_ratchet() {
    let outcomes = run_all(&corpus_root()).expect("corpus loads");
    assert!(!outcomes.is_empty(), "no expectations found");
    let known = load_known_failing(&known_failing_path()).expect("known-failing.toml loads");
    let g = gate(&outcomes, &known);
    let mut msg = String::new();
    for r in &g.regressions {
        msg.push_str(&format!("\n  regression (not in known-failing.toml): {r}"));
    }
    for f in &g.fixed {
        msg.push_str(&format!(
            "\n  now passing, remove from known-failing.toml: {f}"
        ));
    }
    for s in &g.stale {
        msg.push_str(&format!("\n  unknown id in known-failing.toml: {s}"));
    }
    for d in &g.duplicates {
        msg.push_str(&format!("\n  listed twice in known-failing.toml: {d}"));
    }
    assert!(g.ok(), "corpus gate failed:{msg}");
}

/// Soundness (design §12): the engine never says "equal" where the corpus
/// expects anything else. Not subject to the known-failing list.
#[test]
fn never_equal_when_corpus_says_otherwise() {
    let outcomes = run_all(&corpus_root()).expect("corpus loads");
    let bad: Vec<String> = outcomes
        .iter()
        .filter(|o| o.actual == "equal" && o.expected != "equal")
        .map(|o| o.to_string())
        .collect();
    assert!(bad.is_empty(), "unsound \"equal\":\n{}", bad.join("\n"));
}

/// Every case has at least one expectation and every side it names exists.
#[test]
fn cases_are_well_formed() {
    for case in load_cases(&corpus_root()).expect("corpus loads") {
        assert!(!case.expect.is_empty(), "{}: no [[expect]]", case.id);
        for e in &case.expect {
            for s in [&e.a, &e.b] {
                assert!(case.sides.contains_key(s), "{}: no side {s}", case.id);
            }
        }
    }
}
