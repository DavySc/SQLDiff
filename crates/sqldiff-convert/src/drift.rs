//! Drift: keep the template trees of several dialects in step. This is what
//! `sqldiff drift` runs, and what the app's drift runs run.
//!
//! A generator with one template set per dialect drifts when a template is
//! added, changed or rewired in one tree and not in the others. Three
//! checks, all from the files alone:
//!
//! - every template is in every tree (`drift.missing`);
//! - its placeholders are the same names in every tree
//!   (`drift.placeholders`), so one set of values weaves all of them;
//! - its directives (the pack's special forms, with their arguments) are the
//!   same, in the same order, in every tree (`drift.directives`).
//!
//! With a base revision, a template changed since the merge base in some
//! trees and not in the others is `drift.changed-alone`, unless the pack's
//! `intentional_difference` author marker in that template says why. With a
//! manifest, every instance is also woven in the reference tree and in each
//! other tree and checked like [`crate::instances`].

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use regex::Regex;
use serde::Serialize;
use sqldiff_core::hooks::Hooks;
use sqldiff_core::{Dialect, Options};
use sqldiff_source::compose;
use sqldiff_source::pack::MarkerKind;
use sqldiff_source::weave::Manifest;
use sqldiff_source::Resolved;
use sqldiff_syntax::template::SegmentKind;

use crate::instances;
use crate::tree::collect;

/// Engine options for the instance checks of a tree pair: the catalogs of
/// the two dialects, `(reference, tree)`.
pub type OptionsFor = Box<dyn Fn(Dialect, Dialect) -> Result<Options, String> + Send + Sync>;

/// What to check.
pub struct Args {
    /// A dialect's template tree (at least two).
    pub trees: Vec<(Dialect, PathBuf)>,
    /// The tree the others are checked against (default: the first).
    pub reference: Option<Dialect>,
    /// Output directory for the report (and the instance checks).
    pub out: PathBuf,
    /// Template file extensions, without the dot (default: `sql`).
    pub extensions: Vec<String>,
    /// Parameter names, added to the names the manifest gives values for.
    pub parameters: Vec<String>,
    /// The base revision: a template changed between its merge base with
    /// HEAD and the working tree in some trees and not in all is drift.
    pub base: Option<String>,
    /// The instance manifest: weave every instance in the reference tree and
    /// in each other tree, and check each pair as `sqldiff instances`.
    pub manifest: Option<PathBuf>,
    /// Assumptions known to hold, for the instance checks.
    pub accept: Vec<String>,
    /// Parallel jobs for the instance checks (default: available cores).
    pub jobs: Option<usize>,
    /// Replace files in a non-empty output directory.
    pub overwrite: bool,
    pub options: OptionsFor,
    pub cfg: Resolved,
}

/// One instance check of a tree against the reference, as the command
/// prints it.
pub struct InstanceRun {
    /// `instances REF → TREE:`
    pub header: String,
    pub notes: Vec<String>,
    pub summary: String,
    /// `instance-report.json` and `.md`.
    pub reports: [PathBuf; 2],
}

/// A finished check, with its reports written.
pub struct Checked {
    /// No template drifts and every instance check is ok: `sqldiff drift`
    /// exits 0.
    pub ok: bool,
    /// Templates in any tree.
    pub templates: usize,
    /// Templates with drift, and drift items in all.
    pub drifting: usize,
    pub items: usize,
    /// Changes in one tree on purpose.
    pub waived: usize,
    /// Instance checks, with whether each is ok.
    pub instance_ok: Vec<(String, bool)>,
    pub instance_runs: Vec<InstanceRun>,
    /// The text summary `sqldiff drift` prints.
    pub summary: String,
    /// `drift-report.json` and `drift-report.md`.
    pub reports: [PathBuf; 2],
}

#[derive(Debug, Clone, Serialize)]
struct Item {
    rule: &'static str,
    /// The tree the item is about.
    dialect: String,
    text: String,
}

#[derive(Serialize)]
struct TemplateDrift {
    path: String,
    items: Vec<Item>,
}

#[derive(Serialize)]
struct Waived {
    path: String,
    dialect: String,
    /// The author marker's text.
    reason: String,
}

