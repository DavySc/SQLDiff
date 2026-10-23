//! Grammar-aware parser fuzzing (design §12, §14 M2 gate).
//!
//! `SQLDIFF_FUZZ_ITERS` raises the number of generated programs per dialect
//! (default 400) for longer local or nightly runs.

use std::collections::BTreeSet;

use sqldiff_corpus::fuzz::{kinds, measure, program, Lcg, Stats};
use sqldiff_corpus::{corpus_root, load_cases};
use sqldiff_syntax::{Dialect, SyntaxKind};

fn iters() -> u64 {
    std::env::var("SQLDIFF_FUZZ_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(400)
}

/// Generated well-formed programs round-trip and stay under the error budget:
/// error nodes < 1% of statements (M2).
#[test]
fn generated_programs_stay_under_error_budget() {
    for d in Dialect::ALL {
        let mut total = Stats {
            round_trips: true,
            ..Stats::default()
        };
        let mut worst: Option<(usize, String)> = None;
        let mut seen = BTreeSet::new();
        for seed in 0..iters() {
            let text = program(&mut Lcg(seed ^ 0x5eed_f00d), d);
            let s = measure(&text, d);
            seen.extend(kinds(&text, d));
            assert!(s.round_trips, "{d} seed {seed} lost bytes:\n{text}");
            if s.errors + s.diagnostics > 0 && worst.as_ref().map_or(true, |w| s.errors > w.0) {
                worst = Some((s.errors, text.clone()));
            }
            total += s;
        }
        assert!(total.units > 0);
        let mut want = vec![
            SyntaxKind::ROUTINE,
            SyntaxKind::IF_STMT,
            SyntaxKind::LOOP_STMT,
        ];
        want.push(match d {
            Dialect::Tsql => SyntaxKind::TRY_CATCH,
            Dialect::Postgres => SyntaxKind::EXCEPTION_SECTION,
            Dialect::Db2i => SyntaxKind::HANDLER,
        });
        for k in want {
            assert!(
                seen.contains(&k),
                "{d}: generated compounds never parsed as {k:?}"
            );
        }
        let rate = total.errors as f64 / total.units as f64;
        eprintln!(
            "{d}: {} statements, {} error nodes ({:.3}%), {} diagnostics",
            total.units,
            total.errors,
            rate * 100.0,
            total.diagnostics
        );
        assert!(
            rate < 0.01,
            "{d}: error nodes {:.2}% of statements (budget 1%); worst example:\n{}",
            rate * 100.0,
            worst.map(|w| w.1).unwrap_or_default()
        );
    }
}

/// Corpus files with bytes deleted, duplicated or swapped never panic and
/// always round-trip.
#[test]
fn mutated_corpus_files_round_trip() {
    let mut rng = Lcg(0xc0ffee);
    use sqldiff_corpus::fuzz::Source;
    for case in load_cases(&corpus_root()).expect("corpus loads") {
        for side in case.sides.values() {
            let d: Dialect = side.dialect.parse().expect("dialect");
            let text = std::fs::read_to_string(case.dir.join(&side.file)).expect("side file");
            let chars: Vec<char> = text.chars().collect();
            for _ in 0..20 {
                let mut c = chars.clone();
                for _ in 0..1 + rng.below(4) {
                    if c.is_empty() {
                        break;
                    }
                    let i = rng.below(c.len());
                    let len = (1 + rng.below(12)).min(c.len() - i);
                    match rng.below(3) {
                        0 => {
                            c.drain(i..i + len);
                        }
                        1 => {
                            let dup: Vec<char> = c[i..i + len].to_vec();
                            let j = rng.below(c.len());
                            c.splice(j..j, dup);
                        }
                        _ => {
                            let j = rng.below(c.len());
                            c.swap(i, j);
                        }
                    }
                }
                let m: String = c.into_iter().collect();
                let s = measure(&m, d);
                assert!(s.round_trips, "{}: mutated text lost bytes:\n{m}", case.id);
            }
        }
    }
}
