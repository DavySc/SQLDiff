//! A tree converted into an output folder: what `sqldiff convert` does,
//! as one call that the CLI and the GUI share, so both write the same files.

use std::path::PathBuf;

use crate::queue::{self, Queue};
use crate::tree::{self, Hooks, Input, Tree};

/// What to convert, and where to.
pub struct Job {
    pub tree: tree::Args,
    /// The output folder (created; must be empty unless `overwrite`).
    pub out: PathBuf,
    pub overwrite: bool,
    /// Overwrite files edited by hand since the last run.
    pub discard_edits: bool,
    /// `instance-report.json`, or the directory holding it.
    pub instances: Option<PathBuf>,
    /// Tier 6 (nothing found, not proven) in the queue too.
    pub queue_all: bool,
}

/// What a finished job wrote.
pub struct Written {
    /// The source files as read and converted.
    pub inputs: Vec<Input>,
    pub tree: Tree,
    pub queue: Queue,
    /// Review decisions in the ledger that could not be applied, and why.
    pub not_applied: Vec<String>,
    /// `conversion-report.json` and `.md`, `review-queue.json` and `.md`.
    pub reports: [PathBuf; 4],
}

/// Reads the source tree, converts and checks it, applies the review ledger
/// in the output folder and writes the output tree, its report and its
/// review queue. A job stopped through `hooks` fails with
/// [`tree::CANCELLED`] before anything is written into the output folder
/// (which is created if it did not exist).
pub fn convert_into(job: &Job, hooks: &Hooks) -> Result<Written, String> {
    // Read before anything is written, so a bad path fails early.
    let inst = match &job.instances {
        Some(p) => Some(queue::read_instances(p)?),
        None => None,
    };
    let (inputs, unreadable) = tree::read(&job.tree)?;
    tree::prepare_out(&job.tree.src, &job.out, job.overwrite)?;
    tree::guard_hand_edits(&job.out, job.discard_edits)?;
    let mut t = tree::convert_with(&job.tree, &inputs, unreadable, Some(&job.out), hooks)?;
    // Review decisions committed next to the converted tree.
    let not_applied = tree::review_in(&mut t, &inputs, &job.out)?;
    // The last point a job stops at: from here on the output is written.
    hooks.stop_if_cancelled()?;
    let (json, md) = tree::write(&t, &job.out)?;
    let q = queue::build(&t.report, inst.as_ref(), job.queue_all);
    let (qjson, qmd) = queue::write(&q, &job.out)?;
    Ok(Written {
        inputs,
        tree: t,
        queue: q,
        not_applied,
        reports: [json, md, qjson, qmd],
    })
}
