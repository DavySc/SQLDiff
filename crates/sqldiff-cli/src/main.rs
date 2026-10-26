//! `sqldiff` command-line interface.
//!
//! Exit codes: 0 equal (or equal under assumptions), 1 different or
//! unknown, 2 usage or I/O error.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use sqldiff_core::{render, Artifact, Dialect, Report, Verdict};
use sqldiff_syntax::template::{ExpandProblem, SegmentKind};

use sqldiff_source::{compose, tree, Resolved};
use sqldiff_syntax::ast;

mod changes_cmd;
mod convert_cmd;
mod deny_hash;
mod dist_cmd;
mod queue_cmd;
mod review_cmd;
mod source_cmd;

use review_cmd::{Pipeline, ReviewArgs};

#[derive(Parser)]
#[command(
    name = "sqldiff",
    version,
    about = "Semantic diff for T-SQL, PostgreSQL and Db2 for i"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Text,
    Json,
    Sarif,
}

#[derive(Subcommand)]
enum Command {
    /// Compare two artifacts (versions of one object, or one object in two dialects).
    ///
    /// Without a dialect flag, a side's dialect comes from `dialect_map` or
    /// the path convention (`tsql/x.sql`, `x.pg.sql`).
    Diff {
        a: PathBuf,
        b: PathBuf,
        /// Dialect of both sides (overridden by --dialect-a / --dialect-b).
        #[arg(short, long)]
        dialect: Option<Dialect>,
        #[arg(long)]
        dialect_a: Option<Dialect>,
        #[arg(long)]
        dialect_b: Option<Dialect>,
        #[arg(short, long, value_enum, default_value_t = Format::Text)]
        format: Format,
        /// Maximum excerpt lines per location (default: config, else 3).
        #[arg(long)]
        excerpt_lines: Option<usize>,
        #[command(flatten)]
        catalogs: source_cmd::CatalogArgs,
        /// Check mode: templates by name (file stem) for the composed parts;
        /// each generated part is compared with its substituted template.
        #[arg(long, value_name = "DIR")]
        templates: Option<PathBuf>,
        /// Extra parameter value for check mode (source `cli`): NAME=VALUE.
        #[arg(long = "param", value_name = "NAME=VALUE", value_parser = source_cmd::key_value, requires = "templates")]
        params: Vec<(String, String)>,
        /// Both sides are templates in the pack's `[template]` syntax: diff
        /// their SQL fragments, with locations in the templates.
        #[arg(long, conflicts_with = "templates")]
        template: bool,
        /// With --template: a parameter name of the templates (repeatable).
        /// Then only these and the pack's `known_parameters` are placeholders;
        /// any other delimited name is SQL, e.g. a quoted `[name]` in T-SQL.
        #[arg(long = "parameter", value_name = "NAME", requires = "template")]
        parameters: Vec<String>,
        #[command(flatten)]
        flips: queue_cmd::FlipArgs,
        #[command(flatten)]
        cfg: source_cmd::ConfigArgs,
        #[command(flatten)]
        review: ReviewArgs,
    },
    /// Lint one artifact: residue from other dialects, terminator hazards,
    /// parse problems and pack limits.
    Check {
        file: PathBuf,
        /// Dialect (default: from `dialect_map` or the path convention).
        #[arg(short, long)]
        dialect: Option<Dialect>,
        #[arg(short, long, value_enum, default_value_t = Format::Text)]
        format: Format,
        /// Check mode for composed output: templates by name (file stem).
        #[arg(long, value_name = "DIR")]
        templates: Option<PathBuf>,
        /// Extra parameter value for check mode (source `cli`): NAME=VALUE.
        #[arg(long = "param", value_name = "NAME=VALUE", value_parser = source_cmd::key_value, requires = "templates")]
        params: Vec<(String, String)>,
        #[command(flatten)]
        catalogs: source_cmd::CatalogArgs,
        #[command(flatten)]
        cfg: source_cmd::ConfigArgs,
        #[command(flatten)]
        review: ReviewArgs,
    },
    /// Read a template with the pack's `[template]` syntax: list its
    /// placeholders and special forms, parse its SQL fragment and verify the
    /// round trip; with --render, print the output for the --param values.
    Template {
        file: PathBuf,
        #[arg(short, long)]
        dialect: Option<Dialect>,
        /// Parameter value: NAME=VALUE. Repeatable. With --known-only,
        /// placeholder candidates that name no parameter are kept as SQL and
        /// reported as unresolved (for syntaxes that collide with SQL).
        #[arg(long = "param", value_name = "NAME=VALUE", value_parser = source_cmd::key_value)]
        params: Vec<(String, String)>,
        /// Resolve placeholder candidates against the --param names.
        #[arg(long)]
        known_only: bool,
        /// Print the substituted output instead of the summary.
        #[arg(long)]
        render: bool,
        #[command(flatten)]
        cfg: source_cmd::ConfigArgs,
    },
    /// Parse one artifact, verify the lossless round trip and optionally dump the tree.
    Parse {
        file: PathBuf,
        #[arg(short, long)]
        dialect: Dialect,
        #[arg(long)]
        tree: bool,
    },
    /// Collection drift report: pair the SQL files of two trees by pack
    /// pairing rules and diff each pair.
    Tree {
        dir_a: PathBuf,
        dir_b: PathBuf,
        /// Dialect of every file in DIR_A (default: per file, from `dialect_map` or the path convention).
        #[arg(long)]
        dialect_a: Option<Dialect>,
        #[arg(long)]
        dialect_b: Option<Dialect>,
        #[arg(short, long, value_enum, default_value_t = Format::Text)]
        format: Format,
        #[command(flatten)]
        cfg: source_cmd::ConfigArgs,
        #[command(flatten)]
        review: ReviewArgs,
    },
    /// Convert a source tree (or one file) into another dialect: an output
    /// tree with the same layout, checked statement by statement, and a
    /// report (`conversion-report.json` and `.md` in the output directory).
    ///
    /// A unit counts as converted only when its rules needed no assumption
    /// and neither the engine's diff nor the output's own check (binding,
    /// leftover source syntax, target limits) finds anything in it. Every
    /// other unit gets a `-- [sqldiff: <status>]` comment line in the output.
    /// Exit code 0 when every file is converted or converted under
    /// assumptions, 1 otherwise.
    Convert {
        src: PathBuf,
        /// Output directory (created; must be empty unless --overwrite).
        #[arg(short, long)]
        out: PathBuf,
        #[arg(long, default_value = "tsql")]
        from: Dialect,
        #[arg(long)]
        to: Dialect,
        /// Parallel jobs (default: available cores).
        #[arg(short, long)]
        jobs: Option<usize>,
        /// Replace files in a non-empty output directory.
        #[arg(long)]
        overwrite: bool,
        /// Check each file alone, without catalogs built from the whole tree.
        #[arg(long)]
        no_catalog: bool,
        /// Every file is a template in the `[template]` syntax of the loaded
        /// packs or config: the output tree holds templates for the target,
        /// with the placeholders and generator directives kept.
        #[arg(long)]
        template: bool,
        /// With --template: a parameter name (repeatable). Then only these
        /// and the pack's `known_parameters` are placeholders; any other
        /// delimited name is SQL, e.g. a quoted `[name]` in T-SQL.
        #[arg(long = "parameter", value_name = "NAME", requires = "template")]
        parameters: Vec<String>,
        /// File extension to convert (repeatable; default: sql). Matched in
        /// any letter case, without the dot: `--extension sql --extension tpl`.
        #[arg(long = "extension", value_name = "EXT")]
        extensions: Vec<String>,
        #[command(flatten)]
        cfg: source_cmd::ConfigArgs,
    },
    /// Review what a branch changed (the pull request workflow): diff every
    /// SQL file changed between the merge base of --base and --head, base
    /// against head. Files that did not change are not read. SARIF results
    /// are located at the head revision; --markdown writes the pull request
    /// comment.
    Changes(changes_cmd::ChangesArgs),
    /// Review decisions: accept, reject or comment on findings by fingerprint.
    #[command(subcommand)]
    Review(review_cmd::ReviewCommand),
    /// Assemble a ready-to-run folder for colleagues: the executables, a
    /// README and the private config with its packs in `config/`, which the
    /// executables load without setup. Never inside a git work tree.
    Dist(dist_cmd::DistArgs),
    #[command(flatten)]
    Source(source_cmd::SourceCommand),
    #[command(flatten)]
    Queue(queue_cmd::QueueCommand),
}

