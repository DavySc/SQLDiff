//! Runs: long work started from the app, with progress, cancel and a list
//! kept in the data directory (`gui/runs.json`: paths, settings, totals and
//! errors, never SQL text), so finished runs, and their reviews, are there
//! again after a restart. A run converts a tree into an output folder
//! through `sqldiff_convert::job` (what `sqldiff convert` runs), checks
//! the instances of converted templates through
//! `sqldiff_convert::instances` (what `sqldiff instances` runs), or checks
//! the template trees of several dialects for drift through
//! `sqldiff_convert::drift` (what `sqldiff drift` runs), or reviews the SQL
//! a branch changed through `sqldiff_review::branch` (what `sqldiff
//! changes` runs), or lays out a folder's SQL files through
//! `sqldiff_source::format_run` (what `sqldiff fmt --check` or `--write`
//! runs), so a run writes what the command writes. Each run carries that command
//! (`Command`), for scripts and pipelines.
//!
//! A batch (mass conversion) is several source trees converted into several
//! target dialects: one ordinary conversion run per source and target, run
//! one after another, with one summary matrix of the runs' own totals and
//! done gates and a script of the runs' commands (`Batch`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sqldiff_convert::drift;
use sqldiff_convert::instances;
use sqldiff_convert::job::{self, Job};
use sqldiff_convert::tree::{self, Hooks};
use sqldiff_core::format::FormatConfig;
use sqldiff_core::Dialect;
use sqldiff_review::branch;
use sqldiff_review::pipeline::{Pipeline, Settings};
use sqldiff_source::changes::ChangeScope;
use sqldiff_source::config::{self, Sources};
use sqldiff_source::format_run;
use sqldiff_source::Resolved;

use crate::browse;
use crate::convert_review::{self, Review};
use crate::packs::{self, LoadedPack};
use crate::stored::{self, Stored};

/// A tree conversion as the page asks for it.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConvertSpec {
    /// The source tree, or one file.
    pub src: String,
    /// The output folder: created, and empty unless `overwrite`.
    pub out: String,
    /// Dialect ids.
    pub from: String,
    pub to: String,
    /// Replace the files in a non-empty output folder (never files edited by
    /// hand since the last run).
    #[serde(default)]
    pub overwrite: bool,
    /// Every file is a template in the `[template]` syntax of the loaded
    /// packs or config (`--template`).
    #[serde(default)]
    pub template: bool,
    /// Template parameter names (`--parameter`); none means the syntax decides.
    #[serde(default)]
    pub parameters: Vec<String>,
    /// File extensions to convert (`--extension`); none means `sql`.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Packs to load besides the config's (`--pack`), by path.
    #[serde(default)]
    pub packs: Vec<String>,
    /// Base catalogs (`--catalog`): DDL files of objects that exist already,
    /// bound against but not converted into the output, in order.
    #[serde(default)]
    pub catalogs: Vec<String>,
    /// The target's base catalogs (`--catalog-b`): the same objects as DDL
    /// in the target dialect, bound against instead of converting
    /// `catalogs`. Need `catalogs`.
    #[serde(default)]
    pub catalogs_b: Vec<String>,
    /// An instance check of these templates (`--instances`): the review
    /// then judges each template by its woven instances.
    #[serde(default)]
    pub instances: Option<String>,
}

/// An instance check as the page asks for it: the arguments of
/// `sqldiff instances`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstancesSpec {
    /// The instance manifest (TOML).
    pub manifest: String,
    /// The source templates and the converted templates (`--source`,
    /// `--target`); not with `generator_only`.
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    pub from: String,
    pub to: String,
    /// The output folder for the woven instances and the report.
    pub out: String,
    /// The generator's own output for the source and for the target dialect
    /// (`--woven`, `--woven-target`).
    #[serde(default)]
    pub woven: Option<String>,
    #[serde(default)]
    pub woven_target: Option<String>,
    /// Check the generator's output alone (`--generator-only`).
    #[serde(default)]
    pub generator_only: bool,
    /// Parameter names (`--parameter`) and accepted assumptions (`--accept`).
    #[serde(default)]
    pub parameters: Vec<String>,
    #[serde(default)]
    pub accept: Vec<String>,
    /// Packs to load besides the config's (`--pack`), by path.
    #[serde(default)]
    pub packs: Vec<String>,
    #[serde(default)]
    pub overwrite: bool,
}

/// A template tree of one dialect.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DriftTree {
    /// A dialect id.
    pub dialect: String,
    pub dir: String,
}

/// A drift check as the page asks for it: the arguments of `sqldiff drift`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DriftSpec {
    /// The template trees, one per dialect (`--tree`); at least two.
    pub trees: Vec<DriftTree>,
    /// The tree the others are checked against (`--reference`); none: the first.
    #[serde(default)]
    pub reference: Option<String>,
    /// The output folder for the report and the instance checks.
    pub out: String,
    /// Template file extensions (`--extension`); none means `sql`.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Parameter names (`--parameter`) and accepted assumptions (`--accept`).
    #[serde(default)]
    pub parameters: Vec<String>,
    #[serde(default)]
    pub accept: Vec<String>,
    /// A template changed since the merge base with this revision in some
    /// trees and not in all is drift (`--base`).
    #[serde(default)]
    pub base: Option<String>,
    /// The instance manifest (`--manifest`): every instance is also woven in
    /// each tree and checked as `sqldiff instances` does.
    #[serde(default)]
    pub manifest: Option<String>,
    /// Packs to load besides the config's (`--pack`), by path.
    #[serde(default)]
    pub packs: Vec<String>,
    #[serde(default)]
    pub overwrite: bool,
}

/// A branch review as the page asks for it: the arguments of `sqldiff changes`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChangesSpec {
    /// The git repository (`--repo`).
    pub repo: String,
    /// The revision the change is reviewed against (`--base`); the diff
    /// starts at its merge base with the head.
    pub base: String,
    /// The head revision (`--head`); none means `HEAD`.
    #[serde(default)]
    pub head: Option<String>,
    /// Dialect of every changed file (`--dialect`); none: per file, from
    /// `dialect_map` or the path convention.
    #[serde(default)]
    pub dialect: Option<String>,
    /// Only these paths (git pathspecs).
    #[serde(default)]
    pub paths: Vec<String>,
    /// The output folder for `changes.json`, `changes.md` and `changes.sarif`.
    pub out: String,
    /// Packs to load besides the config's (`--pack`), by path.
    #[serde(default)]
    pub packs: Vec<String>,
    /// Replace the files a review writes in a non-empty output folder.
    #[serde(default)]
    pub overwrite: bool,
}

/// What a branch review writes into its output folder.
pub const CHANGES_FILES: [&str; 3] = ["changes.json", "changes.md", "changes.sarif"];

/// A source tree of a batch.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BatchSource {
    /// The source tree, or one file.
    pub src: String,
    /// Its dialect id.
    pub from: String,
}

/// A batch as the page asks for it: every source converted into every
/// target dialect (but its own), each as an ordinary conversion run.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BatchSpec {
    pub sources: Vec<BatchSource>,
    /// Target dialect ids.
    pub targets: Vec<String>,
    /// The folder the runs write into: `OUT/<source name>/<target id>`.
    pub out: String,
    /// As for a run, for every run of the batch.
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default)]
    pub template: bool,
    #[serde(default)]
    pub parameters: Vec<String>,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub packs: Vec<String>,
}

/// Where a run sits in its batch.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct BatchCell {
    /// The batch's id.
    pub batch: u64,
    /// The index of the run's source in the batch's sources.
    pub source: usize,
    /// The target dialect id.
    pub to: String,
}

/// A folder laid out as `sqldiff fmt` lays it out, as the page asks for it.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FormatSpec {
    /// The folder whose SQL files are laid out.
    pub dir: String,
    /// Write the laid-out files back (`--write`); otherwise only check
    /// (`--check`) and write nothing.
    #[serde(default)]
    pub write: bool,
    /// The config file the page showed for the folder: the run is refused
    /// when a command run there would load another one.
    #[serde(default)]
    pub config: Option<String>,
}

/// What a run was asked to do.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Spec {
    Convert(ConvertSpec),
    Instances(InstancesSpec),
    Drift(DriftSpec),
    Changes(ChangesSpec),
    Format(FormatSpec),
}

impl Spec {
    /// The output folder as given.
    fn out(&self) -> &str {
        match self {
            Spec::Convert(s) => &s.out,
            Spec::Instances(s) => &s.out,
            Spec::Drift(s) => &s.out,
            Spec::Changes(s) => &s.out,
            // A format run holds the folder it reads and may write.
            Spec::Format(s) => &s.dir,
        }
    }
}

/// The work a spec comes to, checked before a thread starts.
enum Work {
    Convert(Box<Job>),
    Instances(Box<instances::Args>),
    Drift(Box<drift::Args>),
    Changes(Box<ChangesWork>),
    Format(Box<FormatWork>),
}

/// A folder to lay out, checked and ready to run.
struct FormatWork {
    dir: PathBuf,
    write: bool,
    /// The project and user config files loaded.
    files: (Option<PathBuf>, Option<PathBuf>),
    /// The config's `[format]`.
    format: FormatConfig,
    cfg: Resolved,
}

/// A branch review, checked and ready to run.
struct ChangesWork {
    repo: PathBuf,
    base: String,
    head: String,
    dialect: Option<Dialect>,
    paths: Vec<String>,
    out: PathBuf,
    pipe: Pipeline,
}

/// Where a run is.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum State {
    /// A run of a batch waiting for the runs before it; nothing written yet.
    Queued,
    Running,
    /// Written. For a conversion `totals` is as in `conversion-report.json`,
    /// `complete` says every file is converted or converted under
    /// assumptions (the CLI's exit code 0, not the review's done gate) and
    /// `queue` counts the review queue. For an instance check `totals` holds
    /// the instances per status, the count done and the count in all,
    /// `complete` is the report's and `queue` counts the instances not done.
    Done {
        totals: serde_json::Value,
        complete: bool,
        queue: usize,
        not_applied: Vec<String>,
        /// Notes on the run, such as a `--since` report not reused.
        notes: Vec<String>,
        finished: String,
    },
    /// Stopped before anything was written into the output folder.
    Cancelled {
        finished: String,
    },
    Failed {
        error: String,
        finished: String,
    },
}

