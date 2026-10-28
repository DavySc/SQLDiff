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
//! changes` runs), so a run writes what the command writes. Each run carries that command
//! (`Command`), for scripts and pipelines.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sqldiff_convert::drift;
use sqldiff_convert::instances;
use sqldiff_convert::job::{self, Job};
use sqldiff_convert::tree::{self, Hooks};
use sqldiff_core::Dialect;
use sqldiff_review::branch;
use sqldiff_review::pipeline::{Pipeline, Settings};
use sqldiff_source::changes::ChangeScope;
use sqldiff_source::config::{self, Sources};
use sqldiff_source::Resolved;

use crate::browse;
use crate::convert_review::{self, Review};

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

/// What a run was asked to do.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Spec {
    Convert(ConvertSpec),
    Instances(InstancesSpec),
    Drift(DriftSpec),
    Changes(ChangesSpec),
}

impl Spec {
    /// The output folder as given.
    fn out(&self) -> &str {
        match self {
            Spec::Convert(s) => &s.out,
            Spec::Instances(s) => &s.out,
            Spec::Drift(s) => &s.out,
            Spec::Changes(s) => &s.out,
        }
    }
}

/// The work a spec comes to, checked before a thread starts.
enum Work {
    Convert(Box<Job>),
    Instances(Box<instances::Args>),
    Drift(Box<drift::Args>),
    Changes(Box<ChangesWork>),
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
        mut args: Vec<String>,
        packs: &[PathBuf],
        dir: &Path,
        isolated: bool,
        tail: Vec<String>,
    ) -> Command {
        for p in packs {
            args.extend(["--pack".into(), path(p)]);
        }
        let (project, user) = match isolated {
            true => (None, None),
            false => (
                config::find_project(dir),
                config::user_config_path().filter(|p| p.is_file()),
            ),
        };
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
    /// The done gate as last read from the review, for when it is busy.
    gate: Mutex<Option<GateView>>,
}

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
                    blockers: g.blockers.len(),
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

    /// Asks a running run to stop; it stops before writing anything.
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
}