#[derive(Serialize)]
struct InstanceCheck {
    dialect: String,
    ok: bool,
    /// Relative to the output directory.
    report: String,
    totals: BTreeMap<&'static str, usize>,
}

#[derive(Serialize)]
struct Report {
    reference: String,
    trees: BTreeMap<String, String>,
    base: Option<String>,
    templates: usize,
    /// Templates with drift, by path.
    drift: Vec<TemplateDrift>,
    /// Changes in one tree only that an author marker explains.
    waived: Vec<Waived>,
    instances: Vec<InstanceCheck>,
}

/// One template as the drift checks see it.
struct Shape {
    placeholders: BTreeSet<String>,
    /// Special forms in order, with their arguments.
    directives: Vec<(String, Vec<String>)>,
}

/// Checks the trees and writes the reports. `hooks` gets each step done (a
/// tree read, an instance check, the reports); a cancel stops before the
/// next one, and nothing is written after it. The reports are written last,
/// so a cancelled check has none; instance checks finished before it keep
/// what they wrote under `out/instances`.
pub fn check(a: &Args, hooks: &Hooks) -> Result<Checked, String> {
    let cfg = &a.cfg;
    let def = cfg
        .policy
        .template
        .as_ref()
        .ok_or("drift needs a [template] syntax: give --pack or a sqldiff.toml with one")?;
    let placeholders = compose::Placeholders::new(def).map_err(|e| e.0)?;
    if a.trees.len() < 2 {
        return Err("drift needs at least two --tree".into());
    }
    let mut seen = BTreeSet::new();
    for (d, p) in &a.trees {
        if !seen.insert(*d) {
            return Err(format!("--tree {}: given twice", d.id()));
        }
        if !p.is_dir() {
            return Err(format!("{}: not a directory", p.display()));
        }
    }
    let reference = a.reference.unwrap_or(a.trees[0].0);
    let ref_tree = a
        .trees
        .iter()
        .find(|(d, _)| *d == reference)
        .map(|(_, p)| p.clone())
        .ok_or_else(|| format!("--reference {}: no --tree for it", reference.id()))?;
    prepare_out(a)?;
    let extensions: Vec<String> = if a.extensions.is_empty() {
        vec!["sql".into()]
    } else {
        a.extensions
            .iter()
            .map(|e| e.trim_start_matches('.').to_string())
            .collect()
    };

    // The parameter list, as `sqldiff instances` builds it.
    let manifest = a
        .manifest
        .as_ref()
        .map(|m| Manifest::load(m).map_err(|e| e.0))
        .transpose()?;
    let mut list: BTreeSet<String> = manifest.as_ref().map(|m| m.names()).unwrap_or_default();
    list.extend(a.parameters.iter().cloned());
    if placeholders.ignore_case() {
        list = list.into_iter().map(|n| n.to_uppercase()).collect();
    }
    let list = (!list.is_empty()).then_some(list);

    let steps = a.trees.len()
        + if a.manifest.is_some() {
            a.trees.len() - 1
        } else {
            0
        }
        + 1;
    let mut step = 0;
    let mut done = |hooks: &Hooks| {
        step += 1;
        hooks.report(step, steps);
    };

    // Every tree's templates.
    let mut shapes: BTreeMap<Dialect, BTreeMap<String, Shape>> = BTreeMap::new();
    for (d, dir) in &a.trees {
        hooks.stop_if_cancelled()?;
        let (inputs, unreadable) = collect(dir, &extensions)?;
        if let Some(f) = unreadable.first() {
            return Err(format!(
                "{}: not UTF-8 or UTF-16 text",
                dir.join(f).display()
            ));
        }
        let mut m = BTreeMap::new();
        for i in inputs {
            let (t, _) = placeholders.parse(&i.text, *d, list.as_ref());
            let mut shape = Shape {
                placeholders: BTreeSet::new(),
                directives: Vec::new(),
            };
            for s in &t.segments {
                match &s.kind {
                    SegmentKind::Placeholder { name } => {
                        shape.placeholders.insert(if placeholders.ignore_case() {
                            name.to_uppercase()
                        } else {
                            name.clone()
                        });
                    }
                    SegmentKind::Special { form, args } => {
                        shape.directives.push((form.clone(), args.clone()))
                    }
                    _ => {}
                }
            }
            m.insert(i.rel, shape);
        }
        shapes.insert(*d, m);
        done(hooks);
    }
    let all: BTreeSet<&str> = shapes
        .values()
        .flat_map(|m| m.keys().map(String::as_str))
        .collect();

    // Changed since the merge base, per tree.
    let changed: Option<BTreeMap<Dialect, Changed>> = match &a.base {
        Some(base) => Some(
            a.trees
                .iter()
                .map(|(d, dir)| Ok((*d, changed_since(dir, base)?)))
                .collect::<Result<_, String>>()?,
        ),
        None => None,
    };
    let intentional: Vec<Regex> = cfg
        .policy
        .author_markers
        .iter()
        .filter(|m| m.kind == MarkerKind::IntentionalDifference)
        .filter_map(|m| Regex::new(&m.pattern).ok())
        .collect();

    let mut drift = Vec::new();
    let mut waived = Vec::new();
    for path in &all {
        let mut items = Vec::new();
        let present: Vec<Dialect> = a
            .trees
            .iter()
            .map(|(d, _)| *d)
            .filter(|d| shapes[d].contains_key(*path))
            .collect();
        for (d, _) in &a.trees {
            if !present.contains(d) {
                items.push(Item {
                    rule: "drift.missing",
                    dialect: d.id().into(),
                    text: format!(
                        "not in the {} tree; present in {}",
                        d.display_name(),
                        names(&present)
                    ),
                });
            }
        }
        // Shapes against the reference, or against the first tree that has
        // the template when the reference does not.
        let base_dialect = if present.contains(&reference) {
            reference
        } else {
            present[0]
        };
        let base = shapes[&base_dialect].get(*path).expect("present");
        for d in present.iter().filter(|d| **d != base_dialect) {
            let s = &shapes[d][*path];
            if s.placeholders != base.placeholders {
                let only = |x: &BTreeSet<String>, y: &BTreeSet<String>| {
                    x.difference(y).cloned().collect::<Vec<_>>().join(", ")
                };
                let (extra, missing) = (
                    only(&s.placeholders, &base.placeholders),
                    only(&base.placeholders, &s.placeholders),
                );
                let mut parts = Vec::new();
                if !extra.is_empty() {
                    parts.push(format!("only here: {extra}"));
                }
                if !missing.is_empty() {
                    parts.push(format!(
                        "only in {}: {missing}",
                        base_dialect.display_name()
                    ));
                }
                items.push(Item {
                    rule: "drift.placeholders",
                    dialect: d.id().into(),
                    text: format!("placeholders differ ({})", parts.join("; ")),
                });
            }
            if s.directives != base.directives {
                let show = |v: &[(String, Vec<String>)]| {
                    if v.is_empty() {
                        return "none".to_string();
                    }
                    v.iter()
                        .map(|(f, args)| format!("{f}({})", args.join(", ")))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                items.push(Item {
                    rule: "drift.directives",
                    dialect: d.id().into(),
                    text: format!(
                        "directives differ: {} here, {} in {}",
                        show(&s.directives),
                        show(&base.directives),
                        base_dialect.display_name()
                    ),
                });
            }
        }
        if let Some(changed) = &changed {
            let touched: Vec<Dialect> = present
                .iter()
                .copied()
                .filter(|d| changed[d].paths.contains(*path))
                .collect();
            if !touched.is_empty() && touched.len() < present.len() {
                for d in &touched {
                    // Only a marker this change adds explains it: one
                    // committed long ago says nothing about this change.
                    let added = changed[d].added_lines(path)?;
                    let reason = intentional.iter().find_map(|re| {
                        added.iter().find_map(|l| {
                            re.captures(l).map(|c| {
                                c.get(1)
                                    .or_else(|| c.get(0))
                                    .map_or(String::new(), |m| m.as_str().trim().to_string())
                            })
                        })
                    });
                    match reason {
                        Some(reason) => waived.push(Waived {
                            path: path.to_string(),
                            dialect: d.id().into(),
                            reason,
                        }),
                        None => items.push(Item {
                            rule: "drift.changed-alone",
                            dialect: d.id().into(),
                            text: format!(
                                "changed since the merge base with {} in {} only, not in {}",
                                a.base.as_deref().unwrap_or(""),
                                names(&touched),
                                names(
                                    &present
                                        .iter()
                                        .copied()
                                        .filter(|p| !touched.contains(p))
                                        .collect::<Vec<_>>()
                                )
                            ),
                        }),
                    }
                }
            }
        }
        if !items.is_empty() {
            drift.push(TemplateDrift {
                path: path.to_string(),
                items,
            });
        }
    }

    // Every instance, woven in the reference tree and in each other tree.
    // Each check writes only once it is past its last cancel point.
    let mut instances = Vec::new();
    let mut instance_runs = Vec::new();
    if let Some(m) = &a.manifest {
        let inner = Hooks {
            progress: None,
            cancel: hooks.cancel.clone(),
        };
        for (d, dir) in a.trees.iter().filter(|(d, _)| *d != reference) {
            hooks.stop_if_cancelled()?;
            let rel = format!("instances/{}", d.id());
            let c = instances::check(
                &instances::Args {
                    manifest: m.clone(),
                    source: Some(ref_tree.clone()),
                    target: Some(dir.clone()),
                    from: reference,
                    to: *d,
                    out: a.out.join(&rel),
                    woven: None,
                    woven_target: None,
                    generator_only: false,
                    since: None,
                    parameters: a.parameters.clone(),
                    accept: a.accept.clone(),
                    jobs: a.jobs,
                    overwrite: a.overwrite,
                    options: (a.options)(reference, *d)?,
                    cfg: cfg.clone(),
                },
                &inner,
            )?;
            instance_runs.push(InstanceRun {
                header: format!("instances {} → {}:", reference.id(), d.id()),
                notes: c.notes,
                summary: c.summary,
                reports: c.reports,
            });
            instances.push(InstanceCheck {
                dialect: d.id().into(),
                ok: c.ok,
                report: format!("{rel}/instance-report.md"),
                totals: c.totals,
            });
            done(hooks);
        }
    }

    let report = Report {
        reference: reference.id().into(),
        trees: a
            .trees
            .iter()
            .map(|(d, p)| (d.id().to_string(), p.display().to_string()))
            .collect(),
        base: a.base.clone(),
        templates: all.len(),
        drift,
        waived,
        instances,
    };
    // The last point a check stops at: from here on the reports are written.
    hooks.stop_if_cancelled()?;
    let json = a.out.join("drift-report.json");
    std::fs::write(&json, serde_json::to_string_pretty(&report).unwrap() + "\n")
        .map_err(|e| format!("{}: {e}", json.display()))?;
    let md = a.out.join("drift-report.md");
    std::fs::write(&md, markdown(&report)).map_err(|e| format!("{}: {e}", md.display()))?;
    done(hooks);
    let ok = report.drift.is_empty() && report.instances.iter().all(|i| i.ok);
    Ok(Checked {
        ok,
        templates: report.templates,
        drifting: report.drift.len(),
        items: report.drift.iter().map(|t| t.items.len()).sum(),
        waived: report.waived.len(),
        instance_ok: report
            .instances
            .iter()
            .map(|i| (i.dialect.clone(), i.ok))
            .collect(),
        instance_runs,
        summary: summary(&report),
        reports: [json, md],
    })
}

fn names(ds: &[Dialect]) -> String {
    ds.iter()
        .map(|d| d.display_name())
        .collect::<Vec<_>>()
        .join(", ")
}

/// What changed in one tree since the merge base.
struct Changed {
    dir: PathBuf,
    merge_base: String,
    /// Paths relative to the tree.
    paths: BTreeSet<String>,
}

impl Changed {
    /// The lines the change adds to `path`.
    fn added_lines(&self, path: &str) -> Result<Vec<String>, String> {
        let diff = git(
            &self.dir,
            &[
                "diff",
                "-U0",
                "--relative",
                "--no-ext-diff",
                "--no-renames",
                "--no-color",
                &self.merge_base,
                "--",
                path,
            ],
        )?;
        Ok(diff
            .lines()
            .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
            .map(|l| l[1..].to_string())
            .collect())
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    String::from_utf8(out.stdout).map_err(|_| "git: output is not UTF-8".to_string())
}

/// Template paths under `dir`, relative to it, that differ between the
/// merge base of `base` and HEAD and the working tree.
fn changed_since(dir: &Path, base: &str) -> Result<Changed, String> {
    let git = |args: &[&str]| git(dir, args);
    let mb = git(&["merge-base", base, "HEAD"])?;
    let list = git(&[
        "diff",
        "--name-only",
        "--relative",
        "-z",
        "--no-ext-diff",
        "--no-renames",
        mb.trim(),
        "--",
        ".",
    ])?;
    Ok(Changed {
        dir: dir.to_path_buf(),
        merge_base: mb.trim().to_string(),
        paths: list
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
    })
}

fn prepare_out(a: &Args) -> Result<(), String> {
    std::fs::create_dir_all(&a.out).map_err(|e| format!("{}: {e}", a.out.display()))?;
    let out = std::fs::canonicalize(&a.out).map_err(|e| format!("{}: {e}", a.out.display()))?;
    for (_, d) in &a.trees {
        let d = std::fs::canonicalize(d).map_err(|e| format!("{}: {e}", d.display()))?;
        if out.starts_with(&d) || d.starts_with(&out) {
            return Err(format!(
                "{}: the output directory overlaps {}",
                a.out.display(),
                d.display()
            ));
        }
    }
    let empty = std::fs::read_dir(&out)
        .map_err(|e| format!("{}: {e}", a.out.display()))?
        .next()
        .is_none();
    if !empty && !a.overwrite {
        return Err(format!(
            "{}: not empty; pass --overwrite to replace the files a check writes",
            a.out.display()
        ));
    }
    Ok(())
}

fn summary(r: &Report) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{} templates in {} trees, reference {}",
        r.templates,
        r.trees.len(),
        r.reference
    );
    let mut by_rule: BTreeMap<&str, usize> = BTreeMap::new();
    for t in &r.drift {
        for i in &t.items {
            *by_rule.entry(i.rule).or_default() += 1;
        }
    }
    let _ = writeln!(s, "{:<20} {:>6} templates", "drift", r.drift.len());
    for (rule, n) in by_rule {
        let _ = writeln!(s, "  {rule:<18} {n:>6}");
    }
    if !r.waived.is_empty() {
        let _ = writeln!(s, "{:<20} {:>6}", "waived", r.waived.len());
    }
    for i in &r.instances {
        let _ = writeln!(
            s,
            "instances {:<10} {}",
            i.dialect,
            if i.ok { "ok" } else { "not ok" }
        );
    }
    s
}

