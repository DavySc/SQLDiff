//! `sqldiff instances`: weave every instance the generator produces from the
//! source templates and from the converted templates, and check each pair.
//!
//! A converted template is only as good as the instances woven from it. For
//! each instance in the manifest both sides are woven with the same values
//! (`sqldiff_source::weave`), then the target instance is checked the way
//! `sqldiff convert` checks its output: it must parse as the target, nothing
//! in it may fail the output check (binding against the catalog of every
//! woven target instance, leftover source syntax, target limits, hazards),
//! the engine's diff of source against target may find no difference, and
//! every name a value is part of must be one the target accepts. A
//! template's status is the worst over the instances that use it.
//!
//! With `--woven`, the source weave is compared with the generator's own
//! output first, and generator output no instance accounts for is listed:
//! that is what makes the manifest complete.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::Args as ClapArgs;
use serde::Serialize;
use sqldiff_core::{Artifact, ChangeClass, Dialect, Options, Verdict};
use sqldiff_source::compose;
use sqldiff_source::weave::{Instance, Manifest, Weaver, Woven};

use crate::convert_cmd::{catalog, decode, parallel};
use crate::source_cmd;

#[derive(ClapArgs, Clone)]
pub struct InstancesArgs {
    /// The instance manifest (TOML): every result the generator produces,
    /// with its templates and parameter values.
    pub(crate) manifest: PathBuf,
    /// The source templates (the tree the generator reads today).
    #[arg(long, value_name = "DIR")]
    pub(crate) source: PathBuf,
    /// The converted templates (`sqldiff convert --template` output, or
    /// templates written by hand), with the same layout.
    #[arg(long, value_name = "DIR")]
    pub(crate) target: PathBuf,
    #[arg(long, default_value = "tsql")]
    pub(crate) from: Dialect,
    #[arg(long)]
    pub(crate) to: Dialect,
    /// Output directory for the woven instances and the report.
    #[arg(short, long)]
    pub(crate) out: PathBuf,
    /// The generator's own output: each instance's source weave must equal
    /// its `output` file there, and every file there must belong to an
    /// instance.
    #[arg(long, value_name = "DIR")]
    pub(crate) woven: Option<PathBuf>,
    /// A parameter name (repeatable), added to the names the manifest gives
    /// values for; under `placeholders = "parameters"` only these are
    /// placeholders.
    #[arg(long = "parameter", value_name = "NAME")]
    pub(crate) parameters: Vec<String>,
    /// An assumption you know holds (repeatable), e.g. ASM-DBO-SCHEMA: a
    /// status line in the converted templates that lists only accepted
    /// assumptions no longer holds an instance back. Listed in the report.
    #[arg(long = "accept", value_name = "ID")]
    pub(crate) accept: Vec<String>,
    /// Parallel jobs (default: available cores).
    #[arg(short, long)]
    pub(crate) jobs: Option<usize>,
    /// Replace files in a non-empty output directory.
    #[arg(long)]
    pub(crate) overwrite: bool,
    #[command(flatten)]
    pub(crate) catalogs: source_cmd::CatalogArgs,
    #[command(flatten)]
    pub(crate) cfg: source_cmd::ConfigArgs,
}

/// How far an instance can be trusted, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    /// Nothing found by any check.
    Converted,
    /// Equal only under the assumptions listed.
    Assumptions,
    /// The conversion chose one of several faithful targets and someone
    /// should confirm it (a `needs a decision` line in the template).
    Decision,
    /// Not decided: the engine's diff could neither prove nor rule out a
    /// difference, or the source instance does not parse.
    Unknown,
    /// One side could not be woven, or the source weave is not what the
    /// generator writes: nothing was checked.
    NotWoven,
    /// Found wrong.
    Differs,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Outcome::Converted => "converted",
            Outcome::Assumptions => "under assumptions",
            Outcome::Decision => "needs a decision",
            Outcome::Unknown => "unknown",
            Outcome::NotWoven => "not woven",
            Outcome::Differs => "differs",
        }
    }
}

