//! `sqldiff drift`: keep the template trees of several dialects in step.
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
//! With `--base REV`, a template changed since the merge base in some trees
//! and not in the others is `drift.changed-alone`, unless the pack's
//! `intentional_difference` author marker in that template says why. With
//! `--manifest`, every instance is also woven in the reference tree and in
//! each other tree and checked like `sqldiff instances`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use clap::Args as ClapArgs;
use regex::Regex;
use serde::Serialize;
use sqldiff_core::Dialect;
use sqldiff_source::compose;
use sqldiff_source::pack::MarkerKind;
use sqldiff_source::weave::Manifest;
use sqldiff_syntax::template::SegmentKind;

use crate::convert_cmd::collect;
use crate::instances_cmd::{self, InstancesArgs};
use crate::source_cmd;

#[derive(ClapArgs)]
pub struct DriftArgs {
    /// A dialect's template tree, as DIALECT=DIR (repeatable, at least two):
    /// `--tree tsql=templates/mssql --tree postgres=templates/pg`.
    #[arg(long = "tree", value_name = "DIALECT=DIR", value_parser = tree, required = true)]
    trees: Vec<(Dialect, PathBuf)>,
    /// The tree the others are checked against (default: the first).
    #[arg(long)]
    reference: Option<Dialect>,
    /// Output directory for the report (and the instance checks).
    #[arg(short, long)]
    out: PathBuf,
    /// Template file extension (repeatable; default: sql), any letter case,
    /// without the dot.
    #[arg(long = "extension", value_name = "EXT")]
    extensions: Vec<String>,
    /// A parameter name (repeatable), added to the names the manifest gives
    /// values for; under `placeholders = "parameters"` only these are
    /// placeholders.
    #[arg(long = "parameter", value_name = "NAME")]
    parameters: Vec<String>,
    /// The base revision: a template changed between its merge base with
    /// HEAD and the working tree in some trees and not in all is drift.
    #[arg(long, value_name = "REV")]
    base: Option<String>,
    /// The instance manifest: weave every instance in the reference tree
    /// and in each other tree, and check each pair as `sqldiff instances`.
    #[arg(long, value_name = "FILE")]
    manifest: Option<PathBuf>,
    /// An assumption you know holds (repeatable), for the instance checks.
    #[arg(long = "accept", value_name = "ID")]
    accept: Vec<String>,
    /// Parallel jobs for the instance checks (default: available cores).
    #[arg(short, long)]
    jobs: Option<usize>,
    /// Replace files in a non-empty output directory.
    #[arg(long)]
    overwrite: bool,
    #[command(flatten)]
    catalogs: source_cmd::CatalogArgs,
    #[command(flatten)]
    cfg: source_cmd::ConfigArgs,
}

fn tree(s: &str) -> Result<(Dialect, PathBuf), String> {
    let (d, p) = s
        .split_once('=')
        .ok_or_else(|| format!("`{s}`: expected DIALECT=DIR"))?;
    let d: Dialect = d.parse().map_err(|e| format!("`{d}`: {e}"))?;
    if p.is_empty() {
        return Err(format!("`{s}`: no directory"));
    }
    Ok((d, PathBuf::from(p)))
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
    text: String,
}

pub fn run(a: DriftArgs) -> Result<ExitCode, String> {
    let cfg = a.cfg.resolve()?;
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
    prepare_out(&a)?;
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

    // Every tree's templates.
    let mut shapes: BTreeMap<Dialect, BTreeMap<String, Shape>> = BTreeMap::new();
    for (d, dir) in &a.trees {
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
                text: i.text.clone(),
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
    }
    let all: BTreeSet<&str> = shapes
        .values()
        .flat_map(|m| m.keys().map(String::as_str))
        .collect();

    // Changed since the merge base, per tree.
    let changed: Option<BTreeMap<Dialect, BTreeSet<String>>> = match &a.base {
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
                .filter(|d| changed[d].contains(*path))
                .collect();
            if !touched.is_empty() && touched.len() < present.len() {
                for d in &touched {
                    let reason = intentional.iter().find_map(|re| {
                        re.captures(&shapes[d][*path].text).map(|c| {
                            c.get(1)
                                .or_else(|| c.get(0))
                                .map_or(String::new(), |m| m.as_str().trim().to_string())
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
    let mut instances = Vec::new();
    if let Some(m) = &a.manifest {
        for (d, dir) in a.trees.iter().filter(|(d, _)| *d != reference) {
            let rel = format!("instances/{}", d.id());
            println!("instances {} → {}:", reference.id(), d.id());
            let (ok, totals) = instances_cmd::check(InstancesArgs {
                manifest: m.clone(),
                source: ref_tree.clone(),
                target: dir.clone(),
                from: reference,
                to: *d,
                out: a.out.join(&rel),
                woven: None,
                parameters: a.parameters.clone(),
                accept: a.accept.clone(),
                jobs: a.jobs,
                overwrite: a.overwrite,
                catalogs: a.catalogs.clone(),
                cfg: a.cfg.clone(),
            })?;
            instances.push(InstanceCheck {
                dialect: d.id().into(),
                ok,
                report: format!("{rel}/instance-report.md"),
                totals,
            });
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
    let json = a.out.join("drift-report.json");
    std::fs::write(&json, serde_json::to_string_pretty(&report).unwrap() + "\n")
        .map_err(|e| format!("{}: {e}", json.display()))?;
    let md = a.out.join("drift-report.md");
    std::fs::write(&md, markdown(&report)).map_err(|e| format!("{}: {e}", md.display()))?;
    print!("{}", summary(&report));
    eprintln!("wrote {} and {}", json.display(), md.display());
    let ok = report.drift.is_empty() && report.instances.iter().all(|i| i.ok);
    Ok(ExitCode::from(if ok { 0 } else { 1 }))
}

fn names(ds: &[Dialect]) -> String {
    ds.iter()
        .map(|d| d.display_name())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Template paths under `dir`, relative to it, that differ between the
/// merge base of `base` and HEAD and the working tree.
fn changed_since(dir: &Path, base: &str) -> Result<BTreeSet<String>, String> {
    let git = |args: &[&str]| -> Result<String, String> {
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
    };
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
    Ok(list
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect())
}

fn prepare_out(a: &DriftArgs) -> Result<(), String> {
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
