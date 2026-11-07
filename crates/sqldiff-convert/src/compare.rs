//! What changed between two conversions of a tree: the signal of the tweak
//! loop (an engine, pack or rule edit, then convert again).
//!
//! Computed from two finished reports, never by converting again. Units are
//! joined by their stable id (`review::unit_ids`), then by file path and
//! source line among those left; what is still unmatched is new or gone.
//! The order of the result is a function of the two reports alone: classes
//! in a fixed order, then path, source line and id.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Serialize;

use crate::tree::{FileReport, Report, UnitReport};
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
    /// For a matched unit, what differs: any of `status`, `rules`,
    /// `findings`, `reason`, `rewrites` (the rules' trace, which changes
    /// when a rule edit changes a converted unit's output), `impossible`,
    /// `accepted`, `proof`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub differs: Vec<&'static str>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Comparison {
    pub regressed: usize,
    pub improved: usize,
    pub changed: usize,
    pub new: usize,
    pub gone: usize,
    pub unchanged: usize,
    /// Files the run did not check (an increment): their units count as not
    /// converted (`plan.not-checked`), so they show as regressed.
    pub unchecked_a: usize,
    pub unchecked_b: usize,
    /// The two source folders differ (the folder may only have moved).
    pub src_differs: bool,
    /// Units per status in each run.
    pub totals_a: BTreeMap<String, usize>,
    pub totals_b: BTreeMap<String, usize>,
    pub rows: Vec<Row>,
}

struct Flat<'a> {
    path: &'a str,
    u: &'a UnitReport,
}

fn flatten(files: &[FileReport]) -> Vec<Flat<'_>> {
    files
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

/// What differs between two sides of a unit, in a fixed order. A converted
/// unit has no rules, findings or reason, so its `trace` is what shows that
/// a rule edit changed its output.
fn differs(x: &UnitReport, y: &UnitReport) -> Vec<&'static str> {
    let mut d = Vec::new();
    if x.status != y.status {
        d.push("status");
    }
    if x.rules != y.rules {
        d.push("rules");
    }
    if x.findings != y.findings {
        d.push("findings");
    }
    if x.reason != y.reason {
        d.push("reason");
    }
    if x.trace != y.trace {
        d.push("rewrites");
    }
    if x.impossible != y.impossible {
        d.push("impossible");
    }
    if x.accepted != y.accepted {
        d.push("accepted");
    }
    if x.proof != y.proof {
        d.push("proof");
    }
    d
}

fn tally(units: &[Flat<'_>]) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for f in units {
        *m.entry(f.u.status.label().to_string()).or_insert(0) += 1;
    }
    m
}

/// Compares two reports. Refuses reports of different conversions (other
/// `from` or `to`): their statuses mean nothing side by side.
pub fn compare(a: &Report, b: &Report) -> Result<Comparison, String> {
    if (a.from.as_str(), a.to.as_str()) != (b.from.as_str(), b.to.as_str()) {
        return Err(format!(
            "these are conversions of different dialects ({} to {} against {} to {}): nothing to compare",
            a.from, a.to, b.from, b.to
        ));
    }
    let mut c = compare_files(&a.files, &b.files);
    c.src_differs = a.src != b.src;
    Ok(c)
}

