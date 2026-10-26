//! `sqldiff-mcp`: SQLDiff as a local stdio MCP server.
//!
//! Reads newline-delimited JSON-RPC from stdin and writes responses to
//! stdout. Logs go to stderr. Configuration (packs, excerpt cap, default
//! source) is resolved once at start, like the CLI's.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use sqldiff_source::config::{self, Sources};
use sqldiff_source::Resolved;

#[derive(Parser)]
#[command(
    name = "sqldiff-mcp",
    version,
    about = "SQLDiff as a local stdio MCP server"
)]
struct Cli {
    /// Load a pack (directory with pack.toml, or a .toml file). Repeatable.
    #[arg(long = "pack", value_name = "PATH")]
    packs: Vec<PathBuf>,
    /// Project config file (default: sqldiff.toml here or in a parent directory).
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// User config file (default: $SQLDIFF_CONFIG, else config/config.toml beside
    /// the executable, else ~/.config/sqldiff/config.toml).
    #[arg(long, value_name = "FILE")]
    user_config: Option<PathBuf>,
    /// Ignore project and user config files not named on the command line.
    #[arg(long)]
    isolated: bool,
    /// GUI address for `open_in_gui` links (default: $SQLDIFF_GUI_URL, else http://127.0.0.1:7878).
    #[arg(long, value_name = "URL")]
    gui_url: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    sqldiff_core::with_engine_stack(|| serve(cli))
}

fn serve(cli: Cli) -> ExitCode {
    let cfg = config::cli_layer(&[], &[], None).and_then(|layer| {
        Resolved::load(&Sources {
            project: cli.config,
            user: cli.user_config,
            isolated: cli.isolated,
            cwd: None,
            packs: cli.packs,
            cli: layer,
        })
    });
    let cfg = match cfg {
        Ok(c) => c,
        Err(e) => {
            eprintln!("sqldiff-mcp: {}", e.0);
            return ExitCode::from(2);
        }
    };
    let gui_url = cli
        .gui_url
        .or_else(|| std::env::var("SQLDIFF_GUI_URL").ok());
    let mut server = match sqldiff_mcp::Server::with_config(cfg, gui_url) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("sqldiff-mcp: review store: {e}");
            return ExitCode::from(2);
        }
    };
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("sqldiff-mcp: {e}");
                break;
            }
        };
        if let Some(out) = server.handle_line(&line) {
            if writeln!(stdout, "{out}")
                .and_then(|_| stdout.flush())
                .is_err()
            {
                break;
            }
        }
    }
    ExitCode::SUCCESS
}