impl State {
    /// Queued or running: the run holds its output folder and what it reads.
    pub fn busy(&self) -> bool {
        matches!(self, State::Queued | State::Running)
    }
}

/// The `sqldiff convert` command that writes what a run writes. It names
/// every config file the run loaded and finds no other (`--isolated`), so
/// it loads the same config whatever folder, environment or `sqldiff` on
/// the PATH it is run with.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Command {
    /// The arguments after the program.
    pub args: Vec<String>,
    /// The config files the run loaded, by path.
    pub project_config: Option<String>,
    pub user_config: Option<String>,
    /// The whole command for PowerShell and for a POSIX shell.
    pub powershell: String,
    pub sh: String,
}

impl Command {
    /// The command for `job` with `packs`, with config found from `dir` as
    /// the run finds it.
    pub fn new(program: &str, job: &Job, packs: &[PathBuf], dir: &Path, isolated: bool) -> Command {
        let t = &job.tree;
        let mut args = vec![
            "convert".into(),
            path(&t.src),
            "-o".into(),
            path(&job.out),
            "--from".into(),
            t.from.id().into(),
            "--to".into(),
            t.to.id().into(),
        ];
        if job.overwrite {
            args.push("--overwrite".into());
        }
        if let Some(m) = &t.template {
            args.push("--template".into());
            for p in m.parameters.iter().flatten() {
                args.extend(["--parameter".into(), p.clone()]);
            }
        }
        if t.extensions != tree::extensions(Vec::new()) {
            for e in &t.extensions {
                args.extend(["--extension".into(), e.clone()]);
            }
        }
        for c in &t.catalogs {
            args.extend(["--catalog".into(), path(c)]);
        }
        for c in &t.catalogs_b {
            args.extend(["--catalog-b".into(), path(c)]);
        }
        if let Some(p) = &job.instances {
            args.extend(["--instances".into(), path(p)]);
        }
        Command::with_config(program, args, packs, dir, isolated)
    }

    /// The `sqldiff instances` command for `a` with `packs`, with config
    /// found from `dir` as the run finds it.
    pub fn instances(
        program: &str,
        a: &instances::Args,
        packs: &[PathBuf],
        dir: &Path,
        isolated: bool,
    ) -> Command {
        let mut args = vec!["instances".into(), path(&a.manifest)];
        for (flag, p) in [("--source", &a.source), ("--target", &a.target)] {
            if let Some(p) = p {
                args.extend([flag.into(), path(p)]);
            }
        }
        args.extend([
            "--from".into(),
            a.from.id().into(),
            "--to".into(),
            a.to.id().into(),
            "-o".into(),
            path(&a.out),
        ]);
        for (flag, p) in [("--woven", &a.woven), ("--woven-target", &a.woven_target)] {
            if let Some(p) = p {
                args.extend([flag.into(), path(p)]);
            }
        }
        if a.generator_only {
            args.push("--generator-only".into());
        }
        for p in &a.parameters {
            args.extend(["--parameter".into(), p.clone()]);
        }
        for id in &a.accept {
            args.extend(["--accept".into(), id.clone()]);
        }
        if a.overwrite {
            args.push("--overwrite".into());
        }
        Command::with_config(program, args, packs, dir, isolated)
    }

    /// The `sqldiff drift` command for `a` with `packs`, with config found
    /// from `dir` as the run finds it.
    pub fn drift(
        program: &str,
        a: &drift::Args,
        packs: &[PathBuf],
        dir: &Path,
        isolated: bool,
    ) -> Command {
        let mut args = vec!["drift".into()];
        for (d, p) in &a.trees {
            args.extend(["--tree".into(), format!("{}={}", d.id(), path(p))]);
        }
        if let Some(d) = a.reference {
            args.extend(["--reference".into(), d.id().into()]);
        }
        args.extend(["-o".into(), path(&a.out)]);
        for e in &a.extensions {
            args.extend(["--extension".into(), e.clone()]);
        }
        for p in &a.parameters {
            args.extend(["--parameter".into(), p.clone()]);
        }
        if let Some(b) = &a.base {
            args.extend(["--base".into(), b.clone()]);
        }
        if let Some(m) = &a.manifest {
            args.extend(["--manifest".into(), path(m)]);
        }
        for id in &a.accept {
            args.extend(["--accept".into(), id.clone()]);
        }
        if a.overwrite {
            args.push("--overwrite".into());
        }
        Command::with_config(program, args, packs, dir, isolated)
    }

    /// The `sqldiff changes` command for `w` with `packs`, with config found
    /// from `dir` as the run finds it. It writes the files a run writes, and
    /// does not use the result cache, as a run does not.
    fn changes(
        program: &str,
        w: &ChangesWork,
        packs: &[PathBuf],
        dir: &Path,
        isolated: bool,
    ) -> Command {
        let mut args = vec![
            "changes".into(),
            "--repo".into(),
            path(&w.repo),
            "--base".into(),
            w.base.clone(),
            "--head".into(),
            w.head.clone(),
        ];
        if let Some(d) = w.dialect {
            args.extend(["--dialect".into(), d.id().into()]);
        }
        for (flag, file) in ["--json", "--markdown", "--sarif"]
            .into_iter()
            .zip(CHANGES_FILES)
        {
            args.extend([flag.into(), path(&w.out.join(file))]);
        }
        args.push("--no-cache".into());
        let tail = match w.paths.is_empty() {
            true => Vec::new(),
            false => std::iter::once("--".to_string())
                .chain(w.paths.iter().cloned())
                .collect(),
        };
        Command::with_config_then(program, args, packs, dir, isolated, tail)
    }

    /// The `sqldiff fmt` command for `w`, naming the config files it loaded.
    fn format(program: &str, w: &FormatWork) -> Command {
        let mode = if w.write { "--write" } else { "--check" };
        let args = vec!["fmt".into(), mode.into(), path(&w.dir)];
        Command::with_files(program, args, &[], w.files.clone(), Vec::new())
    }

    /// `args` with the packs and every config file the run loaded named,
    /// and no other found (`--isolated`).
    fn with_config(
        program: &str,
        args: Vec<String>,
        packs: &[PathBuf],
        dir: &Path,
        isolated: bool,
    ) -> Command {
        Command::with_config_then(program, args, packs, dir, isolated, Vec::new())
    }

    /// [`Command::with_config`], then `tail` (arguments that must come last,
    /// such as paths after `--`).
    fn with_config_then(
        program: &str,
        args: Vec<String>,
        packs: &[PathBuf],
        dir: &Path,
        isolated: bool,
        tail: Vec<String>,
    ) -> Command {
        let (project, user) = match isolated {
            true => (None, None),
            false => (
                config::find_project(dir),
                config::user_config_path().filter(|p| p.is_file()),
            ),
        };
        Command::with_files(program, args, packs, (project, user), tail)
    }

    /// `args` with the packs and the config files `project` and `user`
    /// named, and no other found (`--isolated`), then `tail`.
    fn with_files(
        program: &str,
        mut args: Vec<String>,
        packs: &[PathBuf],
        (project, user): (Option<PathBuf>, Option<PathBuf>),
        tail: Vec<String>,
    ) -> Command {
        for p in packs {
            args.extend(["--pack".into(), path(p)]);
        }
        args.push("--isolated".into());
        if let Some(p) = &project {
            args.extend(["--config".into(), path(p)]);
        }
        if let Some(p) = &user {
            args.extend(["--user-config".into(), path(p)]);
        }
        args.extend(tail);
        let line = |program: String, quote: fn(&str) -> String| {
            let mut l = program;
            for a in &args {
                l.push(' ');
                l.push_str(&quote(a));
            }
            l
        };
        let ps_program = match powershell_quote(program) {
            p if p == program => p,
            p => format!("& {p}"),
        };
        Command {
            powershell: line(ps_program, powershell_quote),
            sh: line(sh_quote(program), sh_quote),
            args,
            project_config: project.as_deref().map(path),
            user_config: user.as_deref().map(path),
        }
    }
}

fn path(p: &Path) -> String {
    p.display().to_string()
}

/// A PowerShell argument: bare when nothing in it is special there, else
/// single-quoted with `'` (and the typographic quotes PowerShell reads as
/// `'`) doubled.
pub fn powershell_quote(a: &str) -> String {
    let bare = |c: char| c.is_ascii_alphanumeric() || "-_./:\\=".contains(c);
    if !a.is_empty() && a.chars().all(bare) {
        return a.to_string();
    }
    let mut q = String::from("'");
    for c in a.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            q.push(c);
        }
        q.push(c);
    }
    q.push('\'');
    q
}

/// A POSIX shell argument: bare when nothing in it is special there, else
/// single-quoted with `'` written as `'\''`.
pub fn sh_quote(a: &str) -> String {
    let bare = |c: char| c.is_ascii_alphanumeric() || "-_./:=+,@%".contains(c);
    if !a.is_empty() && a.chars().all(bare) {
        return a.to_string();
    }
    format!("'{}'", a.replace('\'', "'\\''"))
}

