//! T3 of the recorded-reads design (W4.3), per site: the replay tests would
//! notice a read that is not recorded.
//!
//! T1 (`replay_equals_fresh`) shows that a resume equals a fresh run for the
//! generated edits. It cannot show that every place in the check that reads a
//! catalog records its read: a read that was never recorded would simply not
//! be part of the read set, and a test that passes would pass for it as well.
//! So this test takes the places one by one. With the `site-switch` feature
//! of `sqldiff-catalog` every read is attributed to the line in a check crate
//! that asked for it (`#[track_caller]`), and recording is switched off at
//! one line; for each line, some generated edit must then make a resume
//! differ from a fresh run (or fail the run's own comparison of a sample).
//! A line no edit kills is either a gap in the generator, which gets an edit,
//! or is listed in `SURVIVORS` with the reason it cannot matter, and that list
//! is compared both ways like the exemptions of T1.
//!
//! This is its own test binary: the switch is global, and a test that runs
//! beside it would run with a read switched off.

mod support;

use std::collections::BTreeSet;

use sqldiff_catalog::record::site_switch;
use sqldiff_core::Dialect;
use support::edits::{Model, ALL_EDITS};
use support::harness::{try_one, Fixture};

const SEED: u64 = 7;
const DIALECTS: &[Dialect] = &[Dialect::Postgres, Dialect::Db2i];

/// Why the sites below are listed: the current 24 edits do not kill them. A
/// site is only needed when it is the one place that records some question
/// for some file; where another line records the same question (the read set
/// is a map), skipping this one changes nothing, and where no file of the
/// generated tree depends on the answer, the generator has a gap. Each entry
/// is work for the generator (an edit that only this site's read can see), not
/// a claim that the site is dead. The list is compared both ways, so a site an
/// edit starts to kill must leave it.
const GAP: &str = "no generated edit kills it yet";

/// Sites no generated edit kills.
const SURVIVORS: &[(&str, &str)] = &[
    ("crates/sqldiff-core/src/bind/query.rs:1109", GAP),
    ("crates/sqldiff-core/src/bind/query.rs:1113", GAP),
    ("crates/sqldiff-core/src/bind/query.rs:1127", GAP),
    ("crates/sqldiff-core/src/bind/query.rs:1131", GAP),
    ("crates/sqldiff-core/src/bind/query.rs:1193", GAP),
    ("crates/sqldiff-core/src/bind/query.rs:2084", GAP),
    ("crates/sqldiff-core/src/bind/query.rs:2093", GAP),
    ("crates/sqldiff-core/src/bind/query.rs:2408", GAP),
    ("crates/sqldiff-core/src/bind/query.rs:2858", GAP),
    ("crates/sqldiff-core/src/engine.rs:562", GAP),
    ("crates/sqldiff-core/src/engine.rs:563", GAP),
    ("crates/sqldiff-core/src/engine.rs:572", GAP),
    ("crates/sqldiff-core/src/engine.rs:774", GAP),
    ("crates/sqldiff-equiv/src/equiv/call.rs:217", GAP),
    ("crates/sqldiff-equiv/src/equiv/call.rs:1154", GAP),
    ("crates/sqldiff-equiv/src/equiv/canon.rs:744", GAP),
    ("crates/sqldiff-equiv/src/equiv/canon.rs:1063", GAP),
    ("crates/sqldiff-equiv/src/equiv/canon.rs:1080", GAP),
    ("crates/sqldiff-equiv/src/equiv/canon.rs:1097", GAP),
    ("crates/sqldiff-equiv/src/equiv/canon.rs:1339", GAP),
    ("crates/sqldiff-equiv/src/equiv/canon.rs:2627", GAP),
    ("crates/sqldiff-equiv/src/equiv/canon.rs:5509", GAP),
    ("crates/sqldiff-equiv/src/equiv/canon.rs:6456", GAP),
    ("crates/sqldiff-equiv/src/equiv/canon.rs:7263", GAP),
    ("crates/sqldiff-equiv/src/equiv/ddl.rs:372", GAP),
    ("crates/sqldiff-equiv/src/equiv/ddl.rs:375", GAP),
    ("crates/sqldiff-equiv/src/equiv/mod.rs:2354", GAP),
    ("crates/sqldiff-equiv/src/equiv/mod.rs:2381", GAP),
    ("crates/sqldiff-equiv/src/equiv/mod.rs:2413", GAP),
    ("crates/sqldiff-equiv/src/equiv/mod.rs:2594", GAP),
    ("crates/sqldiff-equiv/src/equiv/mod.rs:2595", GAP),
    ("crates/sqldiff-equiv/src/equiv/table.rs:1850", GAP),
    ("crates/sqldiff-equiv/src/equiv/trigger.rs:286", GAP),
    ("crates/sqldiff-equiv/src/equiv/trigger.rs:288", GAP),
];

/// The sites the checks of the generated tree read from, per dialect.
fn sites_of(to: Dialect) -> BTreeSet<site_switch::Site> {
    let f = Fixture::new(&format!("sites-{to:?}"), to);
    f.write(&Model::generate(SEED));
    site_switch::discover();
    f.run("res", false);
    let seen = site_switch::seen();
    site_switch::off();
    seen.into_iter().collect()
}

/// Takes about 25 minutes in a debug build (every site against every edit), so
/// it is run by hand: `cargo test -p sqldiff-convert --test site_switch -- --ignored`.
/// `SQLDIFF_T3_ONLY=canon.rs:1063` limits it to the sites whose name contains
/// the text.
#[test]
#[ignore]
fn every_site_that_reads_a_catalog_is_missed_by_some_edit_when_it_does_not_record() {
    let only = std::env::var("SQLDIFF_T3_ONLY").ok();
    let mut by_dialect: Vec<(Dialect, BTreeSet<site_switch::Site>)> = Vec::new();
    for &d in DIALECTS {
        by_dialect.push((d, sites_of(d)));
    }
    let all: BTreeSet<site_switch::Site> = by_dialect.iter().flat_map(|(_, s)| s.clone()).collect();
    assert!(
        all.len() >= 20,
        "only {} sites seen: the switch is not attributing reads",
        all.len()
    );
    let mut survivors: Vec<String> = Vec::new();
    let mut log = Vec::new();
    for site in &all {
        let name = format!("{}:{}", site.0, site.1);
        if only.as_ref().is_some_and(|o| !name.contains(o.as_str())) {
            continue;
        }
        let mut killed_by = None;
        'search: for (to, seen) in &by_dialect {
            if !seen.contains(site) {
                continue;
            }
            for e in ALL_EDITS.iter().filter(|e| !e.is_no_op()) {
                site_switch::skip(site.clone());
                let o = try_one(*to, SEED, e);
                site_switch::off();
                if o.verdict.is_err() {
                    killed_by = Some(format!("{to:?} {e:?}"));
                    break 'search;
                }
            }
        }
        log.push(format!(
            "{name}: {}",
            killed_by.as_deref().unwrap_or("NOT KILLED")
        ));
        if killed_by.is_none() {
            survivors.push(name);
        }
    }
    println!("{}", log.join("\n"));
    if only.is_some() {
        return;
    }
    let listed: Vec<String> = SURVIVORS.iter().map(|(s, _)| s.to_string()).collect();
    survivors.sort();
    assert_eq!(
        survivors, listed,
        "sites that no generated edit shows missing, against the documented list"
    );
}
