//! T3 of the recorded-reads design (W4.3), per site: the replay tests would
//! notice a read that is not recorded.
//!
//! T1 (`replay_equals_fresh`) shows that a resume equals a fresh run for the
//! generated edits. It cannot show that every place in the check that reads a
//! catalog records its read: a read that was never recorded would simply not
//! be part of the read set, and a test that passes would pass for it as well.
//! So this test takes the places one by one. With the `site-switch` feature
//! of `sqldiff-catalog` every read is attributed to the place in a check
//! crate that asked for it (`#[track_caller]`), and recording is switched off
//! at one place. Each site then lands in exactly one class:
//!
//! - **killed**: some generated edit makes a resume differ from a fresh run
//!   (or fail the run's own comparison of a sample) with the site skipped;
//! - **redundant**: with the site skipped, the recorded read set of every
//!   file is identical to the unskipped one, for the base tree and for every
//!   edit, in both dialects. Skipping it cannot change any replay decision
//!   on these trees, because another site records the same questions with the
//!   same answers;
//! - **gap**: neither. The read sets change but no edit shows it: the
//!   generator lacks an edit, and the site is listed in [`GAP`] until one is
//!   added.
//!
//! A site is named by its file, the function around it and its number among
//! the sites of that function (`file::fn#n`), so a line that moves does not
//! rename it; a read added to or removed from a function renumbers that
//! function's sites and fails the check until it is classified again.
//!
//! The full run (every site against every edit) takes a long time, so it is
//! an ignored test run by hand (`-- --ignored`); the cheap test below runs in
//! CI: every site a check of the base tree reads from is classified, and
//! every classified site still names an existing function.
//!
//! This is its own test binary: the switch is global, and a test that runs
//! beside it would run with a read switched off.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use sqldiff_catalog::record::site_switch;
use sqldiff_core::Dialect;
use support::edits::{Edit, Model, Rng, ALL_EDITS};
use support::harness::{try_one, Fixture};

const SEED: u64 = 7;
const DIALECTS: &[Dialect] = &[Dialect::Postgres, Dialect::Db2i];

/// Sites killed by some generated edit.
const KILLED: &[&str] = &[
    "crates/sqldiff-core/src/bind/query.rs::resolve_call#0",
    "crates/sqldiff-core/src/bind/query.rs::resolve_call#2",
    "crates/sqldiff-core/src/bind/query.rs::table_ref#0",
    "crates/sqldiff-core/src/bind/query.rs::table_ref#2",
    "crates/sqldiff-core/src/bind/query.rs::table_ref#4",
    "crates/sqldiff-core/src/bind/query.rs::unresolved_bare#0",
    "crates/sqldiff-core/src/engine.rs::diff_inner#0",
];

/// Sites whose recording is redundant on every generated tree (see the
/// module comment).
const REDUNDANT: &[&str] = &[
    "crates/sqldiff-core/src/bind/query.rs::pseudo_sources#0",
    "crates/sqldiff-core/src/engine.rs::diff_inner#1",
    "crates/sqldiff-core/src/engine.rs::diff_inner#3",
    "crates/sqldiff-equiv/src/equiv/call.rs::callee#0",
    "crates/sqldiff-equiv/src/equiv/canon.rs::conv_col#0",
    "crates/sqldiff-equiv/src/equiv/canon.rs::leaves#0",
    "crates/sqldiff-equiv/src/equiv/canon.rs::period_omitted#0",
    "crates/sqldiff-equiv/src/equiv/canon.rs::total_order#0",
    "crates/sqldiff-equiv/src/equiv/canon.rs::user_fn#0",
    "crates/sqldiff-equiv/src/equiv/canon.rs::written#0",
    "crates/sqldiff-equiv/src/equiv/ddl.rs::case_check#0",
    "crates/sqldiff-equiv/src/equiv/mod.rs::read#0",
    "crates/sqldiff-equiv/src/equiv/mod.rs::trigger_drop#1",
    "crates/sqldiff-equiv/src/equiv/mod.rs::trigger_drop#2",
    "crates/sqldiff-equiv/src/equiv/xwit.rs::not_null#0",
    "crates/sqldiff-equiv/src/equiv/xwit.rs::types#0",
];