/// `sqldiff` beside this program when it is there (a distribution, or a
/// build), else `sqldiff` on the PATH.
pub fn program() -> String {
    let name = format!("sqldiff{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe()
        .ok()
        .map(|e| e.canonicalize().unwrap_or(e))
        .and_then(|e| e.parent().map(|d| d.join(&name)))
        .filter(|p| p.is_file())
        .map(|p| path(&p))
        .unwrap_or_else(|| "sqldiff".into())
}

pub struct Run {
    pub id: u64,
    pub spec: Spec,
    pub command: Command,
    /// The packs the run's config loaded (a private pack by path only);
    /// `None` for a run kept by an app that did not record them.
    pub packs: Option<Vec<LoadedPack>>,
    /// Where the run sits in its batch, if it is a batch's.
    pub batch: Option<BatchCell>,
    /// Output folder and what the run reads, as `canonical` paths: no two
    /// running runs write into one folder, and none reads where another
    /// writes.
    out: PathBuf,
    reads: Vec<PathBuf>,
    started: String,
    /// Kept from an earlier session of the app.
    past: bool,
    checked: AtomicUsize,
    total: AtomicUsize,
    cancel: Arc<AtomicBool>,
    state: Mutex<State>,
    /// The review of what the run wrote, once it is done.
    review: Mutex<Option<Arc<Mutex<Review>>>>,
    /// A review from an earlier session while it opens again, or why that
    /// failed. Locked after `review`, never before.
    opening: Mutex<Option<Opening>>,
    /// The done gate as last read from the review, for when it is busy.
    gate: Mutex<Option<GateView>>,
}

/// A run's review from an earlier session while its tree is converted
/// again on its own thread (files checked, of the total), or why opening
/// it failed, until a request has said so.
enum Opening {
    Converting(Arc<(AtomicUsize, AtomicUsize)>),
    Failed(String),
}

/// A run's review as a request finds it.
pub enum ReviewOpen {
    Ready(Arc<Mutex<Review>>),
    /// Being opened again: files converted so far, of the total (0 while
    /// not yet known).
    Opening {
        checked: usize,
        total: usize,
    },
}

/// The page shown while a run's review opens again: it reloads itself
/// every second until the review is there.
pub fn opening_page(id: u64, checked: usize, total: usize) -> String {
    let progress = match total {
        0 => "Reading the source tree".to_string(),
        t => format!("Converted {checked} of {t} files"),
    };
    let percent = match total {
        0 => 0,
        t => checked.min(t) * 100 / t,
    };
    OPENING_HTML
        .replace("/*__RUN__*/", &id.to_string())
        .replace("/*__PERCENT__*/", &percent.to_string())
        .replace("/*__PROGRESS__*/", &progress)
}

const OPENING_HTML: &str = include_str!("../assets/opening.html");

/// A converted tree's done gate as the runs list shows it.
#[derive(Debug, Clone, Serialize)]
pub struct GateView {
    pub complete: bool,
    /// Blockers per kind, and in all.
    pub counts: std::collections::BTreeMap<String, usize>,
    pub blockers: usize,
    /// Assumptions accepted and decisions picked, by name, as
    /// `sqldiff review-status` lists them.
    pub accepted: Vec<sqldiff_convert::gate::Accepted>,
    /// Why the gate could not be judged.
    pub error: Option<String>,
}

/// A run as the page lists it.
#[derive(Debug, Serialize)]
pub struct RunView {
    pub id: u64,
    #[serde(flatten)]
    pub spec: Spec,
    pub command: Command,
    /// The packs the run's config loaded (a private pack by path only);
    /// `None` for a run kept by an app that did not record them.
    pub packs: Option<Vec<LoadedPack>>,
    pub batch: Option<BatchCell>,
    pub started: String,
    /// From an earlier session of the app: its review opens again from the
    /// tree on disk.
    pub past: bool,
    pub checked: usize,
    pub total: usize,
    pub cancelling: bool,
    #[serde(flatten)]
    pub state: State,
    /// A conversion's done gate (`sqldiff review-status`), once its review
    /// is open: it follows the decisions recorded there.
    pub gate: Option<GateView>,
}

impl Run {
    pub fn view(&self) -> RunView {
        RunView {
            id: self.id,
            spec: self.spec.clone(),
            command: self.command.clone(),
            packs: self.packs.clone(),
            batch: self.batch.clone(),
            started: self.started.clone(),
            past: self.past,
            checked: self.checked.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            cancelling: self.cancel.load(Ordering::Relaxed),
            state: self.state(),
            gate: self.gate(),
        }
    }

    /// The done gate of the run's review: read now unless the review is busy
    /// (reading the tree again), else as last read.
    fn gate(&self) -> Option<GateView> {
        let review = self.review()?;
        let mut seen = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(r) = review.try_lock() {
            *seen = Some(match r.gate() {
                Ok(g) => GateView {
                    complete: g.complete,
                    counts: g.counts,
                    blockers: g.blockers_total,
                    accepted: g.accepted,
                    error: None,
                },
                Err(e) => GateView {
                    complete: false,
                    counts: Default::default(),
                    blockers: 0,
                    accepted: Vec::new(),
                    error: Some(e),
                },
            });
        }
        seen.clone()
    }

    /// Kept from an earlier session of the app.
    pub fn past(&self) -> bool {
        self.past
    }

    pub fn state(&self) -> State {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Asks a queued or running run to stop; it stops before writing anything.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn review(&self) -> Option<Arc<Mutex<Review>>> {
        self.review
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[derive(Default)]
pub struct Runs {
    next: AtomicU64,
    list: Arc<Mutex<Vec<Arc<Run>>>>,
    /// Where the list is kept; `None` keeps it while the app runs only.
    history: Option<PathBuf>,
    /// Batches, oldest first, kept beside the list (`gui/batches.json`).
    batches: Mutex<Vec<Batch>>,
    /// Conversions kept beside the list (`gui/conversions/`), so a review
    /// opens again at once after a restart; `None` keeps none.
    stored: Option<Arc<Stored>>,
}

/// A batch: what was asked, and when. Its runs carry their `BatchCell`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Batch {
    pub id: u64,
    pub spec: BatchSpec,
    pub started: String,
}

/// A run of a batch as its matrix shows it: the run's own view, where kept.
#[derive(Debug, Serialize)]
pub struct CellView {
    pub source: usize,
    pub to: String,
    /// `None` when the run is no longer kept in the history.
    pub run: Option<u64>,
}

/// A batch as the page shows it.
#[derive(Debug, Serialize)]
pub struct BatchView {
    pub id: u64,
    pub spec: BatchSpec,
    pub started: String,
    /// The folder name each source's runs write under, by source.
    pub names: Vec<String>,
    /// One per source and target, sources first; a source is not converted
    /// into its own dialect.
    pub cells: Vec<CellView>,
    /// Some run is queued or running.
    pub busy: bool,
    /// The run sheet: the runs' commands, one after another.
    pub script: Script,
}

/// A batch's run sheet: a script of its runs' commands, as on each run's
/// sheet, in the order the batch runs them. It runs every one and exits with
/// the worst exit code among them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Script {
    pub powershell: String,
    pub sh: String,
}

impl Script {
    /// The script for batch `id` of `commands`; `missing` runs are no
    /// longer kept and are left out, which the script says.
    pub fn new(id: u64, commands: &[&Command], missing: usize) -> Script {
        let mut head = vec![
            format!("SQLDiff batch {id}: its runs' commands, one after another."),
            "Exit code: the worst of the runs' (0 every file converted, 1 not, 2 error).".into(),
        ];
        if missing > 0 {
            head.push(format!(
                "{missing} run{} of this batch no longer kept by the app {} left out.",
                if missing == 1 { "" } else { "s" },
                if missing == 1 { "is" } else { "are" }
            ));
        }
        let comment = |l: &String| format!("# {l}\n");
        let mut sh = String::from("#!/bin/sh\n");
        sh.extend(head.iter().map(comment));
        sh.push_str("status=0\n");
        let mut ps: String = head.iter().map(comment).collect();
        ps.push_str("$status = 0\n");
        for c in commands {
            sh.push_str(&c.sh);
            sh.push_str("\nc=$?; if [ \"$c\" -gt \"$status\" ]; then status=$c; fi\n");
            // A program that could not be started sets no exit code: an error.
            ps.push_str("$global:LASTEXITCODE = 0\n");
            ps.push_str(&c.powershell);
            ps.push_str("\n$ok = $?\nif (-not $ok -and $LASTEXITCODE -eq 0) { $global:LASTEXITCODE = 2 }\nif ($LASTEXITCODE -gt $status) { $status = $LASTEXITCODE }\n");
        }
        sh.push_str("exit \"$status\"\n");
        ps.push_str("exit $status\n");
        Script { powershell: ps, sh }
    }
}

/// A run as `gui/runs.json` keeps it.
#[derive(Deserialize, Serialize)]
struct Record {
    id: u64,
    spec: Spec,
    command: Command,
    /// Absent in a history written before packs were kept.
    #[serde(default)]
    packs: Option<Vec<LoadedPack>>,
    #[serde(default)]
    batch: Option<BatchCell>,
    started: String,
    checked: usize,
    total: usize,
    state: State,
}

/// Most runs kept in the history.
pub const HISTORY_MAX: usize = 100;

fn history_file(data_dir: &Path) -> PathBuf {
    data_dir.join("gui").join("runs.json")
}

/// The batches kept beside the history `file`.
fn batches_file(file: &Path) -> PathBuf {
    file.with_file_name("batches.json")
}

/// `text` written to `file` through a temporary file, so a reader never
/// sees half of it; a convenience, so a failure is not an error.
fn write_whole(file: &Path, text: String) {
    let Some(dir) = file.parent() else { return };
    let _ = std::fs::create_dir_all(dir);
    let tmp = file.with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, file);
    }
}

/// Writes the newest `HISTORY_MAX` runs of `list` to `file`, through a
/// temporary file so a reader never sees half of it. The history is a
/// convenience: a failure to write it is not an error.
fn save(file: &Path, list: &[Arc<Run>]) {
    let records: Vec<Record> = list
        .iter()
        .rev()
        .take(HISTORY_MAX)
        .rev()
        .map(|r| Record {
            id: r.id,
            spec: r.spec.clone(),
            command: r.command.clone(),
            packs: r.packs.clone(),
            batch: r.batch.clone(),
            started: r.started.clone(),
            checked: r.checked.load(Ordering::Relaxed),
            total: r.total.load(Ordering::Relaxed),
            state: r.state(),
        })
        .collect();
    write_whole(file, serde_json::to_string_pretty(&records).unwrap() + "\n");
}