const ORDER: [Outcome; 6] = [
    Outcome::Converted,
    Outcome::Assumptions,
    Outcome::Decision,
    Outcome::Unknown,
    Outcome::NotWoven,
    Outcome::Differs,
];

#[derive(Debug, Clone, Serialize)]
struct Item {
    status: Outcome,
    rule: String,
    /// `source` or `target` instance; `manifest` for weaving.
    side: &'static str,
    /// 1-based line in the woven file, 0 when it has none.
    line: usize,
    text: String,
}

#[derive(Serialize)]
struct InstanceReport {
    name: String,
    status: Outcome,
    /// Woven files, relative to the output directory.
    source_file: String,
    target_file: String,
    templates: Vec<String>,
    items: Vec<Item>,
    /// Assumptions the engine's diff makes about the whole pair, by id (name
    /// folding, default schema): the same ones `sqldiff convert` makes for
    /// every file, listed here and not counted in the status.
    engine_assumptions: Vec<String>,
}

#[derive(Serialize)]
struct TemplateReport {
    path: String,
    status: Outcome,
    instances: usize,
}

#[derive(Serialize)]
struct Report {
    from: String,
    to: String,
    manifest: String,
    source: String,
    target: String,
    woven: Option<String>,
    /// Assumptions accepted with --accept.
    accepted: Vec<String>,
    instances: Vec<InstanceReport>,
    /// Converted templates, worst instance status first.
    templates: Vec<TemplateReport>,
    /// Target templates no instance uses: never checked.
    not_exercised: Vec<String>,
    /// Generator output no instance accounts for.
    not_in_manifest: Vec<String>,
    totals: BTreeMap<&'static str, usize>,
}

pub fn run(a: InstancesArgs) -> Result<ExitCode, String> {
    let ok = check(a)?.0;
    Ok(ExitCode::from(if ok { 0 } else { 1 }))
}

