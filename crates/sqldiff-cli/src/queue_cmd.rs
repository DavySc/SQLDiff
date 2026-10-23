//! Review commands that mirror the MCP tools (design section 6.1): `queue`,
//! `assumptions`, `comment` and `open`. Assumption flips (`--deny`,
//! `--confirm`) last for one invocation only and never change packs or config.
//! Findings accepted in the review store or marked as deliberate in source
//! are suppressed, as in `diff`.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand};
use serde::de::DeserializeOwned;
use sqldiff_core::{phrase, ChangeClass, Dialect, Report, Severity, Verdict};
use sqldiff_review::{markers, Lookup, Store};
use sqldiff_source::protocol::PairScope;
use sqldiff_source::queue::{self, Filter, Flips, ReviewFilter, Status};
use sqldiff_source::{Resolved, SourceSpec};

use crate::review_cmd::{Pipeline, ReviewArgs};
use crate::source_cmd::ConfigArgs;
use crate::{exit_for, read, Format};

/// A value spelled as in JSON output, e.g. `transactional` or `equal_under_assumptions`.
fn snake<T: DeserializeOwned>(s: &str) -> Result<T, String> {
    serde_json::from_value(serde_json::Value::String(s.replace('-', "_")))
        .map_err(|e| e.to_string())
}

#[derive(Args, Clone, Default)]
pub struct FlipArgs {
    /// Treat an assumption as not holding for this run. Repeatable.
    #[arg(long = "deny", value_name = "ID")]
    deny: Vec<String>,
    /// Treat an assumption as confirmed for this run. Repeatable.
    #[arg(long = "confirm", value_name = "ID")]
    confirm: Vec<String>,
}

impl FlipArgs {
    fn flips(&self) -> Flips {
        let mut f = Flips::new();
        for id in &self.confirm {
            f.insert(id.clone(), true);
        }
        for id in &self.deny {
            f.insert(id.clone(), false);
        }
        f
    }
}

#[derive(Args, Clone)]
pub struct PairArgs {
    a: PathBuf,
    b: PathBuf,
    /// Dialect of both sides (overridden by --dialect-a / --dialect-b).
    #[arg(short, long)]
    dialect: Option<Dialect>,
    #[arg(long)]
    dialect_a: Option<Dialect>,
    #[arg(long)]
    dialect_b: Option<Dialect>,
    #[command(flatten)]
    cfg: ConfigArgs,
}

impl PairArgs {
    /// Diffs the pair as `diff` does (pack policy, author markers, review
    /// suppression); returns the report before flips and the config.
    fn report(&self) -> Result<(Report, Resolved), String> {
        let pipe = Pipeline::new(self.cfg.resolve()?, &ReviewArgs::default())?;
        let a = read(&self.a, self.dialect_a.or(self.dialect), &pipe.cfg)?;
        let b = read(&self.b, self.dialect_b.or(self.dialect), &pipe.cfg)?;
        let r = pipe.run(&a, Some(&b), &pipe.options());
        Ok((r, pipe.cfg))
    }
}

