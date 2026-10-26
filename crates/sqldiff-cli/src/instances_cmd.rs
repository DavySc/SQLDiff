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
//! that is what makes the manifest complete. `--woven-target` does the same
//! for the target weave. `--generator-only` checks the generator's source
//! output against its target output, for a generator changed for the
//! target. An instance is done only when both sides were checked on the
//! generator's output.

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
use sqldiff_syntax::SyntaxKind;

use crate::convert_cmd::{catalog, decode, parallel};
use crate::source_cmd;

#[derive(ClapArgs, Clone)]
pub struct InstancesArgs {
    /// The instance manifest (TOML): every result the generator produces,
    /// with its templates and parameter values.
    pub(crate) manifest: PathBuf,
    /// The source templates (the tree the generator reads today).
    #[arg(long, value_name = "DIR", required_unless_present = "generator_only")]
    pub(crate) source: Option<PathBuf>,
    /// The converted templates (`sqldiff convert --template` output, or
    /// templates written by hand), with the same layout.
    #[arg(long, value_name = "DIR", required_unless_present = "generator_only")]
    pub(crate) target: Option<PathBuf>,
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
    /// The generator's own output for the target dialect: each instance's
    /// target weave must equal its `output` file there. An instance with no
    /// file there is checked on SQLDiff's weave only and never done.
    #[arg(long, value_name = "DIR")]
    pub(crate) woven_target: Option<PathBuf>,
    /// Check the generator's output alone, when the generator was changed
    /// for the target: each instance's `output` under --woven against the
    /// same file under --woven-target. No templates are woven.
    #[arg(
        long,
        requires_all = ["woven", "woven_target"],
        conflicts_with_all = ["source", "target"]
    )]
    pub(crate) generator_only: bool,
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
    /// `source` or `target` instance; `manifest` for weaving; `pair` for
    /// the diff of the two as a whole.
    side: &'static str,
    /// 1-based line in the woven file, 0 when it has none.
    line: usize,
    text: String,
}

#[derive(Serialize)]
struct InstanceReport {
    name: String,
    status: Outcome,
    /// Converted (under assumptions at worst) and checked on the
    /// generator's own output on both sides.
    done: bool,
    /// Sides checked on SQLDiff's weave only, because no generator output
    /// was given for them (`--woven`, `--woven-target`): never done.
    weave_only: Vec<&'static str>,
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
    /// Every instance that uses it is done.
    done: bool,
    instances: usize,
    /// Instances that use it checked on SQLDiff's weave only.
    weave_only: usize,
}

#[derive(Serialize)]
struct Report {
    from: String,
    to: String,
    manifest: String,
    /// The generator's output checked alone (`--generator-only`).
    generator_only: bool,
    source: Option<String>,
    target: Option<String>,
    woven: Option<String>,
    woven_target: Option<String>,
    /// Assumptions accepted with --accept.
    accepted: Vec<String>,
    instances: Vec<InstanceReport>,
    /// Converted templates, worst instance status first.
    templates: Vec<TemplateReport>,
    /// Target templates no instance uses: never checked.
    not_exercised: Vec<String>,
    /// Generator output no instance accounts for.
    not_in_manifest: Vec<String>,
    /// Generator target output no instance accounts for.
    not_in_manifest_target: Vec<String>,
    totals: BTreeMap<&'static str, usize>,
    /// Instances done: converted and checked on the generator's output.
    done: usize,
    /// Instances checked on SQLDiff's weave only.
    weave_only: usize,
    /// Every instance done, every converted template exercised and all
    /// generator output accounted for.
    complete: bool,
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
    let def = cfg.policy.template.as_ref();
    if def.is_none() && !a.generator_only {
        return Err(
            "instances needs a [template] syntax: give --pack or a sqldiff.toml with one".into(),
        );
    }
    let manifest = Manifest::load(&a.manifest).map_err(|e| e.0)?;
    let kinds: BTreeMap<String, sqldiff_convert::ValueKind> = def
        .map(|def| {
            def.values
                .iter()
                .filter_map(|(n, k)| {
                    let n = if def.ignore_case {
                        n.to_uppercase()
                    } else {
                        n.clone()
                    };
                    Some((n, sqldiff_convert::ValueKind::parse(k)?))
                })
                .collect()
        })
        .unwrap_or_default();
    if manifest.instances.is_empty() {
        return Err(format!("{}: no [[instance]]", a.manifest.display()));
    }
    for d in [&a.source, &a.target, &a.woven, &a.woven_target]
        .into_iter()
        .flatten()
    {
        if !d.is_dir() {
            return Err(format!("{}: not a directory", d.display()));
        }
    }
    // The trees each side's catalog comes from: the templates, or the
    // generator's output when no templates are woven.
    let (source_root, target_root) = match (&a.source, &a.target, &a.woven, &a.woven_target) {
        (Some(s), Some(t), _, _) => (s.clone(), t.clone()),
        (_, _, Some(s), Some(t)) if a.generator_only => (s.clone(), t.clone()),
        _ => return Err("give --source and --target, or --generator-only".into()),
    };
    prepare_out(&a)?;
    let jobs = a
        .jobs
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
        .max(1);