/// `p` (absolute) with every link resolved, and on Windows the case and
/// form of its existing part, so that two spellings of one folder compare
/// equal; the part that does not exist yet is kept as given.
fn canonical(p: &Path) -> PathBuf {
    let mut tail = Vec::new();
    let mut base = p;
    loop {
        if let Ok(mut c) = base.canonicalize() {
            c.extend(tail.iter().rev());
            return c;
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name);
                base = parent;
            }
            _ => return p.to_path_buf(),
        }
    }
}

impl Runs {
    /// What the stored conversions take up; `None` when none are kept.
    pub fn stored(&self) -> Option<stored::Usage> {
        self.stored.as_ref().map(|s| s.usage())
    }

    /// Deletes the stored conversions.
    pub fn clear_stored(&self) -> Option<stored::Usage> {
        self.stored.as_ref().map(|s| s.clear())
    }

    /// Runs kept in `data_dir` (`None` keeps none). A run that was still
    /// running when the app stopped is listed as failed: what it wrote may
    /// be incomplete.
    pub fn with_history(data_dir: Option<&Path>) -> Runs {
        let Some(file) = data_dir.map(history_file) else {
            return Runs::default();
        };
        let records: Vec<Record> = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let list: Vec<Arc<Run>> = records
            .into_iter()
            .map(|r| {
                let state = match r.state {
                    State::Queued => State::Failed {
                        error: "the app stopped before this run started; nothing was written".into(),
                        finished: r.started.clone(),
                    },
                    State::Running => State::Failed {
                        error: "the app stopped while this run was running; what it wrote may be incomplete".into(),
                        finished: r.started.clone(),
                    },
                    s => s,
                };
                Arc::new(Run {
                    id: r.id,
                    out: canonical(&browse::absolute(Path::new(r.spec.out().trim()))),
                    reads: Vec::new(),
                    spec: r.spec,
                    command: r.command,
                    packs: r.packs,
                    batch: r.batch,
                    started: r.started,
                    past: true,
                    checked: AtomicUsize::new(r.checked),
                    total: AtomicUsize::new(r.total),
                    cancel: Arc::new(AtomicBool::new(false)),
                    state: Mutex::new(state),
                    review: Mutex::new(None),
                    opening: Mutex::new(None),
                    gate: Mutex::new(None),
                })
            })
            .collect();
        let batches: Vec<Batch> = std::fs::read_to_string(batches_file(&file))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Runs {
            next: AtomicU64::new(list.iter().map(|r| r.id).max().unwrap_or(0)),
            list: Arc::new(Mutex::new(list)),
            history: Some(file),
            batches: Mutex::new(batches),
            stored: data_dir.map(|d| Arc::new(Stored::in_data_dir(d))),
        }
    }

    /// The review of `run`: the one it opened, or for a finished conversion
    /// from an earlier session, its tree on disk opened again as a run of
    /// its settings finds it now. That converts the tree again, which takes
    /// as long as the run did, so it happens on its own thread: until it is
    /// done this says how far it got. `Ok(None)` when the run has no review;
    /// an error when it cannot be opened as the run wrote it (said once:
    /// the next request tries again).
    pub fn open_review(
        &self,
        run: &Arc<Run>,
        isolated: bool,
    ) -> Result<Option<ReviewOpen>, String> {
        let slot = run.review.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = slot.as_ref() {
            return Ok(Some(ReviewOpen::Ready(Arc::clone(r))));
        }
        let mut opening = run.opening.lock().unwrap_or_else(|e| e.into_inner());
        match opening.take() {
            Some(Opening::Converting(p)) => {
                let (checked, total) = (p.0.load(Ordering::Relaxed), p.1.load(Ordering::Relaxed));
                *opening = Some(Opening::Converting(p));
                return Ok(Some(ReviewOpen::Opening { checked, total }));
            }
            Some(Opening::Failed(e)) => return Err(e),
            None => {}
        }
        let (Spec::Convert(spec), State::Done { .. }, true) = (&run.spec, run.state(), run.past)
        else {
            return Ok(None);
        };
        {
            let l = self.list.lock().unwrap_or_else(|e| e.into_inner());
            if l.iter().any(|r| {
                r.state().busy() && (r.out.starts_with(&run.out) || run.out.starts_with(&r.out))
            }) {
                return Err(format!(
                    "{}: another run is writing there; wait for it or cancel it",
                    run.out.display()
                ));
            }
        }
        let (job, _, command, loaded) = prepare(spec, isolated)?;
        if (&command.args, &command.project_config, &command.user_config)
            != (
                &run.command.args,
                &run.command.project_config,
                &run.command.user_config,
            )
        {
            return Err(format!(
                "a run with these settings now loads other config than this run did ({}); start the run again to review it with the config as it is now",
                run.command.project_config.as_deref().or(command.project_config.as_deref()).unwrap_or("project and user config files")
            ));
        }
        let setup = convert_review::Setup {
            tree: job.tree,
            out: job.out,
            instances: job.instances,
            packs: loaded,
        };
        // A stored conversion, if there is one, is read through a setup of
        // its own; `setup` converts when there is none.
        let stored = self
            .stored
            .clone()
            .zip(stored_extra(&run.command, &setup.packs));
        let first = match &stored {
            Some(_) => {
                let (job, _, _, loaded) = prepare(spec, isolated)?;
                Some(convert_review::Setup {
                    tree: job.tree,
                    out: job.out,
                    instances: job.instances,
                    packs: loaded,
                })
            }
            None => None,
        };
        let progress = Arc::new((AtomicUsize::new(0), AtomicUsize::new(0)));
        let p = Arc::clone(&progress);
        let hooks = Hooks {
            progress: Some(Box::new(move |checked, total| {
                p.1.store(total, Ordering::Relaxed);
                p.0.store(checked, Ordering::Relaxed);
            })),
            cancel: None,
        };
        let r = Arc::clone(run);
        std::thread::Builder::new()
            .name(format!("sqldiff-review-{}", run.id))
            .stack_size(sqldiff_core::ENGINE_STACK)
            .spawn(move || reopen(&r, first, setup, stored, &hooks))
            .map_err(|e| format!("could not start opening the review: {e}"))?;
        *opening = Some(Opening::Converting(progress));
        drop((opening, slot));
        // A stored conversion reads in well under a second: wait for it
        // rather than show the progress page for a moment.
        if self.stored.is_some() {
            for _ in 0..60 {
                std::thread::sleep(std::time::Duration::from_millis(25));
                if let Some(r) = run.review() {
                    return Ok(Some(ReviewOpen::Ready(r)));
                }
            }
        }
        let p = run.opening.lock().unwrap_or_else(|e| e.into_inner());
        Ok(Some(match p.as_ref() {
            Some(Opening::Converting(p)) => ReviewOpen::Opening {
                checked: p.0.load(Ordering::Relaxed),
                total: p.1.load(Ordering::Relaxed),
            },
            _ => ReviewOpen::Opening {
                checked: 0,
                total: 0,
            },
        }))
    }

    /// Newest first.
    pub fn list(&self) -> Vec<Arc<Run>> {
        let l = self.list.lock().unwrap_or_else(|e| e.into_inner());
        l.iter().rev().cloned().collect()
    }

    pub fn get(&self, id: u64) -> Option<Arc<Run>> {
        let l = self.list.lock().unwrap_or_else(|e| e.into_inner());
        l.iter().find(|r| r.id == id).cloned()
    }

    /// Starts a tree conversion on its own thread. Config is what a command
    /// run in the source's folder loads (unless `isolated`); `done` gets the
    /// run once its review is open.
    pub fn start(
        &self,
        spec: ConvertSpec,
        isolated: bool,
        done: impl FnOnce(&Arc<Run>) + Send + 'static,
    ) -> Result<Arc<Run>, String> {
        let (job, out, command, loaded) = prepare(&spec, isolated)?;
        let reads = [Some(&job.tree.src), job.instances.as_ref()]
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        self.launch(
            Spec::Convert(spec),
            Work::Convert(Box::new(job)),
            (out, reads),
            (command, loaded),
            done,
        )
    }

    /// Starts an instance check on its own thread. Config is what a command
    /// run in the manifest's folder loads (unless `isolated`).
    pub fn start_instances(
        &self,
        spec: InstancesSpec,
        isolated: bool,
        done: impl FnOnce(&Arc<Run>) + Send + 'static,
    ) -> Result<Arc<Run>, String> {
        let (a, command, loaded) = prepare_instances(&spec, isolated)?;
        let out = a.out.clone();
        let reads = [Some(&a.manifest), a.source.as_ref(), a.target.as_ref()]
            .into_iter()
            .chain([a.woven.as_ref(), a.woven_target.as_ref()])
            .flatten()
            .cloned()
            .collect();
        self.launch(
            Spec::Instances(spec),
            Work::Instances(Box::new(a)),
            (out, reads),
            (command, loaded),
            done,
        )
    }

    /// Starts a drift check on its own thread. Config is what a command run
    /// in the reference tree loads (unless `isolated`).
    pub fn start_drift(
        &self,
        spec: DriftSpec,
        isolated: bool,
        done: impl FnOnce(&Arc<Run>) + Send + 'static,
    ) -> Result<Arc<Run>, String> {
        let (a, command, loaded) = prepare_drift(&spec, isolated)?;
        let out = a.out.clone();
        let reads = a
            .trees
            .iter()
            .map(|(_, p)| p)
            .chain(a.manifest.as_ref())
            .cloned()
            .collect();
        self.launch(
            Spec::Drift(spec),
            Work::Drift(Box::new(a)),
            (out, reads),
            (command, loaded),
            done,
        )
    }

