//! Print how the engine does on the corpus.
//!
//! ```text
//! cargo run -p sqldiff-corpus --bin corpus-report                 # table + gate
//! cargo run -p sqldiff-corpus --bin corpus-report -- --json       # machine-readable
//! cargo run -p sqldiff-corpus --bin corpus-report -- m13 p0       # only ids starting with these
//! cargo run -p sqldiff-corpus --bin corpus-report -- --write-known-failing
//! ```

use std::process::ExitCode;

use sqldiff_corpus::{
    corpus_root, gate, known_failing_path, load_known_failing, render_known_failing, run_all,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|a| a == "--json");
    let write = args.iter().any(|a| a == "--write-known-failing");
    let filters: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();

    let outcomes = match run_all(&corpus_root()) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let known = match load_known_failing(&known_failing_path()) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };

    if write {
        let text = render_known_failing(&outcomes, &known);
        if let Err(e) = std::fs::write(known_failing_path(), text) {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
        eprintln!("wrote {}", known_failing_path().display());
        return ExitCode::SUCCESS;
    }

    let shown: Vec<_> = outcomes
        .iter()
        .filter(|o| filters.is_empty() || filters.iter().any(|f| o.id.starts_with(f.as_str())))
        .collect();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&shown).expect("outcomes serialize")
        );
    } else {
        for o in &shown {
            println!("{} {o}", if o.passed() { "PASS" } else { "FAIL" });
        }
        let pass = shown.iter().filter(|o| o.passed()).count();
        println!("\n{pass}/{} expectations met", shown.len());
    }

    let g = gate(&outcomes, &known);
    for r in &g.regressions {
        eprintln!("regression (not in known-failing.toml): {r}");
    }
    for f in &g.fixed {
        eprintln!("now passing, remove from known-failing.toml: {f}");
    }
    for s in &g.stale {
        eprintln!("known-failing.toml lists an unknown expectation: {s}");
    }
    for d in &g.duplicates {
        eprintln!("known-failing.toml lists {d} twice");
    }
    if g.ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
