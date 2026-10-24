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
use sqldiff_gui::server::serve_with_hosts;
use sqldiff_gui::session::{Origin, Session};
use sqldiff_source::compose::Placeholders;
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
    /// Data directory for the review store and redaction salt (default: configured).
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
        /// Also answer requests addressed to this host name or address
        /// (when bound to a non-loopback address). Repeatable.
        #[arg(long = "allow-host", value_name = "HOST")]
        allow_hosts: Vec<String>,
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
        /// The input is a template: placeholders and special forms follow the
        /// `[template]` syntax of the loaded packs or config, and are kept in
        /// the output.
        #[arg(long)]
        template: bool,
        /// A template parameter name (repeatable): only listed names are
        /// placeholders, as with `sqldiff diff --template --parameter`.
        #[arg(long = "parameter", value_name = "NAME", requires = "template")]
        parameters: Vec<String>,
        /// Load a pack (directory with pack.toml, or a .toml file). Repeatable.
        #[arg(long = "pack", value_name = "PATH")]
        packs: Vec<PathBuf>,
        /// Project config file (default: sqldiff.toml here or in a parent directory).
        #[arg(long, value_name = "FILE")]
        config: Option<PathBuf>,
        /// Ignore project and user config files not named on the command line.
        #[arg(long)]
        isolated: bool,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(short, long, default_value_t = 7879)]
        port: u16,
        /// Also answer requests addressed to this host name or address. Repeatable.
        #[arg(long = "allow-host", value_name = "HOST")]
        allow_hosts: Vec<String>,
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
    let mut cfg = Resolved::load(&sources).map_err(|e| e.0)?;
    if let Some(d) = p.data_dir {
        cfg.data_dir = d;
    }
    let store = Store::from_config(&cfg).map_err(|e| e.0)?;
    Session::open(
        Origin::detect(a, b),
        p.dialect_a.or(p.dialect).or(da),
        p.dialect_b.or(p.dialect).or(db),
        cfg,
        store,
    )
}

/// The URL to open: a wildcard bind (`0.0.0.0`, `::`) is reachable locally as loopback.
fn url(listener: &TcpListener) -> Result<String, String> {
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    if addr.ip().is_unspecified() {
        eprintln!(
            "listening on all interfaces; from another machine, add --allow-host <name or address>"
        );
        return Ok(format!("http://127.0.0.1:{}/", addr.port()));
    }
    Ok(format!("http://{addr}/"))
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
        Command::Serve {
            pair,
            host,
            port,
            allow_hosts,
        } => {
            let session = open(pair)?;
            let listener = TcpListener::bind((host.as_str(), port)).map_err(|e| e.to_string())?;
            println!("SQLDiff: {}", url(&listener)?);
            serve_with_hosts(listener, session, &allow_hosts).map_err(|e| e.to_string())
        }
        Command::Convert {
            file,
            from,
            to,
            stdout,
            template,
            parameters,
            packs,
            config: project,
            isolated,
            host,
            port,
            allow_hosts,
        } => {
            let (name, text) = match &file {
                Some(p) => (
                    p.display().to_string(),
                    std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?,
                ),
                None => (String::new(), convert::SAMPLE.to_string()),
            };
            let template = if template {
                let cfg = Resolved::load(&Sources {
                    project,
                    user: None,
                    isolated,
                    cwd: None,
                    packs,
                    cli: config::cli_layer(&[], &[], None).map_err(|e| e.0)?,
                })
                .map_err(|e| e.0)?;
                let def = cfg.policy.template.as_ref().ok_or(
                    "--template needs a [template] syntax: give --pack or a sqldiff.toml with one",
                )?;
                Some(convert::TemplateSetup {
                    placeholders: Placeholders::new(def).map_err(|e| e.0)?,
                    parameters: (!parameters.is_empty()).then(|| parameters.into_iter().collect()),
                })
            } else {
                None
            };
            let setup = convert::Setup {
                from,
                to,
                template,
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
            println!("SQLDiff convert: {}", url(&listener)?);
            convert::serve(listener, setup, &allow_hosts).map_err(|e| e.to_string())
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
            let md = open(pair)?.session_report(redact)?;
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
