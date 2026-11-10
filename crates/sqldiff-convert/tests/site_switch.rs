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
//! A site is named by its file, the function around it and the text of the
//! line that asks (`file::fn@text`), so a line that moves does not rename
//! it; a changed call line renames it and fails the check until the site is
//! classified again.
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
use support::harness::{per_file, try_one, try_one_in, Fixture};

const SEED: u64 = 7;
const DIALECTS: &[Dialect] = &[Dialect::Postgres, Dialect::Db2i];

/// Sites that read whether a catalog was supplied. That bit is part of a
/// file's store key (one per side), so a run whose answer differs finds no
/// stored result at all, whatever the read set holds: see
/// [`a_changed_supplied_bit_finds_nothing_stored`].
const KEYED: &[&str] = &[
    "crates/sqldiff-core/src/engine.rs::diff_inner@b: Some((&ab, &bound_b, read_b.supplied())),",
    "crates/sqldiff-core/src/engine.rs::check_recorded@a: (&aa, &bound, read.supplied()),",
];

/// Sites killed by some generated edit.
const KILLED: &[&str] = &[
    "crates/sqldiff-core/src/bind/query.rs::resolve_call@_ => match view.routines(parts) {",
    "crates/sqldiff-core/src/bind/query.rs::resolve_call@if view.routines_authoritative(rkind) {",
    "crates/sqldiff-core/src/bind/query.rs::resolve_call@view.origin()",
    "crates/sqldiff-core/src/bind/query.rs::table_ref@Lookup::NoSchema(s) if view.tables_authoritative() => {",
    "crates/sqldiff-core/src/bind/query.rs::table_ref@Lookup::NotFound if view.tables_authoritative() => {",
    "crates/sqldiff-core/src/bind/query.rs::table_ref@match view.table(parts) {",
    "crates/sqldiff-core/src/bind/query.rs::table_ref@view.origin()",
    "crates/sqldiff-core/src/bind/query.rs::table_ref@view.origin()#1",
    "crates/sqldiff-core/src/bind/query.rs::unknown_column_in@view.origin()",
    "crates/sqldiff-core/src/bind/query.rs::unresolved_bare@view.origin()",
    "crates/sqldiff-core/src/engine.rs::diff_inner@schema_a.list_stars_with(&mut schema_b);",
    "crates/sqldiff-equiv/src/equiv/call.rs::case_check@.case_check(&key(n, s.d)?, true)",
    "crates/sqldiff-equiv/src/equiv/canon.rs::col_ty@.table(Self::base_table(table))",
    "crates/sqldiff-equiv/src/equiv/canon.rs::fold_check@(Some(x), None) => schema.case_check(x, false)?,",
    "crates/sqldiff-equiv/src/equiv/ddl.rs::case_check@schema.case_check(&stored(&c.name), false)?;",
    "crates/sqldiff-equiv/src/equiv/ddl.rs::case_check@schema.case_check(&stored(&i.name), false)?;",
    "crates/sqldiff-equiv/src/equiv/ddl.rs::case_check@schema.case_check(&stored(n), false)?;",
    "crates/sqldiff-equiv/src/equiv/ddl.rs::case_check@schema.case_check(&stored(n), true)?;",
    "crates/sqldiff-equiv/src/equiv/schema.rs::list_stars_with@b.ask_table(n);",
    "crates/sqldiff-equiv/src/equiv/table.rs::case_check@schema.case_check(&stored(n), false)?;",
];