/// [`compare`] on the files of two reports. A pair of units is matched once;
/// a repeated id (identical statements) is matched in report order.
pub fn compare_files(a: &[FileReport], b: &[FileReport]) -> Comparison {
    let fa = flatten(a);
    let fb = flatten(b);
    let mut out = Comparison {
        totals_a: tally(&fa),
        totals_b: tally(&fb),
        unchecked_a: a.iter().filter(|f| !f.checked).count(),
        unchecked_b: b.iter().filter(|f| !f.checked).count(),
        ..Default::default()
    };
    let mut taken_b = BTreeSet::new();
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    let mut open_a: Vec<usize> = Vec::new();
    // First: by id (per path, because the id may repeat across files).
    let mut by_id: BTreeMap<(&str, &str), VecDeque<usize>> = BTreeMap::new();
    for (j, y) in fb.iter().enumerate() {
        by_id.entry((y.path, &y.u.id)).or_default().push_back(j);
    }
    for (i, x) in fa.iter().enumerate() {
        match by_id
            .get_mut(&(x.path, x.u.id.as_str()))
            .and_then(VecDeque::pop_front)
        {
            Some(j) => {
                taken_b.insert(j);
                pairs.push((i, j));
            }
            None => open_a.push(i),
        }
    }
    // Then: by path and source line.
    let mut by_line: BTreeMap<(&str, usize), VecDeque<usize>> = BTreeMap::new();
    for (j, y) in fb.iter().enumerate().filter(|(j, _)| !taken_b.contains(j)) {
        by_line
            .entry((y.path, y.u.src_line))
            .or_default()
            .push_back(j);
    }
    let mut gone: Vec<usize> = Vec::new();
    for i in open_a {
        let x = &fa[i];
        match by_line
            .get_mut(&(x.path, x.u.src_line))
            .and_then(VecDeque::pop_front)
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
        let d = differs(x.u, y.u);
        let change = match y.u.status.cmp(&x.u.status) {
            std::cmp::Ordering::Greater => Some(Change::Regressed),
            std::cmp::Ordering::Less => Some(Change::Improved),
            std::cmp::Ordering::Equal if !d.is_empty() => Some(Change::Changed),
            std::cmp::Ordering::Equal => None,
        };
        match change {
            None => out.unchanged += 1,
            Some(c) => out.rows.push(Row {
                change: c,
                path: y.path.to_string(),
                a: Some(side(x.u)),
                b: Some(side(y.u)),
                differs: d,
            }),
        }
    }
    for i in gone {
        out.rows.push(Row {
            change: Change::Gone,
            path: fa[i].path.to_string(),
            a: Some(side(fa[i].u)),
            b: None,
            differs: Vec::new(),
        });
    }
    for y in fb
        .iter()
        .enumerate()
        .filter(|(j, _)| !taken_b.contains(j))
        .map(|(_, y)| y)
    {
        out.rows.push(Row {
            change: Change::New,
            path: y.path.to_string(),
            a: None,
            b: Some(side(y.u)),
            differs: Vec::new(),
        });
    }
    let line = |r: &Row| {
        r.b.as_ref()
            .or(r.a.as_ref())
            .map_or((0, String::new()), |s| (s.src_line, s.id.clone()))
    };
    // The size of a status step: positive when the second run is worse.
    let step = |r: &Row| match (&r.a, &r.b) {
        (Some(a), Some(b)) => (b.status as i8) - (a.status as i8),
        _ => 0,
    };
    // Stable: ties keep report order.
    out.rows.sort_by(|x, y| {
        x.change
            .cmp(&y.change)
            .then_with(|| match x.change {
                // A bigger step down first.
                Change::Regressed => step(y).cmp(&step(x)),
                // A bigger step up first.
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
    use serde_json::json;

    fn unit(id: &str, status: &str, line: usize, rule: &str) -> serde_json::Value {
        json!({
            "id": id, "status": status, "src_line": line, "out_line": line,
            "rules": if rule.is_empty() { vec![] } else { vec![rule] },
            "reason": "", "findings": [], "trace": [],
        })
    }

    fn file(path: &str, units: Vec<serde_json::Value>) -> FileReport {
        serde_json::from_value(json!({
            "path": path, "status": "converted",
            "counts": {"converted": 0, "assumptions": 0, "decision": 0, "unproven": 0,
                       "differs": 0, "not_converted": 0},
            "units": units, "converted_units": [], "output_hash": "", "unplaced": [],
        }))
        .unwrap()
    }

    fn rows(c: &Comparison) -> Vec<(Change, &str, &str)> {
        c.rows
            .iter()
            .map(|r| {
                let s = r.b.as_ref().or(r.a.as_ref()).unwrap();
                (r.change, r.path.as_str(), s.id.as_str())
            })
            .collect()
    }

    #[test]
    fn status_order_is_better_to_worse() {
        assert!(Status::Converted < Status::Differs);
    }

    #[test]
    fn all_five_classes_come_in_a_fixed_order() {
        let a = vec![
            file(
                "b.sql",
                vec![
                    unit("keep", "converted", 1, ""),
                    // converted -> differs: a step of 4.
                    unit("big", "converted", 3, ""),
                    // assumptions -> unproven: a step of 2.
                    unit("small", "assumptions", 5, "ASM-X"),
                    unit("better", "unproven", 7, "equiv.x"),
                    unit("rule", "assumptions", 9, "ASM-X"),
                    unit("vanished", "converted", 11, ""),
                ],
            ),
            // The same step as `small`, in an earlier path: wins the tie.
            file("a.sql", vec![unit("tie", "assumptions", 2, "ASM-X")]),
        ];
        let b = vec![
            file(
                "b.sql",
                vec![
                    unit("keep", "converted", 1, ""),
                    unit("big", "differs", 3, "diff.x"),
                    unit("small", "unproven", 5, "equiv.y"),
                    unit("better", "assumptions", 7, "ASM-X"),
                    unit("rule", "assumptions", 9, "ASM-Y"),
                    unit("fresh", "converted", 13, ""),
                ],
            ),
            file("a.sql", vec![unit("tie", "unproven", 2, "equiv.y")]),
        ];
        let c = compare_files(&a, &b);
        assert_eq!(
            rows(&c),
            vec![
                (Change::Regressed, "b.sql", "big"),
                (Change::Regressed, "a.sql", "tie"),
                (Change::Regressed, "b.sql", "small"),
                (Change::Improved, "b.sql", "better"),
                (Change::Changed, "b.sql", "rule"),
                (Change::New, "b.sql", "fresh"),
                (Change::Gone, "b.sql", "vanished"),
            ]
        );
        assert_eq!(
            (
                c.regressed,
                c.improved,
                c.changed,
                c.new,
                c.gone,
                c.unchanged
            ),
            (3, 1, 1, 1, 1, 1)
        );
        assert_eq!(c.rows[4].differs, vec!["rules"]);
        // The same two runs compared again give the same answer.
        assert_eq!(compare_files(&a, &b), c);
    }

    #[test]
    fn a_changed_rewrite_of_a_converted_unit_is_a_change() {
        let mut x = unit("u", "converted", 1, "");
        x["trace"] = json!([{"rule": "tsql.type.int", "start": 0, "end": 3, "inst": "integer"}]);
        let mut y = x.clone();
        y["trace"] = json!([{"rule": "tsql.type.int", "start": 0, "end": 3, "inst": "bigint"}]);
        let c = compare_files(&[file("f.sql", vec![x.clone()])], &[file("f.sql", vec![y])]);
        assert_eq!(rows(&c), vec![(Change::Changed, "f.sql", "u")]);
        assert_eq!(c.rows[0].differs, vec!["rewrites"]);
        let same = compare_files(&[file("f.sql", vec![x.clone()])], &[file("f.sql", vec![x])]);
        assert!(same.rows.is_empty() && same.unchanged == 1);
    }

    #[test]
    fn an_edited_unit_is_matched_by_its_line() {
        // The id changed with the source text; path and line still pair it.
        let a = [file("f.sql", vec![unit("old", "converted", 4, "")])];
        let b = [file("f.sql", vec![unit("new", "differs", 4, "diff.x")])];
        let c = compare_files(&a, &b);
        assert_eq!(rows(&c), vec![(Change::Regressed, "f.sql", "new")]);
        assert_eq!((c.new, c.gone), (0, 0));
        // Another line: gone and new.
        let b = [file("f.sql", vec![unit("new", "differs", 5, "diff.x")])];
        let c = compare_files(&a, &b);
        assert_eq!((c.new, c.gone, c.regressed), (1, 1, 0));
    }

    #[test]
    fn repeated_ids_pair_in_report_order() {
        let r = |status: &str| unit("same", status, 1, "");
        let a = [file(
            "f.sql",
            vec![r("converted"), r("converted"), r("converted")],
        )];
        let b = [file("f.sql", vec![r("converted"), r("differs")])];
        let c = compare_files(&a, &b);
        // Two pair up (one unchanged, one regressed); the third is gone.
        assert_eq!((c.unchanged, c.regressed, c.gone, c.new), (1, 1, 1, 0));
        // Same id in two files never pairs across files.
        let a = [file("a.sql", vec![r("converted")])];
        let b = [file("b.sql", vec![r("converted")])];
        let c = compare_files(&a, &b);
        assert_eq!((c.gone, c.new, c.unchanged), (1, 1, 0));
    }

    #[test]
    fn unchecked_files_are_counted() {
        let mut f = file(
            "f.sql",
            vec![unit("u", "not_converted", 1, "plan.not-checked")],
        );
        f.checked = false;
        let c = compare_files(&[file("f.sql", vec![unit("u", "converted", 1, "")])], &[f]);
        assert_eq!((c.unchecked_a, c.unchecked_b, c.regressed), (0, 1, 1));
    }
}