#[derive(Subcommand)]
pub enum QueueCommand {
    /// Pair every object of a source, diff each pair and rank them for
    /// review: verdict, severity, verdict-deciding assumptions, recency, size.
    Queue {
        /// A directory, snapshot bundle, git:REPO[@REV][//SUBDIR] or adapter:NAME (default: [source] in config).
        source: Option<String>,
        #[arg(long)]
        dialect_a: Option<String>,
        #[arg(long)]
        dialect_b: Option<String>,
        /// Keep pairs with a finding of this class. Repeatable.
        #[arg(long = "class", value_name = "CLASS", value_parser = snake::<ChangeClass>)]
        classes: Vec<ChangeClass>,
        /// Keep pairs with a finding at least this severe.
        #[arg(long, value_parser = snake::<Severity>)]
        min_severity: Option<Severity>,
        /// Keep pairs with this verdict. Repeatable.
        #[arg(long = "verdict", value_name = "VERDICT", value_parser = snake::<Verdict>)]
        verdicts: Vec<Verdict>,
        /// Keep pairs where either object id starts with this.
        #[arg(long)]
        folder: Option<String>,
        /// `unreviewed` or `reviewed`.
        #[arg(long, value_parser = snake::<ReviewFilter>)]
        review: Option<ReviewFilter>,
        /// Show at most this many pairs (the rest are counted).
        #[arg(long)]
        limit: Option<usize>,
        #[command(flatten)]
        flips: FlipArgs,
        #[arg(short, long, value_enum, default_value_t = Format::Text)]
        format: Format,
        #[command(flatten)]
        cfg: ConfigArgs,
    },
    /// The assumption ledger of a pair: open, declared, confirmed or denied,
    /// the findings relying on each and which ones decide the verdict.
    Assumptions {
        #[command(flatten)]
        pair: PairArgs,
        #[command(flatten)]
        flips: FlipArgs,
        #[arg(short, long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
    /// Draft a plain-text review comment for a pair's findings from the
    /// deterministic phrase grammar.
    Comment {
        #[command(flatten)]
        pair: PairArgs,
        /// Only these findings (fingerprints). Repeatable; default: all.
        #[arg(long = "finding", value_name = "FINGERPRINT")]
        findings: Vec<String>,
        /// Excerpt lines quoted per location.
        #[arg(long, default_value_t = 1)]
        excerpt_lines: usize,
        /// Leave fingerprints out of the text.
        #[arg(long)]
        no_fingerprints: bool,
        #[command(flatten)]
        flips: FlipArgs,
    },
    /// Print a GUI deep link for a pair (and finding), and the command that serves it.
    Open {
        #[command(flatten)]
        pair: PairArgs,
        /// Finding to select (fingerprint).
        #[arg(long)]
        finding: Option<String>,
        /// GUI address (default: $SQLDIFF_GUI_URL, else http://127.0.0.1:7878).
        #[arg(long)]
        gui_url: Option<String>,
    },
}

pub fn run(cmd: QueueCommand) -> Result<ExitCode, String> {
    match cmd {
        QueueCommand::Queue {
            source,
            dialect_a,
            dialect_b,
            classes,
            min_severity,
            verdicts,
            folder,
            review,
            limit,
            flips,
            format,
            cfg,
        } => {
            let cfg = cfg.resolve()?;
            let spec = match source {
                Some(s) => SourceSpec::parse(&s).map_err(|e| e.0)?,
                None => cfg
                    .source
                    .clone()
                    .ok_or("give a SOURCE or set [source] in sqldiff.toml")?,
            };
            let src = sqldiff_source::open(&spec, &cfg).map_err(|e| e.0)?;
            let store = Store::from_config(&cfg).map_err(|e| e.0)?;
            let now = sqldiff_review::time::now();
            let run = queue::run_with(
                src.as_ref(),
                &cfg,
                &PairScope {
                    a_dialect: dialect_a,
                    b_dialect: dialect_b,
                },
                &mut |r, a, b| {
                    markers::apply(r, &a.text, Some(&b.text), &cfg);
                    sqldiff_review::suppress(r, &store, &now);
                },
            )
            .map_err(|e| e.0)?;
            let flips = flips.flips();
            let filter = Filter {
                classes,
                min_severity,
                verdicts,
                folder,
                review,
            };
            let lookup = Lookup {
                store: &store,
                now: now.clone(),
            };
            let rows = queue::entries(&run.pairs, &flips, &filter, &lookup);
            let shown = &rows[..rows.len().min(limit.unwrap_or(usize::MAX))];
            match format {
                Format::Text => {
                    println!(
                        "{:<14} {:<24} {:<24} {:>8}  pair",
                        "id", "verdict", "highest", "findings"
                    );
                    for e in shown {
                        let highest = match (e.highest_severity, e.highest_class) {
                            (Some(s), Some(c)) => format!("{} {}", s.label(), c.label()),
                            _ => "-".to_string(),
                        };
                        println!(
                            "{:<14} {:<24} {:<24} {:>8}  {} ↔ {}",
                            e.id,
                            e.verdict.label(),
                            highest,
                            e.findings,
                            e.a,
                            e.b
                        );
                        if !e.verdict_flipping.is_empty() {
                            println!("{:<14} verdict hinges on {}", "", e.verdict_flipping.join(", "));
                        }
                    }
                    if rows.len() > shown.len() {
                        println!("… {} more", rows.len() - shown.len());
                    }
                }
                Format::Json => println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "source": run.source,
                        "provenance": cfg.layers,
                        "flips": flips,
                        "pairs": shown,
                        "omitted_pairs": rows.len() - shown.len(),
                        "pairing": { "unpaired": run.pairing.unpaired, "shared": run.pairing.shared,
                                     "no_dialect": run.pairing.no_dialect, "ambiguous": run.pairing.ambiguous,
                                     "missing": run.pairing.missing },
                    }))
                    .unwrap()
                ),
                Format::Sarif => {
                    let reports: Vec<Report> = run
                        .pairs
                        .iter()
                        .filter(|p| shown.iter().any(|e| e.id == p.id))
                        .map(|p| queue::view(&p.report, &flips))
                        .collect();
                    println!("{}", sqldiff_core::render::sarif(&reports));
                }
            }
            // Over every matching pair, not just those shown under --limit.
            let worst = rows
                .iter()
                .map(|e| e.verdict)
                .max()
                .unwrap_or(Verdict::Equal);
            Ok(exit_for(worst))
        }
        QueueCommand::Assumptions {
            pair,
            flips,
            format,
        } => {
            let (raw, _) = pair.report()?;
            let flips = flips.flips();
            let view = queue::view(&raw, &flips);
            let ledger = queue::ledger(&raw, &flips);
            match format {
                Format::Text => {
                    println!("Verdict: {}", view.verdict.label().to_uppercase());
                    if ledger.is_empty() {
                        println!("No assumptions.");
                    }
                    for e in &ledger {
                        let status = match e.status {
                            Status::Open => "open",
                            Status::Declared => "declared",
                            Status::Confirmed => "confirmed",
                            Status::Denied => "denied",
                        };
                        println!("\n{}  [{status}]\n    {}", e.id, e.text);
                        if e.flips_verdict {
                            println!(
                                "    flipping it makes the verdict {}",
                                e.verdict_if_flipped.label()
                            );
                        }
                        if !e.findings.is_empty() {
                            println!("    relied on by {}", e.findings.join(", "));
                        }
                    }
                }
                _ => println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "verdict": view.verdict,
                        "flips": flips,
                        "assumptions": ledger,
                    }))
                    .unwrap()
                ),
            }
            Ok(exit_for(view.verdict))
        }
        QueueCommand::Comment {
            pair,
            findings,
            excerpt_lines,
            no_fingerprints,
            flips,
        } => {
            let (raw, cfg) = pair.report()?;
            let flips = flips.flips();
            let view = queue::view(&raw, &flips);
            let unknown: Vec<&String> = findings
                .iter()
                .filter(|fp| !view.findings.iter().any(|f| &f.fingerprint == *fp))
                .collect();
            if !unknown.is_empty() {
                return Err(format!(
                    "no such finding in this pair: {}",
                    unknown
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            let denied = queue::denied(&flips);
            let group = phrase::Group {
                label: String::new(),
                findings: view
                    .findings
                    .iter()
                    .filter(|f| findings.is_empty() || findings.contains(&f.fingerprint))
                    .collect(),
                assumptions: &view.assumptions,
                denied: &denied,
            };
            print!(
                "{}",
                phrase::comment(
                    &[group],
                    &phrase::Style {
                        excerpt_lines: excerpt_lines.min(cfg.excerpt_lines),
                        fingerprints: !no_fingerprints,
                    }
                )
            );
            Ok(ExitCode::SUCCESS)
        }
        QueueCommand::Open {
            pair,
            finding,
            gui_url,
        } => {
            let (raw, _) = pair.report()?;
            if let Some(fp) = &finding {
                if !raw.findings.iter().any(|f| &f.fingerprint == fp) {
                    return Err(format!("no finding {fp} in this pair"));
                }
            }
            let base = gui_url
                .or_else(|| std::env::var("SQLDIFF_GUI_URL").ok())
                .unwrap_or_else(|| queue::DEFAULT_GUI_URL.to_string());
            let (a, b) = (pair.a.display().to_string(), pair.b.display().to_string());
            println!(
                "{}",
                queue::deep_link(&base, &a, Some(&b), finding.as_deref())
            );
            eprintln!(
                "If the GUI is not running: sqldiff-gui serve --dialect-a {} --dialect-b {} {} {}",
                raw.a.dialect,
                raw.b.as_ref().map(|x| x.dialect.as_str()).unwrap_or(""),
                queue::shell_word(&a),
                queue::shell_word(&b)
            );
            Ok(ExitCode::SUCCESS)
        }
    }
}