/// Sites whose recording is redundant on every generated tree (see the
/// module comment).
const REDUNDANT: &[&str] = &[
    "crates/sqldiff-core/src/bind/query.rs::pseudo_sources@_ => match view.table(&tr.table) {",
    "crates/sqldiff-core/src/engine.rs::diff_inner@a: (&aa, &bound_a, read_a.supplied()),",
    "crates/sqldiff-core/src/engine.rs::diff_inner@if read_a.supplied() || read_b.supplied() {",
    "crates/sqldiff-equiv/src/equiv/call.rs::callee@let found: Vec<&Routine> = match view.routines(&parts) {",
    "crates/sqldiff-equiv/src/equiv/canon.rs::conv_col@.table(Self::base_table(table))",
    "crates/sqldiff-equiv/src/equiv/canon.rs::leaves@let k = self.schema.table(Self::base_table(t)).and_then(|tab| {",
    "crates/sqldiff-equiv/src/equiv/canon.rs::period_omitted@let Some(t) = self.schema.table(Self::base_table(table)) else {",
    "crates/sqldiff-equiv/src/equiv/canon.rs::total_order@match self.schema.table(Self::base_table(&table)) {",
    "crates/sqldiff-equiv/src/equiv/canon.rs::user_fn@let found: Vec<&crate::catalog::Routine> = match view.routines(&parts) {",
    "crates/sqldiff-equiv/src/equiv/canon.rs::written@.table(Self::base_table(table))",
    "crates/sqldiff-equiv/src/equiv/mod.rs::read@Some((n, _)) => ddl::case_check(dd, &s.with_ddl(&n.text().to_string(), d)),",
    "crates/sqldiff-equiv/src/equiv/mod.rs::trigger_drop@.whole()#1",
    "crates/sqldiff-equiv/src/equiv/mod.rs::trigger_drop@if cat_p.whole().triggers.iter().any(|d| {",
    "crates/sqldiff-equiv/src/equiv/xwit.rs::not_null@.table(t)",
    "crates/sqldiff-equiv/src/equiv/xwit.rs::types@.and_then(|t| side.schema.table(t))",
];

/// Sites whose recorded reads are covered by others in every tree, by the
/// condition of [`depends`].
const DOMINATED: &[&str] = &[
    "crates/sqldiff-core/src/bind/query.rs::lookup_table_silent@_ => match self.input.view.table(parts) {",
    "crates/sqldiff-equiv/src/equiv/canon.rs::all_columns@None => self.schema.table_exact(",
    "crates/sqldiff-equiv/src/equiv/canon.rs::visible_star@.table(Self::base_table(&b.table))",
    "crates/sqldiff-equiv/src/equiv/canon.rs::visible_star@b.derived.is_none() && self.schema.is_star_view(&lc(Self::base_table(&b.table)))",
    "crates/sqldiff-equiv/src/equiv/ddl.rs::case_check@schema.case_check(&stored(n), true)?;#2",
    "crates/sqldiff-equiv/src/equiv/ddl.rs::case_check@schema.case_check(&stored(n), x.kind == \"TABLE\")?;",
    "crates/sqldiff-equiv/src/equiv/mod.rs::col_facts@let t = schema.table_exact(want.0.as_deref(), &want.1)?;",
    "crates/sqldiff-equiv/src/equiv/mod.rs::table_pair@sa.with_ddl(&p.ddl.0, a.dialect()),",
    "crates/sqldiff-equiv/src/equiv/mod.rs::table_pair@sb.with_ddl(&p.ddl.1, b.dialect()),",
    "crates/sqldiff-equiv/src/equiv/mod.rs::trigger_drop@.whole()",
    "crates/sqldiff-equiv/src/equiv/trigger.rs::pairs_for@.with_images(&tr.table, &names)",
    "crates/sqldiff-equiv/src/equiv/trigger.rs::pairs_for@let sa2 = sa.with_images(&table, &names).unwrap_or_else(|| sa.clone());",
];