/// Reads an artifact. Dialect: the given one, else `dialect_map` or the path
/// convention.
fn read(path: &Path, dialect: Option<Dialect>, cfg: &Resolved) -> Result<Artifact, String> {
    tree::read(path, dialect, cfg).map_err(|e| e.0)
}

fn template_dir(
    dir: Option<&Path>,
    params: Vec<(String, String)>,
) -> Result<Option<compose::TemplateDir>, String> {
    dir.map(|d| compose::TemplateDir::new(d).map(|t| t.with_params(params.into_iter().collect())))
        .transpose()
        .map_err(|e| e.0)
}

fn template_cmd(
    art: &Artifact,
    params: &BTreeMap<String, String>,
    known_only: bool,
    render: bool,
    cfg: &Resolved,
) -> Result<ExitCode, String> {
    // The same placeholder rules as `diff --template` and pair diffs.
    let def = cfg
        .policy
        .template
        .as_ref()
        .ok_or("no [template] syntax in the loaded packs or config")?;
    let placeholders = compose::Placeholders::new(def).map_err(|e| e.0)?;
    let known: BTreeSet<String> = params.keys().cloned().collect();
    let (t, _) = placeholders.parse(&art.text, art.dialect, known_only.then_some(&known));
    if t.print() != art.text {
        return Err("round trip failed: template segments differ from input".into());
    }
    let lines = sqldiff_syntax::LineIndex::new(&art.text);
    let unresolved: Vec<_> = t.unresolved().collect();
    if render {
        let mut problems: Vec<(usize, String)> = unresolved
            .iter()
            .map(|s| {
                (
                    s.start,
                    format!("unresolved `{}`: not a parameter", t.slice(s)),
                )
            })
            .collect();
        match t.substitute(params) {
            Ok(out) if problems.is_empty() => {
                print!("{}", out.text);
                return Ok(ExitCode::SUCCESS);
            }
            Ok(_) => {}
            Err(e) => problems.extend(e.iter().map(|p| {
                let off = match p {
                    ExpandProblem::MissingParameter { offset, .. }
                    | ExpandProblem::SpecialForm { offset, .. } => *offset,
                };
                (off, p.to_string())
            })),
        }
        problems.sort();
        for (off, msg) in problems {
            let (l, c) = lines.line_col(off);
            eprintln!("{}:{l}:{c}: {msg}", art.path);
        }
        return Ok(ExitCode::from(1));
    }
    for s in &t.segments {
        let (l, c) = lines.line_col(s.start);
        match &s.kind {
            SegmentKind::Sql => {}
            SegmentKind::Placeholder { name } => {
                println!("{}:{l}:{c}: placeholder {name}", art.path)
            }
            SegmentKind::Special { form, args } => {
                println!(
                    "{}:{l}:{c}: special form {form} {}",
                    art.path,
                    args.join(" ")
                )
            }
            SegmentKind::Unresolved { name } => println!(
                "{}:{l}:{c}: unresolved `{}`: {name} is not a parameter; kept as SQL",
                art.path,
                t.slice(s)
            ),
        }
    }
    let (parse, diags) = t.fragment().parse(art.dialect);
    for d in &diags {
        let (l, c) = lines.line_col(d.offset);
        eprintln!("{}:{l}:{c}: {}", art.path, d.message);
    }
    let statements = parse
        .syntax()
        .descendants()
        .filter(|n| n.kind().is_unit())
        .count();
    eprintln!(
        "{}: {} parameters, {statements} statements in the fragment, {} diagnostics, round trip ok",
        art.path,
        t.parameters().len(),
        diags.len()
    );
    Ok(ExitCode::from(
        if diags.is_empty() && unresolved.is_empty() {
            0
        } else {
            1
        },
    ))
}

