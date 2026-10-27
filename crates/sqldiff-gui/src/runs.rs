//! Runs: long work started from the app, with progress, cancel and a list
//! kept while the app runs. The one kind so far converts a tree into an
//! output folder through `sqldiff_convert::job`, the code `sqldiff convert`
//! runs, so a run writes what the command writes. Each run carries that
//! command (`Command`), for scripts and pipelines.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sqldiff_convert::job::{self, Job};
use sqldiff_convert::tree::{self, Hooks};
use sqldiff_core::Dialect;
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
    /// An instance check of these templates (`--instances`): the review
    /// then judges each template by its woven instances.
    #[serde(default)]
    pub instances: Option<String>,
}

/// Where a run is.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum State {
    Running,
    /// Written. `totals` as in `conversion-report.json`; `complete` when
    /// every file is converted or converted under assumptions (the CLI's
    /// exit code 0), which is not the review's done gate.
    Done {
        totals: serde_json::Value,
        complete: bool,
        queue: usize,
        not_applied: Vec<String>,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
        if let Some(p) = &job.instances {
            args.extend(["--instances".into(), path(p)]);
        }
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
    pub spec: ConvertSpec,
    pub command: Command,
    /// Absolute output folder, for the one-run-per-folder rule.
    out: PathBuf,
    started: String,
    checked: AtomicUsize,
    total: AtomicUsize,
    cancel: Arc<AtomicBool>,
    state: Mutex<State>,
    /// The review of what the run wrote, once it is done.
    review: Mutex<Option<Arc<Mutex<Review>>>>,
}

/// A run as the page lists it.
#[derive(Debug, Serialize)]
pub struct RunView {
    pub id: u64,
    pub kind: &'static str,
    #[serde(flatten)]
    pub spec: ConvertSpec,
    pub command: Command,
    pub started: String,
    pub checked: usize,
    pub total: usize,
    pub cancelling: bool,
    #[serde(flatten)]
    pub state: State,
}

impl Run {
    pub fn view(&self) -> RunView {
        RunView {
            id: self.id,
            kind: "convert",
            spec: self.spec.clone(),
            command: self.command.clone(),
            started: self.started.clone(),
            checked: self.checked.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            cancelling: self.cancel.load(Ordering::Relaxed),
            state: self.state(),
        }
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
    list: Mutex<Vec<Arc<Run>>>,
}

impl Runs {
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
        let run = {
            let mut l = self.list.lock().unwrap_or_else(|e| e.into_inner());
            if l.iter()
                .any(|r| r.out == out && matches!(r.state(), State::Running))
            {
                return Err(format!(
                    "{}: another run is writing there; wait for it or cancel it",
                    out.display()
                ));
            }
            let run = Arc::new(Run {
                id: self.next.fetch_add(1, Ordering::Relaxed) + 1,
                spec,
                command,
                out,
                started: sqldiff_review::time::now(),
                checked: AtomicUsize::new(0),
                total: AtomicUsize::new(0),
                cancel: Arc::new(AtomicBool::new(false)),
                state: Mutex::new(State::Running),
                review: Mutex::new(None),
            });
            l.push(Arc::clone(&run));
            run
        };
        let r = Arc::clone(&run);
        std::thread::Builder::new()
            .stack_size(sqldiff_core::ENGINE_STACK)
            .spawn(move || {
                let state = execute(&r, job);
                *r.state.lock().unwrap_or_else(|e| e.into_inner()) = state;
                done(&r);
            })
            .map_err(|e| e.to_string())?;
        Ok(run)
    }
}

/// The command a run of `spec` would carry, without starting one.
pub fn command(spec: &ConvertSpec, isolated: bool) -> Result<Command, String> {
    prepare(spec, isolated).map(|(_, _, c)| c)
}

/// The job a spec asks for, checked before a thread is started, its
/// absolute output folder and the command that runs it.
fn prepare(spec: &ConvertSpec, isolated: bool) -> Result<(Job, PathBuf, Command), String> {
    let dialect = |id: &str| -> Result<Dialect, String> {
        id.parse().map_err(|_| format!("unknown dialect `{id}`"))
    };
    let (from, to) = (dialect(&spec.from)?, dialect(&spec.to)?);
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
    let given = |p: &String| -> Result<PathBuf, String> {
        let p = browse::absolute(Path::new(p.trim()));
        match p.exists() {
            true => Ok(p),
            false => Err(format!("{}: not found", p.display())),
        }
    };
    let packs = spec
        .packs
        .iter()
        .filter(|p| !p.trim().is_empty())
        .map(given)
        .collect::<Result<Vec<_>, _>>()?;
    let instances = match spec.instances.as_ref().filter(|p| !p.trim().is_empty()) {
        Some(p) => Some(given(p)?),
        None => None,
    };
    let names = |v: &[String]| -> Vec<String> {
        v.iter()
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .collect()
    };
    let parameters = names(&spec.parameters);
    if !spec.template && !parameters.is_empty() {
        return Err("parameters are for template runs".into());
    }
    let dir = match src.is_dir() {
        true => src.clone(),
        false => src.parent().map(Path::to_path_buf).unwrap_or_default(),
    };
    let cfg = Resolved::load(&Sources {
        project: None,
        user: None,
        isolated,
        cwd: Some(dir.clone()),
        packs: packs.clone(),
        cli: config::cli_layer(&[], &[], None).map_err(|e| e.0)?,
    })
    .map_err(|e| e.0)?;
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
        },
        out: out.clone(),
        overwrite: spec.overwrite,
        discard_edits: false,
        instances,
        queue_all: false,
    };
    let command = Command::new(&program(), &job, &packs, &dir, isolated);
    Ok((job, out, command))
}

fn execute(run: &Arc<Run>, job: Job) -> State {
    let r = Arc::clone(run);
    let hooks = Hooks {
        progress: Some(Box::new(move |checked, total| {
            r.total.store(total, Ordering::Relaxed);
            r.checked.store(checked, Ordering::Relaxed);
        })),
        cancel: Some(Arc::clone(&run.cancel)),
    };
    let finished = sqldiff_review::time::now;
    let w = match job::convert_into(&job, &hooks) {
        Ok(w) => w,
        Err(e) if e == tree::CANCELLED => {
            return State::Cancelled {
                finished: finished(),
            }
        }
        Err(error) => {
            return State::Failed {
                error,
                finished: finished(),
            }
        }
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
        finished: finished(),
    }
}
