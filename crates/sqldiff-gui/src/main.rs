//! `sqldiff-gui`: open a pair of SQL artifacts in the pair view.
//!
//! `serve` runs a local server (edits in the scratchpad re-diff live);
//! `export` writes a self-contained, read-only HTML file.

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use sqldiff_core::{Artifact, Dialect, Options};
use sqldiff_gui::server::{serve, Pair};

#[derive(Parser)]
#[command(name = "sqldiff-gui", version, about = "Pair view for SQLDiff reports")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct PairArgs {
    /// Side A. Optional with --report.
    a: Option<PathBuf>,
    /// Side B. Optional with --report.
    b: Option<PathBuf>,
    /// Take paths and dialects from a `sqldiff diff -f json` report.
    #[arg(long)]
    report: Option<PathBuf>,
    /// Dialect of both sides (overridden by --dialect-a / --dialect-b).
    #[arg(short, long)]
    dialect: Option<Dialect>,
    #[arg(long)]
    dialect_a: Option<Dialect>,
    #[arg(long)]
    dialect_b: Option<Dialect>,
    /// Maximum excerpt lines per location.
    #[arg(long, default_value_t = 3)]
    excerpt_lines: usize,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the pair view on a local port and print its URL.
    Serve {
        #[command(flatten)]
        pair: PairArgs,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(short, long, default_value_t = 7878)]
        port: u16,
    },
    /// Write a self-contained, read-only HTML pair view.
    Export {
        #[command(flatten)]
        pair: PairArgs,
        #[arg(short, long)]
        output: PathBuf,
    },
}

fn resolve(p: PairArgs) -> Result<Pair, String> {
    let (mut a, mut b, mut da, mut db) = (p.a, p.b, None, None);
    if let Some(r) = &p.report {
        let text = std::fs::read_to_string(r).map_err(|e| format!("{}: {e}", r.display()))?;
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", r.display()))?;
        let side = |k: &str| -> Option<(PathBuf, Dialect)> {
            let s = v.get(k)?;
            Some((
                PathBuf::from(s.get("path")?.as_str()?),
                s.get("dialect")?.as_str()?.parse().ok()?,
            ))
        };
        let (pa, sa) = side("a").ok_or("report has no side a")?;
        let (pb, sb) = side("b").ok_or("report has no side b (is it a `check` report?)")?;
        a = a.or(Some(pa));
        b = b.or(Some(pb));
        da = Some(sa);
        db = Some(sb);
    }
    Ok(Pair {
        a: a.ok_or("give two files or --report")?,
        b: b.ok_or("give two files or --report")?,
        dialect_a: p
            .dialect_a
            .or(p.dialect)
            .or(da)
            .ok_or("give --dialect or --dialect-a")?,
        dialect_b: p
            .dialect_b
            .or(p.dialect)
            .or(db)
            .ok_or("give --dialect or --dialect-b")?,
        excerpt_lines: p.excerpt_lines,
    })
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sqldiff-gui: {e}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::Serve { pair, host, port } => {
            let pair = resolve(pair)?;
            let listener = TcpListener::bind((host.as_str(), port)).map_err(|e| e.to_string())?;
            let addr = listener.local_addr().map_err(|e| e.to_string())?;
            println!("SQLDiff pair view: http://{addr}/");
            serve(listener, pair).map_err(|e| e.to_string())
        }
        Command::Export { pair, output } => {
            let pair = resolve(pair)?;
            let read = |p: &PathBuf| {
                std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))
            };
            let a = Artifact::new(pair.a.display().to_string(), pair.dialect_a, read(&pair.a)?);
            let b = Artifact::new(pair.b.display().to_string(), pair.dialect_b, read(&pair.b)?);
            let opts = Options {
                excerpt_lines: pair.excerpt_lines,
                ..Options::default()
            };
            let html = sqldiff_gui::page(&sqldiff_gui::build(&a, &b, &opts, false));
            std::fs::write(&output, html).map_err(|e| format!("{}: {e}", output.display()))?;
            eprintln!("wrote {}", output.display());
            Ok(())
        }
    }
}
