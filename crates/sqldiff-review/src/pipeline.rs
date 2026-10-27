//! The pipeline `diff`, `check`, `changes` and the app's checks share: the
//! engine, then pack policy, author markers and review suppression.

use sqldiff_core::{Artifact, Options, Report};
use sqldiff_source::compose::{self, TemplateDir};
use sqldiff_source::{policy, Resolved};

use crate::{markers, Cache, Redactor, Store};

/// What the pipeline applies besides the engine.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    /// Report findings accepted in the review store instead of suppressing them.
    pub no_suppress: bool,
    /// Do not read or write the result cache.
    pub no_cache: bool,
    /// Replace pack-derived identifiers with stable placeholders.
    pub redact: bool,
    /// With `redact`, drop source excerpts instead of redacting them.
    pub drop_excerpts: bool,
}

/// Runs the engine, then pack policy, author markers and review suppression.
pub struct Pipeline {
    pub cfg: Resolved,
    /// Problems found opening the review store, for the caller to show.
    pub warnings: Vec<String>,
    store: Option<Store>,
    cache: Cache,
    redactor: Option<Redactor>,
    now: String,
    /// Check mode for composed output (`--templates`).
    pub templates: Option<TemplateDir>,
}

impl Pipeline {
    pub fn new(cfg: Resolved, args: &Settings) -> Result<Pipeline, String> {
        let mut warnings = Vec::new();
        let store = if args.no_suppress {
            None
        } else {
            let s = Store::from_config(&cfg).map_err(|e| e.0)?;
            warnings.extend(s.warnings().iter().map(|w| format!("review store: {w}")));
            Some(s)
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
            warnings,
            store,
            cache,
            redactor,
            now: crate::time::now(),
            templates: None,
        })
    }

    pub fn options(&self) -> Options {
        self.cfg.options()
    }

    /// `diff` (or `check` when `b` is `None`) with everything applied,
    /// session flips last so they also reach assumptions that markers add.
    pub fn run(&self, a: &Artifact, b: Option<&Artifact>, opts: &Options) -> Report {
        let mut r = self.run_scenario(a, b, opts);
        sqldiff_core::ledger::relabel(&mut r, &opts.flips);
        r
    }

    /// [`run`](Pipeline::run) with only the scenario flips in `opts` (they
    /// re-run the behavioural analysis); the others are not applied.
    pub fn run_scenario(&self, a: &Artifact, b: Option<&Artifact>, opts: &Options) -> Report {
        let opts = Options {
            flips: sqldiff_core::ledger::scenario_flips(&opts.flips),
            ..opts.clone()
        };
        let opts = &opts;
        let mut r = self.cache.report(a, b, opts, &self.cfg, || match b {
            Some(b) => sqldiff_core::diff(a, b, opts),
            None => sqldiff_core::check(a, opts),
        });
        compose::apply(
            &mut r,
            a,
            b,
            &self.cfg.policy,
            self.templates.as_ref(),
            opts,
        );
        policy::add_limits(&mut r, a, b, &self.cfg);
        policy::apply(&mut r, &self.cfg);
        markers::apply(&mut r, &a.text, b.map(|b| b.text.as_str()), &self.cfg);
        if let Some(s) = &self.store {
            crate::suppress(&mut r, s, &self.now);
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

    /// Redacted copies of reports, each checked for leftover terms.
    pub fn redact_reports(&mut self, reports: Vec<Report>) -> Result<Vec<Report>, String> {
        let Some(red) = &mut self.redactor else {
            return Ok(reports);
        };
        // Register every report's terms before redacting any, so a name
        // from one pair is also replaced in another pair's witnesses.
        for r in &reports {
            red.add_report(r);
        }
        reports
            .iter()
            .map(|r| {
                let out = red.redact_report(r);
                red.verify_report(&out).map_err(|e| e.0)?;
                Ok(out)
            })
            .collect()
    }

    /// Redacts and checks the string values of other output.
    pub fn redact_json(&mut self, v: &mut serde_json::Value) -> Result<(), String> {
        if let Some(r) = &mut self.redactor {
            r.redact_value(v);
            r.verify_value(v).map_err(|e| e.0)?;
        }
        Ok(())
    }
}
