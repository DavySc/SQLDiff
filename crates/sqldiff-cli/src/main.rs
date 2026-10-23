//! `sqldiff` command-line interface.
//!
//! Exit codes: 0 equal (or equal under assumptions), 1 different or
//! unknown, 2 usage or I/O error.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use sqldiff_core::{render, Artifact, Dialect, Options, Report, Verdict};
use sqldiff_syntax::ast;

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
        /// Maximum excerpt lines per location.
        #[arg(long, default_value_t = 3)]
        excerpt_lines: usize,
    },
    /// Lint one artifact: residue from other dialects, terminator hazards, parse problems.
    Check {
        file: PathBuf,
        #[arg(short, long)]
        dialect: Dialect,
        #[arg(short, long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
    /// Parse one artifact, verify the lossless round trip and optionally dump the tree.
    Parse {
        file: PathBuf,
        #[arg(short, long)]
        dialect: Dialect,
        #[arg(long)]
        tree: bool,
    },
    /// Collection drift report: pair `.sql` files of two trees by name and diff each pair.
    Tree {
        dir_a: PathBuf,
        dir_b: PathBuf,
        #[arg(long)]
        dialect_a: Dialect,
        #[arg(long)]
        dialect_b: Dialect,
        #[arg(short, long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
}

fn read(path: &Path, dialect: Dialect) -> Result<Artifact, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Artifact::new(path.display().to_string(), dialect, text))
}

fn exit_for(verdict: Verdict) -> ExitCode {
    match verdict {
        Verdict::Equal | Verdict::EqualUnderAssumptions => ExitCode::from(0),
        Verdict::Different | Verdict::Unknown => ExitCode::from(1),
    }
}

fn emit(report: &Report, format: Format) {
    match format {
        Format::Text => print!("{}", render::text(report, false)),
        Format::Json => println!("{}", render::json(report)),
        Format::Sarif => println!("{}", render::sarif(std::slice::from_ref(report))),
    }
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
        } => {
            let da = dialect_a
                .or(dialect)
                .ok_or("give --dialect or --dialect-a")?;
            let db = dialect_b
                .or(dialect)
                .ok_or("give --dialect or --dialect-b")?;
            let report =
                sqldiff_core::diff(&read(&a, da)?, &read(&b, db)?, &Options { excerpt_lines });
            emit(&report, format);
            Ok(exit_for(report.verdict))
        }
        Command::Check {
            file,
            dialect,
            format,
        } => {
            let report = sqldiff_core::check(&read(&file, dialect)?, &Options::default());
            emit(&report, format);
            Ok(exit_for(report.verdict))
        }
        Command::Parse {
            file,
            dialect,
            tree,
        } => {
            let art = read(&file, dialect)?;
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
        } => tree(&dir_a, &dir_b, dialect_a, dialect_b, format),
    }
}

/// Pairing key: file stem, case-insensitive, `_` and `-` ignored, dialect suffixes stripped.
fn pair_key(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let stem = name.split('.').next().unwrap_or("").to_string();
    stem.replace(['_', '-'], "")
}

fn sql_files(dir: &Path) -> Result<BTreeMap<String, PathBuf>, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).map_err(|e| format!("{}: {e}", d.display()))? {
            let p = entry.map_err(|e| e.to_string())?.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("sql")) {
                out.insert(pair_key(&p), p);
            }
        }
    }
    Ok(out)
}

fn tree(
    dir_a: &Path,
    dir_b: &Path,
    da: Dialect,
    db: Dialect,
    format: Format,
) -> Result<ExitCode, String> {
    let fa = sql_files(dir_a)?;
    let fb = sql_files(dir_b)?;
    let mut reports = Vec::new();
    let mut only_a = Vec::new();
    let mut only_b = Vec::new();
    for (k, pa) in &fa {
        match fb.get(k) {
            Some(pb) => reports.push(sqldiff_core::diff(
                &read(pa, da)?,
                &read(pb, db)?,
                &Options::default(),
            )),
            None => only_a.push(pa.display().to_string()),
        }
    }
    for (k, pb) in &fb {
        if !fa.contains_key(k) {
            only_b.push(pb.display().to_string());
        }
    }
    // Most severe first.
    reports.sort_by_key(|r| {
        std::cmp::Reverse((
            r.verdict,
            r.findings.iter().map(|f| f.severity).max(),
            r.findings.len(),
        ))
    });
    match format {
        Format::Json => {
            let v = serde_json::json!({ "pairs": reports, "only_a": only_a, "only_b": only_b });
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
        }
        Format::Sarif => println!("{}", render::sarif(&reports)),
        Format::Text => {
            println!(
                "{:<26} {:<14} {:>8}  pair",
                "verdict", "highest", "findings"
            );
            for r in &reports {
                let b = r.b.as_ref().map(|b| b.path.as_str()).unwrap_or("");
                println!(
                    "{:<26} {:<14} {:>8}  {} ↔ {}",
                    r.verdict.label(),
                    r.highest_class().map(|c| c.label()).unwrap_or("-"),
                    r.findings.len(),
                    r.a.path,
                    b
                );
            }
            for p in &only_a {
                println!("{:<26} {:<14} {:>8}  {p}", "only in A", "-", "-");
            }
            for p in &only_b {
                println!("{:<26} {:<14} {:>8}  {p}", "only in B", "-", "-");
            }
        }
    }
    let worst = reports
        .iter()
        .map(|r| r.verdict)
        .max()
        .unwrap_or(Verdict::Equal);
    Ok(exit_for(if only_a.is_empty() && only_b.is_empty() {
        worst
    } else {
        worst.max(Verdict::Unknown)
    }))
}