fn exit_for(verdict: Verdict) -> ExitCode {
    match verdict {
        Verdict::Equal | Verdict::EqualUnderAssumptions => ExitCode::from(0),
        Verdict::Different | Verdict::Unknown => ExitCode::from(1),
    }
}

/// Prints a report, redacted and leak-checked when the pipeline redacts.
fn emit(report: Report, format: Format, pipe: &mut Pipeline) -> Result<(), String> {
    let report = pipe.redact_reports(vec![report])?.remove(0);
    let out = match format {
        Format::Text => render::text(&report, false),
        Format::Json => render::json(&report) + "\n",
        Format::Sarif => render::sarif(std::slice::from_ref(&report)) + "\n",
    };
    print!("{out}");
    Ok(())
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sqldiff: {e}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    match cli.command {
        Command::Diff {
            a,
            b,
            dialect,
            dialect_a,
            dialect_b,
            format,
            excerpt_lines,
            catalogs,
            flips,
            templates,
            params,
            template,
            parameters,
            cfg,
            review,
        } => {
            let cfg = cfg.resolve()?;
            let a = read(&a, dialect_a.or(dialect), &cfg)?;
            let b = read(&b, dialect_b.or(dialect), &cfg)?;
            let mut opts = cfg.options();
            opts.excerpt_lines = excerpt_lines.unwrap_or(cfg.excerpt_lines);
            opts.flips = flips.flips();
            catalogs.apply(&mut opts, a.dialect, Some(b.dialect))?;
            let mut pipe = Pipeline::new(cfg, &review)?;
            if template {
                let known: Option<BTreeSet<String>> =
                    (!parameters.is_empty()).then(|| parameters.into_iter().collect());
                let report =
                    tree::template_diff(&a, &b, [known.as_ref(), known.as_ref()], &pipe.cfg, &opts)
                        .map_err(|e| e.0)?;
                let verdict = report.open_verdict();
                emit(report, format, &mut pipe)?;
                return Ok(exit_for(verdict));
            }
            pipe.templates = template_dir(templates.as_deref(), params)?;
            let report = pipe.run(&a, Some(&b), &opts);
            let verdict = report.open_verdict();
            emit(report, format, &mut pipe)?;
            Ok(exit_for(verdict))
        }
        Command::Check {
            file,
            dialect,
            format,
            catalogs,
            templates,
            params,
            cfg,
            review,
        } => {
            let cfg = cfg.resolve()?;
            let art = read(&file, dialect, &cfg)?;
            let mut pipe = Pipeline::new(cfg, &review)?;
            pipe.templates = template_dir(templates.as_deref(), params)?;
            let mut opts = pipe.options();
            catalogs.apply(&mut opts, art.dialect, None)?;
            let report = pipe.run(&art, None, &opts);
            let verdict = report.open_verdict();
            emit(report, format, &mut pipe)?;
            Ok(exit_for(verdict))
        }
        Command::Template {
            file,
            dialect,
            params,
            known_only,
            render,
            cfg,
        } => {
            let cfg = cfg.resolve()?;
            let art = read(&file, dialect, &cfg)?;
            template_cmd(
                &art,
                &params.into_iter().collect(),
                known_only,
                render,
                &cfg,
            )
        }
        Command::Parse {
            file,
            dialect,
            tree,
        } => {
            let art = Artifact::new(
                file.display().to_string(),
                dialect,
                std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?,
            );
            let parse = sqldiff_syntax::parse(&art.text, dialect);
            let root = parse.syntax();
            if tree {
                print!("{}", ast::dump(&root));
            }
            let lines = sqldiff_syntax::LineIndex::new(&art.text);
            for d in &parse.diagnostics {
                let (l, c) = lines.line_col(d.offset);
                eprintln!("{}:{l}:{c}: {}", art.path, d.message);
            }
            if root.text().to_string() != art.text {
                return Err("round trip failed: tree text differs from input".into());
            }
            let statements = root.descendants().filter(|n| n.kind().is_unit()).count();
            eprintln!(
                "{}: {statements} statements, {} diagnostics, round trip ok",
                art.path,
                parse.diagnostics.len()
            );
            Ok(ExitCode::from(if parse.diagnostics.is_empty() {
                0
            } else {
                1
            }))
        }
        Command::Tree {
            dir_a,
            dir_b,
            dialect_a,
            dialect_b,
            format,
            cfg,
            review,
        } => tree(
            &dir_a,
            &dir_b,
            dialect_a,
            dialect_b,
            format,
            Pipeline::new(cfg.resolve()?, &review)?,
        ),
        Command::Convert {
            src,
            out,
            from,
            to,
            jobs,
            overwrite,
            no_catalog,
            template,
            parameters,
            extensions,
            cfg,
        } => {
            let template = if template {
                // The same placeholder rules as `diff --template`.
                let cfg = cfg.resolve()?;
                let def = cfg.policy.template.as_ref().ok_or(
                    "--template needs a [template] syntax: give --pack or a sqldiff.toml with one",
                )?;
                Some(convert_cmd::TemplateMode {
                    placeholders: compose::Placeholders::new(def).map_err(|e| e.0)?,
                    parameters: (!parameters.is_empty()).then(|| parameters.into_iter().collect()),
                })
            } else {
                None
            };
            convert_cmd::run(convert_cmd::Args {
                src,
                out,
                from,
                to,
                jobs,
                overwrite,
                no_catalog,
                template,
                extensions: if extensions.is_empty() {
                    vec!["sql".to_string()]
                } else {
                    extensions
                        .into_iter()
                        .map(|e| e.trim_start_matches('.').to_string())
                        .collect()
                },
            })
        }
        Command::Changes(args) => changes_cmd::run(args),
        Command::Review(cmd) => review_cmd::run(cmd),
        Command::Dist(args) => dist_cmd::run(args),
        Command::Source(cmd) => source_cmd::run(cmd),
        Command::Queue(cmd) => queue_cmd::run(cmd),
    }
}

