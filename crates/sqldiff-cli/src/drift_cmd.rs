//! `sqldiff drift`: keep the template trees of several dialects in step.
//!
//! A generator with one template set per dialect drifts when a template is
//! added, changed or rewired in one tree and not in the others. Three
//! checks, all from the files alone:
//!
//! - every template is in every tree (`drift.missing`);
//! - its placeholders are the same names in every tree
//!   (`drift.placeholders`), so one set of values weaves all of them;
//! - its directives (the pack's special forms, with their arguments) are the
//!   same, in the same order, in every tree (`drift.directives`).
//!
//! With `--base REV`, a template changed since the merge base in some trees
//! and not in the others is `drift.changed-alone`, unless the pack's
//! `intentional_difference` author marker in that template says why. With
//! `--manifest`, every instance is also woven in the reference tree and in
//! each other tree and checked like `sqldiff instances`. The check is
//! `sqldiff_convert::drift`, which the app's runs use too.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args as ClapArgs;
use sqldiff_convert::drift;
use sqldiff_core::hooks::Hooks;
use sqldiff_core::{Dialect, Options};

use crate::source_cmd;

#[derive(ClapArgs)]
pub struct DriftArgs {
    /// A dialect's template tree, as DIALECT=DIR (repeatable, at least two):
    /// `--tree tsql=templates/mssql --tree postgres=templates/pg`.
    #[arg(long = "tree", value_name = "DIALECT=DIR", value_parser = tree, required = true)]
    trees: Vec<(Dialect, PathBuf)>,
    /// The tree the others are checked against (default: the first).
    #[arg(long)]
    reference: Option<Dialect>,
    /// Output directory for the report (and the instance checks).
    #[arg(short, long)]
    out: PathBuf,
    /// Template file extension (repeatable; default: sql), any letter case,
    /// without the dot.
    #[arg(long = "extension", value_name = "EXT")]
    extensions: Vec<String>,
    /// A parameter name (repeatable), added to the names the manifest gives
    /// values for; under `placeholders = "parameters"` only these are
    /// placeholders.
    #[arg(long = "parameter", value_name = "NAME")]
    parameters: Vec<String>,
    /// The base revision: a template changed between its merge base with
    /// HEAD and the working tree in some trees and not in all is drift.
    #[arg(long, value_name = "REV")]
    base: Option<String>,
    /// The instance manifest: weave every instance in the reference tree
    /// and in each other tree, and check each pair as `sqldiff instances`.
    #[arg(long, value_name = "FILE")]
    manifest: Option<PathBuf>,
    /// An assumption you know holds (repeatable), for the instance checks.
    #[arg(long = "accept", value_name = "ID")]
    accept: Vec<String>,
    /// Parallel jobs for the instance checks (default: available cores).
    #[arg(short, long)]
    jobs: Option<usize>,
    /// Replace files in a non-empty output directory.
    #[arg(long)]
    overwrite: bool,
    #[command(flatten)]
    catalogs: source_cmd::CatalogArgs,
    #[command(flatten)]
    cfg: source_cmd::ConfigArgs,
}

fn tree(s: &str) -> Result<(Dialect, PathBuf), String> {
    let (d, p) = s
        .split_once('=')
        .ok_or_else(|| format!("`{s}`: expected DIALECT=DIR"))?;
    let d: Dialect = d.parse().map_err(|e| format!("`{d}`: {e}"))?;
    if p.is_empty() {
        return Err(format!("`{s}`: no directory"));
    }
    Ok((d, PathBuf::from(p)))
}

pub fn run(a: DriftArgs) -> Result<ExitCode, String> {
    let cfg = a.cfg.resolve()?;
    let catalogs = a.catalogs.clone();
    let c = drift::check(
        &drift::Args {
            trees: a.trees,
            reference: a.reference,
            out: a.out,
            extensions: a.extensions,
            parameters: a.parameters,
            base: a.base,
            manifest: a.manifest,
            accept: a.accept,
            jobs: a.jobs,
            overwrite: a.overwrite,
            options: Box::new(move |from, to| {
                let mut options = Options::default();
                catalogs.apply(&mut options, from, Some(to))?;
                Ok(options)
            }),
            cfg,
        },
        &Hooks::default(),
    )?;
    for r in &c.instance_runs {
        println!("{}", r.header);
        for n in &r.notes {
            eprintln!("{n}");
        }
        print!("{}", r.summary);
        let [json, md] = &r.reports;
        eprintln!("wrote {} and {}", json.display(), md.display());
    }
    print!("{}", c.summary);
    let [json, md] = &c.reports;
    eprintln!("wrote {} and {}", json.display(), md.display());
    Ok(ExitCode::from(if c.ok { 0 } else { 1 }))
}
