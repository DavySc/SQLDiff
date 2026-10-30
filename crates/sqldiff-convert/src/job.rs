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
    /// The counts and the refusal histogram, as `totals.proof` in
    /// `conversion-report.json`.
    pub summary: crate::tree::ProofTotals,
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
    /// deployment profile, nothing lies outside a routine, and every unit
    /// converted (under assumptions at worst). Inside a confirmed routine
    /// the proof stands where the engine did not compare (`not proven`
    /// only because of `equiv.not-compared`), and a `c2a` confirmation
    /// discharges the converter assumptions it proves
    /// (`cert::DISCHARGED_BY_C2A`; with error handling in the C4 slice also
    /// `cert::DISCHARGED_BY_C4`), and every residual id it names whenever
    /// needed but did not (`cert::NAMED_WHEN_NEEDED`: `ASM-CONCAT-NULL`
    /// after a leading `SET CONCAT_NULL_YIELDS_NULL ON`). A difference is
    /// never overridden.
    pub fn gate(
        &self,
        w: &Written,
        deployment: &sqldiff_source::deployment::Profile,
    ) -> Vec<String> {
        let mut why = Vec::new();
        if !w.tree.report.unreadable.is_empty() {
            why.push("some files could not be read (see conversion-report.md)".to_string());
        }
        // A callee's conversion counts as confirmed when its certificate is
        // confirmed and rests on no callee itself (no circular reasoning).
        let all: Vec<&crate::cert::Certificate> =
            self.files.iter().flat_map(|f| &f.routines).collect();
        let callee_ok = |caller: &str, callee: &str| {
            let key = crate::cert::pair_key(callee, self.from);
            key != crate::cert::pair_key(caller, self.from)
                && all.iter().any(|c| {
                    crate::cert::pair_key(&c.routine, self.from) == key
                        && matches!(c.answer, crate::cert::Answer::Confirmed { .. })
                        && !c
                            .assumptions
                            .iter()
                            .any(|a| a.starts_with(crate::cert::CALLEE_CONFIRMED))
                })
        };
        let accepted = |id: &str| !deployment.accepting(id).is_empty();
        for f in &self.files {
            for c in &f.routines {
                match &c.answer {
                    crate::cert::Answer::Unconfirmed { reason } => why.push(format!(
                        "{}: {}: not confirmed: {reason}",
                        f.path, c.routine
                    )),
                    crate::cert::Answer::Confirmed { .. } => {
                        let proven = |a: &str| {
                            a.strip_prefix(crate::cert::CALLEE_CONFIRMED)
                                .and_then(|x| x.strip_prefix(':'))
                                .is_some_and(|callee| callee_ok(&c.routine, callee))
                        };
                        for a in c.assumptions.iter().filter(|a| !accepted(a) && !proven(a)) {
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
            let certs: Vec<&crate::cert::Certificate> = self
                .files
                .iter()
                .filter(|c| c.path == f.path)
                .flat_map(|c| &c.routines)
                .collect();
            let mut seen = Vec::new();
            for u in &conv.units {
                // The confirmed routine this unit lies in, if any.
                let proof = certs.iter().find(|c| {
                    matches!(c.answer, crate::cert::Answer::Confirmed { .. })
                        && c.span.0 <= u.src.start
                        && u.src.end <= c.span.1
                });
                for n in &u.notes {
                    let open = match n.status {
                        crate::Status::Converted => false,
                        crate::Status::Assumptions => {
                            let proven = proof.is_some_and(|c| {
                                c.scalars.as_deref() == Some("c2a")
                                    && (crate::cert::DISCHARGED_BY_C2A
                                        .contains(&n.rule.as_str())
                                        // A residual id the slice names when it is
                                        // needed, and did not name here.
                                        || (crate::cert::NAMED_WHEN_NEEDED
                                            .contains(&n.rule.as_str())
                                            && !c.assumptions.iter().any(|a| *a == n.rule))
                                        || (c.errors.as_deref() == Some("c4")
                                            && crate::cert::DISCHARGED_BY_C4
                                                .contains(&n.rule.as_str())))
                            });
                            !proven && !accepted(&n.rule)
                        }
                        crate::Status::Unproven => {
                            !(proof.is_some() && n.rule == crate::NOT_COMPARED)
                        }
                        _ => true,
                    };
                    if open && !seen.iter().any(|s| *s == n.rule) {
                        seen.push(n.rule.to_string());
                        why.push(match n.status {
                            crate::Status::Assumptions => format!(
                                "{}: rests on {}, which the deployment profile does not accept",
                                f.path, n.rule
                            ),
                            s => format!("{}: a unit is {} ({})", f.path, s.label(), n.rule),
                        });
                    }
                }
                if u.checked == crate::Status::Differs && !seen.contains(&"differs".to_string()) {
                    seen.push("differs".to_string());
                    why.push(format!("{}: the engine found a difference", f.path));
                }
            }
            for &x in &conv.unplaced {
                let rule = conv
                    .output_check
                    .findings
                    .get(x)
                    .map_or("?", |g| g.rule.as_str());
                why.push(format!(
                    "{}: an output finding outside any unit ({rule})",
                    f.path
                ));
            }
            for r in &conv.unproven {
                why.push(format!("{}: not compared outside any unit: {r}", f.path));
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
    // The certificates come first: the report carries each unit's proof,
    // so it is written once, with them.
    let certificates = if job.proof.mode == sqldiff_core::proof::Mode::Off {
        None
    } else {
        let checker = job.proof.checker.as_deref();
        // Tables are read from the whole tree, source and output as written;
        // a file that could not be read may change any of them, so then none.
        // (C3d part 2) With the base catalogs' DDL on each side, as each side
        // binds against it (`base::Base::ddl`).
        let texts: Vec<&str> = inputs.iter().map(|i| i.text.as_str()).collect();
        let tables = if t.report.unreadable.is_empty() {
            let (base_a, base_b) = &t.base_ddl;
            crate::cert::Tables::read(
                &texts
                    .iter()
                    .copied()
                    .chain(base_a.iter().map(String::as_str))
                    .collect::<Vec<_>>(),
                &t.outputs
                    .iter()
                    .chain(base_b)
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                job.tree.from,
                job.tree.to,
            )
        } else {
            crate::cert::Tables::default()
        };
        let trusted = crate::cert::Trusted {
            tables,
            row_counts: job.row_counts,
            binary_collation: !job
                .tree
                .deployment
                .accepting(crate::cert::BINARY_COLLATION)
                .is_empty(),
            declared_routines: crate::cert::declared_routines(&texts, job.tree.from),
            output_routines: crate::cert::output_routines(
                &t.outputs.iter().map(String::as_str).collect::<Vec<_>>(),
                job.tree.to,
            ),
        };
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
                &trusted,
                checker,
            );
            if !routines.is_empty() {
                files.push(FileCertificates {
                    path: f.path.clone(),
                    routines,
                });
            }
        }
        let summary = tree::annotate_proof(&mut t, &inputs, &files);
        Some(Certificates {
            checker: checker.map(|k| k.name()),
            from: job.tree.from,
            summary,
            files,
        })
    };
    let (json, md) = tree::write(&t, &job.out)?;
    let q = queue::build(&t.report, inst.as_ref(), job.queue_all);
    let (qjson, qmd) = queue::write(&q, &job.out)?;
    let certificates = match certificates {
        None => None,
        Some(c) => {
            let path = job.out.join("certificates.json");
            let text = serde_json::to_string_pretty(&c).map_err(|e| e.to_string())?;
            std::fs::write(&path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))?;
            Some((c, path))
        }
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
