//! Pack author markers applied to reports (design sections 5.2, 6.3 and 9).
//!
//! - `intentional_difference`: a deliberate difference stated in the
//!   source. Findings on that side whose location covers the marker's line,
//!   or starts on the next line, are suppressed with kind `in_source`.
//! - `assumption`: a premise the author states but has not verified. It
//!   becomes an assumption with source `author`, open until resolved.
//! - `review_note` and `sync_partner` are returned for display only.
//!
//! Markers are parsed by pattern only; no AI is involved.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqldiff_core::model::{
    hex, Assumption, AssumptionSource, Finding, Location, Side, Suppressed, SuppressionKind,
};
use sqldiff_core::{Report, Verdict};
use sqldiff_source::pack::MarkerKind;
use sqldiff_source::policy::{markers, Marker};
use sqldiff_source::Resolved;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideMarker {
    pub side: Side,
    #[serde(flatten)]
    pub marker: Marker,
}

/// What [`apply`] found and did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkerSummary {
    pub markers: Vec<SideMarker>,
    /// Findings suppressed by deliberate-difference markers.
    pub suppressed: usize,
    /// Ids of the author-declared assumptions added.
    pub assumptions: Vec<String>,
}

/// Author markers of both sides.
pub fn find(a_text: &str, b_text: Option<&str>, cfg: &Resolved) -> Vec<SideMarker> {
    let mut out: Vec<SideMarker> = markers(a_text, cfg)
        .into_iter()
        .map(|marker| SideMarker {
            side: Side::A,
            marker,
        })
        .collect();
    if let Some(b) = b_text {
        out.extend(markers(b, cfg).into_iter().map(|marker| SideMarker {
            side: Side::B,
            marker,
        }));
    }
    out
}

/// Stable id of an author-declared assumption: its text, case and
/// whitespace folded.
pub fn assumption_id(text: &str) -> String {
    let norm = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let h = Sha256::digest(norm.as_bytes());
    format!("ASM-AUTHOR-{}", hex(&h[..4]).to_uppercase())
}

fn covers(loc: &Location, line: usize) -> bool {
    (loc.start_line <= line && line <= loc.end_line) || loc.start_line == line + 1
}

fn marked(f: &Finding, side: Side, line: usize) -> bool {
    let loc = match side {
        Side::A => f.a.as_ref(),
        Side::B => f.b.as_ref(),
    };
    loc.is_some_and(|l| covers(l, line))
}

/// Applies author markers to a report made from `a_text` (and `b_text`).
pub fn apply(
    report: &mut Report,
    a_text: &str,
    b_text: Option<&str>,
    cfg: &Resolved,
) -> MarkerSummary {
    let found = find(a_text, b_text, cfg);
    let mut summary = MarkerSummary::default();
    for m in &found {
        match m.marker.kind {
            MarkerKind::IntentionalDifference => {
                let (hit, kept): (Vec<Finding>, Vec<Finding>) =
                    std::mem::take(&mut report.findings)
                        .into_iter()
                        .partition(|f| marked(f, m.side, m.marker.line));
                report.findings = kept;
                for f in hit {
                    report.suppressed.push(Suppressed {
                        finding: f,
                        kind: SuppressionKind::InSource,
                        justification: format!(
                            "deliberate difference marked on side {:?} line {}: {}",
                            m.side, m.marker.line, m.marker.value
                        ),
                    });
                    summary.suppressed += 1;
                }
            }
            MarkerKind::Assumption => {
                let id = assumption_id(&m.marker.value);
                let cite = format!("{:?}:{}", m.side, m.marker.line);
                match report.assumptions.iter_mut().find(|x| x.id == id) {
                    Some(x) => {
                        let c = x.cite.get_or_insert_with(String::new);
                        if !c.split(", ").any(|p| p == cite) {
                            if !c.is_empty() {
                                c.push_str(", ");
                            }
                            c.push_str(&cite);
                        }
                    }
                    None => {
                        report.assumptions.push(Assumption {
                            id: id.clone(),
                            text: m.marker.value.clone(),
                            source: AssumptionSource::Author,
                            cite: Some(cite),
                        });
                        summary.assumptions.push(id);
                    }
                }
            }
            MarkerKind::ReviewNote | MarkerKind::SyncPartner => {}
        }
    }
    if !summary.assumptions.is_empty() && report.verdict == Verdict::Equal {
        report.verdict = Verdict::EqualUnderAssumptions;
    }
    summary.markers = found;
    summary
}