    // The generator's own output, when given.
    let read = |dir: &Option<PathBuf>| match dir {
        Some(d) => read_generated(d, &manifest),
        None => Ok((Vec::new(), Vec::new())),
    };
    let (gen_source, not_in_manifest) = read(&a.woven)?;
    let (gen_target, not_in_manifest_target) = read(&a.woven_target)?;

    let woven: Vec<(Woven, Woven)> = match def {
        Some(def) if !a.generator_only => {
            let placeholders = compose::Placeholders::new(def).map_err(|e| e.0)?;
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
            let (ws, wt) = (weaver(&source_root, a.from), weaver(&target_root, a.to));
            parallel(jobs, &manifest.instances, |i| {
                (ws.weave(&manifest, i), wt.weave(&manifest, i))
            })
        }
        // The generator's output is the instance itself.
        _ => manifest
            .instances
            .iter()
            .enumerate()
            .map(|(k, i)| {
                (
                    from_generator(i, &gen_source[k], "source"),
                    from_generator(i, &gen_target[k], "target"),
                )
            })
            .collect(),
    };

    // Catalogs: every woven instance on its side, plus any given.
    let mut opts = Options::default();
    a.catalogs.apply(&mut opts, a.from, Some(a.to))?;
    opts.facts_a.push(Arc::new(catalog(
        woven.iter().map(|(s, _)| s.text.as_str()),
        a.from,
        &source_root,
        None,
    )));
    opts.facts_b.push(Arc::new(catalog(
        woven.iter().map(|(_, t)| t.text.as_str()),
        a.to,
        &target_root,
        None,
    )));

