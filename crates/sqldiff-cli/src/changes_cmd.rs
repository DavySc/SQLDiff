//! `sqldiff changes`: review the SQL a branch changed (the pull request
//! workflow). Only changed files are read and diffed, base against head.
//! SARIF results point at the head revision, so code scanning can show
//! every one of them on the pull request; the Markdown summary is the
//! pull request comment. The review itself is `sqldiff_review::branch`,
//! which the app's runs call too.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, ValueEnum};
use sqldiff_core::hooks::Hooks;
use sqldiff_core::{Dialect, Verdict};
use sqldiff_review::branch;
use sqldiff_source::changes::ChangeScope;

use crate::review_cmd::{self, ReviewArgs};
use crate::source_cmd::ConfigArgs;
use crate::Format;

/// When the command exits 1.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FailOn {
    /// A change is different, or not proven equal, or was not compared.
    Unknown,
    /// A change is proven different.
    Different,
    /// Never: exit 0 whatever the verdict (2 still means an error).
    Never,
}

#[derive(Args)]
pub struct ChangesArgs {
    /// Base revision: the branch or commit the change is reviewed against.
    /// The diff starts at its merge base with HEAD, as a pull request does.
    #[arg(long)]
    base: String,
    /// Head revision.
    #[arg(long, default_value = "HEAD")]
    head: String,
    /// The git repository.
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Dialect of every changed file (default: per file, from `dialect_map`
    /// or the path convention).
    #[arg(short, long)]
    dialect: Option<Dialect>,
    /// Format on standard output.
    #[arg(short, long, value_enum, default_value_t = Format::Text)]
    format: Format,
    /// Also write SARIF 2.1.0 here, every result located at the head revision.
    #[arg(long, value_name = "FILE")]
    sarif: Option<PathBuf>,
    /// Also write the Markdown summary (the pull request comment) here.
    #[arg(long, value_name = "FILE")]
    markdown: Option<PathBuf>,
    /// Also write the JSON report here.
    #[arg(long, value_name = "FILE")]
    json: Option<PathBuf>,
    /// What makes the command exit 1.
    #[arg(long, value_enum, default_value_t = FailOn::Unknown)]
    fail_on: FailOn,
    #[command(flatten)]
    cfg: ConfigArgs,
    #[command(flatten)]
    review: ReviewArgs,
    /// Only these paths (git pathspecs).
    #[arg(last = true, value_name = "PATH")]
    paths: Vec<String>,
}

pub fn run(args: ChangesArgs) -> Result<ExitCode, String> {
    let cfg = args.cfg.resolve()?;
    let pipe = review_cmd::pipeline(cfg, &args.review)?;
    let review = branch::review(
        &ChangeScope {
            repo: &args.repo,
            base: &args.base,
            head: &args.head,
            dialect: args.dialect,
            paths: &args.paths,
        },
        &pipe,
        &Hooks::default(),
    )?;
    let worst = review.verdict();
    if let Some(p) = &args.sarif {
        write(p, &review.sarif())?;
    }
    if let Some(p) = &args.markdown {
        write(p, &review.markdown())?;
    }
    if let Some(p) = &args.json {
        write(p, &review.json())?;
    }
    print!(
        "{}",
        match args.format {
            Format::Text => review.text(),
            Format::Json => review.json(),
            Format::Sarif => review.sarif(),
        }
    );
    let fail = match args.fail_on {
        FailOn::Unknown => worst >= Verdict::Unknown,
        FailOn::Different => worst == Verdict::Different,
        FailOn::Never => false,
    };
    Ok(ExitCode::from(u8::from(fail)))
}

/// Writes `text` to `path`, making its folder if there is none, so the
/// command on a run's sheet works on a machine that has not seen the folder.
fn write(path: &Path, text: &str) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("{}: {e}", path.display());
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(fail)?;
    }
    std::fs::write(path, text).map_err(fail)
}
