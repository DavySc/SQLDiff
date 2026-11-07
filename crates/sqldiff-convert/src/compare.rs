//! What changed between two conversions of a tree: the signal of the tweak
//! loop (an engine, pack or rule edit, then convert again).
//!
//! Computed from two finished reports, never by converting again. Units are
//! joined by their stable id (`review::unit_ids`), then by file path and
//! source line among those left; what is still unmatched is new or gone.
//! The order of the result is a function of the two reports alone: classes
//! in a fixed order, then path, source line and id.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::tree::{Report, UnitReport};
use crate::Status;

/// How a unit differs between the two runs, in report order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    /// A worse status in the second run.
    Regressed,
    /// A better status in the second run.
    Improved,
    /// The same status, but other rules, findings or reason.
    Changed,
    /// Only in the second run.
    New,
    /// Only in the first run.
    Gone,
}

/// One unit's side of a row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Side {
    pub id: String,
    pub status: Status,
    pub src_line: usize,
    pub rules: Vec<String>,
    pub findings: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Row {
    pub change: Change,
    pub path: String,
    /// The first run's side; absent for a new unit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub a: Option<Side>,
    /// The second run's side; absent for a gone unit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b: Option<Side>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Comparison {
    pub regressed: usize,
    pub improved: usize,
    pub changed: usize,
    pub new: usize,
    pub gone: usize,
    pub unchanged: usize,
    /// Units per status in each run.
    pub totals_a: BTreeMap<String, usize>,
    pub totals_b: BTreeMap<String, usize>,
    pub rows: Vec<Row>,
}

struct Flat<'a> {
    path: &'a str,
    u: &'a UnitReport,
}

fn flatten(r: &Report) -> Vec<Flat<'_>> {
    r.files
        .iter()
        .flat_map(|f| {
            f.units
                .iter()
                .chain(&f.converted_units)
                .map(move |u| Flat { path: &f.path, u })
        })
        .collect()
}

fn side(u: &UnitReport) -> Side {
    Side {
        id: u.id.clone(),
        status: u.status,
        src_line: u.src_line,
        rules: u.rules.clone(),
        findings: u.findings.clone(),
        reason: u.reason.clone(),
    }
}

fn same(x: &UnitReport, y: &UnitReport) -> bool {
    x.status == y.status && x.rules == y.rules && x.findings == y.findings && x.reason == y.reason
}

fn tally(units: &[Flat<'_>]) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for f in units {
        *m.entry(f.u.status.label().to_string()).or_insert(0) += 1;
    }
    m
}

/// Compares two reports. A pair of units is matched once; a repeated id
/// (identical statements) is matched in report order.
pub fn compare(a: &Report, b: &Report) -> Comparison {
    let fa = flatten(a);
    let fb = flatten(b);
    let mut out = Comparison {
        totals_a: tally(&fa),
        totals_b: tally(&fb),
        ..Default::default()
    };
    let mut taken_b = BTreeSet::new();
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    let mut open_a: Vec<usize> = Vec::new();
    // First: by id (per path, because the id may repeat across files).
    let mut by_id: BTreeMap<(&str, &str), Vec<usize>> = BTreeMap::new();
    for (j, y) in fb.iter().enumerate() {
        by_id.entry((y.path, &y.u.id)).or_default().push(j);
    }
    for (i, x) in fa.iter().enumerate() {
        let hit = by_id
            .get_mut(&(x.path, x.u.id.as_str()))
            .and_then(|v| (!v.is_empty()).then(|| v.remove(0)));
        match hit {
            Some(j) => {
                taken_b.insert(j);
                pairs.push((i, j));
            }
            None => open_a.push(i),
        }
    }
    // Then: by path and source line.
    let mut by_line: BTreeMap<(&str, usize), Vec<usize>> = BTreeMap::new();
    for (j, y) in fb.iter().enumerate().filter(|(j, _)| !taken_b.contains(j)) {
        by_line.entry((y.path, y.u.src_line)).or_default().push(j);
    }
    let mut gone: Vec<usize> = Vec::new();
    for i in open_a {
        let x = &fa[i];
        match by_line
            .get_mut(&(x.path, x.u.src_line))
            .and_then(|v| (!v.is_empty()).then(|| v.remove(0)))
        {
            Some(j) => {
                taken_b.insert(j);
                pairs.push((i, j));
            }
            None => gone.push(i),
        }
    }
    for &(i, j) in &pairs {
        let (x, y) = (&fa[i], &fb[j]);
        let change = match y.u.status.cmp(&x.u.status) {
            std::cmp::Ordering::Greater => Some(Change::Regressed),
            std::cmp::Ordering::Less => Some(Change::Improved),
            std::cmp::Ordering::Equal if !same(x.u, y.u) => Some(Change::Changed),
            std::cmp::Ordering::Equal => None,
        };
        match change {
            None => out.unchanged += 1,
            Some(c) => out.rows.push(Row {
                change: c,
                path: y.path.to_string(),
                a: Some(side(x.u)),
                b: Some(side(y.u)),
            }),
        }
    }
    for i in gone {
        out.rows.push(Row {
            change: Change::Gone,
            path: fa[i].path.to_string(),
            a: Some(side(fa[i].u)),
            b: None,
        });
    }
    for (j, y) in fb.iter().enumerate().filter(|(j, _)| !taken_b.contains(j)) {
        let _ = j;
        out.rows.push(Row {
            change: Change::New,
            path: y.path.to_string(),
            a: None,
            b: Some(side(y.u)),
        });
    }
    let line = |r: &Row| {
        r.b.as_ref()
            .or(r.a.as_ref())
            .map_or((0, String::new()), |s| (s.src_line, s.id.clone()))
    };
    out.rows.sort_by(|x, y| {
        // Worst first within regressions: a bigger step down sorts earlier.
        let step = |r: &Row| match (&r.a, &r.b) {
            (Some(a), Some(b)) => (b.status as i8) - (a.status as i8),
            _ => 0,
        };
        x.change
            .cmp(&y.change)
            .then_with(|| match x.change {
                Change::Regressed => step(y).cmp(&step(x)),
                Change::Improved => step(x).cmp(&step(y)),
                _ => std::cmp::Ordering::Equal,
            })
            .then_with(|| x.path.cmp(&y.path))
            .then_with(|| line(x).cmp(&line(y)))
    });
    for r in &out.rows {
        match r.change {
            Change::Regressed => out.regressed += 1,
            Change::Improved => out.improved += 1,
            Change::Changed => out.changed += 1,
            Change::New => out.new += 1,
            Change::Gone => out.gone += 1,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_order_is_better_to_worse() {
        assert!(Status::Converted < Status::Differs);
    }
}