/// A run as `gui/runs.json` keeps it.
#[derive(Deserialize, Serialize)]
struct Record {
    id: u64,
    spec: Spec,
    command: Command,
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
            started: r.started.clone(),
            checked: r.checked.load(Ordering::Relaxed),
            total: r.total.load(Ordering::Relaxed),
            state: r.state(),
        })
        .collect();
    let Some(dir) = file.parent() else { return };
    let _ = std::fs::create_dir_all(dir);
    let tmp = file.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(&records).unwrap() + "\n";
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, file);
    }
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
                    started: r.started,
                    past: true,
                    checked: AtomicUsize::new(r.checked),
                    total: AtomicUsize::new(r.total),
                    cancel: Arc::new(AtomicBool::new(false)),
                    state: Mutex::new(state),
                    review: Mutex::new(None),
                    gate: Mutex::new(None),
                })
            })
            .collect();
        Runs {
            next: AtomicU64::new(list.iter().map(|r| r.id).max().unwrap_or(0)),
            list: Arc::new(Mutex::new(list)),
            history: Some(file),
        }
    }

    /// The review of `run`: the one it opened, or for a finished conversion
    /// from an earlier session, its tree on disk opened again as a run of
    /// its settings finds it now. `Ok(None)` when the run has no review;
    /// an error when it cannot be opened as the run wrote it.
    pub fn open_review(
        &self,
        run: &Arc<Run>,
        isolated: bool,
    ) -> Result<Option<Arc<Mutex<Review>>>, String> {
        let mut slot = run.review.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = slot.as_ref() {
            return Ok(Some(Arc::clone(r)));
        }
        let (Spec::Convert(spec), State::Done { .. }, true) = (&run.spec, run.state(), run.past)
        else {
            return Ok(None);
        };
        {
            let l = self.list.lock().unwrap_or_else(|e| e.into_inner());
            if l.iter().any(|r| {
                matches!(r.state(), State::Running)
                    && (r.out.starts_with(&run.out) || run.out.starts_with(&r.out))
            }) {
                return Err(format!(
                    "{}: another run is writing there; wait for it or cancel it",
                    run.out.display()
                ));
            }
        }
        let (job, _, command) = prepare(spec, isolated)?;
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
        let review = Review::open(convert_review::Setup {
            tree: job.tree,
            out: job.out,
            instances: job.instances,
        })?;
        let review = Arc::new(Mutex::new(review));
        *slot = Some(Arc::clone(&review));
        Ok(Some(review))
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
        let (job, out, command) = prepare(&spec, isolated)?;
        let reads = [Some(&job.tree.src), job.instances.as_ref()]
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        self.launch(
            Spec::Convert(spec),
            Work::Convert(Box::new(job)),
            (out, reads),
            command,
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
        let (a, command) = prepare_instances(&spec, isolated)?;
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
            command,
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
        let (a, command) = prepare_drift(&spec, isolated)?;
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
            command,
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
        let (w, command) = prepare_changes(&spec, isolated)?;
        let (out, reads) = (w.out.clone(), vec![w.repo.clone()]);
        self.launch(
            Spec::Changes(spec),
            Work::Changes(Box::new(w)),
            (out, reads),
            command,
            done,
        )
    }

    fn launch(
        &self,
        spec: Spec,
        work: Work,
        (out, reads): (PathBuf, Vec<PathBuf>),
        command: Command,
        done: impl FnOnce(&Arc<Run>) + Send + 'static,
    ) -> Result<Arc<Run>, String> {
        let out = canonical(&out);
        let reads: Vec<PathBuf> = reads.iter().map(|p| canonical(p)).collect();
        let run = {
            let mut l = self.list.lock().unwrap_or_else(|e| e.into_inner());
            let overlap = |a: &Path, b: &Path| a.starts_with(b) || b.starts_with(a);
            for r in l.iter().filter(|r| matches!(r.state(), State::Running)) {
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
            let run = Arc::new(Run {
                id: self.next.fetch_add(1, Ordering::Relaxed) + 1,
                spec,
                command,
                out,
                reads,
                started: sqldiff_review::time::now(),
                past: false,
                checked: AtomicUsize::new(0),
                total: AtomicUsize::new(0),
                cancel: Arc::new(AtomicBool::new(false)),
                state: Mutex::new(State::Running),
                review: Mutex::new(None),
                gate: Mutex::new(None),
            });
            l.push(Arc::clone(&run));
            if let Some(f) = &self.history {
                save(f, &l);
            }
            run
        };
        let r = Arc::clone(&run);
        let (list, history) = (Arc::clone(&self.list), self.history.clone());
        let finish = move |r: &Arc<Run>, state: State| {
            let l = list.lock().unwrap_or_else(|e| e.into_inner());
            *r.state.lock().unwrap_or_else(|e| e.into_inner()) = state;
            if let Some(f) = &history {
                save(f, &l);
            }
        };
        let fail = finish.clone();
        let spawned = std::thread::Builder::new()
            .stack_size(sqldiff_core::ENGINE_STACK)
            .spawn(move || {
                let state = execute(&r, work);
                finish(&r, state);
                done(&r);
            });
        if let Err(e) = spawned {
            // Never left running: that would hold its folder until a restart.
            let error = format!("the run could not be started: {e}");
            fail(
                &run,
                State::Failed {
                    error: error.clone(),
                    finished: sqldiff_review::time::now(),
                },
            );
            return Err(error);
        }
        Ok(run)
    }
}

/// The command a run of `spec` would carry, without starting one.
pub fn command(spec: &ConvertSpec, isolated: bool) -> Result<Command, String> {
    prepare(spec, isolated).map(|(_, _, c)| c)
}

/// The command an instance check of `spec` would carry, without starting one.
pub fn instances_command(spec: &InstancesSpec, isolated: bool) -> Result<Command, String> {
    prepare_instances(spec, isolated).map(|(_, c)| c)
}

/// The command a drift check of `spec` would carry, without starting one.
pub fn drift_command(spec: &DriftSpec, isolated: bool) -> Result<Command, String> {
    prepare_drift(spec, isolated).map(|(_, c)| c)
}

/// The command a branch review of `spec` would carry, without starting one.
pub fn changes_command(spec: &ChangesSpec, isolated: bool) -> Result<Command, String> {
    prepare_changes(spec, isolated).map(|(_, c)| c)
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
) -> Result<(instances::Args, Command), String> {
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
    Ok((a, command))
}

/// The branch review a spec asks for, checked before a thread is started,
/// and the command that runs it. The review store is read (decisions suppress
/// findings, as for the command) and the result cache is not used, so a run
/// writes nothing but its output folder.
fn prepare_changes(spec: &ChangesSpec, isolated: bool) -> Result<(ChangesWork, Command), String> {
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
    Ok((w, command))
}

/// The drift check a spec asks for, checked before a thread is started, and
/// the command that runs it.
fn prepare_drift(spec: &DriftSpec, isolated: bool) -> Result<(drift::Args, Command), String> {
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
    Ok((a, command))
}

/// The job a spec asks for, checked before a thread is started, its
/// absolute output folder and the command that runs it.
fn prepare(spec: &ConvertSpec, isolated: bool) -> Result<(Job, PathBuf, Command), String> {
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
    Ok((job, out, command))
}

fn execute(run: &Arc<Run>, work: Work) -> State {
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
    };
    match Review::open_converted(setup, w) {
        Ok(review) => {
            *run.review.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(Arc::new(Mutex::new(review)));
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