/// Runs the check and writes the report: whether every instance is
/// converted (under assumptions at worst) and all generator output is
/// accounted for, and the count of instances per status.
pub(crate) fn check(a: InstancesArgs) -> Result<(bool, BTreeMap<&'static str, usize>), String> {
    let cfg = a.cfg.resolve()?;
    let def = cfg
        .policy
        .template
        .as_ref()
        .ok_or("instances needs a [template] syntax: give --pack or a sqldiff.toml with one")?;
    let placeholders = compose::Placeholders::new(def).map_err(|e| e.0)?;
    let manifest = Manifest::load(&a.manifest).map_err(|e| e.0)?;
    let kinds: BTreeMap<String, sqldiff_convert::ValueKind> = def
        .values
        .iter()
        .filter_map(|(n, k)| {
            let n = if def.ignore_case {
                n.to_uppercase()
            } else {
                n.clone()
            };
            Some((n, sqldiff_convert::ValueKind::parse(k)?))
        })
        .collect();
    if manifest.instances.is_empty() {
        return Err(format!("{}: no [[instance]]", a.manifest.display()));
    }
    for d in [&a.source, &a.target] {
        if !d.is_dir() {
            return Err(format!("{}: not a directory", d.display()));
        }
    }
    prepare_out(&a)?;
    let jobs = a
        .jobs
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
        .max(1);
    let mut list = manifest.names();
    list.extend(a.parameters.iter().cloned());
    if placeholders.ignore_case() {
        list = list.into_iter().map(|n| n.to_uppercase()).collect();
    }
    let weaver = |root: &Path, dialect| Weaver {
        root: root.to_path_buf(),
        dialect,
        placeholders: &placeholders,
        include: def.include.clone(),
        list: Some(list.clone()),
    };
    let (ws, wt) = (weaver(&a.source, a.from), weaver(&a.target, a.to));
    let woven: Vec<(Woven, Woven)> = parallel(jobs, &manifest.instances, |i| {
        (ws.weave(&manifest, i), wt.weave(&manifest, i))
    });

    // The generator's own output, when given.
    let mut not_in_manifest = Vec::new();
    let mut generated: Vec<Option<Result<String, String>>> = vec![None; woven.len()];
    if let Some(dir) = &a.woven {
        let wanted: BTreeSet<&str> = manifest
            .instances
            .iter()
            .map(|i| i.output.as_str())
            .collect();
        let mut files = Vec::new();
        walk(dir, dir, &mut files).map_err(|e| format!("{}: {e}", dir.display()))?;
        not_in_manifest = files
            .into_iter()
            .filter(|f| !wanted.contains(f.as_str()))
            .collect();
        for (k, i) in manifest.instances.iter().enumerate() {
            let p = dir.join(&i.output);
            generated[k] = Some(match std::fs::read(&p) {
                Ok(b) => {
                    decode(&b).ok_or_else(|| format!("{}: not UTF-8 or UTF-16 text", i.output))
                }
                Err(e) => Err(format!("{}: {e}", i.output)),
            });
        }
    }

    // Catalogs: every woven instance on its side, plus any given.
    let mut opts = Options::default();
    a.catalogs.apply(&mut opts, a.from, Some(a.to))?;
    opts.facts_a.push(Arc::new(catalog(
        woven.iter().map(|(s, _)| s.text.as_str()),
        a.from,
        &a.source,
        None,
    )));
    opts.facts_b.push(Arc::new(catalog(
        woven.iter().map(|(_, t)| t.text.as_str()),
        a.to,
        &a.target,
        None,
    )));

    let work: Vec<(usize, &Instance)> = manifest.instances.iter().enumerate().collect();
    let mut results: Vec<Checked> = parallel(jobs, &work, |&(k, i)| {
        let (s, t) = &woven[k];
        check_instance(
            i,
            s,
            t,
            generated[k].as_ref(),
            a.from,
            a.to,
            &opts,
            &a.accept,
            &kinds,
        )
    });

    // Two values that PostgreSQL truncates to the same name.
    let mut truncated: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (_, _, names) in &results {
        for (cut, full) in names {
            truncated.entry(cut).or_default().insert(full);
        }
    }
    let clashes: BTreeMap<String, Vec<String>> = truncated
        .into_iter()
        .filter(|(_, full)| full.len() > 1)
        .map(|(c, f)| (c.to_string(), f.into_iter().map(String::from).collect()))
        .collect();
    for (items, _, names) in &mut results {
        for (cut, full) in names.iter() {
            if let Some(all) = clashes.get(cut) {
                items.push(Item {
                    status: Outcome::Differs,
                    rule: "pg.name-collision".into(),
                    side: "target",
                    line: 0,
                    text: format!(
                        "`{full}` and {} become the same PostgreSQL name `{cut}` after truncation",
                        all.iter()
                            .filter(|n| *n != full)
                            .map(|n| format!("`{n}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                });
            }
        }
    }

    // Woven files and the per-instance report.
    let mut instances = Vec::new();
    for ((i, (s, t)), (items, engine, _)) in manifest.instances.iter().zip(&woven).zip(results) {
        let (sf, tf) = (
            format!("source/{}.sql", i.name),
            format!("target/{}.sql", i.name),
        );
        for (f, text) in [(&sf, &s.text), (&tf, &t.text)] {
            let p = a.out.join(f);
            if let Some(d) = p.parent() {
                std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
            }
            std::fs::write(&p, text).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        let status = items
            .iter()
            .map(|x| x.status)
            .max()
            .unwrap_or(Outcome::Converted);
        instances.push(InstanceReport {
            name: i.name.clone(),
            status,
            source_file: sf,
            target_file: tf,
            templates: t.templates.iter().cloned().collect(),
            items,
            engine_assumptions: engine,
        });
    }

    // Templates: the worst instance that uses them.
    let mut by_template: BTreeMap<&str, (Outcome, usize)> = BTreeMap::new();
    for r in &instances {
        for t in &r.templates {
            let e = by_template.entry(t).or_insert((Outcome::Converted, 0));
            e.0 = e.0.max(r.status);
            e.1 += 1;
        }
    }
    let mut targets = Vec::new();
    walk(&a.target, &a.target, &mut targets).map_err(|e| format!("{}: {e}", a.target.display()))?;
    let not_exercised: Vec<String> = targets
        .into_iter()
        .filter(|f| !by_template.contains_key(f.as_str()) && !is_report(f))
        .collect();
    let mut templates: Vec<TemplateReport> = by_template
        .into_iter()
        .map(|(p, (status, n))| TemplateReport {
            path: p.to_string(),
            status,
            instances: n,
        })
        .collect();
    templates.sort_by(|x, y| y.status.cmp(&x.status).then(x.path.cmp(&y.path)));

    let mut totals: BTreeMap<&'static str, usize> = ORDER.iter().map(|o| (o.label(), 0)).collect();
    for r in &instances {
        *totals.entry(r.status.label()).or_default() += 1;
    }
    let report = Report {
        from: a.from.id().to_string(),
        to: a.to.id().to_string(),
        manifest: a.manifest.display().to_string(),
        source: a.source.display().to_string(),
        target: a.target.display().to_string(),
        woven: a.woven.as_ref().map(|w| w.display().to_string()),
        accepted: a.accept.clone(),
        instances,
        templates,
        not_exercised,
        not_in_manifest,
        totals,
    };
    let json = a.out.join("instance-report.json");
    std::fs::write(&json, serde_json::to_string_pretty(&report).unwrap() + "\n")
        .map_err(|e| format!("{}: {e}", json.display()))?;
    let md = a.out.join("instance-report.md");
    std::fs::write(&md, markdown(&report)).map_err(|e| format!("{}: {e}", md.display()))?;
    print!("{}", summary(&report));
    eprintln!("wrote {} and {}", json.display(), md.display());
    let ok = report
        .instances
        .iter()
        .all(|r| r.status <= Outcome::Assumptions)
        && report.not_in_manifest.is_empty();
    Ok((ok, report.totals))
}

/// An instance's items, the engine's assumptions about the whole pair, and
/// the PostgreSQL names over the limit, as (truncated, full).
type Checked = (Vec<Item>, Vec<String>, Vec<(String, String)>);

/// Checks one instance; also returns the PostgreSQL names its values make
/// that are over the limit, as (truncated, full), for the collision check.
#[allow(clippy::too_many_arguments)]
fn check_instance(
    i: &Instance,
    s: &Woven,
    t: &Woven,
    generated: Option<&Result<String, String>>,
    from: Dialect,
    to: Dialect,
    opts: &Options,
    accepted: &[String],
    kinds: &BTreeMap<String, sqldiff_convert::ValueKind>,
) -> Checked {
    let mut items = Vec::new();
    let weave = |side, w: &Woven, items: &mut Vec<Item>| {
        for p in &w.problems {
            items.push(Item {
                status: Outcome::NotWoven,
                rule: "weave".into(),
                side,
                line: 0,
                text: p.clone(),
            });
        }
    };
    weave("source", s, &mut items);
    weave("target", t, &mut items);
    match generated {
        Some(Ok(g)) => {
            if let Some(line) = first_difference(&s.text, g) {
                items.push(Item {
                    status: Outcome::NotWoven,
                    rule: "weave.generator-mismatch".into(),
                    side: "source",
                    line,
                    text: format!(
                        "the source weave differs from the generator's output `{}` from line {line}: the manifest or the weaver does not match the generator",
                        i.output
                    ),
                });
            }
        }
        Some(Err(e)) => items.push(Item {
            status: Outcome::NotWoven,
            rule: "weave.generator-output".into(),
            side: "source",
            line: 0,
            text: format!("the generator's output for this instance could not be read: {e}"),
        }),
        None => {}
    }
    if !items.is_empty() {
        return (items, Vec::new(), Vec::new());
    }

    items.extend(markers(&t.text, accepted));
    let lines_s = sqldiff_syntax::LineIndex::new(&s.text);
    let lines_t = sqldiff_syntax::LineIndex::new(&t.text);
    for d in sqldiff_syntax::parse(&s.text, from).diagnostics {
        items.push(Item {
            status: Outcome::Unknown,
            rule: "parse".into(),
            side: "source",
            line: lines_s.line_col(d.offset).0,
            text: format!(
                "the source instance does not parse as {}: {}",
                from.display_name(),
                d.message
            ),
        });
    }
    for d in sqldiff_syntax::parse(&t.text, to).diagnostics {
        items.push(Item {
            status: Outcome::Differs,
            rule: "parse".into(),
            side: "target",
            line: lines_t.line_col(d.offset).0,
            text: format!(
                "the target instance does not parse as {}: {}",
                to.display_name(),
                d.message
            ),
        });
    }
    let sa = Artifact::new(format!("source/{}.sql", i.name), from, s.text.as_str());
    let ta = Artifact::new(format!("target/{}.sql", i.name), to, t.text.as_str());
    let own = Options {
        catalog_a: opts.catalog_b.clone(),
        facts_a: opts.facts_b.clone(),
        catalog_b: Vec::new(),
        facts_b: Vec::new(),
        ..opts.clone()
    };
    for f in sqldiff_core::check(&ta, &own).findings {
        if sqldiff_convert::gates(&f) {
            items.push(Item {
                status: Outcome::Differs,
                rule: f.rule.clone(),
                side: "target",
                line: f.a.as_ref().map_or(0, |l| l.start_line),
                text: f.witness.clone(),
            });
        }
    }
    // Every finding of the diff counts: proven different, not decided
    // (unknown), or equal only under the assumptions it names.
    let report = sqldiff_core::diff(&sa, &ta, opts);
    for f in &report.findings {
        let (status, rule) = if f.verdict == Verdict::Different
            || f.class == ChangeClass::Residue && f.b.is_some()
        {
            (Outcome::Differs, f.rule.clone())
        } else {
            match f.verdict {
                Verdict::Unknown => (Outcome::Unknown, f.rule.clone()),
                Verdict::EqualUnderAssumptions => (
                    Outcome::Assumptions,
                    if f.assumptions.is_empty() {
                        f.rule.clone()
                    } else {
                        format!("{}: {}", f.rule, f.assumptions.join(", "))
                    },
                ),
                _ => continue,
            }
        };
        let (side, loc) = match (&f.b, &f.a) {
            (Some(b), _) => ("target", Some(b)),
            (None, a) => ("source", a.as_ref()),
        };
        items.push(Item {
            status,
            rule,
            side,
            line: loc.map_or(0, |l| l.start_line),
            text: f.witness.clone(),
        });
    }
    let engine: Vec<String> = report.assumptions.iter().map(|x| x.id.clone()).collect();
    // Names: only values that are names. Dialect code given once for every
    // dialect is used unchanged in the target.
    let kind = |n: &str| {
        kinds
            .get(n)
            .copied()
            .unwrap_or(sqldiff_convert::ValueKind::Name)
    };
    let spans: Vec<(usize, usize)> = t
        .values
        .iter()
        .filter(|v| kind(&v.name) == sqldiff_convert::ValueKind::Name)
        .map(|v| (v.start, v.end))
        .collect();
    let mut reported = BTreeSet::new();
    for v in &t.values {
        if kind(&v.name).per_dialect() && !v.per_dialect && reported.insert(&v.name) {
            items.push(Item {
                status: Outcome::Assumptions,
                rule: "ASM-TEMPLATE-VALUE-SAME".into(),
                side: "target",
                line: lines_t.line_col(v.start).0,
                text: format!(
                    "{}'s value `{}` is {} code given once for every dialect and used unchanged in {}; give it in params_for.{} if {} needs another",
                    v.name,
                    &t.text[v.start..v.end],
                    kind(&v.name).label(),
                    to.display_name(),
                    to.id(),
                    to.display_name()
                ),
            });
        }
    }
    let mut names = Vec::new();
    for n in sqldiff_convert::names::check_values(&t.text, to, &spans) {
        if let Some(cut) = &n.truncated {
            names.push(cut.clone());
        }
        items.push(Item {
            status: match n.status {
                sqldiff_convert::Status::Converted => Outcome::Converted,
                sqldiff_convert::Status::Assumptions => Outcome::Assumptions,
                sqldiff_convert::Status::Decision => Outcome::Decision,
                _ => Outcome::Differs,
            },
            rule: n.rule.to_string(),
            side: "target",
            line: lines_t.line_col(n.start).0,
            text: n.text,
        });
    }
    items.sort_by(|x, y| y.status.cmp(&x.status).then(x.line.cmp(&y.line)));
    (items, engine, names)
}

/// Assumptions the woven instances discharge: the values are known, the
/// includes are woven in, and each instance is woven with its own values.
/// A value for dialect code is checked per instance below, and a fragment
/// is checked in the woven statement.
const DISCHARGED: [&str; 5] = [
    "ASM-TEMPLATE-PLACEHOLDER",
    "ASM-TEMPLATE-DIRECTIVE",
    "ASM-TEMPLATE-SHARED-VALUES",
    "ASM-TEMPLATE-FRAGMENT",
    "template.value-per-dialect",
];

/// What `sqldiff convert` left in the converted templates: its status lines
/// (`-- [sqldiff: <status>] <rules>`) and refused units (`-- [not
/// converted]`). A rule's assumption or decision is not something a woven
/// instance can check, so it stands until someone resolves it and deletes
/// the line; only the template assumptions are discharged here.
fn markers(text: &str, accepted: &[String]) -> Vec<Item> {
    let mut items = Vec::new();
    for (k, line) in text.lines().enumerate() {
        let l = line.trim_start();
        if l.starts_with("-- [not converted]") {
            items.push(Item {
                status: Outcome::Differs,
                rule: "not-converted".into(),
                side: "target",
                line: k + 1,
                text: l.trim_start_matches("-- ").to_string(),
            });
            continue;
        }
        let Some(rest) = l.strip_prefix("-- [sqldiff: ") else {
            continue;
        };
        let Some((status, rules)) = rest.split_once("] ") else {
            continue;
        };
        let status = match status {
            "under assumptions" => Outcome::Assumptions,
            "needs a decision" => Outcome::Decision,
            "differs" | "not converted" => Outcome::Differs,
            _ => continue,
        };
        let rules: Vec<&str> = rules
            .split(", ")
            .map(|r| r.split(':').next().unwrap_or("").trim())
            .filter(|r| !r.is_empty() && !DISCHARGED.contains(r))
            .collect();
        // Accepted assumptions drop out; a decision or difference stays.
        let rules: Vec<&str> = if status == Outcome::Assumptions {
            rules
                .into_iter()
                .filter(|r| !accepted.iter().any(|x| x == r))
                .collect()
        } else {
            rules
        };
        if rules.is_empty() {
            continue;
        }
        items.push(Item {
            status,
            rule: rules.join(", "),
            side: "target",
            line: k + 1,
            text: if status == Outcome::Assumptions {
                "the converted template marks this unit under assumptions: accept them with --accept, or resolve them and delete the line".to_string()
            } else {
                format!(
                    "the converted template marks this unit {}: resolve it and delete the line",
                    status.label()
                )
            },
        });
    }
    items
}

/// The first line where two texts differ, ignoring trailing white space on
/// each line and at the end; `None` when they are the same.
fn first_difference(a: &str, b: &str) -> Option<usize> {
    let norm = |s: &str| -> Vec<String> {
        let mut v: Vec<String> = s.lines().map(|l| l.trim_end().to_string()).collect();
        while v.last().is_some_and(|l| l.is_empty()) {
            v.pop();
        }
        v
    };
    let (x, y) = (norm(a), norm(b));
    (0..x.len().max(y.len()))
        .find(|&k| x.get(k) != y.get(k))
        .map(|k| k + 1)
}

/// Files under `dir` (hidden entries skipped), relative to `root` with `/`.
fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let ty = e.file_type()?;
        if ty.is_dir() {
            walk(root, &e.path(), out)?;
        } else if ty.is_file() {
            let p = e.path();
            let rel = p.strip_prefix(root).unwrap_or(&p);
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// `sqldiff convert` writes its reports into the converted tree.
fn is_report(rel: &str) -> bool {
    matches!(rel, "conversion-report.json" | "conversion-report.md")
}

fn prepare_out(a: &InstancesArgs) -> Result<(), String> {
    std::fs::create_dir_all(&a.out).map_err(|e| format!("{}: {e}", a.out.display()))?;
    let out = std::fs::canonicalize(&a.out).map_err(|e| format!("{}: {e}", a.out.display()))?;
    for d in [Some(&a.source), Some(&a.target), a.woven.as_ref()]
        .into_iter()
        .flatten()
    {
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
        "{} instances, {} templates, {} → {}",
        r.instances.len(),
        r.templates.len(),
        r.from,
        r.to
    );
    for o in ORDER {
        let _ = writeln!(
            s,
            "{:<20} {:>6}",
            o.label(),
            r.totals.get(o.label()).copied().unwrap_or(0)
        );
    }
    if !r.not_exercised.is_empty() {
        let _ = writeln!(
            s,
            "{:<20} {:>6} templates",
            "not exercised",
            r.not_exercised.len()
        );
    }
    if !r.not_in_manifest.is_empty() {
        let _ = writeln!(
            s,
            "{:<20} {:>6} files",
            "not in manifest",
            r.not_in_manifest.len()
        );
    }
    s
}

fn markdown(r: &Report) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# Instance report\n");
    let _ = writeln!(
        s,
        "Manifest `{}`: source templates `{}` ({}), converted templates `{}` ({}){}.\n",
        r.manifest,
        r.source,
        r.from,
        r.target,
        r.to,
        r.woven
            .as_ref()
            .map(|w| format!(", generator output `{w}`"))
            .unwrap_or_default()
    );
    let _ = writeln!(
        s,
        "An instance counts as converted only when its target weave parses, nothing in it fails the output check, the engine's diff against the source weave finds no difference, and every name a value is part of is one the target accepts. A template is as good as the worst instance that uses it.\n"
    );
    let _ = writeln!(s, "```\n{}```\n", summary(r));
    if !r.accepted.is_empty() {
        let _ = writeln!(s, "Accepted assumptions: {}.\n", r.accepted.join(", "));
    }
    if !r.not_in_manifest.is_empty() {
        let _ = writeln!(s, "## Generator output no instance accounts for\n");
        for f in &r.not_in_manifest {
            let _ = writeln!(s, "- `{f}`");
        }
        s.push('\n');
    }
    let _ = writeln!(
        s,
        "## Templates\n\n| status | instances | template |\n|---|---:|---|"
    );
    for t in &r.templates {
        let _ = writeln!(
            s,
            "| {} | {} | `{}` |",
            t.status.label(),
            t.instances,
            t.path
        );
    }
    for t in &r.not_exercised {
        let _ = writeln!(s, "| not exercised | 0 | `{t}` |");
    }
    let _ = writeln!(s, "\n## Instances\n");
    let mut inst: Vec<&InstanceReport> = r.instances.iter().collect();
    inst.sort_by(|x, y| y.status.cmp(&x.status).then(x.name.cmp(&y.name)));
    for i in inst {
        let _ = writeln!(s, "### `{}`: {}\n", i.name, i.status.label());
        for it in &i.items {
            let at = if it.line > 0 {
                format!("{} line {}", it.side, it.line)
            } else {
                it.side.to_string()
            };
            let _ = writeln!(
                s,
                "- **{}** {} ({at}): {}",
                it.status.label(),
                it.rule,
                it.text.lines().next().unwrap_or("")
            );
        }
        s.push('\n');
    }
    s
}
