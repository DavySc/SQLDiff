//! Commands that read sources, packs and config (design section 5).
//!
//! A SOURCE is a directory, a snapshot bundle (`.json` file),
//! `git:REPO[@REV][//SUBDIR]` or `adapter:NAME`. Without one, the `[source]`
//! of the project or user config is used.

use std::io::BufReader;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand};
use sqldiff_core::{render, Report, Verdict};
use sqldiff_source::bundle::CaptureOptions;
use sqldiff_source::config::{self, Sources};
use sqldiff_source::protocol::{ObjectFilter, PairScope};
use sqldiff_source::{pairing, process, queue, Bundle, Pack, Resolved, Source, SourceSpec};

use crate::{exit_for, Format};

#[derive(Args, Clone, Default)]
pub struct ConfigArgs {
    /// Load a pack (directory with pack.toml, or a .toml file). Repeatable.
    #[arg(long = "pack", value_name = "PATH")]
    packs: Vec<PathBuf>,
    /// Project config file (default: sqldiff.toml here or in a parent directory).
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// User config file (default: $SQLDIFF_CONFIG or ~/.config/sqldiff/config.toml).
    #[arg(long, value_name = "FILE")]
    user_config: Option<PathBuf>,
    /// Ignore project and user config files not named on the command line.
    #[arg(long)]
    isolated: bool,
    /// Declare an assumption: ID=TEXT. Repeatable.
    #[arg(long = "assume", value_name = "ID=TEXT", value_parser = key_value)]
    assume: Vec<(String, String)>,
    /// Target version for a dialect: DIALECT=VERSION. Repeatable.
    #[arg(long = "dialect-version", value_name = "DIALECT=VERSION", value_parser = key_value)]
    dialect_versions: Vec<(String, String)>,
    /// Data directory for the review store and cache (default: config, else
    /// $SQLDIFF_DATA_DIR or the platform data directory).
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,
}

pub fn key_value(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, _)| !k.is_empty())
        .ok_or_else(|| format!("expected KEY=VALUE, got `{s}`"))
}

impl ConfigArgs {
    pub fn resolve(&self) -> Result<Resolved, String> {
        let sources = Sources {
            project: self.config.clone(),
            user: self.user_config.clone(),
            isolated: self.isolated,
            cwd: None,
            packs: self.packs.clone(),
            cli: {
                let mut l = config::cli_layer(&self.assume, &self.dialect_versions, None)
                    .map_err(|e| e.0)?;
                l.file.data_dir = self.data_dir.clone();
                l
            },
        };
        Resolved::load(&sources).map_err(|e| e.0)
    }
}

#[derive(Subcommand)]
pub enum SourceCommand {
    /// List the objects a source holds, with dialect, kind and content hash.
    Objects {
        source: Option<String>,
        #[arg(short, long)]
        dialect: Option<String>,
        #[arg(short, long, value_enum, default_value_t = Format::Text)]
        format: Format,
        #[command(flatten)]
        cfg: ConfigArgs,
    },
    /// Pair a source's objects across dialects by pack rules; with --diff, diff every pair.
    Pairs {
        source: Option<String>,
        #[arg(long)]
        dialect_a: Option<String>,
        #[arg(long)]
        dialect_b: Option<String>,
        /// Run the semantic diff on every pair.
        #[arg(long)]
        diff: bool,
        #[arg(short, long, value_enum, default_value_t = Format::Text)]
        format: Format,
        #[command(flatten)]
        cfg: ConfigArgs,
    },
    /// Capture a source into a snapshot bundle (offline mode and test fixtures).
    Snapshot {
        source: Option<String>,
        /// Output file.
        #[arg(short, long)]
        output: PathBuf,
        /// Also capture per-object history (git and adapter sources).
        #[arg(long)]
        history: bool,
        #[arg(short, long)]
        dialect: Option<String>,
        #[command(flatten)]
        cfg: ConfigArgs,
    },
    /// Show the merged configuration and the layers it came from.
    Config {
        #[command(flatten)]
        cfg: ConfigArgs,
    },
    /// Validate packs, or print the merged deny-list for the leak scan.
    Pack {
        #[command(subcommand)]
        command: PackCommand,
    },
    /// Serve a source over the adapter protocol on stdin and stdout.
    Serve {
        source: Option<String>,
        /// Largest get_artifact chunk in bytes.
        #[arg(long, default_value_t = sqldiff_source::protocol::DEFAULT_CHUNK)]
        max_chunk: usize,
        #[command(flatten)]
        cfg: ConfigArgs,
    },
}

