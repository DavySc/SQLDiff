//! `sqldiff` command-line interface.
//!
//! Exit codes: 0 equal (or equal under assumptions), 1 different or
//! unknown, 2 usage or I/O error.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use sqldiff_core::{render, Artifact, Dialect, Options, Report, Verdict};
use sqldiff_source::protocol::ObjectFilter;
use sqldiff_source::{pairing, policy, FsSource, Resolved, Source};
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

/// Path with `/` separators, as `dialect_map` globs and pairing see it.
fn slashed(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
        .replace("//", "/")
}

/// Reads an artifact. Dialect: the given one, else `dialect_map` or the path
/// convention. Version: `dialect_map`, else the configured target version.
fn read(path: &Path, dialect: Option<Dialect>, cfg: &Resolved) -> Result<Artifact, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    artifact(
        path.display().to_string(),
        &slashed(path),
        dialect,
        text,
        cfg,
    )
}

fn artifact(
    display: String,
    rel: &str,
    dialect: Option<Dialect>,
    text: String,
    cfg: &Resolved,
) -> Result<Artifact, String> {
    let (found, version) = sqldiff_source::classify(rel, cfg);
    let d = dialect.or(found).ok_or_else(|| {
        format!("{display}: dialect unknown; give a dialect flag or map it with dialect_map")
    })?;
    let mut a = Artifact::new(display, d, text);
    a.version = match version {
        Some(v) if found == Some(d) => v,
        _ => cfg.dialect_version(d),
    };
    Ok(a)
}

fn with_policy(mut r: Report, a: &Artifact, b: Option<&Artifact>, cfg: &Resolved) -> Report {
    policy::add_limits(&mut r, a, b, cfg);
    policy::apply(&mut r, cfg);
    r
}

fn exit_for(verdict: Verdict) -> ExitCode {
    match verdict {
        Verdict::Equal | Verdict::EqualUnderAssumptions => ExitCode::from(0),
        Verdict::Different | Verdict::Unknown => ExitCode::from(1),
    }
}

