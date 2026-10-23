//! Suppression of accepted findings (design section 6.3).
//!
//! Suppressed findings move from `findings` to `suppressed`, with the
//! reason, so every output can still show them (SARIF marks them with
//! `suppressions`). `verdict` becomes the verdict over the open findings and
//! the engine's verdict is kept in `verdict_before_suppression`; suppression
//! never turns a difference into "equal".

use sqldiff_core::model::{Suppressed, SuppressionKind};
use sqldiff_core::{Report, Verdict};

use crate::store::{snapshot, ReviewState, Store};

/// Moves findings accepted in `store` (for this report's snapshot, or for
/// any snapshot, and not expired at `now`) to `report.suppressed`.
/// Returns how many were suppressed.
pub fn suppress(report: &mut Report, store: &Store, now: &str) -> usize {
    let snap = snapshot(report);
    let mut kept = Vec::with_capacity(report.findings.len());
    let mut n = 0;
    for f in std::mem::take(&mut report.findings) {
        let status = store.status(&f.fingerprint, &snap, now);
        if status.state == ReviewState::Accepted {
            let d = status.decision.expect("accepted has a decision");
            let mut why = match (&d.author, &d.comment) {
                (Some(a), Some(c)) => format!("accepted by {a}: {c}"),
                (None, Some(c)) => format!("accepted: {c}"),
                (Some(a), None) => format!("accepted by {a}"),
                (None, None) => "accepted".to_string(),
            };
            why.push_str(&format!(" ({})", d.at));
            report.suppressed.push(Suppressed {
                finding: f,
                kind: SuppressionKind::Review,
                justification: why,
            });
            n += 1;
        } else {
            kept.push(f);
        }
    }
    report.findings = kept;
    report.settle_suppression();
    n
}

/// The verdict a gate (CLI exit code, CI) should use: the verdict over the
/// open findings.
pub fn gate_verdict(report: &Report) -> Verdict {
    report.open_verdict()
}
