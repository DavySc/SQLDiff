//! Review store, suppression, redacted export and cache in the CLI
//! (design sections 5.5, 6.3 and 10).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand};
use sqldiff_core::artifact::content_hash;
use sqldiff_core::{Artifact, Options, Report};
use sqldiff_review::{markers, Cache, Decision, Redactor, State, Store, ANY_SNAPSHOT};
use sqldiff_source::{policy, Resolved};

use crate::source_cmd::ConfigArgs;
use crate::Format;

/// Flags shared by `diff`, `check` and `tree`.
#[derive(Args, Clone, Default)]
pub struct ReviewArgs {
    /// Report findings accepted in the review store instead of suppressing them.
    #[arg(long)]
    no_suppress: bool,
    /// Do not read or write the result cache.
    #[arg(long)]
    no_cache: bool,
    /// Replace pack-derived identifiers (objects, schemas, paths, work items,
    /// deny-listed terms) with stable placeholders.
    #[arg(long)]
    redact: bool,
    /// With --redact, drop source excerpts instead of redacting them.
    #[arg(long, requires = "redact")]
    drop_excerpts: bool,
}

/// Runs the engine, then pack policy, author markers and review suppression.
pub struct Pipeline {
    pub cfg: Resolved,
    store: Option<Store>,
    cache: Cache,
    redactor: Option<Redactor>,
    now: String,
}

impl Pipeline {
    pub fn new(cfg: Resolved, args: &ReviewArgs) -> Result<Pipeline, String> {
        let store = if args.no_suppress {
            None
        } else {
            Some(Store::from_config(&cfg).map_err(|e| e.0)?)
        };
        let cache = if args.no_cache {
            Cache::disabled()
        } else {
            Cache::from_config(&cfg).map_err(|e| e.0)?
        };
        let redactor = if args.redact {
            let mut r = Redactor::from_config(&cfg).map_err(|e| e.0)?;
            r.options.drop_excerpts = args.drop_excerpts;
            Some(r)
        } else {
            None
        };
        Ok(Pipeline {
            cfg,
            store,
            cache,
            redactor,
            now: sqldiff_review::time::now(),
        })
    }

    pub fn options(&self) -> Options {
        self.cfg.options()
    }

    /// `diff` (or `check` when `b` is `None`) with everything applied.
    pub fn run(&self, a: &Artifact, b: Option<&Artifact>, opts: &Options) -> Report {
        let mut r = self.cache.report(a, b, opts, &self.cfg, || match b {
            Some(b) => sqldiff_core::ledger::evaluate(a, b, opts),
            None => sqldiff_core::check(a, opts),
        });
        policy::add_limits(&mut r, a, b, &self.cfg);
        policy::apply(&mut r, &self.cfg);
        markers::apply(&mut r, &a.text, b.map(|b| b.text.as_str()), &self.cfg);
        if let Some(s) = &self.store {
            sqldiff_review::suppress(&mut r, s, &self.now);
        }
        r
    }

    pub fn redacting(&self) -> bool {
        self.redactor.is_some()
    }

    /// Registers paths that appear in output outside any report.
    pub fn add_paths<'a>(&mut self, paths: impl IntoIterator<Item = &'a str>) {
        if let Some(r) = &mut self.redactor {
            for p in paths {
                r.add_path(p);
            }
        }
    }

    pub fn redact_reports(&mut self, reports: Vec<Report>) -> Vec<Report> {
        let Some(red) = &mut self.redactor else {
            return reports;
        };
        // Register every report's terms before redacting any, so a name
        // from one pair is also replaced in another pair's witnesses.
        for r in &reports {
            red.add_report(r);
        }
        reports.iter().map(|r| red.redact_report(r)).collect()
    }

    pub fn redact_json(&mut self, v: &mut serde_json::Value) {
        if let Some(r) = &mut self.redactor {
            r.redact_value(v);
        }
    }

    /// Checks redacted output for leftover terms before it is printed.
    pub fn verify(&mut self, out: &str) -> Result<(), String> {
        match &mut self.redactor {
            Some(r) => r.verify(out).map_err(|e| e.0),
            None => Ok(()),
        }
    }
}

#[derive(Subcommand)]
pub enum ReviewCommand {
    /// Accept a finding as a deliberate difference: it is suppressed from now on.
    Accept(DecideArgs),
    /// Mark a finding as a real problem.
    Reject(DecideArgs),
    /// Comment on a finding without deciding it.
    Comment(DecideArgs),
    /// Remove the decision for a finding.
    Clear {
        fingerprint: String,
        #[command(flatten)]
        snap: SnapshotArgs,
        #[command(flatten)]
        cfg: ConfigArgs,
    },
    /// List decisions.
    List {
        /// Only decisions in this state.
        #[arg(long)]
        state: Option<State>,
        /// Only decisions for this fingerprint.
        #[arg(long)]
        fingerprint: Option<String>,
        #[arg(short, long, value_enum, default_value_t = Format::Text)]
        format: Format,
        #[command(flatten)]
        cfg: ConfigArgs,
    },
    /// Rewrite the store with one record per decision, dropping expired ones.
    Compact {
        #[command(flatten)]
        cfg: ConfigArgs,
    },
    /// Delete the result cache.
    ClearCache {
        #[command(flatten)]
        cfg: ConfigArgs,
    },
}

#[derive(Args, Clone)]
pub struct SnapshotArgs {
    /// Snapshot the decision applies to (`<hash_a>:<hash_b>`, as in JSON
    /// output). Default: every snapshot.
    #[arg(long, conflicts_with = "pair")]
    snapshot: Option<String>,
    /// Compute the snapshot from the two files of the pair (one for a check).
    #[arg(long, num_args = 1..=2, value_name = "FILE")]
    pair: Vec<PathBuf>,
}