    /// Starts a branch review on its own thread. Config is what a command
    /// run in the repository loads (unless `isolated`).
    pub fn start_changes(
        &self,
        spec: ChangesSpec,
        isolated: bool,
        done: impl FnOnce(&Arc<Run>) + Send + 'static,
    ) -> Result<Arc<Run>, String> {
        let (w, command, loaded) = prepare_changes(&spec, isolated)?;
        let (out, reads) = (w.out.clone(), vec![w.repo.clone()]);
        self.launch(
            Spec::Changes(spec),
            Work::Changes(Box::new(w)),
            (out, reads),
            (command, loaded),
            done,
        )
    }

    /// Starts laying out a folder on its own thread. Config is what a
    /// command run in the folder loads (unless `isolated`). The run holds
    /// the folder: no other run reads or writes there meanwhile.
    pub fn start_format(
        &self,
        spec: FormatSpec,
        isolated: bool,
        done: impl FnOnce(&Arc<Run>) + Send + 'static,
    ) -> Result<Arc<Run>, String> {
        let (w, command) = prepare_format(&spec, isolated)?;
        let loaded = packs::loaded(&w.cfg);
        let out = w.dir.clone();
        self.launch(
            Spec::Format(spec),
            Work::Format(Box::new(w)),
            (out, Vec::new()),
            (command, loaded),
            done,
        )
    }

    fn launch(
        &self,
        spec: Spec,
        work: Work,
        (out, reads): (PathBuf, Vec<PathBuf>),
        (command, packs): (Command, Vec<LoadedPack>),
        done: impl FnOnce(&Arc<Run>) + Send + 'static,
    ) -> Result<Arc<Run>, String> {
        let item = Admit {
            spec,
            work,
            out,
            reads,
            command,
            packs,
            cell: None,
        };
        let admitted = self.admit(vec![item], None)?;
        let run = Arc::clone(&admitted[0].0);
        let mut done = Some(done);
        self.spawn(admitted, move |r| {
            if let Some(d) = done.take() {
                d(r)
            }
        })?;
        Ok(run)
    }

    /// Starts a batch: every source converted into every target dialect but
    /// its own, each as an ordinary conversion run (what `start` starts) into
    /// `OUT/<source name>/<target id>`, run one after another on one thread.
    /// Every run is checked first: if any is refused, none starts.
    pub fn start_batch(&self, spec: BatchSpec, isolated: bool) -> Result<BatchView, String> {
        let mut items = Vec::new();
        for (source, to, cell) in batch_cells(&spec)? {
            let label = || format!("{} → {to}", spec.sources[source].src.trim());
            let (job, out, command, loaded) =
                prepare(&cell, isolated).map_err(|e| format!("{}: {e}", label()))?;
            // Refused here rather than by the run, so that none starts.
            let occupied = std::fs::read_dir(&out)
                .map(|mut d| d.next().is_some())
                .unwrap_or(false);
            if occupied && !spec.overwrite {
                return Err(format!(
                    "{}: not empty; replace the files a conversion writes to use it",
                    out.display()
                ));
            }
            let reads = vec![job.tree.src.clone()];
            items.push(Admit {
                spec: Spec::Convert(cell),
                work: Work::Convert(Box::new(job)),
                out,
                reads,
                command,
                packs: loaded,
                cell: Some((source, to)),
            });
        }
        let admitted = self.admit(items, Some(spec))?;
        let id = admitted[0].0.id;
        self.spawn(admitted, |_| {})?;
        self.batch(id)
            .ok_or_else(|| "the batch is not listed".to_string())
    }

    /// Checks `items` against the runs that hold a folder and against each
    /// other, and lists them, all or none: queued when they are a batch's
    /// (`batch`), else running.
    fn admit(
        &self,
        items: Vec<Admit>,
        batch: Option<BatchSpec>,
    ) -> Result<Vec<(Arc<Run>, Work)>, String> {
        let overlap = |a: &Path, b: &Path| a.starts_with(b) || b.starts_with(a);
        let mut l = self.list.lock().unwrap_or_else(|e| e.into_inner());
        let mut checked: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
        for it in &items {
            let out = canonical(&it.out);
            let reads: Vec<PathBuf> = it.reads.iter().map(|p| canonical(p)).collect();
            for r in l.iter().filter(|r| r.state().busy()) {
                if overlap(&r.out, &out) {
                    return Err(format!(
                        "{}: another run is writing there; wait for it or cancel it",
                        out.display()
                    ));
                }
                if let Some(p) = reads.iter().find(|p| overlap(&r.out, p)) {
                    return Err(format!(
                        "{}: another run is writing there, so this run would read a half-written tree; wait for it or cancel it",
                        p.display()
                    ));
                }
                if r.reads.iter().any(|p| overlap(p, &out)) {
                    return Err(format!(
                        "{}: another run is reading there; wait for it or cancel it",
                        out.display()
                    ));
                }
            }
            for (o, rs) in &checked {
                if overlap(o, &out) {
                    return Err(format!(
                        "{}: two runs of this batch would write there",
                        out.display()
                    ));
                }
                if let Some(p) = reads
                    .iter()
                    .chain(rs)
                    .find(|p| overlap(o, p) || overlap(&out, p))
                {
                    return Err(format!(
                        "{}: a run of this batch would write where another reads",
                        p.display()
                    ));
                }
            }
            // A batch's runs write under one folder: none may read there.
            if let Some(p) = reads.iter().find(|p| batch.is_some() && overlap(&out, p)) {
                return Err(format!(
                    "{}: a run of this batch would write where it reads",
                    p.display()
                ));
            }
            checked.push((out, reads));
        }
        let first = self.next.fetch_add(items.len() as u64, Ordering::Relaxed) + 1;
        let started = sqldiff_review::time::now();
        let state = match batch {
            Some(_) => State::Queued,
            None => State::Running,
        };
        let mut admitted = Vec::new();
        for (i, (it, (out, reads))) in items.into_iter().zip(checked).enumerate() {
            let run = Arc::new(Run {
                id: first + i as u64,
                spec: it.spec,
                command: it.command,
                packs: Some(it.packs),
                batch: it.cell.map(|(source, to)| BatchCell {
                    batch: first,
                    source,
                    to,
                }),
                out,
                reads,
                started: started.clone(),
                past: false,
                checked: AtomicUsize::new(0),
                total: AtomicUsize::new(0),
                cancel: Arc::new(AtomicBool::new(false)),
                state: Mutex::new(state.clone()),
                review: Mutex::new(None),
                opening: Mutex::new(None),
                gate: Mutex::new(None),
            });
            l.push(Arc::clone(&run));
            admitted.push((run, it.work));
        }
        if let Some(spec) = batch {
            let mut b = self.batches.lock().unwrap_or_else(|e| e.into_inner());
            b.push(Batch {
                id: first,
                spec,
                started,
            });
            // Kept while one of its runs is.
            let kept = l
                .iter()
                .rev()
                .take(HISTORY_MAX)
                .filter_map(|r| r.batch.as_ref());
            let kept: std::collections::BTreeSet<u64> = kept.map(|c| c.batch).collect();
            b.retain(|x| kept.contains(&x.id));
            if let Some(f) = &self.history {
                write_whole(
                    &batches_file(f),
                    serde_json::to_string_pretty(&*b).unwrap() + "\n",
                );
            }
        }
        if let Some(f) = &self.history {
            save(f, &l);
        }
        Ok(admitted)
    }

    /// Runs `admitted` one after another on a thread of its own; `done` gets
    /// each once it is finished. A queued run asked to stop before its turn
    /// is cancelled without starting.
    fn spawn(
        &self,
        admitted: Vec<(Arc<Run>, Work)>,
        mut done: impl FnMut(&Arc<Run>) + Send + 'static,
    ) -> Result<(), String> {
        let runs: Vec<Arc<Run>> = admitted.iter().map(|(r, _)| Arc::clone(r)).collect();
        let (list, history) = (Arc::clone(&self.list), self.history.clone());
        let stored = self.stored.clone();
        let set = move |r: &Arc<Run>, state: State| {
            let l = list.lock().unwrap_or_else(|e| e.into_inner());
            *r.state.lock().unwrap_or_else(|e| e.into_inner()) = state;
            if let Some(f) = &history {
                save(f, &l);
            }
        };
        let fail = set.clone();
        let spawned = std::thread::Builder::new()
            .stack_size(sqldiff_core::ENGINE_STACK)
            .spawn(move || {
                for (r, work) in admitted {
                    if matches!(r.state(), State::Queued) {
                        if r.cancel.load(Ordering::Relaxed) {
                            let finished = sqldiff_review::time::now();
                            set(&r, State::Cancelled { finished });
                            done(&r);
                            continue;
                        }
                        set(&r, State::Running);
                    }
                    let state = execute(&r, work, stored.as_ref());
                    set(&r, state);
                    done(&r);
                }
            });
        if let Err(e) = spawned {
            // Never left running: that would hold its folder until a restart.
            let error = format!("the run could not be started: {e}");
            for r in &runs {
                fail(
                    r,
                    State::Failed {
                        error: error.clone(),
                        finished: sqldiff_review::time::now(),
                    },
                );
            }
            return Err(error);
        }
        Ok(())
    }

    /// Batches, newest first.
    pub fn batches(&self) -> Vec<BatchView> {
        let ids: Vec<u64> = {
            let b = self.batches.lock().unwrap_or_else(|e| e.into_inner());
            b.iter().rev().map(|x| x.id).collect()
        };
        ids.into_iter().filter_map(|id| self.batch(id)).collect()
    }

    /// Batch `id` with its runs as they are now.
    pub fn batch(&self, id: u64) -> Option<BatchView> {
        let b = {
            let b = self.batches.lock().unwrap_or_else(|e| e.into_inner());
            b.iter().find(|x| x.id == id)?.clone()
        };
        let runs: Vec<Arc<Run>> = {
            let l = self.list.lock().unwrap_or_else(|e| e.into_inner());
            l.iter()
                .filter(|r| r.batch.as_ref().is_some_and(|c| c.batch == id))
                .cloned()
                .collect()
        };
        let cells = batch_cells(&b.spec).ok()?;
        let mut views = Vec::new();
        let mut commands = Vec::new();
        for (source, to, _) in cells {
            let run = runs.iter().find(|r| {
                r.batch
                    .as_ref()
                    .is_some_and(|c| c.source == source && c.to == to)
            });
            if let Some(r) = run {
                commands.push(&r.command);
            }
            views.push(CellView {
                source,
                to,
                run: run.map(|r| r.id),
            });
        }
        let missing = views.len() - commands.len();
        Some(BatchView {
            id,
            script: Script::new(id, &commands, missing),
            busy: runs.iter().any(|r| r.state().busy()),
            names: b
                .spec
                .sources
                .iter()
                .map(|s| source_name(&s.src).unwrap_or_default())
                .collect(),
            spec: b.spec,
            started: b.started,
            cells: views,
        })
    }

