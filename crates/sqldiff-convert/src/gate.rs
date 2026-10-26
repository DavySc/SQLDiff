//! The done gate: whether a converted tree is complete, for CI.
//!
//! Complete means every unit is converted or converted under assumptions,
//! every assumption is accepted and every decision picked by a person, by
//! name; no rejection stands; every unit an agent wrote is approved by a
//! person; no decision was made on other bytes of its file; the output
//! directory holds exactly what converting the source gives now; and, for
//! templates, an instance check that is complete (every instance converted
//! and checked on the generator's own output) and was run on these very
//! trees. Plainly converted units need no decision: "nothing found" is
//! listed as such and never as reviewed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use crate::queue::Instances;
use crate::review::{By, State, FILE_CHANGED};
use crate::tree::Tree;
use crate::Status;

/// One reason the tree is not complete.
#[derive(Debug, Clone, Serialize)]
pub struct Blocker {
    /// What kind: a unit status, `open`, `rejected`, `agent-written`,
    /// `outdated`, `unplaced`, `unreadable`, `stale-output`, `instances`,
    /// `ledger`.
    pub kind: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// 1-based line in the written output file; 0 when none.
    pub out_line: usize,
    pub what: String,
}

/// An assumption accepted, or a decision picked, wherever it is current.
#[derive(Debug, Clone, Serialize)]
pub struct Accepted {
    pub rule: String,
    pub state: State,
    pub units: usize,
    pub authors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Gate {
    pub complete: bool,
    pub blockers: Vec<Blocker>,
    /// Blockers per kind.
    pub counts: BTreeMap<String, usize>,
    pub accepted: Vec<Accepted>,
    /// Units checked, and those plainly converted with nothing decided
    /// (nothing found, not proven, not reviewed).
    pub units: usize,
    pub nothing_found: usize,
    /// Whether an instance report was read and found current.
    pub instances: Option<bool>,
}

/// Judges a converted tree. `t` is a fresh conversion with `a` of its
/// source, with the ledger in `out` applied (`tree::review_in`, or
/// `tree::review` with an empty ledger); `inst` the instance check's report.
pub fn check(
    t: &Tree,
    a: &crate::tree::Args,
    out: &Path,
    inst: Option<&Instances>,
) -> Result<Gate, String> {
    let (src, templates) = (a.src.as_path(), a.template.is_some());
    let mut b = Vec::new();
    let mut accepted: BTreeMap<(String, State), (usize, BTreeSet<String>)> = BTreeMap::new();
    let (mut units, mut nothing_found) = (0, 0);
    let mut unchecked = Vec::new();
    let block = |b: &mut Vec<Blocker>, kind: &str, path: &str, unit: Option<&str>, line, what| {
        b.push(Blocker {
            kind: kind.to_string(),
            path: path.to_string(),
            unit: unit.map(str::to_string),
            out_line: line,
            what,
        })
    };

    // A line nobody can read may be someone's rejection.
    for l in t.report.ledger.iter().flat_map(|l| &l.rejected_lines) {
        block(
            &mut b,
            "ledger",
            crate::review::LEDGER_FILE,
            None,
            0,
            format!("a line that does not parse or is not allowed: {l}"),
        );
    }
    for p in &t.report.unreadable {
        block(
            &mut b,
            "unreadable",
            p,
            None,
            0,
            "could not be read as text: not converted".into(),
        );
    }
    // Nothing in OUT this conversion does not write: a converted file whose
    // source is gone would otherwise stay, unchecked.
    let written: BTreeSet<&str> = t.report.files.iter().map(|f| f.path.as_str()).collect();
    for rel in crate::tree::collect_paths(out, &a.extensions)? {
        if !written.contains(rel.as_str()) && !crate::tree::is_report(&rel) {
            block(
                &mut b,
                "stale-output",
                &rel,
                None,
                0,
                "no source file converts to it: remove it, or restore its source".into(),
            );
        }
    }
    for (f, text) in t.report.files.iter().zip(&t.outputs) {
        // The committed output must be what converting the source gives now.
        match std::fs::read(out.join(&f.path)) {
            Ok(bytes) if bytes == text.as_bytes() => {}
            Ok(_) => block(
                &mut b,
                "stale-output",
                &f.path,
                None,
                0,
                "differs from a fresh conversion: run `sqldiff convert --overwrite` (or record a hand edit as a written text)".into(),
            ),
            Err(_) => block(
                &mut b,
                "stale-output",
                &f.path,
                None,
                0,
                "missing from the output directory: run `sqldiff convert`".into(),
            ),
        }
        if !f.unplaced.is_empty() {
            block(
                &mut b,
                "unplaced",
                &f.path,
                None,
                0,
                format!(
                    "output findings outside any unit: {}",
                    f.unplaced.join(", ")
                ),
            );
        }
        if !f.unproven.is_empty() {
            block(
                &mut b,
                Status::Unproven.label(),
                &f.path,
                None,
                0,
                format!("not compared, outside any unit: {}", f.unproven.join("; ")),
            );
        }
        for u in f.units.iter().chain(&f.converted_units) {
            units += 1;
            let id = Some(u.id.as_str());
            let Some(r) = &u.review else {
                return Err(format!(
                    "{}: unit {} was not reviewed against the ledger",
                    f.path, u.id
                ));
            };
            // A template unit written by hand is judged by the instance
            // check alone: it passes once that check is complete and current.
            if u.status == Status::Unproven && r.undischargeable == ["hand-written.unchecked"] {
                unchecked.push((f.path.clone(), u.id.clone(), u.out_line));
            } else if u.status >= Status::Unproven {
                block(
                    &mut b,
                    u.status.label(),
                    &f.path,
                    id,
                    u.out_line,
                    format!("{}: {}", u.rules.join(", "), u.reason),
                );
            } else if !r.undischargeable.is_empty() {
                block(
                    &mut b,
                    Status::Unproven.label(),
                    &f.path,
                    id,
                    u.out_line,
                    r.undischargeable.join(", "),
                );
            }
            for rule in &r.open {
                block(
                    &mut b,
                    "open",
                    &f.path,
                    id,
                    u.out_line,
                    format!("`{rule}` is not accepted or picked by a person"),
                );
            }
            let current = |s: State| {
                r.decisions
                    .iter()
                    .filter(move |d| d.current && d.record.state == s)
            };
            for d in current(State::Rejected) {
                block(
                    &mut b,
                    "rejected",
                    &f.path,
                    id,
                    u.out_line,
                    format!("rejected by {}: {}", d.record.author, d.record.reason),
                );
            }
            if r.written_by == Some(By::Agent)
                && !current(State::Approved).any(|d| d.record.by == By::Human)
            {
                block(
                    &mut b,
                    "agent-written",
                    &f.path,
                    id,
                    u.out_line,
                    "written by an agent and not approved by a person".into(),
                );
            }
            for d in &r.decisions {
                if !d.current && d.why_not.as_deref() == Some(FILE_CHANGED) {
                    block(
                        &mut b,
                        "outdated",
                        &f.path,
                        id,
                        u.out_line,
                        format!(
                            "{} {}{} on other bytes of the file: confirm it again",
                            d.record.author,
                            d.record.state.label(),
                            if d.record.rule.is_empty() {
                                String::new()
                            } else {
                                format!(" `{}`", d.record.rule)
                            }
                        ),
                    );
                }
            }
            for s in [State::Accepted, State::Picked] {
                for d in current(s) {
                    let e = accepted.entry((d.record.rule.clone(), s)).or_default();
                    e.0 += 1;
                    e.1.insert(d.record.author.clone());
                }
            }
            if u.status == Status::Converted && r.decisions.is_empty() && r.written_by.is_none() {
                nothing_found += 1;
            }
        }
    }

    let instances = match inst {
        None if templates => {
            block(
                &mut b,
                "instances",
                "",
                None,
                0,
                "templates need the instance check (`--instances`): nothing else shows the woven code behaves the same".into(),
            );
            None
        }
        None => None,
        Some(i) => Some(instances_current(i, src, out, &mut b)?),
    };
    if instances != Some(true) {
        for (path, unit, line) in unchecked {
            block(
                &mut b,
                Status::Unproven.label(),
                &path,
                Some(&unit),
                line,
                "hand-written.unchecked: written by hand in a template; only a complete, current instance check proves it".into(),
            );
        }
    }

    let mut counts = BTreeMap::new();
    for x in &b {
        *counts.entry(x.kind.clone()).or_insert(0) += 1;
    }
    Ok(Gate {
        complete: b.is_empty(),
        blockers: b,
        counts,
        accepted: accepted
            .into_iter()
            .map(|((rule, state), (units, authors))| Accepted {
                rule,
                state,
                units,
                authors: authors.into_iter().collect(),
            })
            .collect(),
        units,
        nothing_found,
        instances,
    })
}

/// Whether the instance report is complete and was made on these trees.
fn instances_current(
    i: &Instances,
    src: &Path,
    out: &Path,
    b: &mut Vec<Blocker>,
) -> Result<bool, String> {
    let before = b.len();
    let mut add = |what: String| {
        b.push(Blocker {
            kind: "instances".into(),
            path: String::new(),
            unit: None,
            out_line: 0,
            what,
        })
    };
    match i.complete {
        Some(true) => {}
        Some(false) => {
            let not_done = i.instances.iter().filter(|x| !x.done).count();
            add(format!(
                "the instance check is not complete: {not_done} of {} instance(s) not done (not converted, or checked on SQLDiff's weave only), {} template(s) no instance uses, {} generator file(s) no instance accounts for",
                i.instances.len(),
                i.not_exercised.len(),
                i.not_in_manifest.len() + i.not_in_manifest_target.len()
            ))
        }
        None => add(
            "the instance report predates the generator output check: run `sqldiff instances` again".into(),
        ),
    }
    // Made on these trees: the same bytes as now.
    let mut compare = |key: &str, path: Option<&Path>| -> Result<(), String> {
        let Some(want) = i.hashes.get(key) else {
            add(format!(
                "the instance report holds no hash of its {key}: run `sqldiff instances` again"
            ));
            return Ok(());
        };
        match path {
            Some(p) if p.exists() => {
                if &crate::tree::dir_hash(p)? != want {
                    add(format!(
                        "the {key} ({}) changed since the instance check: run `sqldiff instances` again",
                        p.display()
                    ));
                }
            }
            Some(p) => add(format!(
                "the {key} the instance check read ({}) is not there to compare",
                p.display()
            )),
            None => add(format!("the instance report does not name its {key}")),
        }
        Ok(())
    };
    compare("source", Some(src))?;
    compare("target", Some(out))?;
    compare("manifest", i.manifest.as_deref().map(Path::new))?;
    for (key, p) in [("woven", &i.woven), ("woven_target", &i.woven_target)] {
        if let Some(p) = p {
            compare(key, Some(Path::new(p)))?;
        }
    }
    Ok(b.len() == before)
}

/// The gate as text: a verdict line, blockers by kind, and what was
/// accepted by name.
pub fn summary(g: &Gate, max: usize) -> String {
    let mut s = String::new();
    if g.complete {
        s.push_str(&format!(
            "Review complete: {} unit(s); {} plainly converted with nothing decided (nothing found, not proven).\n",
            g.units, g.nothing_found
        ));
    } else {
        s.push_str(&format!(
            "Review not complete: {} blocker(s) ({}).\n",
            g.blockers.len(),
            g.counts
                .iter()
                .map(|(k, n)| format!("{n} {k}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        for x in g.blockers.iter().take(max) {
            let at = match (x.path.is_empty(), x.out_line) {
                (true, _) => String::new(),
                (false, 0) => format!("{} ", x.path),
                (false, l) => format!("{}:{l} ", x.path),
            };
            s.push_str(&format!("  {at}[{}] {}\n", x.kind, x.what));
        }
        if g.blockers.len() > max {
            s.push_str(&format!("  {} more\n", g.blockers.len() - max));
        }
    }
    if !g.accepted.is_empty() {
        s.push_str("Accepted by name:\n");
        for a in &g.accepted {
            s.push_str(&format!(
                "  {} {} on {} unit(s) by {}\n",
                a.state.label(),
                a.rule,
                a.units,
                a.authors.join(", ")
            ));
        }
    }
    s
}
