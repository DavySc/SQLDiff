//! `sqldiff instances`: weave every instance the generator produces from the
//! source templates and from the converted templates, and check each pair.
//! The check itself is `sqldiff_convert::instances`, which the app's runs
//! use too.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args as ClapArgs;
use sqldiff_convert::instances::{self, Checked};
use sqldiff_convert::tree::Hooks;
use sqldiff_core::{Dialect, Options};

use crate::source_cmd;

#[derive(ClapArgs, Clone)]
pub struct InstancesArgs {
    /// The instance manifest (TOML): every result the generator produces,
    /// with its templates and parameter values.
    pub(crate) manifest: PathBuf,
    /// The source templates (the tree the generator reads today).
    #[arg(long, value_name = "DIR", required_unless_present = "generator_only")]
    pub(crate) source: Option<PathBuf>,
    /// The converted templates (`sqldiff convert --template` output, or
    /// templates written by hand), with the same layout.
    #[arg(long, value_name = "DIR", required_unless_present = "generator_only")]
    pub(crate) target: Option<PathBuf>,
    #[arg(long, default_value = "tsql")]
    pub(crate) from: Dialect,
    #[arg(long)]
    pub(crate) to: Dialect,
    /// Output directory for the woven instances and the report.
    #[arg(short, long)]
    pub(crate) out: PathBuf,
    /// The generator's own output: each instance's source weave must equal
    /// its `output` file there, and every file there must belong to an
    /// instance.
    #[arg(long, value_name = "DIR")]
    pub(crate) woven: Option<PathBuf>,
    /// The generator's own output for the target dialect: each instance's
    /// target weave must equal its `output` file there. An instance with no
    /// file there is checked on SQLDiff's weave only and never done.
    #[arg(long, value_name = "DIR")]
    pub(crate) woven_target: Option<PathBuf>,
    /// Check the generator's output alone, when the generator was changed
    /// for the target: each instance's `output` under --woven against the
    /// same file under --woven-target. No templates are woven.
    #[arg(
        long,
        requires_all = ["woven", "woven_target"],
        conflicts_with_all = ["source", "target"]
    )]
    pub(crate) generator_only: bool,
    /// A previous run's `instance-report.json` (or its output directory):
    /// instances whose woven text, generator output and values are what
    /// they were then reuse its results, when everything else the checks
    /// read (catalogs, options, the sqldiff build) is the same too.
    /// The report is trusted as a cache: give only one your own run wrote.
    #[arg(long, value_name = "PATH")]
    pub(crate) since: Option<PathBuf>,
    /// A parameter name (repeatable), added to the names the manifest gives
    /// values for; under `placeholders = "parameters"` only these are
    /// placeholders.
    #[arg(long = "parameter", value_name = "NAME")]
    pub(crate) parameters: Vec<String>,
    /// An assumption you know holds (repeatable), e.g. ASM-DBO-SCHEMA: a
    /// status line in the converted templates that lists only accepted
    /// assumptions no longer holds an instance back. Listed in the report.
    #[arg(long = "accept", value_name = "ID")]
    pub(crate) accept: Vec<String>,
    /// Parallel jobs (default: available cores).
    #[arg(short, long)]
    pub(crate) jobs: Option<usize>,
    /// Replace files in a non-empty output directory.
    #[arg(long)]
    pub(crate) overwrite: bool,
    #[command(flatten)]
    pub(crate) catalogs: source_cmd::CatalogArgs,
    #[command(flatten)]
    pub(crate) cfg: source_cmd::ConfigArgs,
}

impl InstancesArgs {
    /// The library's arguments: config resolved, catalogs loaded.
    pub(crate) fn resolve(self) -> Result<instances::Args, String> {
        let cfg = self.cfg.resolve()?;
        let mut options = Options::default();
        self.catalogs
            .apply(&mut options, self.from, Some(self.to))?;
        Ok(instances::Args {
            manifest: self.manifest,
            source: self.source,
            target: self.target,
            from: self.from,
            to: self.to,
            out: self.out,
            woven: self.woven,
            woven_target: self.woven_target,
            generator_only: self.generator_only,
            since: self.since,
            parameters: self.parameters,
            accept: self.accept,
            jobs: self.jobs,
            overwrite: self.overwrite,
            options,
            cfg,
        })
    }
}

pub fn run(a: InstancesArgs) -> Result<ExitCode, String> {
    let ok = check(a)?.0;
    Ok(ExitCode::from(if ok { 0 } else { 1 }))
}

/// Runs the check, prints its summary and says what it wrote: whether every
/// instance is converted (under assumptions at worst) and all generator
/// output is accounted for, and the count of instances per status.
pub(crate) fn check(a: InstancesArgs) -> Result<(bool, BTreeMap<&'static str, usize>), String> {
    let c: Checked = instances::check(&a.resolve()?, &Hooks::default())?;
    for n in &c.notes {
        eprintln!("{n}");
    }
    print!("{}", c.summary);
    let [json, md] = &c.reports;
    eprintln!("wrote {} and {}", json.display(), md.display());
    Ok((c.ok, c.totals))
}
