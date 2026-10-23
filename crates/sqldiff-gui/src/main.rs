//! `sqldiff-gui`: open a pair of SQL artifacts, or two trees as a review
//! queue, in the browser.
//!
//! `serve` runs a local server (scratchpad edits re-diff, assumption flips
//! and review decisions apply live); `export` writes a self-contained,
//! read-only HTML file.

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use sqldiff_core::Dialect;
use sqldiff_gui::convert;
use sqldiff_gui::review::Store;
use sqldiff_gui::server::serve;
use sqldiff_gui::session::{Origin, Session};
use sqldiff_source::config::{self, Sources};
use sqldiff_source::Resolved;

#[derive(Parser)]
#[command(
    name = "sqldiff-gui",
    version,
    about = "Review queue and pair view for SQLDiff reports"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct PairArgs {
    /// Side A: a file, or a directory to pair with directory B. Optional with --report.
    a: Option<PathBuf>,
    /// Side B. Optional with --report.
    b: Option<PathBuf>,
    /// Take paths and dialects from a `sqldiff diff -f json` report.
    #[arg(long)]
    report: Option<PathBuf>,
    /// Dialect of both sides (overridden by --dialect-a / --dialect-b).
    /// Default: `dialect_map` or the path convention.
    #[arg(short, long)]
    dialect: Option<Dialect>,
    #[arg(long)]
    dialect_a: Option<Dialect>,
    #[arg(long)]
    dialect_b: Option<Dialect>,
    /// Maximum excerpt lines per location (default: config, else 3).
    #[arg(long)]
    excerpt_lines: Option<usize>,
    /// Load a pack (directory with pack.toml, or a .toml file). Repeatable.
    #[arg(long = "pack", value_name = "PATH")]
    packs: Vec<PathBuf>,
    /// Project config file (default: sqldiff.toml here or in a parent directory).
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Ignore project and user config files not named on the command line.
    #[arg(long)]
    isolated: bool,
    /// Review store directory (default: the configured data directory).
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the GUI on a local port and print its URL.
    Serve {
        #[command(flatten)]
        pair: PairArgs,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(short, long, default_value_t = 7878)]
        port: u16,
    },
    /// Convert mode: edit T-SQL on the left, see PostgreSQL on the right, live.
    Convert {
        /// Initial input; a sample is shown when omitted.
        file: Option<PathBuf>,
        #[arg(long, default_value = "tsql")]
        from: Dialect,
        #[arg(long, default_value = "postgres")]
        to: Dialect,
        /// Print the conversion and the unit badges instead of serving the page.
        #[arg(long)]
        stdout: bool,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(short, long, default_value_t = 7879)]
        port: u16,
    },
    /// Write a self-contained, read-only HTML file.
    Export {
        #[command(flatten)]
        pair: PairArgs,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Write the session report as Markdown (to stdout without --output).
    Report {
        #[command(flatten)]
        pair: PairArgs,
        /// Replace paths, names, identifiers and literals with placeholders.
        #[arg(long)]
        redact: bool,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

fn open(p: PairArgs) -> Result<Session, String> {
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
    let a = a.ok_or("give two files, two directories or --report")?;
    let b = b.ok_or("give two files, two directories or --report")?;
    let sources = Sources {
        project: p.config,
        user: None,
        isolated: p.isolated,
        cwd: None,
        packs: p.packs,
        cli: config::cli_layer(&[], &[], p.excerpt_lines).map_err(|e| e.0)?,
    };
    let cfg = Resolved::load(&sources).map_err(|e| e.0)?;
    let store = Store::open(p.data_dir.as_ref().unwrap_or(&cfg.data_dir))?;
    Session::open(
        Origin::detect(a, b),
        p.dialect_a.or(p.dialect).or(da),
        p.dialect_b.or(p.dialect).or(db),
        cfg,
        store,
    )
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
            let session = open(pair)?;
            let listener = TcpListener::bind((host.as_str(), port)).map_err(|e| e.to_string())?;
            let addr = listener.local_addr().map_err(|e| e.to_string())?;
            println!("SQLDiff: http://{addr}/");
            serve(listener, session).map_err(|e| e.to_string())
        }
        Command::Convert {
            file,
            from,
            to,
            stdout,
            host,
            port,
        } => {
            let (name, text) = match &file {
                Some(p) => (
                    p.display().to_string(),
                    std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?,
                ),
                None => (String::new(), convert::SAMPLE.to_string()),
            };
            let setup = convert::Setup {
                from,
                to,
                text,
                name,
            };
            if stdout {
                let p = convert::run(&setup, &setup.text, false);
                let c = p.conversion.ok_or(p.error.unwrap_or_default())?;
                print!("{}", c.output);
                for u in &c.units {
                    eprintln!(
                        "line {:>4} -> {:>4}  {}{}",
                        u.src.line,
                        u.out.line,
                        u.status.label(),
                        u.notes
                            .first()
                            .map(|n| format!(": {}", n.text))
                            .unwrap_or_default()
                    );
                }
                return Ok(());
            }
            if !sqldiff_convert::supported(from, to) {
                return Err(format!("no conversion rules from {from} to {to} yet"));
            }
            let listener = TcpListener::bind((host.as_str(), port)).map_err(|e| e.to_string())?;
            let addr = listener.local_addr().map_err(|e| e.to_string())?;
            println!("SQLDiff convert: http://{addr}/");
            convert::serve(listener, setup).map_err(|e| e.to_string())
        }
        Command::Export { pair, output } => {
            let session = open(pair)?;
            let html = sqldiff_gui::page(&session.page(None, false));
            std::fs::write(&output, html).map_err(|e| format!("{}: {e}", output.display()))?;
            eprintln!("wrote {}", output.display());
            Ok(())
        }
        Command::Report {
            pair,
            redact,
            output,
        } => {
            let md = open(pair)?.session_report(redact);
            match output {
                Some(o) => std::fs::write(&o, md).map_err(|e| format!("{}: {e}", o.display())),
                None => {
                    print!("{md}");
                    Ok(())
                }
            }
        }
    }
}