fn markdown(r: &Report) -> String {
    let mut s = String::from("# Drift report\n\n");
    let _ = writeln!(
        s,
        "Trees: {}. Reference: {}.{}\n",
        r.trees
            .iter()
            .map(|(d, p)| format!("{d} `{p}`"))
            .collect::<Vec<_>>()
            .join(", "),
        r.reference,
        r.base
            .as_ref()
            .map(|b| format!(" Changes since the merge base with `{b}`."))
            .unwrap_or_default()
    );
    let _ = writeln!(
        s,
        "Every template must be in every tree, with the same placeholders and the same directives in the same order{}.\n",
        if r.base.is_some() {
            ", and a change in one tree must be made in all of them or carry the pack's intentional-difference marker"
        } else {
            ""
        }
    );
    s.push_str("```\n");
    s.push_str(&summary(r));
    s.push_str("```\n");
    if !r.drift.is_empty() {
        s.push_str("\n## Drift\n\n| template | tree | rule | detail |\n|---|---|---|---|\n");
        for t in &r.drift {
            for i in &t.items {
                let _ = writeln!(
                    s,
                    "| `{}` | {} | {} | {} |",
                    t.path,
                    i.dialect,
                    i.rule,
                    i.text.replace('|', "\\|")
                );
            }
        }
    }
    if !r.waived.is_empty() {
        s.push_str(
            "\n## Changed in one tree on purpose\n\n| template | tree | reason |\n|---|---|---|\n",
        );
        for w in &r.waived {
            let _ = writeln!(
                s,
                "| `{}` | {} | {} |",
                w.path,
                w.dialect,
                w.reason.replace('|', "\\|")
            );
        }
    }
    if !r.instances.is_empty() {
        s.push_str("\n## Instances\n\n");
        for i in &r.instances {
            let _ = writeln!(
                s,
                "- {} → {}: {} ([report]({}))",
                r.reference,
                i.dialect,
                if i.ok { "ok" } else { "not ok" },
                i.report
            );
        }
    }
    s
}
