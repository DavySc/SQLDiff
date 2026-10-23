//! Session-local assumption flips (design §6.2 assumption panel, §9).
//!
//! A reviewer can deny an assumption ("this caller does run with
//! XACT_ABORT ON") or confirm it. Denying never makes a result stronger:
//! every finding that relied on the assumption drops to unknown, and the
//! report verdict is derived again.

use crate::model::{AssumptionSource, Report, Verdict};

/// The report verdict from its findings, unknown reasons and assumptions.
pub fn derive_verdict(r: &Report) -> Verdict {
    if r.findings.iter().any(|f| f.verdict == Verdict::Different) {
        Verdict::Different
    } else if r.findings.iter().any(|f| f.verdict == Verdict::Unknown)
        || !r.unknown_reasons.is_empty()
    {
        Verdict::Unknown
    } else if !r.assumptions.is_empty() {
        Verdict::EqualUnderAssumptions
    } else {
        Verdict::Equal
    }
}

/// Marks assumptions as not holding for this session. Findings that list a
/// denied id become unknown (a `different` witness was only shown under it
/// too); the assumption stays listed and an unknown reason names it.
pub fn deny_assumptions(r: &mut Report, ids: &[String]) {
    let mut hit = false;
    for f in &mut r.findings {
        if f.assumptions.iter().any(|a| ids.contains(a)) {
            f.verdict = Verdict::Unknown;
            hit = true;
        }
    }
    for a in &r.assumptions {
        if ids.contains(&a.id) {
            let reason = format!("assumption {} is denied in this session: {}", a.id, a.text);
            if !r.unknown_reasons.contains(&reason) {
                r.unknown_reasons.push(reason);
            }
            hit = true;
        }
    }
    if hit {
        r.verdict = derive_verdict(r);
    }
}

/// Marks assumptions as confirmed by the reviewer. The verdict does not change.
pub fn confirm_assumptions(r: &mut Report, ids: &[String]) {
    for a in &mut r.assumptions {
        if ids.contains(&a.id) {
            a.source = AssumptionSource::Declared;
        }
    }
}