    /// Asks every run of batch `id` that has not finished to stop.
    pub fn cancel_batch(&self, id: u64) -> Option<BatchView> {
        for r in self.list() {
            if r.batch.as_ref().is_some_and(|c| c.batch == id) && r.state().busy() {
                r.cancel();
            }
        }
        self.batch(id)
    }
}

/// A run checked and ready to list.
struct Admit {
    spec: Spec,
    work: Work,
    out: PathBuf,
    reads: Vec<PathBuf>,
    command: Command,
    packs: Vec<LoadedPack>,
    /// Its source's index and target dialect, for a batch's run.
    cell: Option<(usize, String)>,
}

/// The runs batch `spec` asks for, in the order it runs them: its source's
/// index, the target dialect id and the run's spec. A source is not
/// converted into its own dialect; the names its runs write under must
/// differ.
pub fn batch_cells(spec: &BatchSpec) -> Result<Vec<(usize, String, ConvertSpec)>, String> {
    if spec.out.trim().is_empty() {
        return Err("give an output folder".into());
    }
    let out = browse::absolute(Path::new(spec.out.trim()));
    let mut targets: Vec<Dialect> = Vec::new();
    for t in &spec.targets {
        let d: Dialect = t
            .trim()
            .parse()
            .map_err(|_| format!("unknown dialect `{t}`"))?;
        if targets.contains(&d) {
            return Err(format!("{}: given twice", d.display_name()));
        }
        targets.push(d);
    }
    if spec.sources.is_empty() || targets.is_empty() {
        return Err("give at least one source and one target dialect".into());
    }
    let mut names: Vec<String> = Vec::new();
    let mut cells = Vec::new();
    for (i, s) in spec.sources.iter().enumerate() {
        let from: Dialect = s
            .from
            .trim()
            .parse()
            .map_err(|_| format!("unknown dialect `{}`", s.from))?;
        let name = source_name(&s.src)
            .ok_or_else(|| format!("{}: give a source folder or file", s.src.trim()))?;
        // Compared without case: one folder on Windows and macOS.
        if names
            .iter()
            .any(|n| n.to_lowercase() == name.to_lowercase())
        {
            return Err(format!(
                "two sources are named `{name}`, so their runs would write into one folder; give one of them under another name"
            ));
        }
        names.push(name.clone());
        for &to in targets.iter().filter(|&&t| t != from) {
            cells.push((
                i,
                to.id().to_string(),
                ConvertSpec {
                    src: s.src.trim().to_string(),
                    out: path(&out.join(&name).join(to.id())),
                    from: from.id().into(),
                    to: to.id().into(),
                    overwrite: spec.overwrite,
                    template: spec.template,
                    parameters: spec.parameters.clone(),
                    extensions: spec.extensions.clone(),
                    packs: spec.packs.clone(),
                    catalogs: Vec::new(),
                    catalogs_b: Vec::new(),
                    instances: None,
                },
            ));
        }
    }
    if cells.is_empty() {
        return Err("every source is already in every target dialect: nothing to convert".into());
    }
    Ok(cells)
}

/// The folder name a batch's runs of source `src` write under.
fn source_name(src: &str) -> Option<String> {
    let src = src.trim();
    browse::absolute(Path::new(src))
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !src.is_empty() && !n.is_empty())
}

/// The command a run of `spec` would carry, without starting one.
pub fn command(spec: &ConvertSpec, isolated: bool) -> Result<Command, String> {
    prepare(spec, isolated).map(|(_, _, c, _)| c)
}

/// The command an instance check of `spec` would carry, without starting one.
pub fn instances_command(spec: &InstancesSpec, isolated: bool) -> Result<Command, String> {
    prepare_instances(spec, isolated).map(|(_, c, _)| c)
}

/// The command a drift check of `spec` would carry, without starting one.
pub fn drift_command(spec: &DriftSpec, isolated: bool) -> Result<Command, String> {
    prepare_drift(spec, isolated).map(|(_, c, _)| c)
}

/// The command a format run of `spec` would carry, without starting one.
pub fn format_command(spec: &FormatSpec, isolated: bool) -> Result<Command, String> {
    prepare_format(spec, isolated).map(|(_, c)| c)
}

/// The command a branch review of `spec` would carry, without starting one.
pub fn changes_command(spec: &ChangesSpec, isolated: bool) -> Result<Command, String> {
    prepare_changes(spec, isolated).map(|(_, c, _)| c)
}

fn dialects(from: &str, to: &str) -> Result<(Dialect, Dialect), String> {
    let dialect = |id: &str| -> Result<Dialect, String> {
        id.parse().map_err(|_| format!("unknown dialect `{id}`"))
    };
    Ok((dialect(from)?, dialect(to)?))
}

/// `p`, absolute; it must exist.
fn existing(p: &str) -> Result<PathBuf, String> {
    let p = browse::absolute(Path::new(p.trim()));
    match p.exists() {
        true => Ok(p),
        false => Err(format!("{}: not found", p.display())),
    }
}

/// The given paths, absolute; each must exist. Blank ones are left out.
fn existing_all(v: &[String]) -> Result<Vec<PathBuf>, String> {
    v.iter()
        .filter(|p| !p.trim().is_empty())
        .map(|p| existing(p))
        .collect()
}

/// The given names, trimmed; blank ones are left out.
fn names(v: &[String]) -> Vec<String> {
    v.iter()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .collect()
}

/// Config as a command run in `dir` with `packs` loads it.
fn load_config(dir: &Path, packs: &[PathBuf], isolated: bool) -> Result<Resolved, String> {
    Resolved::load(&Sources {
        project: None,
        user: None,
        isolated,
        cwd: Some(dir.to_path_buf()),
        packs: packs.to_vec(),
        cli: config::cli_layer(&[], &[], None).map_err(|e| e.0)?,
    })
    .map_err(|e| e.0)
}

/// The instance check a spec asks for, checked before a thread is started,
/// and the command that runs it.
fn prepare_instances(
    spec: &InstancesSpec,
    isolated: bool,
) -> Result<(instances::Args, Command, Vec<LoadedPack>), String> {
    let (from, to) = dialects(&spec.from, &spec.to)?;
    if spec.manifest.trim().is_empty() || spec.out.trim().is_empty() {
        return Err("give a manifest and an output folder".into());
    }
    let opt = |p: &Option<String>| -> Result<Option<PathBuf>, String> {
        match p.as_deref().filter(|p| !p.trim().is_empty()) {
            Some(p) => existing(p).map(Some),
            None => Ok(None),
        }
    };
    let manifest = existing(&spec.manifest)?;
    let (source, target) = (opt(&spec.source)?, opt(&spec.target)?);
    let (woven, woven_target) = (opt(&spec.woven)?, opt(&spec.woven_target)?);
    if spec.generator_only {
        if woven.is_none() || woven_target.is_none() {
            return Err("checking the generator's output alone needs both of its outputs".into());
        }
        if source.is_some() || target.is_some() {
            return Err("checking the generator's output alone takes no templates".into());
        }
    } else if source.is_none() || target.is_none() {
        return Err("give the source templates and the converted templates".into());
    }
    let packs = existing_all(&spec.packs)?;
    let dir = manifest.parent().map(Path::to_path_buf).unwrap_or_default();
    let cfg = load_config(&dir, &packs, isolated)?;
    let loaded = packs::loaded(&cfg);
    let a = instances::Args {
        manifest,
        source,
        target,
        from,
        to,
        out: browse::absolute(Path::new(spec.out.trim())),
        woven,
        woven_target,
        generator_only: spec.generator_only,
        since: None,
        parameters: names(&spec.parameters),
        accept: names(&spec.accept),
        jobs: None,
        overwrite: spec.overwrite,
        options: sqldiff_core::Options::default(),
        cfg,
    };
    let command = Command::instances(&program(), &a, &packs, &dir, isolated);
    Ok((a, command, loaded))
}

