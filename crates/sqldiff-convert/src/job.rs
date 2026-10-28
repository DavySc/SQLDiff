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
    /// The proof checker: with a mode other than off, every converted
    /// routine gets a certificate (`certificates.json`).
    pub proof: sqldiff_core::proof::Settings,
    /// The observer reads rows-affected counts (decision 1).
    pub row_counts: bool,
}

/// `certificates.json`: what the checker said about each converted routine.
#[derive(serde::Serialize)]
pub struct Certificates {
    pub checker: Option<String>,
    #[serde(skip)]
    pub from: sqldiff_core::Dialect,
    pub files: Vec<FileCertificates>,
}

#[derive(serde::Serialize)]
pub struct FileCertificates {
    pub path: String,
    pub routines: Vec<crate::cert::Certificate>,
}

impl Certificates {
    /// The release gate of `--proof require` (decision 14 in
    /// docs/verified-conversion.md): why the converted tree still needs a
    /// review, if it does. Empty when every routine is confirmed, every
    /// assumption a confirmation or a unit rests on is accepted by the
    /// deployment profile, nothing lies outside a routine, and the tree
    /// converted.
    pub fn gate(
        &self,
        w: &Written,
        deployment: &sqldiff_source::deployment::Profile,
    ) -> Vec<String> {
        let mut why = Vec::new();
        if !w.tree.report.ok() {
            why.push("the tree is not converted (see conversion-report.md)".to_string());
        }
        let accepted = |id: &str| !deployment.accepting(id).is_empty();
        for f in &self.files {
            for c in &f.routines {
                match &c.answer {
                    crate::cert::Answer::Unconfirmed { reason } => why.push(format!(
                        "{}: {}: not confirmed: {reason}",
                        f.path, c.routine
                    )),
                    crate::cert::Answer::Confirmed { .. } => {
                        for a in c.assumptions.iter().filter(|a| !accepted(a)) {
                            why.push(format!(
                                "{}: {}: rests on {a}, which the deployment profile does not accept",
                                f.path, c.routine
                            ));
                        }
                    }
                }
            }
        }
        for (k, f) in w.tree.report.files.iter().enumerate() {
            if let Some(input) = w.inputs.iter().find(|i| i.rel == f.path) {
                let n = crate::cert::outside_routines(&input.text, self.from);
                if n > 0 {
                    why.push(format!(
                        "{}: {n} statement(s) outside a routine, which no certificate covers",
                        f.path
                    ));
                }
            }
            let Some(conv) = w.tree.conversions.get(k) else {
                continue;
            };
            let mut seen = Vec::new();
            for n in conv.units.iter().flat_map(|u| &u.notes) {
                if n.status == crate::Status::Assumptions
                    && !accepted(&n.rule)
                    && !seen.contains(&n.rule)
                {
                    seen.push(n.rule.clone());
                    why.push(format!(
                        "{}: rests on {}, which the deployment profile does not accept",
                        f.path, n.rule
                    ));
                }
            }
        }
        why
    }

    /// (confirmed, all) routines.
    pub fn counts(&self) -> (usize, usize) {
        let all = self.files.iter().flat_map(|f| &f.routines);
        let confirmed = all
            .clone()
            .filter(|c| matches!(c.answer, crate::cert::Answer::Confirmed { .. }))
            .count();
        (confirmed, all.count())
    }
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
    /// The certificates, when the job asked the proof checker.
    pub certificates: Option<(Certificates, PathBuf)>,
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
    let certificates = if job.proof.mode == sqldiff_core::proof::Mode::Off {
        None
    } else {
        let checker = job.proof.checker.as_deref();
        let mut files = Vec::new();
        for (k, f) in t.report.files.iter().enumerate() {
            let (Some(input), Some(output)) =
                (inputs.iter().find(|i| i.rel == f.path), t.outputs.get(k))
            else {
                continue;
            };
            // The text as written to disk, status lines and all.
            let routines = crate::cert::certify(
                &input.text,
                output,
                job.tree.from,
                job.tree.to,
                job.row_counts,
                checker,
            );
            if !routines.is_empty() {
                files.push(FileCertificates {
                    path: f.path.clone(),
                    routines,
                });
            }
        }
        let c = Certificates {
            checker: checker.map(|k| k.name()),
            from: job.tree.from,
            files,
        };
        let path = job.out.join("certificates.json");
        let text = serde_json::to_string_pretty(&c).map_err(|e| e.to_string())?;
        std::fs::write(&path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))?;
        Some((c, path))
    };
    Ok(Written {
        inputs,
        tree: t,
        queue: q,
        not_applied,
        reports: [json, md, qjson, qmd],
        certificates,
    })
}
