//! `sqldiff` command-line interface.
//!
//! Exit codes: 0 equal (or equal under assumptions), 1 different or
//! unknown, 2 usage or I/O error.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use sqldiff_core::{render, Artifact, Dialect, Options, Report, Verdict};

use sqldiff_source::{tree, Resolved};
use sqldiff_syntax::ast;

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
        /// DDL file (CREATE TABLE, CREATE UNIQUE INDEX) describing the catalog
        /// both sides run against, read in each side's dialect. Repeatable.
        /// Keys, nullability and column types discharge equivalence side conditions.
        #[arg(long, value_name = "FILE")]
        catalog: Vec<PathBuf>,
        /// Catalog DDL for side A only. Repeatable.
        #[arg(long, value_name = "FILE")]
        catalog_a: Vec<PathBuf>,
        /// Catalog DDL for side B only. Repeatable.
        #[arg(long, value_name = "FILE")]
        catalog_b: Vec<PathBuf>,
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
        #[command(flatten)]
        cfg: source_cmd::ConfigArgs,
        #[command(flatten)]
        review: ReviewArgs,
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
    /// Review decisions: accept, reject or comment on findings by fingerprint.
    #[command(subcommand)]
    Review(review_cmd::ReviewCommand),
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
            catalog,
            catalog_a,
            catalog_b,
            cfg,
            review,
        } => {
            let cfg = cfg.resolve()?;
            let a = read(&a, dialect_a.or(dialect), &cfg)?;
            let b = read(&b, dialect_b.or(dialect), &cfg)?;
            let ddl = |paths: &[PathBuf], d: Dialect| -> Result<Vec<Artifact>, String> {
                paths
                    .iter()
                    .map(|p| {
                        let text = std::fs::read_to_string(p)
                            .map_err(|e| format!("{}: {e}", p.display()))?;
                        Ok(Artifact::new(p.display().to_string(), d, text))
                    })
                    .collect()
            };
            let mut cat_a = ddl(&catalog, a.dialect)?;
            cat_a.extend(ddl(&catalog_a, a.dialect)?);
            let mut cat_b = ddl(&catalog, b.dialect)?;
            cat_b.extend(ddl(&catalog_b, b.dialect)?);
            let opts = Options {
                excerpt_lines: excerpt_lines.unwrap_or(cfg.excerpt_lines),
                catalog_a: cat_a,
                catalog_b: cat_b,
            };
            let mut pipe = Pipeline::new(cfg, &review)?;
            let report = pipe.run(&a, Some(&b), &opts);
            let verdict = report.open_verdict();
            emit(report, format, &mut pipe)?;
            Ok(exit_for(verdict))
        }
        Command::Check {
            file,
            dialect,
            format,
            cfg,
            review,
        } => {
            let cfg = cfg.resolve()?;
            let art = read(&file, dialect, &cfg)?;
            let mut pipe = Pipeline::new(cfg, &review)?;
            let report = pipe.run(&art, None, &pipe.options());
            let verdict = report.open_verdict();
            emit(report, format, &mut pipe)?;
            Ok(exit_for(verdict))
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
        Command::Review(cmd) => review_cmd::run(cmd),
        Command::Source(cmd) => source_cmd::run(cmd),
        Command::Queue(cmd) => queue_cmd::run(cmd),
    }
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
    let opts = Options {
        excerpt_lines: cfg.excerpt_lines,
        ..Options::default()
    };
    let mut reports = Vec::new();
    for p in &t.pairs {
        let (a, b) = t.read(p, cfg).map_err(|e| e.0)?;
        reports.push(pipe.run(&a, Some(&b), &opts));
    }
    let (only_a, only_b) = (t.only_a, t.only_b);
    let ambiguous: Vec<serde_json::Value> = t
        .ambiguous
        .iter()
        .map(|a| serde_json::json!({ "side": a.side, "key": a.key, "ids": a.ids }))
        .collect();
    let no_dialect = t.no_dialect;
    // Most severe first.
    reports.sort_by_key(|r| {
        std::cmp::Reverse((
            r.open_verdict(),
            r.findings.iter().map(|f| f.severity).max(),
            r.findings.len(),
        ))
    });
    let worst = reports
        .iter()
        .map(|r| r.open_verdict())
        .max()
        .unwrap_or(Verdict::Equal);
    if pipe.redacting() {
        let mut paths: Vec<String> = vec![dir_a.display().to_string(), dir_b.display().to_string()];
        paths.extend(only_a.iter().chain(&only_b).chain(&no_dialect).cloned());
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
            for r in &reports {
                let b = r.b.as_ref().map(|b| b.path.as_str()).unwrap_or("");
                let _ = writeln!(
                    out,
                    "{:<26} {:<14} {:>8}  {} ↔ {}",
                    r.verdict.label(),
                    r.highest_class().map(|c| c.label()).unwrap_or("-"),
                    r.findings.len(),
                    r.a.path,
                    b
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