/// The branch review a spec asks for, checked before a thread is started,
/// and the command that runs it. The review store is read (decisions suppress
/// findings, as for the command) and the result cache is not used, so a run
/// writes nothing but its output folder.
/// The folder a format run lays out, checked before a thread is started,
/// and the command that runs it.
fn prepare_format(spec: &FormatSpec, isolated: bool) -> Result<(FormatWork, Command), String> {
    let dir = existing(&spec.dir)?;
    if !dir.is_dir() {
        return Err(format!("{}: not a folder", dir.display()));
    }
    // The page shows the folder's own `sqldiff.toml`, present or not.
    let shown = spec
        .config
        .as_deref()
        .filter(|c| !c.trim().is_empty())
        .map(|c| browse::absolute(Path::new(c.trim())));
    let own = dir.join(config::PROJECT_FILE);
    if shown
        .as_deref()
        .is_some_and(|c| canonical(c) != canonical(&own))
    {
        return Err(format!(
            "settings are shown for {}, which is not {}",
            shown.as_deref().unwrap_or(&own).display(),
            own.display()
        ));
    }
    let project = match (isolated, &shown) {
        // An app that ignores config still formats with the settings it
        // shows: the folder's own file, named.
        (true, Some(_)) => Some(own.clone()).filter(|p| p.is_file()),
        (true, None) => None,
        (false, _) => config::find_project(&dir),
    };
    // The page shows that file, or the defaults when there is none: a command
    // run there must load the same, not a file further up.
    let expected = own.is_file().then(|| canonical(&own));
    if !isolated && shown.is_some() && project.as_deref().map(canonical) != expected {
        return Err(format!(
            "a command run in {} loads {} rather than the settings shown; open that folder's settings, or run the command",
            dir.display(),
            project
                .as_deref()
                .map_or("no project config".into(), |f| f.display().to_string())
        ));
    }
    let user = match isolated {
        true => None,
        false => config::user_config_path().filter(|p| p.is_file()),
    };
    if let Some(p) = &project {
        // As on the Settings page: the user config or a private pack is
        // never read as a folder's config, and nothing of it is quoted.
        if crate::settings::private_file(p, isolated, &[]) {
            return Err(format!(
                "{}: this file is also the user config or a private pack (or the configuration that would tell cannot be loaded), whose contents are not shown; choose another folder",
                p.display()
            ));
        }
        // An error in the folder's own file is the user's to read.
        config::ConfigLayer::load(p).map_err(|e| e.0)?;
    }
    let cfg = Resolved::load_without_search(&Sources {
        project: project.clone(),
        user: user.clone(),
        isolated: true,
        cwd: Some(dir.clone()),
        packs: Vec::new(),
        cli: config::cli_layer(&[], &[], None).map_err(|e| e.0)?,
    })
    // One in another layer may quote a private config.
    .map_err(|_| {
        "another configuration layer (user config or a pack) could not be loaded; run `sqldiff config` to see why".to_string()
    })?;
    let w = FormatWork {
        dir,
        write: spec.write,
        files: (project, user),
        format: cfg.policy.format.clone(),
        cfg,
    };
    let command = Command::format(&program(), &w);
    Ok((w, command))
}

fn prepare_changes(
    spec: &ChangesSpec,
    isolated: bool,
) -> Result<(ChangesWork, Command, Vec<LoadedPack>), String> {
    let repo = existing(&spec.repo)?;
    if !repo.is_dir() {
        return Err(format!("{}: not a folder", repo.display()));
    }
    let base = spec.base.trim().to_string();
    if base.is_empty() || spec.out.trim().is_empty() {
        return Err("give a base revision and an output folder".into());
    }
    let head = spec
        .head
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .unwrap_or("HEAD")
        .to_string();
    let dialect = match spec
        .dialect
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        Some(d) => Some(
            d.parse::<Dialect>()
                .map_err(|_| format!("unknown dialect `{d}`"))?,
        ),
        None => None,
    };
    let out = browse::absolute(Path::new(spec.out.trim()));
    let occupied = std::fs::read_dir(&out)
        .map(|mut d| d.next().is_some())
        .unwrap_or(false);
    if occupied && !spec.overwrite {
        return Err(format!(
            "{}: not empty; replace the files a review writes to use it",
            out.display()
        ));
    }
    let packs = existing_all(&spec.packs)?;
    let cfg = load_config(&repo, &packs, isolated)?;
    let loaded = packs::loaded(&cfg);
    let pipe = Pipeline::new(
        cfg,
        &Settings {
            no_cache: true,
            ..Settings::default()
        },
    )?;
    let w = ChangesWork {
        repo: repo.clone(),
        base,
        head,
        dialect,
        paths: names(&spec.paths),
        out,
        pipe,
    };
    let command = Command::changes(&program(), &w, &packs, &repo, isolated);
    Ok((w, command, loaded))
}

/// The drift check a spec asks for, checked before a thread is started, and
/// the command that runs it.
fn prepare_drift(
    spec: &DriftSpec,
    isolated: bool,
) -> Result<(drift::Args, Command, Vec<LoadedPack>), String> {
    let dialect = |id: &str| -> Result<Dialect, String> {
        id.trim()
            .parse()
            .map_err(|_| format!("unknown dialect `{id}`"))
    };
    if spec.out.trim().is_empty() {
        return Err("give an output folder".into());
    }
    let mut trees: Vec<(Dialect, PathBuf)> = Vec::new();
    for t in &spec.trees {
        let d = dialect(&t.dialect)?;
        if trees.iter().any(|(x, _)| *x == d) {
            return Err(format!("{}: given twice", d.display_name()));
        }
        let dir = existing(&t.dir)?;
        if !dir.is_dir() {
            return Err(format!("{}: not a folder", dir.display()));
        }
        trees.push((d, dir));
    }
    if trees.len() < 2 {
        return Err("give the template trees of at least two dialects".into());
    }
    let reference = match spec.reference.as_deref().filter(|r| !r.trim().is_empty()) {
        Some(r) => {
            let d = dialect(r)?;
            if !trees.iter().any(|(x, _)| *x == d) {
                return Err(format!(
                    "{}: has no tree to check against",
                    d.display_name()
                ));
            }
            Some(d)
        }
        None => None,
    };
    let manifest = match spec.manifest.as_deref().filter(|p| !p.trim().is_empty()) {
        Some(p) => Some(existing(p)?),
        None => None,
    };
    let base = spec
        .base
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .map(str::to_string);
    let packs = existing_all(&spec.packs)?;
    let ref_tree = reference.unwrap_or(trees[0].0);
    let dir = trees
        .iter()
        .find(|(d, _)| *d == ref_tree)
        .map(|(_, p)| p.clone())
        .unwrap_or_default();
    let cfg = load_config(&dir, &packs, isolated)?;
    let loaded = packs::loaded(&cfg);
    if cfg.policy.template.is_none() {
        return Err(
            "drift needs a [template] syntax: load a pack or a sqldiff.toml that has one".into(),
        );
    }
    let a = drift::Args {
        trees,
        reference,
        out: browse::absolute(Path::new(spec.out.trim())),
        extensions: names(&spec.extensions),
        parameters: names(&spec.parameters),
        base,
        manifest,
        accept: names(&spec.accept),
        jobs: None,
        overwrite: spec.overwrite,
        options: Box::new(|_, _| Ok(sqldiff_core::Options::default())),
        cfg,
    };
    let command = Command::drift(&program(), &a, &packs, &dir, isolated);
    Ok((a, command, loaded))
}

/// The job a spec asks for, checked before a thread is started, its
/// absolute output folder and the command that runs it.
fn prepare(
    spec: &ConvertSpec,
    isolated: bool,
) -> Result<(Job, PathBuf, Command, Vec<LoadedPack>), String> {
    let (from, to) = dialects(&spec.from, &spec.to)?;
    if !sqldiff_convert::supported(from, to) {
        return Err(format!(
            "no conversion rules from {} to {} yet",
            from.display_name(),
            to.display_name()
        ));
    }
    if spec.src.trim().is_empty() || spec.out.trim().is_empty() {
        return Err("give a source and an output folder".into());
    }
    let src = browse::absolute(Path::new(spec.src.trim()));
    let out = browse::absolute(Path::new(spec.out.trim()));
    if !src.exists() {
        return Err(format!("{}: not found", src.display()));
    }
    let packs = existing_all(&spec.packs)?;
    let catalogs = existing_all(&spec.catalogs)?;
    let catalogs_b = existing_all(&spec.catalogs_b)?;
    // Read now, so a bad one is refused before the run starts.
    sqldiff_convert::base::read(&catalogs, false)?;
    sqldiff_convert::base::read_b(&catalogs_b, &catalogs, false)?;
    let instances = match spec.instances.as_deref().filter(|p| !p.trim().is_empty()) {
        Some(p) => Some(existing(p)?),
        None => None,
    };
    let parameters = names(&spec.parameters);
    if !spec.template && !parameters.is_empty() {
        return Err("parameters are for template runs".into());
    }
    let dir = match src.is_dir() {
        true => src.clone(),
        false => src.parent().map(Path::to_path_buf).unwrap_or_default(),
    };
    let cfg = load_config(&dir, &packs, isolated)?;
    let loaded = packs::loaded(&cfg);
    let template = match spec.template {
        true => Some(tree::TemplateMode::configured(
            &cfg,
            (!parameters.is_empty()).then(|| parameters.into_iter().collect()),
        )?),
        false => None,
    };
    let job = Job {
        tree: tree::Args {
            src,
            from,
            to,
            jobs: None,
            no_catalog: false,
            template,
            extensions: tree::extensions(names(&spec.extensions)),
            helpers: cfg.options().helpers,
            deployment: cfg.deployment.clone(),
            catalogs,
            catalogs_b,
        },
        out: out.clone(),
        overwrite: spec.overwrite,
        discard_edits: false,
        instances,
        queue_all: false,
        // The GUI does not start the proof checker yet.
        proof: Default::default(),
        row_counts: false,
    };
    let command = Command::new(&program(), &job, &packs, &dir, isolated);
    Ok((job, out, command, loaded))
}

/// Opens `r`'s review again: from its stored conversion (`first`), if
/// one was stored under the key of the tree as it is now; else by
/// converting `setup` again, which is stored in turn.
fn reopen(
    r: &Arc<Run>,
    first: Option<convert_review::Setup>,
    setup: convert_review::Setup,
    stored: Option<(Arc<Stored>, Vec<u8>)>,
    hooks: &Hooks,
) {
    let from_store = (|| {
        let ((st, extra), first) = (stored.as_ref()?, first?);
        let (key, now) = convert_review::stored_key(&first, extra).ok()?;
        Review::open_stored(first, st.get(&key)?, now).ok()
    })();
    let fresh = from_store.is_none();
    let opened = match from_store {
        Some(review) => Ok(review),
        None => Review::open_with(setup, hooks),
    };
    // Stored after the review is shown: compressing takes a moment.
    let keep = match (&opened, &stored) {
        (Ok(review), Some((st, extra))) if fresh => {
            Some((Arc::clone(st), review.stored_key(extra), review.tree_json()))
        }
        _ => None,
    };
    {
        // As requests lock them: the review first, then `opening`.
        let mut slot = r.review.lock().unwrap_or_else(|e| e.into_inner());
        let mut opening = r.opening.lock().unwrap_or_else(|e| e.into_inner());
        match opened {
            Ok(review) => {
                *slot = Some(Arc::new(Mutex::new(review)));
                *opening = None;
            }
            Err(e) => *opening = Some(Opening::Failed(e)),
        }
    }
    if let Some((st, key, json)) = keep {
        st.put(&key, &json);
    }
}