/// Sites whose read sets change but that no generated edit kills: work for
/// the generator.
const GAP: &[&str] = &[
    "crates/sqldiff-core/src/bind/query.rs::lookup_table_silent#0",
    "crates/sqldiff-core/src/bind/query.rs::resolve_call#1",
    "crates/sqldiff-core/src/bind/query.rs::table_ref#1",
    "crates/sqldiff-core/src/bind/query.rs::table_ref#3",
    "crates/sqldiff-core/src/bind/query.rs::unknown_column_in#0",
    "crates/sqldiff-core/src/engine.rs::check_recorded#0",
    "crates/sqldiff-core/src/engine.rs::diff_inner#2",
    "crates/sqldiff-equiv/src/equiv/call.rs::case_check#0",
    "crates/sqldiff-equiv/src/equiv/canon.rs::all_columns#0",
    "crates/sqldiff-equiv/src/equiv/canon.rs::col_ty#0",
    "crates/sqldiff-equiv/src/equiv/canon.rs::fold_check#0",
    "crates/sqldiff-equiv/src/equiv/canon.rs::visible_star#0",
    "crates/sqldiff-equiv/src/equiv/canon.rs::visible_star#1",
    "crates/sqldiff-equiv/src/equiv/ddl.rs::case_check#1",
    "crates/sqldiff-equiv/src/equiv/ddl.rs::case_check#2",
    "crates/sqldiff-equiv/src/equiv/ddl.rs::case_check#3",
    "crates/sqldiff-equiv/src/equiv/ddl.rs::case_check#4",
    "crates/sqldiff-equiv/src/equiv/mod.rs::table_pair#0",
    "crates/sqldiff-equiv/src/equiv/mod.rs::table_pair#1",
    "crates/sqldiff-equiv/src/equiv/mod.rs::trigger_drop#0",
    "crates/sqldiff-equiv/src/equiv/schema.rs::list_stars_with#0",
    "crates/sqldiff-equiv/src/equiv/table.rs::case_check#0",
    "crates/sqldiff-equiv/src/equiv/trigger.rs::pairs_for#0",
    "crates/sqldiff-equiv/src/equiv/trigger.rs::pairs_for#1",
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Names the sites of `sites` as `file::fn#n`.
fn keys_of(sites: &BTreeSet<site_switch::Site>) -> BTreeMap<site_switch::Site, String> {
    let mut by_fn: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut out = BTreeMap::new();
    let mut cache: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for site in sites {
        let lines = cache.entry(site.0.clone()).or_insert_with(|| {
            std::fs::read_to_string(root().join(&site.0))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        });
        let name = lines[..(site.1 as usize).min(lines.len())]
            .iter()
            .rev()
            .find_map(|l| {
                let t = l.trim_start();
                let t = t.strip_prefix("pub ").unwrap_or(t);
                let t = t.strip_prefix("pub(crate) ").unwrap_or(t);
                let t = t.strip_prefix("pub(super) ").unwrap_or(t);
                let t = t.strip_prefix("const ").unwrap_or(t);
                let t = t.strip_prefix("unsafe ").unwrap_or(t);
                t.strip_prefix("fn ").map(|r| {
                    r.chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect::<String>()
                })
            })
            .unwrap_or_else(|| "?".into());
        let n = by_fn.entry((site.0.clone(), name.clone())).or_insert(0);
        out.insert(site.clone(), format!("{}::{name}#{n}", site.0));
        *n += 1;
    }
    out
}

fn model_with(edit: Option<&Edit>) -> Model {
    let mut m = Model::generate(SEED);
    if let Some(e) = edit {
        let mut r = Rng::new(SEED ^ 0xA5A5);
        e.apply(&mut m, &mut r);
    }
    m
}

/// The sites that read while converting the tree after `edit` to `to`, and
/// the ids of the entries it stored (`<key>/<id>`, the id being the hash of a
/// file's read set).
fn run_tree(to: Dialect, edit: Option<&Edit>) -> (BTreeSet<String>, Vec<site_switch::Site>) {
    let (ids, seen, _) = run_tree_with_reads(to, edit);
    (ids, seen)
}

/// [`run_tree`], and the questions of each entry (for a diagnosis).
fn run_tree_with_reads(
    to: Dialect,
    edit: Option<&Edit>,
) -> (
    BTreeSet<String>,
    Vec<site_switch::Site>,
    BTreeMap<String, Vec<String>>,
) {
    let f = Fixture::new(&format!("t3-{to:?}-{edit:?}"), to);
    f.write(&model_with(edit));
    f.run("res", false);
    let seen = site_switch::seen();
    let results = f.dir.join("res/.sqldiff/results");
    let mut ids = BTreeSet::new();
    let mut qs = BTreeMap::new();
    for k in std::fs::read_dir(results).unwrap().flatten() {
        for e in std::fs::read_dir(k.path()).unwrap().flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.ends_with(".reads") {
                let id = format!("{}/{n}", k.file_name().to_string_lossy());
                if std::env::var("SQLDIFF_T3_ONLY").is_ok() {
                    // framed JSON: digest, newline, body
                    let raw = std::fs::read_to_string(e.path()).unwrap_or_default();
                    let v: serde_json::Value =
                        serde_json::from_str(raw.get(65..).unwrap_or("null")).unwrap_or_default();
                    let reads = v["reads"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .map(|r| format!("{} {}", r[0], r[1]))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    qs.insert(id.clone(), reads);
                }
                ids.insert(id);
            }
        }
    }
    (ids, seen, qs)
}

/// Every site a check of the base tree reads from, per dialect.
fn base_sites() -> BTreeSet<site_switch::Site> {
    let mut all = BTreeSet::new();
    for &d in DIALECTS {
        site_switch::discover();
        run_tree(d, None);
        all.extend(site_switch::seen());
        site_switch::off();
    }
    all
}

#[test]
fn every_site_the_base_tree_reads_from_is_classified_and_the_classified_sites_exist() {
    let known: BTreeSet<&str> = KILLED.iter().chain(REDUNDANT).chain(GAP).copied().collect();
    assert_eq!(
        known.len(),
        KILLED.len() + REDUNDANT.len() + GAP.len(),
        "a site is in two classes"
    );
    let keys = keys_of(&base_sites());
    let unknown: Vec<&String> = keys
        .values()
        .filter(|k| !known.contains(k.as_str()))
        .collect();
    assert!(
        unknown.is_empty(),
        "sites that read a catalog and are not classified in tests/site_switch.rs \
         (run the ignored test, `SQLDIFF_T3_ONLY=<name>` for one): {unknown:?}"
    );
    // Each classified site names a function that is still in its file.
    for k in &known {
        let (file, rest) = k.split_once("::").unwrap();
        let f = rest.rsplit_once('#').unwrap().0;
        let text = std::fs::read_to_string(root().join(file))
            .unwrap_or_else(|_| panic!("{k}: {file} is gone"));
        assert!(
            text.contains(&format!("fn {f}")),
            "{k}: no function {f} in {file}; reclassify the sites of this file"
        );
    }
}

/// Takes a long time in a debug build (every site against every edit), so it
/// is run by hand: `cargo test -p sqldiff-convert --test site_switch --
/// --ignored --nocapture`. `SQLDIFF_T3_ONLY=canon.rs` limits it to the sites
/// whose name contains the text. It prints the class of every site and fails
/// when a class differs from the lists above.
#[test]
#[ignore]
fn every_site_that_reads_a_catalog_is_classified_by_the_edits() {
    let only = std::env::var("SQLDIFF_T3_ONLY").ok();
    // The trees: the base and every edit, in both dialects.
    let trees: Vec<(Dialect, Option<&Edit>)> = DIALECTS
        .iter()
        .flat_map(|&d| {
            std::iter::once((d, None)).chain(ALL_EDITS.iter().map(move |e| (d, Some(e))))
        })
        .collect();
    // The reference run of each tree: the sites it reads from, the read sets.
    let mut reference: Vec<(BTreeSet<String>, BTreeSet<site_switch::Site>)> = Vec::new();
    let mut all: BTreeSet<site_switch::Site> = BTreeSet::new();
    for (d, e) in &trees {
        site_switch::discover();
        let (ids, seen) = run_tree(*d, *e);
        site_switch::off();
        let seen: BTreeSet<_> = seen.into_iter().collect();
        all.extend(seen.iter().cloned());
        reference.push((ids, seen));
    }
    let keys = keys_of(&all);
    assert!(all.len() >= 20, "only {} sites seen", all.len());
    let (mut killed, mut redundant, mut gap) = (Vec::new(), Vec::new(), Vec::new());
    for site in &all {
        let name = keys[site].clone();
        if only.as_ref().is_some_and(|o| !name.contains(o.as_str())) {
            continue;
        }
        // Redundant: the read sets are the same on every tree the site reads
        // from (and, by construction, on the others it is never reached).
        let mut is_redundant = true;
        for ((d, e), (ids, seen)) in trees.iter().zip(&reference) {
            if !seen.contains(site) {
                continue;
            }
            site_switch::skip(site.clone());
            let (skipped, _, sq) = run_tree_with_reads(*d, *e);
            site_switch::off();
            if &skipped != ids {
                is_redundant = false;
                if only.is_some() {
                    // What the skipped run no longer asks, for the generator.
                    site_switch::off();
                    let (_, _, rq) = run_tree_with_reads(*d, *e);
                    let gone: BTreeSet<&String> = rq
                        .iter()
                        .filter(|(i, _)| !skipped.contains(*i))
                        .flat_map(|(_, q)| q.iter())
                        .collect();
                    let new: BTreeSet<&String> = sq
                        .iter()
                        .filter(|(i, _)| !ids.contains(*i))
                        .flat_map(|(_, q)| q.iter())
                        .collect();
                    let only_gone: Vec<_> = gone.difference(&new).take(4).collect();
                    println!("  {name}: {d:?} {e:?}: reads that vanish: {only_gone:?}");
                }
                break;
            }
        }
        if is_redundant {
            println!("{name}: REDUNDANT");
            redundant.push(name);
            continue;
        }
        let mut by = None;
        'search: for &d in DIALECTS {
            if !trees
                .iter()
                .zip(&reference)
                .any(|((td, _), (_, s))| *td == d && s.contains(site))
            {
                continue;
            }
            for e in ALL_EDITS.iter().filter(|e| !e.is_no_op()) {
                site_switch::skip(site.clone());
                let o = try_one(d, SEED, e);
                site_switch::off();
                if o.verdict.is_err() {
                    by = Some(format!("{d:?} {e:?}"));
                    break 'search;
                }
            }
        }
        match by {
            Some(b) => {
                println!("{name}: KILLED by {b}");
                killed.push(name);
            }
            None => {
                println!("{name}: GAP");
                gap.push(name);
            }
        }
    }
    println!("KILLED = {killed:#?}\nREDUNDANT = {redundant:#?}\nGAP = {gap:#?}");
    if only.is_some() {
        return;
    }
    let sorted = |v: &[&str]| {
        let mut v: Vec<String> = v.iter().map(|s| s.to_string()).collect();
        v.sort();
        v
    };
    killed.sort();
    redundant.sort();
    gap.sort();
    assert_eq!(killed, sorted(KILLED), "killed sites");
    assert_eq!(redundant, sorted(REDUNDANT), "redundant sites");
    assert_eq!(gap, sorted(GAP), "gap sites");
}