    // The name values each instance gives the target, for the name checks
    // of the generator's own output.
    let ci = def.is_some_and(|d| d.ignore_case);
    let name_values: Vec<Vec<String>> = manifest
        .instances
        .iter()
        .map(|i| {
            let mut v: BTreeSet<String> = BTreeSet::new();
            for p in &i.parts {
                for (n, x) in manifest.values(i, p, a.to, ci) {
                    let kind = kinds
                        .get(&n)
                        .copied()
                        .unwrap_or(sqldiff_convert::ValueKind::Name);
                    if kind == sqldiff_convert::ValueKind::Name && !x.is_empty() {
                        v.insert(x);
                    }
                }
            }
            v.into_iter().collect()
        })
        .collect();
    let work: Vec<(usize, &Instance)> = manifest.instances.iter().enumerate().collect();
    let generated = |k: usize| {
        if a.generator_only {
            // Nothing to compare with: the checked text is the generator's.
            (None, None)
        } else {
            (gen_source.get(k), gen_target.get(k))
        }
    };
    let mut results: Vec<Checked> = parallel(jobs, &work, |&(k, i)| {
        let (s, t) = &woven[k];
        let (gs, gt) = generated(k);
        check_instance(
            i,
            s,
            t,
            gs,
            gt,
            a.from,
            a.to,
            &opts,
            &a.accept,
            &kinds,
            a.generator_only,
            &name_values[k],
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

    // An object two target instances create: the catalog holds one of the
    // definitions, so what binds against it is not decided.
    let mut creators: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (k, (_, t)) in woven.iter().enumerate() {
        if !t.problems.is_empty() {
            continue;
        }
        let mut c = sqldiff_core::catalog::Catalog::default();
        let root = sqldiff_syntax::parse(&t.text, a.to).syntax();
        sqldiff_core::catalog::ddl::load_tree(&mut c, &root, a.to);
        let full = |schema: &Option<String>, name: &str| {
            format!("{}.{name}", schema.as_deref().unwrap_or("")).to_ascii_lowercase()
        };
        let mut mine: BTreeSet<String> = BTreeSet::new();
        mine.extend(c.tables.iter().map(|t| full(&t.schema, &t.name)));
        mine.extend(
            c.routines
                .iter()
                .map(|r| format!("routine {}", full(&r.schema, &r.name))),
        );
        for n in mine {
            creators.entry(n).or_default().push(k);
        }
    }
    for (name, ks) in creators.iter().filter(|(_, ks)| ks.len() > 1) {
        let all: Vec<String> = ks
            .iter()
            .map(|&k| format!("`{}`", manifest.instances[k].name))
            .collect();
        for &k in ks {
            results[k].0.push(Item {
                status: Outcome::Unknown,
                rule: "weave.object-twice".into(),
                side: "target",
                line: 0,
                text: format!(
                    "`{}` is created by instances {}: the later one fails or replaces the other, and binding against it used one of the definitions",
                    name.trim_start_matches("routine ").trim_start_matches('.'),
                    all.join(", ")
                ),
            });
        }
    }

    // Woven files and the per-instance report.
    let mut instances = Vec::new();
    for (k, ((i, (s, t)), (items, engine, _))) in manifest
        .instances
        .iter()
        .zip(&woven)
        .zip(results)
        .enumerate()
    {
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
        // A side counts as the generator's only when its text is the
        // generator's output (equal to it, or read from it).
        let has = |g: &[Generated]| matches!(g.get(k), Some(Generated::Text(_)));
        let weave_only: Vec<&'static str> = [("source", &gen_source), ("target", &gen_target)]
            .into_iter()
            .filter(|(_, g)| !has(g))
            .map(|(side, _)| side)
            .collect();
        instances.push(InstanceReport {
            name: i.name.clone(),
            status,
            done: status <= Outcome::Assumptions && weave_only.is_empty(),
            weave_only,
            source_file: sf,
            target_file: tf,
            templates: t.templates.iter().cloned().collect(),
            items,
            engine_assumptions: engine,
        });
    }

    // Templates: the worst instance that uses them.
    let mut by_template: BTreeMap<&str, (Outcome, usize, usize)> = BTreeMap::new();
    for r in &instances {
        for t in &r.templates {
            let e = by_template.entry(t).or_insert((Outcome::Converted, 0, 0));
            e.0 = e.0.max(r.status);
            e.1 += 1;
            e.2 += usize::from(!r.weave_only.is_empty());
        }
    }
    // Without a converted tree there is nothing to list as not exercised.
    let mut targets = Vec::new();
    if let Some(dir) = &a.target {
        walk(dir, dir, &mut targets).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let not_exercised: Vec<String> = targets
        .into_iter()
        .filter(|f| !by_template.contains_key(f.as_str()) && !is_report(f))
        .collect();
    let mut templates: Vec<TemplateReport> = by_template
        .into_iter()
        .map(|(p, (status, n, weave_only))| TemplateReport {
            path: p.to_string(),
            status,
            done: status <= Outcome::Assumptions && weave_only == 0,
            instances: n,
            weave_only,
        })
        .collect();
    templates.sort_by(|x, y| y.status.cmp(&x.status).then(x.path.cmp(&y.path)));

    let mut totals: BTreeMap<&'static str, usize> = ORDER.iter().map(|o| (o.label(), 0)).collect();
    for r in &instances {
        *totals.entry(r.status.label()).or_default() += 1;
    }
    let done = instances.iter().filter(|r| r.done).count();
    let weave_only = instances
        .iter()
        .filter(|r| !r.weave_only.is_empty())
        .count();
    let complete = done == instances.len()
        && not_in_manifest.is_empty()
        && not_in_manifest_target.is_empty()
        && not_exercised.is_empty();
    let shown = |p: &Option<PathBuf>| p.as_ref().map(|w| w.display().to_string());
    let report = Report {
        from: a.from.id().to_string(),
        to: a.to.id().to_string(),
        manifest: a.manifest.display().to_string(),
        generator_only: a.generator_only,
        source: shown(&a.source),
        target: shown(&a.target),
        woven: shown(&a.woven),
        woven_target: shown(&a.woven_target),
        accepted: a.accept.clone(),
        instances,
        templates,
        not_exercised,
        not_in_manifest,
        not_in_manifest_target,
        totals,
        done,
        weave_only,
        complete,
    };
    let json = a.out.join("instance-report.json");
    std::fs::write(&json, serde_json::to_string_pretty(&report).unwrap() + "\n")
        .map_err(|e| format!("{}: {e}", json.display()))?;
    let md = a.out.join("instance-report.md");
    std::fs::write(&md, markdown(&report)).map_err(|e| format!("{}: {e}", md.display()))?;
    print!("{}", summary(&report));
    eprintln!("wrote {} and {}", json.display(), md.display());
    // Generator output given for the target must cover every instance.
    let ok = report
        .instances
        .iter()
        .all(|r| r.status <= Outcome::Assumptions)
        && report.not_in_manifest.is_empty()
        && report.not_in_manifest_target.is_empty()
        && (a.woven_target.is_none()
            || report
                .instances
                .iter()
                .all(|r| !r.weave_only.contains(&"target")));
    Ok((ok, report.totals))
}

/// The name tokens of `text` that hold one of `values`, ignoring letter
/// case (the generator may fold names).
fn value_tokens(text: &str, d: Dialect, values: &[String]) -> Vec<(usize, usize)> {
    let values: Vec<String> = values.iter().map(|v| v.to_lowercase()).collect();
    let root = sqldiff_syntax::parse(text, d).syntax();
    root.descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| matches!(t.kind(), SyntaxKind::IDENT | SyntaxKind::QUOTED_IDENT))
        .filter(|t| {
            let low = t.text().to_lowercase();
            values.iter().any(|v| low.contains(v.as_str()))
        })
        .map(|t| {
            let r = t.text_range();
            (usize::from(r.start()), usize::from(r.end()))
        })
        .collect()
}

/// A generator output file of one instance.
#[derive(Debug, Clone)]
enum Generated {
    /// No file at the instance's `output` path.
    Missing,
    /// The file could not be read, or is not text.
    Unreadable(String),
    Text(String),
}

/// Every instance's `output` file under `dir`, and the files there no
/// instance names.
fn read_generated(dir: &Path, m: &Manifest) -> Result<(Vec<Generated>, Vec<String>), String> {
    let wanted: BTreeSet<&str> = m.instances.iter().map(|i| i.output.as_str()).collect();
    let mut files = Vec::new();
    walk(dir, dir, &mut files).map_err(|e| format!("{}: {e}", dir.display()))?;
    let extra = files
        .into_iter()
        .filter(|f| !wanted.contains(f.as_str()))
        .collect();
    let read = m
        .instances
        .iter()
        .map(|i| match std::fs::read(dir.join(&i.output)) {
            Ok(b) => decode(&b).map_or_else(
                || Generated::Unreadable(format!("{}: not UTF-8 or UTF-16 text", i.output)),
                Generated::Text,
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Generated::Missing,
            Err(e) => Generated::Unreadable(format!("{}: {e}", i.output)),
        })
        .collect();
    Ok((read, extra))
}

/// An instance as the generator wrote it, for `--generator-only`: its
/// templates are the ones the manifest names (the generator's includes are
/// not known).
fn from_generator(i: &Instance, g: &Generated, side: &str) -> Woven {
    let mut w = Woven {
        templates: i.parts.iter().map(|p| p.template.clone()).collect(),
        ..Woven::default()
    };
    match g {
        Generated::Text(t) => w.text = t.clone(),
        Generated::Missing => w.problems.push(format!(
            "the generator's {side} output `{}` does not exist",
            i.output
        )),
        Generated::Unreadable(e) => w.problems.push(format!(
            "the generator's {side} output could not be read: {e}"
        )),
    }
    w
}

/// An instance's items, the engine's assumptions about the whole pair, and
/// the PostgreSQL names over the limit, as (truncated, full).
type Checked = (Vec<Item>, Vec<String>, Vec<(String, String)>);

/// Assumptions the engine's diff makes about every pair of a conversion:
/// names fold the same way, the default schemas are the same one, T-SQL's
/// ownership chain. Listed per instance, not counted.
const EVERY_PAIR: [&str; 4] = [
    "ASM-IDENT-FOLD",
    "ASM-DEFAULT-SCHEMA",
    "ASM-DBO-SCHEMA",
    "ASM-TSQL-OWNERSHIP-CHAIN",
];

/// How the engine's diff says it skipped a `CREATE TABLE`.
const DDL_TABLE: &str = "a DDL statement `CREATE TABLE";

/// Schemas a dialect resolves an unqualified name in by default, as the
/// engine's own default-schema assumptions read them: `dbo` on SQL Server,
/// `public` on PostgreSQL. Db2 for i has none fixed (the current schema).
fn default_schema(d: Dialect) -> Option<&'static str> {
    match d {
        Dialect::Tsql => Some("dbo"),
        Dialect::Postgres => Some("public"),
        Dialect::Db2i => None,
    }
}

/// A significant token: its kind and its text as compared.
type Token = (SyntaxKind, String);

/// The significant tokens of `text`: identifiers and keywords in lower
/// case (unquoted names fold), quoted names as written, no `;`.
fn tokens(text: &str, d: Dialect) -> Vec<Token> {
    let root = sqldiff_syntax::parse(text, d).syntax();
    root.descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| !t.kind().is_trivia() && t.kind() != SyntaxKind::SEMICOLON)
        .map(|t| {
            let text = if t.kind() == SyntaxKind::QUOTED_IDENT || t.kind() == SyntaxKind::STRING {
                t.text().to_string()
            } else {
                t.text().to_ascii_lowercase()
            };
            (t.kind(), text)
        })
        .collect()
}

/// Top-level `CREATE TABLE` statements of `text`, as byte ranges.
fn create_tables(text: &str, d: Dialect) -> Vec<(usize, usize)> {
    let root = sqldiff_syntax::parse(text, d).syntax();
    root.children()
        .filter(|n| {
            let mut sig = n
                .descendants_with_tokens()
                .filter_map(|e| e.into_token())
                .filter(|t| !t.kind().is_trivia());
            let first = sig.next().map(|t| t.text().to_ascii_uppercase());
            let second = sig.next().map(|t| t.text().to_ascii_uppercase());
            first.as_deref() == Some("CREATE") && second.as_deref() == Some("TABLE")
        })
        .map(|n| {
            let r = n.text_range();
            (usize::from(r.start()), usize::from(r.end()))
        })
        .collect()
}

/// A table statement split into its name (schema dropped when it is the
/// dialect's default) and everything after it.
fn table_shape(toks: &[Token], d: Dialect) -> Option<(Vec<String>, &[Token])> {
    // CREATE TABLE name ( ...
    let open = toks.iter().position(|(k, _)| *k == SyntaxKind::L_PAREN)?;
    let mut name: Vec<String> = toks[2..open]
        .iter()
        .filter(|(k, _)| *k != SyntaxKind::DOT)
        .map(|(_, t)| t.clone())
        .collect();
    if name.len() == 2 && default_schema(d).is_some_and(|s| name[0] == s) {
        name.remove(0);
    }
    Some((name, &toks[open..]))
}

/// The woven target's tables against what the conversion rules write from
/// the woven source's tables, in order. Equal tables are as good as
/// `sqldiff convert`'s own output: the conversion's assumptions become
/// items (default-schema ones go with the engine's), anything worse than an
/// assumption keeps the tables undecided. `Err` says why they are not
/// decided.
fn tables_by_rules(
    source: &str,
    target: &str,
    from: Dialect,
    to: Dialect,
    accepted: &[String],
) -> Result<(Vec<Item>, Vec<String>), String> {
    let (st, tt) = (create_tables(source, from), create_tables(target, to));
    if st.len() != tt.len() {
        return Err(format!(
            "the source instance creates {} tables and the target {}",
            st.len(),
            tt.len()
        ));
    }
    let conv = sqldiff_convert::convert(source, from, to)?;
    let mut items = Vec::new();
    let mut engine = Vec::new();
    let lines = sqldiff_syntax::LineIndex::new(target);
    for (k, (&(ss, se), &(ts, te))) in st.iter().zip(&tt).enumerate() {
        let line = lines.line_col(ts).0;
        let units: Vec<&sqldiff_convert::Unit> = conv
            .units
            .iter()
            .filter(|u| u.src.start < se && ss < u.src.end)
            .collect();
        let [u] = units.as_slice() else {
            return Err(format!(
                "table {} (target line {line}): the conversion does not give it as one statement",
                k + 1
            ));
        };
        if u.status > sqldiff_convert::Status::Assumptions {
            return Err(format!(
                "table {} (target line {line}): the conversion rules do not convert the source table ({})",
                k + 1,
                u.status.label()
            ));
        }
        let want = tokens(&conv.output[u.out.start..u.out.end], to);
        let have = tokens(&target[ts..te], to);
        let same = match (table_shape(&want, to), table_shape(&have, to)) {
            (Some((wn, wb)), Some((hn, hb))) => wn == hn && wb == hb,
            _ => false,
        };
        if !same {
            return Err(format!(
                "table {} (target line {line}) is not the table the conversion rules write from the source table: `{}`",
                k + 1,
                conv.output[u.out.start..u.out.end].trim()
            ));
        }
        for n in &u.notes {
            if n.status != sqldiff_convert::Status::Assumptions {
                continue;
            }
            if n.rule == "ASM-DBO-SCHEMA" {
                if !engine.contains(&n.rule) {
                    engine.push(n.rule.clone());
                }
            } else if !accepted.contains(&n.rule) {
                items.push(Item {
                    status: Outcome::Assumptions,
                    rule: n.rule.clone(),
                    side: "target",
                    line,
                    text: n.text.clone(),
                });
            }
        }
    }
    Ok((items, engine))
}

/// Checks one instance; also returns the PostgreSQL names its values make
/// that are over the limit, as (truncated, full), for the collision check.
#[allow(clippy::too_many_arguments)]
fn check_instance(
    i: &Instance,
    s: &Woven,
    t: &Woven,
    gen_source: Option<&Generated>,
    gen_target: Option<&Generated>,
    from: Dialect,
    to: Dialect,
    opts: &Options,
    accepted: &[String],
    kinds: &BTreeMap<String, sqldiff_convert::ValueKind>,
    generator_only: bool,
    name_values: &[String],
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
    // Each weave against the generator's own output for its side. No
    // source output is a missing file of the manifest; no target output
    // leaves the instance checked on SQLDiff's weave only.
    for (side, woven, g, dir) in [
        ("source", s, gen_source, "--woven"),
        ("target", t, gen_target, "--woven-target"),
    ] {
        match g {
            Some(Generated::Text(g)) => {
                if let Some(line) = first_difference(&woven.text, g) {
                    items.push(Item {
                        status: Outcome::NotWoven,
                        rule: if side == "source" {
                            "weave.generator-mismatch".into()
                        } else {
                            "weave.generator-target-mismatch".into()
                        },
                        side,
                        line,
                        text: format!(
                            "the {side} weave differs from the generator's output `{}` ({dir}) from line {line}: {}",
                            i.output,
                            if side == "source" {
                                "the manifest or the weaver does not match the generator"
                            } else {
                                "the generator does not weave the converted templates as SQLDiff does; check its output alone with --generator-only"
                            }
                        ),
                    });
                }
            }
            Some(Generated::Unreadable(e)) => items.push(Item {
                status: Outcome::NotWoven,
                rule: "weave.generator-output".into(),
                side,
                line: 0,
                text: format!(
                    "the generator's {side} output for this instance could not be read: {e}"
                ),
            }),
            Some(Generated::Missing) if side == "source" => items.push(Item {
                status: Outcome::NotWoven,
                rule: "weave.generator-output".into(),
                side,
                line: 0,
                text: format!(
                    "the generator's output for this instance could not be read: {}: no such file",
                    i.output
                ),
            }),
            Some(Generated::Missing) | None => {}
        }
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
    // What the diff could not compare at all (DDL, say) is not decided
    // either: no finding about it is no evidence.
    // Tables the engine cannot compare count only when the target table is
    // the one the conversion rules write from the source table.
    // The assumptions the diff makes about every pair do not count, as in
    // `convert`; one about this pair (argument lengths, say) does.
    let mut engine: Vec<String> = Vec::new();
    for x in &report.assumptions {
        if EVERY_PAIR.contains(&x.id.as_str()) {
            engine.push(x.id.clone());
        } else if !accepted.contains(&x.id) {
            items.push(Item {
                status: Outcome::Assumptions,
                rule: x.id.clone(),
                side: "pair",
                line: 0,
                text: x.text.clone(),
            });
        }
    }
    let tables = report
        .unknown_reasons
        .iter()
        .any(|r| r.contains(DDL_TABLE))
        .then(|| tables_by_rules(&s.text, &t.text, from, to, accepted));
    match &tables {
        Some(Ok((found, asm))) => {
            items.extend(found.iter().cloned());
            for a in asm {
                if !engine.contains(a) {
                    engine.push(a.clone());
                }
            }
        }
        Some(Err(why)) => items.push(Item {
            status: Outcome::Unknown,
            rule: "ddl.not-as-converted".into(),
            side: "target",
            line: 0,
            text: why.clone(),
        }),
        None => {}
    }
    for r in &report.unknown_reasons {
        if r.contains(DDL_TABLE) && matches!(tables, Some(Ok(_))) {
            continue;
        }
        items.push(Item {
            status: Outcome::Unknown,
            rule: "equiv.not-covered".into(),
            side: "pair",
            line: 0,
            text: r.clone(),
        });
    }
    if report.verdict == Verdict::Unknown
        && report.unknown_reasons.is_empty()
        && !items.iter().any(|x| x.status >= Outcome::Unknown)
    {
        items.push(Item {
            status: Outcome::Unknown,
            rule: "equiv.unknown".into(),
            side: "pair",
            line: 0,
            text: "the engine's diff of the source and target instances is UNKNOWN".into(),
        });
    }
    // Names: only values that are names. Dialect code given once for every
    // dialect is used unchanged in the target.
    let kind = |n: &str| {
        kinds
            .get(n)
            .copied()
            .unwrap_or(sqldiff_convert::ValueKind::Name)
    };
    // The generator's own output has no value positions: every name token
    // that holds a name value is checked (a keyword that holds one, too,
    // which can only make an instance worse, never better).
    let spans: Vec<(usize, usize)> = if generator_only {
        value_tokens(&t.text, to, name_values)
    } else {
        t.values
            .iter()
            .filter(|v| kind(&v.name) == sqldiff_convert::ValueKind::Name)
            .map(|v| (v.start, v.end))
            .collect()
    };
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
    let mut said = BTreeSet::new();
    for n in sqldiff_convert::names::check_values(&t.text, to, &spans) {
        if let Some(cut) = &n.truncated {
            names.push(cut.clone());
        }
        // Every name in the generator's output: each finding once, at its
        // first line.
        if generator_only && !said.insert((n.rule, n.text.clone())) {
            continue;
        }
        items.push(Item {
            status: match n.status {
                sqldiff_convert::Status::Converted => Outcome::Converted,
                sqldiff_convert::Status::Assumptions => Outcome::Assumptions,
                sqldiff_convert::Status::Decision => Outcome::Decision,
                sqldiff_convert::Status::Unproven => Outcome::Unknown,
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
            // The engine's diff of the template could not decide the unit;
            // the diff of each woven instance below decides it again, and
            // counts every unknown it finds.
            "not proven" => continue,
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
    for d in [&a.source, &a.target, &a.woven, &a.woven_target]
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
    if !r.not_in_manifest_target.is_empty() {
        let _ = writeln!(
            s,
            "{:<20} {:>6} target files",
            "not in manifest",
            r.not_in_manifest_target.len()
        );
    }
    let _ = writeln!(
        s,
        "{:<20} {:>6} of {} (checked on the generator's output; {} on SQLDiff's weave only)",
        "done",
        r.done,
        r.instances.len(),
        r.weave_only
    );
    s
}

fn markdown(r: &Report) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# Instance report\n");
    let given = |what: &str, p: &Option<String>| {
        p.as_ref()
            .map(|p| format!(", {what} `{p}`"))
            .unwrap_or_default()
    };
    let _ = writeln!(
        s,
        "Manifest `{}`{}{}{}{}, {} → {}.\n",
        r.manifest,
        given("source templates", &r.source),
        given("converted templates", &r.target),
        given("generator output", &r.woven),
        given("generator target output", &r.woven_target),
        r.from,
        r.to
    );
    if r.generator_only {
        let _ = writeln!(
            s,
            "Generator only: each instance is the generator's source output against its target output; no template was woven, and a template is known only where the manifest names it (not the generator's includes).\n"
        );
    }
    let _ = writeln!(
        s,
        "An instance counts as converted only when its target weave parses, nothing in it fails the output check, the engine's diff against the source weave finds no difference, and every name a value is part of is one the target accepts. It is done only when both sides were also checked on the generator's own output: one checked on SQLDiff's weave only is never done. A template is as good as the worst instance that uses it.\n"
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
    if !r.not_in_manifest_target.is_empty() {
        let _ = writeln!(s, "## Generator target output no instance accounts for\n");
        for f in &r.not_in_manifest_target {
            let _ = writeln!(s, "- `{f}`");
        }
        s.push('\n');
    }
    let _ = writeln!(
        s,
        "## Templates\n\n| status | instances | on SQLDiff's weave only | template |\n|---|---:|---:|---|"
    );
    for t in &r.templates {
        let _ = writeln!(
            s,
            "| {} | {} | {} | `{}` |",
            t.status.label(),
            t.instances,
            t.weave_only,
            t.path
        );
    }
    for t in &r.not_exercised {
        let _ = writeln!(s, "| not exercised | 0 | 0 | `{t}` |");
    }
    let _ = writeln!(s, "\n## Instances\n");
    let mut inst: Vec<&InstanceReport> = r.instances.iter().collect();
    inst.sort_by(|x, y| y.status.cmp(&x.status).then(x.name.cmp(&y.name)));
    for i in inst {
        let only = if i.weave_only.is_empty() {
            String::new()
        } else {
            format!(
                ", checked on SQLDiff's weave only (no generator {} output)",
                i.weave_only.join(" or ")
            )
        };
        let _ = writeln!(s, "### `{}`: {}{only}\n", i.name, i.status.label());
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