/// Sites whose read sets change but that no generated edit kills: work for
/// the generator.
const GAP: &[&str] = &[];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Names the sites of `sites` as `file::fn@line text` (the trimmed source
/// line of the call), followed by `#k` when the same text occurs earlier in
/// the function (k counts those). The name does not depend on which other
/// sites were seen, and a line that moves keeps it.
fn keys_of(sites: &BTreeSet<site_switch::Site>) -> BTreeMap<site_switch::Site, String> {
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
        let at = (site.1 as usize).min(lines.len()).max(1) - 1;
        let fn_at = lines[..=at].iter().rposition(|l| {
            let t = l.trim_start();
            let t = t.strip_prefix("pub ").unwrap_or(t);
            let t = t.strip_prefix("pub(crate) ").unwrap_or(t);
            let t = t.strip_prefix("pub(super) ").unwrap_or(t);
            let t = t.strip_prefix("const ").unwrap_or(t);
            let t = t.strip_prefix("unsafe ").unwrap_or(t);
            t.starts_with("fn ")
        });
        let (name, from) = match fn_at {
            Some(i) => {
                let t = lines[i].trim_start();
                let t = t.strip_prefix("pub ").unwrap_or(t);
                let t = t.strip_prefix("pub(crate) ").unwrap_or(t);
                let t = t.strip_prefix("pub(super) ").unwrap_or(t);
                let t = t.strip_prefix("const ").unwrap_or(t);
                let t = t.strip_prefix("unsafe ").unwrap_or(t);
                let n: String = t[3..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                (n, i)
            }
            None => ("?".to_string(), 0),
        };
        let text = lines[at].trim().to_string();
        let k = lines[from..at].iter().filter(|l| l.trim() == text).count();
        let key = if k == 0 {
            format!("{}::{name}@{text}", site.0)
        } else {
            format!("{}::{name}@{text}#{k}", site.0)
        };
        out.insert(site.clone(), key);
    }
    out
}

fn model_with(edit: Option<&Edit>, hidden: bool) -> Model {
    let mut m = Model::generate_with(SEED, hidden);
    if let Some(e) = edit {
        let mut r = Rng::new(SEED ^ 0xA5A5);
        e.apply(&mut m, &mut r);
    }
    m
}

/// The sites that read while converting the tree after `edit` to `to`, and
/// the ids of the entries it stored (`<key>/<id>`, the id being the hash of a
/// file's read set).
fn run_tree(
    to: Dialect,
    hidden: bool,
    edit: Option<&Edit>,
) -> (BTreeSet<String>, Vec<site_switch::Site>) {
    let (ids, seen, _) = run_tree_with_reads(to, hidden, edit);
    (ids, seen)
}

/// What each file (its store key) was answered for each question, as the
/// union over its entries: `side query` to the set of answer digests.
type Answers = BTreeMap<String, BTreeMap<String, BTreeSet<String>>>;

/// [`run_tree`], and the questions of each entry (for a diagnosis).
fn run_tree_with_reads(
    to: Dialect,
    hidden: bool,
    edit: Option<&Edit>,
) -> (
    BTreeSet<String>,
    Vec<site_switch::Site>,
    BTreeMap<String, Vec<String>>,
) {
    let (ids, seen, qs, _) = run_tree_answers(to, hidden, edit);
    (ids, seen, qs)
}

/// [`run_tree_with_reads`], and the answers too.
fn run_tree_answers(
    to: Dialect,
    hidden: bool,
    edit: Option<&Edit>,
) -> (
    BTreeSet<String>,
    Vec<site_switch::Site>,
    BTreeMap<String, Vec<String>>,
    Answers,
) {
    let f = Fixture::new(&format!("t3-{to:?}-{hidden}-{edit:?}"), to);
    f.write(&model_with(edit, hidden));
    f.run("res", false);
    let seen = site_switch::seen();
    let results = f.dir.join("res/.sqldiff/results");
    let mut ids = BTreeSet::new();
    let mut qs = BTreeMap::new();
    let mut answers: Answers = BTreeMap::new();
    for k in std::fs::read_dir(results).unwrap().flatten() {
        for e in std::fs::read_dir(k.path()).unwrap().flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.ends_with(".reads") {
                let key = k.file_name().to_string_lossy().into_owned();
                let id = format!("{key}/{n}");
                // framed JSON: digest, newline, body
                let raw = std::fs::read_to_string(e.path()).unwrap_or_default();
                let v: serde_json::Value =
                    serde_json::from_str(raw.get(65..).unwrap_or("null")).unwrap_or_default();
                let mut reads = Vec::new();
                for r in v["reads"].as_array().into_iter().flatten() {
                    let q = format!("{} {}", r[0], r[1]);
                    answers
                        .entry(key.clone())
                        .or_default()
                        .entry(q.clone())
                        .or_default()
                        .insert(r[2].to_string());
                    reads.push(q);
                }
                qs.insert(id.clone(), reads);
                ids.insert(id);
            }
        }
    }
    (ids, seen, qs, answers)
}

/// Every site a check of the base tree reads from, per dialect.
fn base_sites() -> BTreeSet<site_switch::Site> {
    let mut all = BTreeSet::new();
    for &d in DIALECTS {
        for hidden in [false, true] {
            site_switch::discover();
            run_tree(d, hidden, None);
            all.extend(site_switch::seen());
            site_switch::off();
        }
    }
    all
}

/// The reads of each file (its store key) in one run, as `side query` text.
fn per_key(qs: &BTreeMap<String, Vec<String>>) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (id, reads) in qs {
        let key = id.split('/').next().unwrap_or_default().to_string();
        out.entry(key).or_default().extend(reads.iter().cloned());
    }
    out
}