impl SnapshotArgs {
    fn resolve(&self) -> Result<String, String> {
        if let Some(s) = &self.snapshot {
            return Ok(s.clone());
        }
        if self.pair.is_empty() {
            return Ok(ANY_SNAPSHOT.to_string());
        }
        let hashes = self
            .pair
            .iter()
            .map(|p| {
                std::fs::read_to_string(p)
                    .map(|t| content_hash(&t))
                    .map_err(|e| format!("{}: {e}", p.display()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(hashes.join(":"))
    }
}

#[derive(Args, Clone)]
pub struct DecideArgs {
    fingerprint: String,
    #[command(flatten)]
    snap: SnapshotArgs,
    #[arg(short, long)]
    comment: Option<String>,
    /// Default: $SQLDIFF_AUTHOR, else $USER.
    #[arg(long)]
    author: Option<String>,
    /// The decision lapses at this time (YYYY-MM-DD or RFC 3339).
    #[arg(long)]
    expires: Option<String>,
    /// Rule of the finding, shown in listings.
    #[arg(long)]
    rule: Option<String>,
    /// Object of the finding, shown in listings.
    #[arg(long)]
    object: Option<String>,
    #[command(flatten)]
    cfg: ConfigArgs,
}

pub fn run(cmd: ReviewCommand) -> Result<ExitCode, String> {
    match cmd {
        ReviewCommand::Accept(a) => decide(State::Accepted, a),
        ReviewCommand::Reject(a) => decide(State::Rejected, a),
        ReviewCommand::Comment(a) => {
            if a.comment.is_none() {
                return Err("comment needs --comment".into());
            }
            decide(State::Commented, a)
        }
        ReviewCommand::Clear {
            fingerprint,
            snap,
            cfg,
        } => {
            let mut store = Store::from_config(&cfg.resolve()?).map_err(|e| e.0)?;
            let snapshot = snap.resolve()?;
            if store.clear(&fingerprint, &snapshot).map_err(|e| e.0)? {
                eprintln!("cleared {fingerprint} @ {snapshot}");
                Ok(ExitCode::from(0))
            } else {
                eprintln!("no decision for {fingerprint} @ {snapshot}");
                Ok(ExitCode::from(1))
            }
        }
        ReviewCommand::List {
            state,
            fingerprint,
            format,
            cfg,
        } => {
            let store = Store::from_config(&cfg.resolve()?).map_err(|e| e.0)?;
            let now = sqldiff_review::time::now();
            let rows: Vec<_> = store
                .all()
                .into_iter()
                .filter(|(k, d)| {
                    state.map_or(true, |s| s == d.state)
                        && fingerprint.as_ref().map_or(true, |f| *f == k.fingerprint)
                })
                .collect();
            match format {
                Format::Text => {
                    for (k, d) in &rows {
                        println!(
                            "{} {:<9} {}{}  {}  {}{}",
                            k.fingerprint,
                            d.state.label(),
                            if d.is_expired(&now) { "(expired) " } else { "" },
                            d.at,
                            short_snapshot(&k.snapshot),
                            d.rule.as_deref().unwrap_or(""),
                            d.comment
                                .as_deref()
                                .map(|c| format!("  {c}"))
                                .unwrap_or_default()
                        );
                    }
                }
                Format::Json | Format::Sarif => {
                    let v: Vec<_> = rows
                        .iter()
                        .map(|(k, d)| {
                            serde_json::json!({
                                "fingerprint": k.fingerprint,
                                "snapshot": k.snapshot,
                                "expired": d.is_expired(&now),
                                "decision": d,
                            })
                        })
                        .collect();
                    println!("{}", serde_json::to_string_pretty(&v).unwrap());
                }
            }
            Ok(ExitCode::from(0))
        }
        ReviewCommand::Compact { cfg } => {
            let mut store = Store::from_config(&cfg.resolve()?).map_err(|e| e.0)?;
            let dropped = store
                .compact(&sqldiff_review::time::now())
                .map_err(|e| e.0)?;
            eprintln!(
                "{} decision(s) kept, {dropped} expired dropped",
                store.all().len()
            );
            Ok(ExitCode::from(0))
        }
        ReviewCommand::ClearCache { cfg } => {
            let cache = Cache::from_config(&cfg.resolve()?).map_err(|e| e.0)?;
            let had = cache.clear().map_err(|e| e.0)?;
            eprintln!(
                "{} {}",
                if had { "removed" } else { "no cache at" },
                cache.dir().display()
            );
            Ok(ExitCode::from(0))
        }
    }
}

fn short_snapshot(s: &str) -> String {
    if s == ANY_SNAPSHOT {
        return "any snapshot".into();
    }
    s.split(':')
        .map(|h| &h[..h.len().min(8)])
        .collect::<Vec<_>>()
        .join(":")
}

fn decide(state: State, a: DecideArgs) -> Result<ExitCode, String> {
    let cfg = a.cfg.resolve()?;
    let mut store = Store::from_config(&cfg).map_err(|e| e.0)?;
    let snapshot = a.snap.resolve()?;
    let mut d = Decision::new(state);
    d.comment = a.comment;
    d.author = a
        .author
        .or_else(|| std::env::var("SQLDIFF_AUTHOR").ok())
        .or_else(|| std::env::var("USER").ok())
        .filter(|s| !s.is_empty());
    d.rule = a.rule;
    d.object = a.object;
    if let Some(e) = &a.expires {
        d = d.expires(e).map_err(|e| e.0)?;
    }
    store.put(&a.fingerprint, &snapshot, d).map_err(|e| e.0)?;
    eprintln!(
        "{} {} @ {}",
        state.label(),
        a.fingerprint,
        short_snapshot(&snapshot)
    );
    Ok(ExitCode::from(0))
}