#[derive(Subcommand)]
pub enum PackCommand {
    /// Load and validate packs.
    Validate { paths: Vec<PathBuf> },
    /// Print the deny-list of the loaded packs and config, one term per line.
    DenyList {
        #[command(flatten)]
        cfg: ConfigArgs,
    },
}

fn open(source: &Option<String>, cfg: &Resolved) -> Result<Box<dyn Source>, String> {
    let spec = match source {
        Some(s) => SourceSpec::parse(s).map_err(|e| e.0)?,
        None => cfg
            .source
            .clone()
            .ok_or("give a SOURCE or set [source] in sqldiff.toml")?,
    };
    sqldiff_source::open(&spec, cfg).map_err(|e| e.0)
}

pub fn run(cmd: SourceCommand) -> Result<ExitCode, String> {
    match cmd {
        SourceCommand::Objects {
            source,
            dialect,
            format,
            cfg,
        } => {
            let cfg = cfg.resolve()?;
            let src = open(&source, &cfg)?;
            let objects = src
                .list_objects(&ObjectFilter {
                    dialect,
                    ..ObjectFilter::default()
                })
                .map_err(|e| e.0)?;
            match format {
                Format::Text => {
                    for o in &objects {
                        println!(
                            "{:<9} {:<10} {}  {}{}",
                            o.dialect.as_deref().unwrap_or("?"),
                            o.kind,
                            &o.content_hash[..12],
                            o.id,
                            if o.stale { "  (stale)" } else { "" }
                        );
                    }
                }
                _ => println!("{}", serde_json::to_string_pretty(&objects).unwrap()),
            }
            Ok(ExitCode::SUCCESS)
        }
        SourceCommand::Pairs {
            source,
            dialect_a,
            dialect_b,
            diff,
            format,
            cfg,
        } => {
            let cfg = cfg.resolve()?;
            let src = open(&source, &cfg)?;
            pairs(
                src.as_ref(),
                &cfg,
                PairScope {
                    a_dialect: dialect_a,
                    b_dialect: dialect_b,
                },
                diff,
                format,
            )
        }
        SourceCommand::Snapshot {
            source,
            output,
            history,
            dialect,
            cfg,
        } => {
            let cfg = cfg.resolve()?;
            let src = open(&source, &cfg)?;
            let opts = CaptureOptions {
                filter: ObjectFilter {
                    dialect,
                    ..ObjectFilter::default()
                },
                history,
                taken_at: Some(now()),
            };
            let b = Bundle::capture(src.as_ref(), &cfg, &opts).map_err(|e| e.0)?;
            b.write(&output).map_err(|e| e.0)?;
            eprintln!(
                "{}: snapshot {} ({} objects, {} pairs)",
                output.display(),
                b.snapshot_id,
                b.objects.len(),
                b.pairs.len()
            );
            Ok(ExitCode::SUCCESS)
        }
        SourceCommand::Config { cfg } => {
            let cfg = cfg.resolve()?;
            let mut v = config::summary(&cfg);
            // The deny-list is private; `pack deny-list` prints it on request.
            if let Some(p) = v.get_mut("policy").and_then(|p| p.as_object_mut()) {
                let n = cfg.policy.deny_list.len();
                p.insert(
                    "deny_list".into(),
                    serde_json::json!(format!("{n} term(s)")),
                );
            }
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
            Ok(ExitCode::SUCCESS)
        }
        SourceCommand::Pack { command } => match command {
            PackCommand::Validate { paths } => {
                let mut failed = false;
                for p in &paths {
                    match Pack::load(p) {
                        Ok(pack) => println!(
                            "{}: ok ({} {}, {})",
                            p.display(),
                            pack.meta.name,
                            pack.meta.version,
                            &pack.hash[..12]
                        ),
                        Err(e) => {
                            failed = true;
                            eprintln!("{e}");
                        }
                    }
                }
                Ok(ExitCode::from(if failed { 1 } else { 0 }))
            }
            PackCommand::DenyList { cfg } => {
                for t in &cfg.resolve()?.policy.deny_list {
                    println!("{t}");
                }
                Ok(ExitCode::SUCCESS)
            }
        },
        SourceCommand::Serve {
            source,
            max_chunk,
            cfg,
        } => {
            let cfg = cfg.resolve()?;
            let src = open(&source, &cfg)?;
            let stdin = std::io::stdin();
            process::serve(
                src.as_ref(),
                BufReader::new(stdin.lock()),
                std::io::stdout().lock(),
                max_chunk,
            )
            .map_err(|e| e.0)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn pairs(
    src: &dyn Source,
    cfg: &Resolved,
    scope: PairScope,
    diff: bool,
    format: Format,
) -> Result<ExitCode, String> {
    if !diff {
        let objects = src
            .list_objects(&ObjectFilter::default())
            .map_err(|e| e.0)?;
        let mut p = pairing::pair(&objects, cfg, &scope).map_err(|e| e.0)?;
        // An adapter that pairs by itself wins over pack rules.
        let adapter_pairs = src.get_pairs(&scope).map_err(|e| e.0)?;
        if !adapter_pairs.is_empty() {
            p.pairs = adapter_pairs;
        }
        let source = src.describe().map_err(|e| e.0)?;
        match format {
            Format::Text => {
                for x in &p.pairs {
                    println!("{} ↔ {}  ({}: {})", x.a, x.b, x.rule, x.key);
                }
                print_leftovers(&p);
            }
            _ => println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "source": source,
                    "provenance": cfg.layers,
                    "pairing": p,
                }))
                .unwrap()
            ),
        }
        return Ok(ExitCode::SUCCESS);
    }

    let run = queue::run(src, cfg, &scope).map_err(|e| e.0)?;
    let (source, p) = (run.source, run.pairing);
    let mut reports: Vec<Report> = run.pairs.into_iter().map(|x| x.report).collect();
    reports.sort_by_key(|r| {
        std::cmp::Reverse((
            r.verdict,
            r.findings.iter().map(|f| f.severity).max(),
            r.findings.len(),
        ))
    });
    match format {
        Format::Json => println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "source": source,
                "provenance": cfg.layers,
                "pairs": reports,
                "pairing": { "unpaired": p.unpaired, "shared": p.shared, "no_dialect": p.no_dialect,
                             "ambiguous": p.ambiguous, "missing": p.missing },
            }))
            .unwrap()
        ),
        Format::Sarif => println!("{}", render::sarif(&reports)),
        Format::Text => {
            println!(
                "{:<26} {:<14} {:>8}  pair",
                "verdict", "highest", "findings"
            );
            for r in &reports {
                println!(
                    "{:<26} {:<14} {:>8}  {} ↔ {}",
                    r.verdict.label(),
                    r.highest_class().map(|c| c.label()).unwrap_or("-"),
                    r.findings.len(),
                    r.a.path,
                    r.b.as_ref().map(|b| b.path.as_str()).unwrap_or("")
                );
            }
            print_leftovers(&p);
            let names: Vec<String> = cfg
                .layers
                .iter()
                .map(|l| format!("{} {}", l.name, &l.hash[..8]))
                .collect();
            println!("\nsource: {} · config: {}", source.source, names.join(", "));
        }
    }
    let worst = reports
        .iter()
        .map(|r| r.verdict)
        .max()
        .unwrap_or(Verdict::Equal);
    let incomplete = !p.unpaired.is_empty() || !p.ambiguous.is_empty() || !p.missing.is_empty();
    Ok(exit_for(if incomplete {
        worst.max(Verdict::Unknown)
    } else {
        worst
    }))
}

fn print_leftovers(p: &pairing::Pairing) {
    for id in &p.unpaired {
        println!("unpaired: {id}");
    }
    for a in &p.ambiguous {
        println!(
            "ambiguous ({} `{}`): {}",
            a.dialect,
            a.key,
            a.ids.join(", ")
        );
    }
    for id in &p.no_dialect {
        println!("no dialect: {id}");
    }
    for id in &p.shared {
        println!("shared: {id}");
    }
    for id in &p.missing {
        println!("missing (explicit pair): {id}");
    }
}

fn now() -> String {
    // Seconds precision, UTC; the bundle's taken_at is informational.
    let t = std::time::SystemTime::now();
    sqldiff_source::fs::rfc3339(t)
}