/// Whether skipping the recording of `v` could go unnoticed in one pair of
/// runs of a file, the stored one (`old`, before the edit) and the checked
/// one (`new`, after it). A stored entry is taken exactly when every read it
/// holds has the answer it had, so an entry without `v` is taken stale only
/// if `v`'s answer changed and no read that remains (`kept`) changed with
/// it. Whatever the reason a kept read changes (it asks the same question of
/// the other side, a whole-catalog digest, the table a view reads), the
/// entry is refused then, and the file is checked again. A read missing from
/// one of the two runs counts as changed.
fn depends(
    v: &str,
    kept: &BTreeSet<String>,
    old: &BTreeMap<String, BTreeSet<String>>,
    new: &BTreeMap<String, BTreeSet<String>>,
) -> bool {
    let changed = |q: &String| old.get(q) != new.get(q);
    !changed(&v.to_string()) || kept.iter().any(changed)
}

#[test]
fn every_site_the_base_tree_reads_from_is_classified_and_the_classified_sites_exist() {
    let known: BTreeSet<&str> = KILLED
        .iter()
        .chain(REDUNDANT)
        .chain(GAP)
        .chain(DOMINATED)
        .chain(KEYED)
        .copied()
        .collect();
    assert_eq!(
        known.len(),
        KILLED.len() + REDUNDANT.len() + GAP.len() + DOMINATED.len() + KEYED.len(),
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
        let f = rest.split_once('@').unwrap().0;
        let text = std::fs::read_to_string(root().join(file))
            .unwrap_or_else(|_| panic!("{k}: {file} is gone"));
        assert!(
            text.contains(&format!("fn {f}")),
            "{k}: no function {f} in {file}; reclassify the sites of this file"
        );
    }
}