/// What a stored conversion's key takes besides the tree: this build of
/// the app, the run's command, and the contents of the config files and
/// packs it loads. `None` (store nothing) when the build is not known.
fn stored_extra(command: &Command, packs: &[LoadedPack]) -> Option<Vec<u8>> {
    let build = stored::build();
    if build.is_empty() {
        return None;
    }
    let pinned = (&command.args, &command.project_config, &command.user_config);
    let mut out = build.as_bytes().to_vec();
    out.extend(serde_json::to_vec(&pinned).unwrap_or_default());
    for f in [&command.project_config, &command.user_config]
        .into_iter()
        .flatten()
    {
        stored::contents(Path::new(f), &mut out)?;
    }
    // The `pack.toml` the pack was loaded from: all `Pack::load` reads.
    for p in packs {
        stored::contents(Path::new(&p.path), &mut out)?;
    }
    Some(out)
}

fn execute(run: &Arc<Run>, work: Work, stored: Option<&Arc<Stored>>) -> State {
    let r = Arc::clone(run);
    let hooks = Hooks {
        progress: Some(Box::new(move |checked, total| {
            r.total.store(total, Ordering::Relaxed);
            r.checked.store(checked, Ordering::Relaxed);
        })),
        cancel: Some(Arc::clone(&run.cancel)),
    };
    let finished = sqldiff_review::time::now;
    let stopped = |e: String| match e == tree::CANCELLED {
        true => State::Cancelled {
            finished: finished(),
        },
        false => State::Failed {
            error: e,
            finished: finished(),
        },
    };
    let job = match work {
        Work::Convert(job) => job,
        Work::Changes(w) => {
            return match review_changes(&w, &hooks) {
                Ok(mut state) => {
                    if let State::Done {
                        notes, finished: f, ..
                    } = &mut state
                    {
                        notes.extend(w.pipe.warnings.iter().cloned());
                        *f = finished();
                    }
                    state
                }
                Err(e) => stopped(e),
            }
        }
        Work::Format(w) => {
            return match format_folder(&w, &hooks) {
                Ok(mut state) => {
                    if let State::Done { finished: f, .. } = &mut state {
                        *f = finished();
                    }
                    state
                }
                Err(e) => stopped(e),
            }
        }
        Work::Drift(a) => {
            return match drift::check(&a, &hooks) {
                Ok(c) => State::Done {
                    totals: serde_json::json!({
                        "templates": c.templates,
                        "drifting": c.drifting,
                        "items": c.items,
                        "waived": c.waived,
                        "instances": c.instance_ok
                            .iter()
                            .map(|(d, ok)| serde_json::json!({ "dialect": d, "ok": ok }))
                            .collect::<Vec<_>>(),
                    }),
                    complete: c.ok,
                    queue: c.drifting,
                    not_applied: Vec::new(),
                    notes: c.instance_runs.into_iter().flat_map(|r| r.notes).collect(),
                    finished: finished(),
                },
                Err(e) => stopped(e),
            }
        }
        Work::Instances(a) => {
            return match instances::check(&a, &hooks) {
                Ok(c) => State::Done {
                    totals: serde_json::json!({
                        "statuses": c.totals,
                        "done": c.done,
                        "instances": c.instances,
                    }),
                    complete: c.complete,
                    queue: c.instances - c.done,
                    not_applied: Vec::new(),
                    notes: c.notes,
                    finished: finished(),
                },
                Err(e) => stopped(e),
            }
        }
    };
    let w = match job::convert_into(&job, &hooks) {
        Ok(w) => w,
        Err(e) => return stopped(e),
    };
    let totals = serde_json::to_value(&w.tree.report.totals).unwrap_or_default();
    let complete = w.tree.report.ok();
    let queue = w.queue.items.len();
    let not_applied = w.not_applied.clone();
    let setup = convert_review::Setup {
        tree: job.tree,
        out: job.out,
        instances: job.instances,
        packs: run.packs.clone().unwrap_or_default(),
    };
    let extra = stored.zip(stored_extra(&run.command, &setup.packs));
    match Review::open_converted(setup, w) {
        Ok(review) => {
            let keep = extra.map(|(st, extra)| {
                (
                    Arc::clone(st),
                    review.stored_key(&extra),
                    review.tree_json(),
                )
            });
            *run.review.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(Arc::new(Mutex::new(review)));
            // Compressing a large tree takes a moment: not on the run's time.
            if let Some((st, key, json)) = keep {
                let _ = std::thread::Builder::new()
                    .name(format!("sqldiff-store-{}", run.id))
                    .spawn(move || st.put(&key, &json));
            }
        }
        // Written all the same: the review can be opened from the command line.
        Err(e) => {
            return State::Failed {
                error: format!("converted, but the review could not be opened: {e}"),
                finished: finished(),
            }
        }
    }
    State::Done {
        totals,
        complete,
        queue,
        not_applied,
        notes: Vec::new(),
        finished: finished(),
    }
}

/// Lays out every file first; a write run then writes the changed files,
/// after the last cancel point. `totals` holds the files, those laid out
/// differently from what is on disk (written, for a write run), the
/// statements kept as written and the files skipped; `complete` is the
/// command's exit code 0.
fn format_folder(w: &FormatWork, hooks: &Hooks) -> Result<State, String> {
    let files = format_run::check(std::slice::from_ref(&w.dir), None, &w.format, &w.cfg, hooks)?;
    hooks.stop_if_cancelled()?;
    let mut notes = Vec::new();
    let (mut changed, mut kept, mut skipped) = (0, 0, 0);
    for f in &files {
        if let Some(line) = f.skipped_line() {
            notes.push(line);
            skipped += 1;
            continue;
        }
        notes.extend(f.kept_lines());
        kept += f.kept.len();
        if f.changed {
            changed += 1;
        }
    }
    if w.write {
        let mut written = 0;
        for f in &files {
            match f.write() {
                Ok(true) => written += 1,
                Ok(false) => {}
                Err(e) => return Err(format!("{e} ({written} files written before it)")),
            }
        }
    }
    Ok(State::Done {
        totals: serde_json::json!({
            "files": files.len(),
            "changed": changed,
            "written": w.write,
            "kept": kept,
            "skipped": skipped,
        }),
        complete: skipped == 0 && (w.write || changed == 0),
        queue: if w.write { 0 } else { changed },
        not_applied: Vec::new(),
        notes,
        finished: String::new(),
    })
}

/// Reviews the branch, then writes the three files. Everything is read and
/// judged first and the files are written last, after the last cancel point.
fn review_changes(w: &ChangesWork, hooks: &Hooks) -> Result<State, String> {
    let r = branch::review(
        &ChangeScope {
            repo: &w.repo,
            base: &w.base,
            head: &w.head,
            dialect: w.dialect,
            paths: &w.paths,
        },
        &w.pipe,
        hooks,
    )?;
    hooks.stop_if_cancelled()?;
    std::fs::create_dir_all(&w.out).map_err(|e| format!("{}: {e}", w.out.display()))?;
    for (file, text) in CHANGES_FILES
        .iter()
        .zip([r.json(), r.markdown(), r.sarif()])
    {
        let p = w.out.join(file);
        std::fs::write(&p, text).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    let verdict = r.verdict();
    let c = r.counts();
    Ok(State::Done {
        totals: serde_json::json!({
            "verdict": verdict.label(),
            "files": r.files(),
            "counts": c,
            "base": w.base,
            "head": w.head,
        }),
        // The command's exit code: nothing differs and nothing is unproven.
        complete: verdict < sqldiff_core::Verdict::Unknown,
        queue: c.different + c.not_proven + c.not_compared,
        not_applied: Vec::new(),
        notes: Vec::new(),
        finished: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(config: Option<&Path>) -> Command {
        Command {
            args: vec!["convert".into()],
            project_config: config.map(|p| p.display().to_string()),
            user_config: None,
            powershell: String::new(),
            sh: String::new(),
        }
    }

    fn pack(path: &Path) -> LoadedPack {
        LoadedPack {
            private: true,
            name: None,
            version: None,
            path: path.display().to_string(),
        }
    }

    #[test]
    fn the_key_follows_the_config_and_pack_contents() {
        let d = std::env::temp_dir().join(format!("sqldiff-extra-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let (cfg, p) = (d.join("sqldiff.toml"), d.join("pack.toml"));
        std::fs::write(&cfg, "a = 1\n").unwrap();
        std::fs::write(&p, "b = 1\n").unwrap();
        let key = || stored_extra(&command(Some(&cfg)), &[pack(&p)]).unwrap();
        let first = key();
        assert_eq!(key(), first);
        std::fs::write(&p, "b = 2\n").unwrap();
        let second = key();
        assert_ne!(second, first, "pack contents");
        std::fs::write(&cfg, "a = 2\n").unwrap();
        assert_ne!(key(), second, "config contents");
        // A file that cannot be read: nothing is stored.
        std::fs::remove_file(&p).unwrap();
        assert!(stored_extra(&command(Some(&cfg)), &[pack(&p)]).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// `Pack::load` reads `pack.toml` through a link: so does the key.
    #[cfg(unix)]
    #[test]
    fn a_linked_pack_toml_is_keyed_by_what_it_points_to() {
        let d = std::env::temp_dir().join(format!("sqldiff-extra-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("pack")).unwrap();
        let target = d.join("real.toml");
        std::fs::write(&target, "b = 1\n").unwrap();
        let link = d.join("pack").join("pack.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let key = || stored_extra(&command(None), &[pack(&link)]).unwrap();
        let first = key();
        std::fs::write(&target, "b = 2\n").unwrap();
        assert_ne!(key(), first);
        let _ = std::fs::remove_dir_all(&d);
    }
}