/// Prints a report, redacted and leak-checked when the pipeline redacts.
fn emit(report: Report, format: Format, pipe: &mut Pipeline) -> Result<(), String> {
    let report = pipe.redact_reports(vec![report]).remove(0);
    let out = match format {
        Format::Text => render::text(&report, false),
        Format::Json => render::json(&report) + "\n",
        Format::Sarif => render::sarif(std::slice::from_ref(&report)) + "\n",
    };
    pipe.verify(&out)?;
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
            cfg,
            review,
        } => {
            let cfg = cfg.resolve()?;
            let a = read(&a, dialect_a.or(dialect), &cfg)?;
            let b = read(&b, dialect_b.or(dialect), &cfg)?;
            let opts = Options {
                excerpt_lines: excerpt_lines.unwrap_or(cfg.excerpt_lines),
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

/// One side of a tree: its objects by pairing key.
struct Side {
    dir: PathBuf,
    dialect: Option<Dialect>,
    by_key: BTreeMap<String, String>,
    /// Keys held by several objects; none of them is paired.
    ambiguous: BTreeMap<String, Vec<String>>,
    no_dialect: Vec<String>,
}

impl Side {
    /// Lists a directory with pack `ignore` and `dialect_map`, skipping shared
    /// artifacts, and keys every object by its pairing rule.
    fn load(dir: &Path, dialect: Option<Dialect>, cfg: &Resolved) -> Result<Side, String> {
        let src = FsSource::new(dir, cfg).map_err(|e| e.0)?;
        let objects = src
            .list_objects(&ObjectFilter::default())
            .map_err(|e| e.0)?;
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut no_dialect = Vec::new();
        for o in objects {
            if cfg.is_shared(&o.id) {
                continue;
            }
            let Some(d) = dialect.or_else(|| o.dialect.as_deref().and_then(|d| d.parse().ok()))
            else {
                no_dialect.push(o.id);
                continue;
            };
            let rule = cfg.pairing_rule(&o.id, Some(d));
            groups
                .entry(pairing::key(&o.name, &rule))
                .or_default()
                .push(o.id);
        }
        let (unique, ambiguous): (BTreeMap<_, _>, BTreeMap<_, _>) =
            groups.into_iter().partition(|(_, ids)| ids.len() == 1);
        Ok(Side {
            dir: dir.to_path_buf(),
            dialect,
            by_key: unique
                .into_iter()
                .map(|(k, mut ids)| (k, ids.remove(0)))
                .collect(),
            ambiguous,
            no_dialect,
        })
    }

    fn display(&self, id: &str) -> String {
        self.dir.join(id).display().to_string()
    }

    fn read(&self, id: &str, cfg: &Resolved) -> Result<Artifact, String> {
        let p = self.dir.join(id);
        let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        artifact(p.display().to_string(), id, self.dialect, text, cfg)
    }

    /// Takes an object out by id, wherever it is keyed.
    fn take(&mut self, id: &str) -> bool {
        let key = self
            .by_key
            .iter()
            .find(|(_, v)| v.as_str() == id)
            .map(|(k, _)| k.clone());
        key.is_some_and(|k| self.by_key.remove(&k).is_some())
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
    let mut sa = Side::load(dir_a, da, cfg)?;
    let mut sb = Side::load(dir_b, db, cfg)?;
    let opts = Options {
        excerpt_lines: cfg.excerpt_lines,
    };
    let mut pairs: Vec<(String, String)> = Vec::new();
    // Explicit pack pairs first: `a` relative to DIR_A, `b` relative to DIR_B.
    for e in &cfg.policy.pairing.pairs {
        for (x, y) in [(&e.a, &e.b), (&e.b, &e.a)] {
            if sa.by_key.values().any(|v| v == x) && sb.by_key.values().any(|v| v == y) {
                sa.take(x);
                sb.take(y);
                pairs.push((x.clone(), y.clone()));
                break;
            }
        }
    }
    let mut only_a = Vec::new();
    let mut only_b = Vec::new();
    for (k, ia) in &sa.by_key {
        match sb.by_key.get(k) {
            Some(ib) => pairs.push((ia.clone(), ib.clone())),
            None => only_a.push(sa.display(ia)),
        }
    }
    for (k, ib) in &sb.by_key {
        if !sa.by_key.contains_key(k) {
            only_b.push(sb.display(ib));
        }
    }
    let mut reports = Vec::new();
    for (ia, ib) in &pairs {
        let a = sa.read(ia, cfg)?;
        let b = sb.read(ib, cfg)?;
        reports.push(pipe.run(&a, Some(&b), &opts));
    }
    let ambiguous: Vec<serde_json::Value> = [(&sa, "A"), (&sb, "B")]
        .iter()
        .flat_map(|(s, side)| {
            s.ambiguous.iter().map(move |(k, ids)| {
                serde_json::json!({
                    "side": side,
                    "key": k,
                    "ids": ids.iter().map(|i| s.display(i)).collect::<Vec<_>>(),
                })
            })
        })
        .collect();
    let no_dialect: Vec<String> = sa
        .no_dialect
        .iter()
        .map(|i| sa.display(i))
        .chain(sb.no_dialect.iter().map(|i| sb.display(i)))
        .collect();
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
    let reports = pipe.redact_reports(reports);
    let mut out = String::new();
    match format {
        Format::Json => {
            let mut v = serde_json::json!({
                "pairs": reports,
                "only_a": only_a,
                "only_b": only_b,
                "ambiguous": ambiguous,
                "no_dialect": no_dialect,
                "provenance": cfg.layers,
            });
            pipe.redact_json(&mut v);
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
    pipe.verify(&out)?;
    print!("{out}");
    let complete =
        only_a.is_empty() && only_b.is_empty() && ambiguous.is_empty() && no_dialect.is_empty();
    Ok(exit_for(if complete {
        worst
    } else {
        worst.max(Verdict::Unknown)
    }))
}