/// Reports of every pair, in pair order. Pairs are independent, so they run
/// on all cores; each report is the one a sequential run gives.
fn run_pairs(
    pipe: &Pipeline,
    sides: &[(Artifact, Artifact)],
    opts: &sqldiff_core::Options,
) -> Vec<Report> {
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(sides.len());
    if threads <= 1 {
        return sides
            .iter()
            .map(|(a, b)| pipe.run(a, Some(b), opts))
            .collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut done: Vec<(usize, Report)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some((a, b)) = sides.get(i) else {
                            return out;
                        };
                        out.push((i, pipe.run(a, Some(b), opts)));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    });
    done.sort_by_key(|(i, _)| *i);
    done.into_iter().map(|(_, r)| r).collect()
}

fn tree(
    dir_a: &Path,
    dir_b: &Path,
    da: Option<Dialect>,
    db: Option<Dialect>,
    format: Format,
    mut pipe: Pipeline,
) -> Result<ExitCode, String> {
    let cfg = &pipe.cfg.clone();
    let t = tree::pair_dirs(dir_a, dir_b, da, db, cfg).map_err(|e| e.0)?;
    let opts = cfg.options();
    let sides = t
        .pairs
        .iter()
        .map(|p| t.read(p, cfg).map_err(|e| e.0))
        .collect::<Result<Vec<_>, _>>()?;
    let frag = |f: Option<&str>| f.map(|f| format!("#{f}")).unwrap_or_default();
    let mut reports: Vec<_> = run_pairs(&pipe, &sides, &opts)
        .into_iter()
        .zip(&t.pairs)
        .map(|(r, p)| {
            let (fa, fb) = p.fragments();
            (r, [frag(fa), frag(fb)])
        })
        .collect();
    let (only_a, only_b) = (t.only_a, t.only_b);
    let ambiguous: Vec<serde_json::Value> = t
        .ambiguous
        .iter()
        .map(|a| serde_json::json!({ "side": a.side, "key": a.key, "ids": a.ids }))
        .collect();
    let no_dialect = t.no_dialect;
    // Most severe first.
    reports.sort_by_key(|(r, _)| {
        std::cmp::Reverse((
            r.open_verdict(),
            r.findings.iter().map(|f| f.severity).max(),
            r.findings.len(),
        ))
    });
    // Which object of a file with several each pair row compares.
    let (reports, fragments): (Vec<_>, Vec<_>) = reports.into_iter().unzip();
    let worst = reports
        .iter()
        .map(|r| r.open_verdict())
        .max()
        .unwrap_or(Verdict::Equal);
    if pipe.redacting() {
        let mut paths: Vec<String> = vec![dir_a.display().to_string(), dir_b.display().to_string()];
        paths.extend(only_a.iter().chain(&only_b).chain(&no_dialect).cloned());
        let objects = fragments
            .iter()
            .flatten()
            .filter_map(|f| f.strip_prefix('#'));
        paths.extend(objects.map(str::to_string));
        for a in &ambiguous {
            for id in a["ids"].as_array().into_iter().flatten() {
                paths.extend(id.as_str().map(str::to_string));
            }
        }
        pipe.add_paths(paths.iter().map(String::as_str));
    }
    let complete =
        only_a.is_empty() && only_b.is_empty() && ambiguous.is_empty() && no_dialect.is_empty();
    let reports = pipe.redact_reports(reports)?;
    // Everything printed outside the reports goes through the redactor too.
    let mut extra = serde_json::json!({
        "only_a": only_a,
        "only_b": only_b,
        "ambiguous": ambiguous,
        "no_dialect": no_dialect,
        "provenance": cfg.layers,
        "fragments": fragments,
    });
    pipe.redact_json(&mut extra)?;
    let strings = |v: &serde_json::Value| -> Vec<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect()
    };
    let only_a = strings(&extra["only_a"]);
    let only_b = strings(&extra["only_b"]);
    let no_dialect = strings(&extra["no_dialect"]);
    let ambiguous: Vec<serde_json::Value> =
        extra["ambiguous"].as_array().cloned().unwrap_or_default();
    let mut out = String::new();
    match format {
        Format::Json => {
            let v = serde_json::json!({
                "pairs": reports,
                "only_a": only_a,
                "only_b": only_b,
                "ambiguous": ambiguous,
                "no_dialect": no_dialect,
                "provenance": extra["provenance"],
            });
            let _ = writeln!(out, "{}", serde_json::to_string_pretty(&v).unwrap());
        }
        Format::Sarif => {
            let _ = writeln!(out, "{}", render::sarif(&reports));
        }
        Format::Text => {
            let _ = writeln!(
                out,
                "{:<26} {:<14} {:>8}  pair",
                "verdict", "highest", "findings"
            );
            for (i, r) in reports.iter().enumerate() {
                let b = r.b.as_ref().map(|b| b.path.as_str()).unwrap_or("");
                let frag = |side: usize| extra["fragments"][i][side].as_str().unwrap_or("");
                let _ = writeln!(
                    out,
                    "{:<26} {:<14} {:>8}  {}{} ↔ {}{}",
                    r.verdict.label(),
                    r.highest_class().map(|c| c.label()).unwrap_or("-"),
                    r.findings.len(),
                    r.a.path,
                    frag(0),
                    b,
                    frag(1)
                );
            }
            for p in &only_a {
                let _ = writeln!(out, "{:<26} {:<14} {:>8}  {p}", "only in A", "-", "-");
            }
            for p in &only_b {
                let _ = writeln!(out, "{:<26} {:<14} {:>8}  {p}", "only in B", "-", "-");
            }
            for a in &ambiguous {
                let _ = writeln!(
                    out,
                    "{:<26} {:<14} {:>8}  {}",
                    format!("ambiguous in {}", a["side"].as_str().unwrap_or("")),
                    "-",
                    "-",
                    a["ids"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            for p in &no_dialect {
                let _ = writeln!(out, "{:<26} {:<14} {:>8}  {p}", "no dialect", "-", "-");
            }
        }
    }
    print!("{out}");
    Ok(exit_for(if complete {
        worst
    } else {
        worst.max(Verdict::Unknown)
    }))
}