/// The proof for [`KEYED`]: with `--no-catalog` (the catalogs' supplied bit
/// flips on both sides) no stored result of the run before is found, in both
/// dialects, and the resumed run still equals a fresh one.
#[test]
fn a_changed_supplied_bit_finds_nothing_stored() {
    for &d in DIALECTS {
        let o = try_one(d, SEED, &Edit::NoCatalog);
        assert!(o.verdict.is_ok(), "{d:?}: {:?}", o.verdict);
        assert_eq!(
            o.reused.0, 0,
            "{d:?}: a result was found under another supplied bit"
        );
        assert!(o.reused.1 > 0, "{d:?}: nothing was checked");
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
    // The trees: the base and every edit, in both dialects, with and without
    // a system-versioned table in the tree from the start.
    let trees: Vec<(Dialect, bool, Option<&Edit>)> = DIALECTS
        .iter()
        .flat_map(|&d| {
            [false, true].into_iter().flat_map(move |h| {
                std::iter::once((d, h, None)).chain(ALL_EDITS.iter().map(move |e| (d, h, Some(e))))
            })
        })
        .collect();
    // The reference run of each tree: the sites it reads from, the read sets.
    type Reference = (
        BTreeSet<String>,
        BTreeSet<site_switch::Site>,
        BTreeMap<String, Vec<String>>,
        Answers,
    );
    let mut reference: Vec<Reference> = Vec::new();
    let mut all: BTreeSet<site_switch::Site> = BTreeSet::new();
    for (d, h, e) in &trees {
        site_switch::discover();
        let (ids, seen, qs, ans) = run_tree_answers(*d, *h, *e);
        site_switch::off();
        let seen: BTreeSet<_> = seen.into_iter().collect();
        all.extend(seen.iter().cloned());
        reference.push((ids, seen, qs, ans));
    }
    let keys = keys_of(&all);
    assert!(all.len() >= 20, "only {} sites seen", all.len());
    let (mut killed, mut redundant, mut gap, mut dom) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for site in &all {
        let name = keys[site].clone();
        if only
            .as_ref()
            .is_some_and(|o| !o.split('|').any(|p| name.contains(p)))
        {
            continue;
        }
        if KEYED.contains(&name.as_str()) {
            println!("{name}: KEYED");
            continue;
        }
        // Redundant: the read sets are the same on every tree the site reads
        // from (and, by construction, on the others it is never reached).
        let mut is_redundant = true;
        for ((d, h, e), (ids, seen, _, _)) in trees.iter().zip(&reference) {
            if !seen.contains(site) {
                continue;
            }
            site_switch::skip(site.clone());
            let (skipped, _, sq) = run_tree_with_reads(*d, *h, *e);
            site_switch::off();
            if &skipped != ids {
                is_redundant = false;
                if only.is_some() {
                    // What the skipped run no longer asks, for the generator.
                    site_switch::off();
                    let (_, _, rq) = run_tree_with_reads(*d, *h, *e);
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
                    println!("  {name}: {d:?} hidden={h} {e:?}: reads that vanish: {only_gone:?}");
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
        'search: for (d, hidden) in DIALECTS.iter().flat_map(|&d| [(d, false), (d, true)]) {
            if !trees
                .iter()
                .zip(&reference)
                .any(|((td, th, _), (_, s, _, _))| *td == d && *th == hidden && s.contains(site))
            {
                continue;
            }
            for e in ALL_EDITS.iter().filter(|e| !e.is_no_op()) {
                site_switch::skip(site.clone());
                let o = try_one_in(d, SEED, e, hidden);
                site_switch::off();
                if only.is_some() && std::env::var("SQLDIFF_T3_VERBOSE").is_ok() {
                    println!(
                        "    {d:?} hidden={hidden} {e:?}: reused {:?}, changed {:?}, again {:?}",
                        o.reused, o.dependents_changed, o.again
                    );
                }
                if o.verdict.is_err() {
                    by = Some(format!("{d:?} hidden={hidden} {e:?}"));
                    break 'search;
                }
            }
        }
        if let Some(b) = by {
            println!("{name}: KILLED by {b}");
            killed.push(name);
            continue;
        }
        // Dominated: on every edited tree the site reads from, every read
        // that vanishes when the site is skipped and whose answer the edit
        // changes goes with a read that remains in the same file and that
        // the edit changes too (see [`depends`]): a stored entry without the
        // vanished read cannot then hold after the edit.
        let mut dominated = true;
        'dom: for ((d, h, e), (_, seen, rq, edit_ans)) in trees.iter().zip(&reference) {
            // `--no-catalog` changes every file's store key (the supplied
            // bit; `a_changed_supplied_bit_finds_nothing_stored`): nothing is
            // reused across it, whatever the read sets hold.
            if e.is_none() || !seen.contains(site) || matches!(e, Some(Edit::NoCatalog)) {
                continue;
            }
            // The stored run: the same tree before the edit.
            let Some((_, _, _, base_ans)) = trees
                .iter()
                .zip(&reference)
                .find(|((td, th, te), _)| td == d && th == h && te.is_none())
                .map(|(_, r)| r)
            else {
                continue;
            };
            site_switch::skip(site.clone());
            let (_, _, sq) = run_tree_with_reads(*d, *h, *e);
            site_switch::off();
            let (rk, sk) = (per_key(rq), per_key(&sq));
            for (k, reads) in &rk {
                // A file whose key is new has no stored entry to reuse.
                let Some(old) = base_ans.get(k) else {
                    continue;
                };
                let new = edit_ans.get(k).cloned().unwrap_or_default();
                let kept = sk.get(k).cloned().unwrap_or_default();
                for v in reads.difference(&kept) {
                    if !depends(v, &kept, old, &new) {
                        println!(
                            "  {name}: {d:?} hidden={h} {e:?}: {v} changes and no read of the file that remains does"
                        );
                        dominated = false;
                        break 'dom;
                    }
                }
            }
        }
        if dominated {
            println!("{name}: DOMINATED");
            dom.push(name);
        } else {
            println!("{name}: GAP");
            gap.push(name);
        }
    }
    println!(
        "KILLED = {killed:#?}\nREDUNDANT = {redundant:#?}\nDOMINATED = {dom:#?}\nGAP = {gap:#?}"
    );
    if only.is_some() {
        // A shard (`SQLDIFF_T3_ONLY`, as the workflow `t3-sites` runs them):
        // every site it classified must be in the class pinned for it.
        let wrong = |got: &[String], pinned: &[&str], class: &str| {
            let bad: Vec<&String> = got
                .iter()
                .filter(|n| !pinned.contains(&n.as_str()))
                .collect();
            assert!(
                bad.is_empty(),
                "sites found {class} that are not pinned {class}: {bad:?}"
            );
        };
        wrong(&killed, KILLED, "KILLED");
        wrong(&redundant, REDUNDANT, "REDUNDANT");
        wrong(&dom, DOMINATED, "DOMINATED");
        assert!(gap.is_empty(), "sites with no proof: {gap:?}");
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
    dom.sort();
    assert_eq!(dom, sorted(DOMINATED), "dominated sites");
    assert_eq!(gap, sorted(GAP), "gap sites");
}

/// What an edit does to the files whose text it leaves alone: for the
/// generator work. `SQLDIFF_T3_PROBE=DropAllTables cargo test ... probe -- --ignored --nocapture`.
#[test]
#[ignore]
fn probe() {
    let Ok(names) = std::env::var("SQLDIFF_T3_PROBE") else {
        return;
    };
    for e in ALL_EDITS
        .iter()
        .filter(|e| names.split(',').any(|n| n == format!("{e:?}")))
    {
        for &d in DIALECTS {
            let o = try_one(d, SEED, e);
            println!(
                "{d:?} {e:?}: reused {:?}, verdict {:?}\n  changed: {:?}",
                o.reused,
                o.verdict.as_ref().err(),
                o.dependents_changed
            );
        }
    }
}

/// Prints what files give before and after an edit:
/// `SQLDIFF_T3_SHOW=DropAllTables:dependents/GhostTable.sql,...`.
#[test]
#[ignore]
fn show() {
    let Ok(spec) = std::env::var("SQLDIFF_T3_SHOW") else {
        return;
    };
    let (edit, files) = spec.split_once(':').unwrap();
    let e = ALL_EDITS.iter().find(|e| format!("{e:?}") == edit).unwrap();
    for &d in DIALECTS {
        let f = Fixture::new("show", d);
        let mut m = Model::generate_with(SEED, std::env::var("SQLDIFF_T3_HIDDEN").is_ok());
        f.write(&m);
        f.run("a", true);
        e.apply(&mut m, &mut Rng::new(SEED ^ 0xA5A5));
        f.write(&m);
        f.run("b", true);
        let (a, b) = (per_file(&f.dir.join("a")), per_file(&f.dir.join("b")));
        for p in files.split(',') {
            println!(
                "{d:?} {p}\n--- before\n{}\n--- after\n{}",
                a.get(p).map_or("-", |s| s),
                b.get(p).map_or("-", |s| s)
            );
        }
    }
}

/// The recorded reads of the files whose entry names `SQLDIFF_T3_READS`
/// (a path part), before and after the edit named by `SQLDIFF_T3_EDIT`.
#[test]
#[ignore]
fn reads() {
    let (Ok(part), Ok(edit)) = (
        std::env::var("SQLDIFF_T3_READS"),
        std::env::var("SQLDIFF_T3_EDIT"),
    ) else {
        return;
    };
    let e = ALL_EDITS.iter().find(|e| format!("{e:?}") == edit).unwrap();
    // `SQLDIFF_T3_SKIP`: a part of a site's name to switch off.
    let skip = std::env::var("SQLDIFF_T3_SKIP").ok().and_then(|part| {
        let sites = base_sites();
        let keys = keys_of(&sites);
        sites.into_iter().find(|s| keys[s].contains(&part))
    });
    for (label, ed) in [("before", None), ("after", Some(e))] {
        let f = Fixture::new("reads", Dialect::Postgres);
        f.write(&model_with(ed, false));
        if let Some(s) = &skip {
            site_switch::skip(s.clone());
        }
        f.run("res", false);
        site_switch::off();
        let results = f.dir.join("res/.sqldiff/results");
        for k in std::fs::read_dir(results).unwrap().flatten() {
            for en in std::fs::read_dir(k.path()).unwrap().flatten() {
                let n = en.file_name().to_string_lossy().into_owned();
                if !n.ends_with(".json") {
                    continue;
                }
                let body = std::fs::read_to_string(en.path()).unwrap_or_default();
                if !body.contains(&part) {
                    continue;
                }
                let rp = en.path().with_extension("reads");
                let raw = std::fs::read_to_string(&rp).unwrap_or_default();
                let v: serde_json::Value =
                    serde_json::from_str(raw.get(65..).unwrap_or("null")).unwrap_or_default();
                println!("{label} {}:", en.path().display());
                for r in v["reads"].as_array().into_iter().flatten() {
                    println!(
                        "   {} {} {}",
                        r[0],
                        r[1],
                        r[2].to_string().chars().take(16).collect::<String>()
                    );
                }
            }
        }
    }
}
