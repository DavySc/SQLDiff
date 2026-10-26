//! The conversion review queue: what a person should look at, worst first.
//!
//! Built from a converted tree's report (with the review ledger applied, see
//! `tree::review`) and, for templates, the instance check's report. Items
//! are ranked by tier, never by a score: a score lets many clean units
//! average away one that nothing was claimed about.
//!
//! | Tier | Items |
//! |---|---|
//! | 1 | Differs, not converted, rejected in review; a missing object many units fail to bind on, as one item; output findings outside any unit; templates whose instances differ |
//! | 2 | Not proven; instances that are unknown or not woven; templates no instance uses; generator output no instance accounts for |
//! | 3 | Output an agent wrote that no person approved |
//! | 4 | Open decisions |
//! | 5 | Open assumptions (unverified vendor facts and template premises first) |
//! | 6 | Plainly converted units nobody approved (listed only on request): nothing found, not proven |
//!
//! Inside a tier: blast radius (instances woven from the template, units
//! failing on a missing object), then path and line. Reviewed units are not
//! listed; nothing is ever listed as safe.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::review::By;
use crate::tree::{FileReport, Report, UnitReport};
use crate::Status;

/// The parts of `instance-report.json` (`sqldiff instances`) the queue
/// reads. Unknown fields are ignored; a status the queue does not know is
/// treated as unknown (tier 2), never as converted.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Instances {
    #[serde(default)]
    pub instances: Vec<Instance>,
    #[serde(default)]
    pub templates: Vec<TemplateStatus>,
    #[serde(default)]
    pub not_exercised: Vec<String>,
    #[serde(default)]
    pub not_in_manifest: Vec<String>,
    /// Generator target output no instance accounts for.
    #[serde(default)]
    pub not_in_manifest_target: Vec<String>,
    /// Every instance done, every template exercised, all generator output
    /// accounted for (absent in reports from before the generator check).
    #[serde(default)]
    pub complete: Option<bool>,
    /// The inputs as given to `sqldiff instances`, and their `tree::dir_hash`.
    #[serde(default)]
    pub manifest: Option<String>,
    #[serde(default)]
    pub woven: Option<String>,
    #[serde(default)]
    pub woven_target: Option<String>,
    #[serde(default)]
    pub hashes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Instance {
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub templates: Vec<String>,
    /// Converted and checked on the generator's output. A report without it
    /// checked nothing on the generator's output, so it defaults to false.
    #[serde(default)]
    pub done: bool,
    /// Sides checked on SQLDiff's weave only.
    #[serde(default)]
    pub weave_only: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TemplateStatus {
    pub path: String,
    pub status: String,
    #[serde(default)]
    pub instances: usize,
    /// Every instance that uses it is done (false when the report predates it).
    #[serde(default)]
    pub done: bool,
    /// Instances that use it checked on SQLDiff's weave only.
    #[serde(default)]
    pub weave_only: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// One converted unit.
    Unit,
    /// An object several units fail to bind on (`units` lists them).
    MissingObject,
    /// A gating output finding, or something the engine did not compare,
    /// outside any unit of a file.
    Unplaced,
    /// A template judged by its woven instances.
    Template,
    /// A converted template no instance uses: never checked.
    NotExercised,
    /// Generator output no instance in the manifest accounts for.
    NotInManifest,
    /// A source file that could not be read as text: not converted.
    Unreadable,
}

#[derive(Debug, Clone, Serialize)]
pub struct Item {
    /// Stable within a run: the unit id, or `kind:path[:what]`.
    pub key: String,
    pub tier: u8,
    pub kind: Kind,
    /// File in the converted tree (for `not_in_manifest`, the generator file).
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// 1-based lines in the input and in the written output file; 0 when none.
    pub src_line: usize,
    pub out_line: usize,
    /// The unit's status, or the instance status for templates.
    pub status: String,
    pub rules: Vec<String>,
    pub reason: String,
    /// Units this item stands for (a missing object), by id.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub units: Vec<String>,
    /// Instances woven from this file's template (with an instance report).
    pub instances: usize,
    /// Instances of this template that are not converted, by name and status.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failing_instances: Vec<(String, String)>,
    /// Assumptions and decisions nobody accepted or picked yet.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub open: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub written_by: Option<By>,
    /// A decision on this unit no longer applies: someone must confirm it again.
    pub outdated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Queue {
    pub items: Vec<Item>,
    /// Items per tier, 1 to 6.
    pub tiers: BTreeMap<u8, usize>,
    /// Units reviewed by people (not listed).
    pub reviewed: usize,
    /// Whether an instance report was read.
    pub instances: bool,
}

pub fn tier_label(t: u8) -> &'static str {
    match t {
        1 => "wrong or missing",
        2 => "nothing claimed",
        3 => "written by an agent",
        4 => "needs a decision",
        5 => "under assumptions",
        _ => "nothing found, not reviewed",
    }
}

/// Per template path: instances woven from it, and those not converted
/// (name, status).
type PerTemplate<'a> = BTreeMap<&'a str, (usize, Vec<(String, String)>)>;

/// Binding rules whose finding names an object another file should create.
const MISSING: [&str; 3] = [
    "bind.unknown-table",
    "bind.unknown-routine",
    "bind.unknown-type",
];

/// Builds the queue. `all` also lists plainly converted units nobody
/// approved (tier 6).
pub fn build(r: &Report, inst: Option<&Instances>, all: bool) -> Queue {
    let mut items = Vec::new();
    let mut reviewed = 0;
    // Instances per template, and the failing ones.
    let mut per_template: PerTemplate = BTreeMap::new();
    if let Some(i) = inst {
        for t in &i.templates {
            per_template.entry(t.path.as_str()).or_default().0 = t.instances;
        }
        for x in &i.instances {
            // Not done: not converted, or checked on SQLDiff's weave only.
            let why = if x.status != "converted" {
                Some(x.status.clone())
            } else if !x.done {
                Some(weave_only_label(&x.status, &x.weave_only))
            } else {
                None
            };
            if let Some(why) = why {
                for t in &x.templates {
                    per_template
                        .entry(t.as_str())
                        .or_default()
                        .1
                        .push((x.name.clone(), why.clone()));
                }
            }
        }
    }
    let blast = |p: &str| per_template.get(p).map_or(0, |e| e.0);

    // Units failing only because an object is missing, grouped by object.
    let mut missing: BTreeMap<String, Vec<(&FileReport, &UnitReport)>> = BTreeMap::new();
    for f in &r.files {
        for u in f.units.iter().chain(&f.converted_units) {
            let rev = u.review.as_ref();
            if rev.is_some_and(|x| x.reviewed) {
                reviewed += 1;
                continue;
            }
            if let Some(obj) = missing_object(u) {
                missing.entry(obj).or_default().push((f, u));
                continue;
            }
            let Some(tier) = unit_tier(u, all) else {
                continue;
            };
            items.push(unit_item(f, u, tier, &per_template));
        }
        if !f.unplaced.is_empty() {
            items.push(Item {
                key: format!("unplaced:{}", f.path),
                tier: 1,
                kind: Kind::Unplaced,
                path: f.path.clone(),
                unit: None,
                src_line: 0,
                out_line: 0,
                status: Status::Differs.label().to_string(),
                rules: f
                    .unplaced
                    .iter()
                    .map(|p| p.split(':').next().unwrap_or(p).to_string())
                    .collect(),
                reason: format!(
                    "{} output finding(s) outside any unit: the file cannot count as converted",
                    f.unplaced.len()
                ),
                units: Vec::new(),
                instances: blast(&f.path),
                failing_instances: Vec::new(),
                open: Vec::new(),
                written_by: None,
                outdated: false,
            });
        }
        if !f.unproven.is_empty() {
            items.push(Item {
                key: format!("unproven:{}", f.path),
                tier: 2,
                kind: Kind::Unplaced,
                path: f.path.clone(),
                unit: None,
                src_line: 0,
                out_line: 0,
                status: Status::Unproven.label().to_string(),
                rules: vec![crate::NOT_COMPARED.to_string()],
                reason: format!(
                    "the engine did not compare {} thing(s) outside any unit: {}",
                    f.unproven.len(),
                    f.unproven.join("; ")
                ),
                units: Vec::new(),
                instances: blast(&f.path),
                failing_instances: Vec::new(),
                open: Vec::new(),
                written_by: None,
                outdated: false,
            });
        }
    }
    for (obj, us) in missing {
        if us.len() == 1 {
            let (f, u) = us[0];
            items.push(unit_item(f, u, 1, &per_template));
            continue;
        }
        let (f, u) = us[0];
        items.push(Item {
            key: format!("missing:{obj}"),
            tier: 1,
            kind: Kind::MissingObject,
            path: f.path.clone(),
            unit: None,
            src_line: u.src_line,
            out_line: u.out_line,
            status: Status::Differs.label().to_string(),
            rules: u.rules.clone(),
            reason: format!(
                "{} units in {} file(s) fail to bind on {obj}: convert or create it first",
                us.len(),
                us.iter()
                    .map(|(f, _)| f.path.as_str())
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
            ),
            units: us.iter().map(|(_, u)| u.id.clone()).collect(),
            instances: us.iter().map(|(f, _)| blast(&f.path)).sum(),
            failing_instances: Vec::new(),
            open: Vec::new(),
            written_by: None,
            outdated: us
                .iter()
                .any(|(_, u)| u.review.as_ref().is_some_and(outdated)),
        });
    }
    if let Some(i) = inst {
        for t in &i.templates {
            let tier = match t.status.as_str() {
                "converted" if t.done => continue,
                "assumptions" => 5,
                "decision" => 4,
                "differs" => 1,
                _ => 2,
            };
            // Instances checked on SQLDiff's weave only say nothing about
            // the generator's output: never better than nothing claimed.
            let tier = if t.done { tier } else { tier.min(2) };
            items.push(Item {
                key: format!("template:{}", t.path),
                tier,
                kind: Kind::Template,
                path: t.path.clone(),
                unit: None,
                src_line: 0,
                out_line: 0,
                status: t.status.clone(),
                rules: Vec::new(),
                reason: if t.status == "converted" || t.weave_only > 0 {
                    format!(
                        "{} of {} instance(s) woven from it are not done; {} checked on SQLDiff's weave only, not on the generator's output",
                        per_template.get(t.path.as_str()).map_or(0, |e| e.1.len()),
                        t.instances,
                        t.weave_only
                    )
                } else {
                    format!(
                        "{} of {} instance(s) woven from it are not converted",
                        per_template.get(t.path.as_str()).map_or(0, |e| e.1.len()),
                        t.instances
                    )
                },
                units: Vec::new(),
                instances: t.instances,
                failing_instances: per_template
                    .get(t.path.as_str())
                    .map(|e| e.1.clone())
                    .unwrap_or_default(),
                open: Vec::new(),
                written_by: None,
                outdated: false,
            });
        }
        for (paths, kind, prefix, reason) in [
            (
                &i.not_exercised,
                Kind::NotExercised,
                "not-exercised",
                "no instance uses this template: it was never checked; add an instance",
            ),
            (
                &i.not_in_manifest,
                Kind::NotInManifest,
                "not-in-manifest",
                "generator output no instance accounts for: the manifest does not describe the generator",
            ),
            (
                &i.not_in_manifest_target,
                Kind::NotInManifest,
                "not-in-manifest-target",
                "generator target output no instance accounts for: the manifest does not describe the generator",
            ),
        ] {
            for p in paths {
                items.push(Item {
                    key: format!("{prefix}:{p}"),
                    tier: 2,
                    kind,
                    path: p.clone(),
                    unit: None,
                    src_line: 0,
                    out_line: 0,
                    status: "unknown".into(),
                    rules: Vec::new(),
                    reason: reason.into(),
                    units: Vec::new(),
                    instances: 0,
                    failing_instances: Vec::new(),
                    open: Vec::new(),
                    written_by: None,
                    outdated: false,
                });
            }
        }
    }
    for p in &r.unreadable {
        items.push(Item {
            key: format!("unreadable:{p}"),
            tier: 1,
            kind: Kind::Unreadable,
            path: p.clone(),
            unit: None,
            src_line: 0,
            out_line: 0,
            status: Status::NotConverted.label().to_string(),
            rules: Vec::new(),
            reason: "the source file could not be read as text: it was not converted".into(),
            units: Vec::new(),
            instances: blast(p),
            failing_instances: Vec::new(),
            open: Vec::new(),
            written_by: None,
            outdated: false,
        });
    }
    items.sort_by(|a, b| {
        a.tier
            .cmp(&b.tier)
            .then(rank(b).cmp(&rank(a)))
            .then(b.instances.cmp(&a.instances))
            .then(b.units.len().cmp(&a.units.len()))
            .then(a.path.cmp(&b.path))
            .then(a.out_line.cmp(&b.out_line))
    });
    let mut tiers = BTreeMap::new();
    for t in 1..=6u8 {
        tiers.insert(t, items.iter().filter(|i| i.tier == t).count());
    }
    Queue {
        items,
        tiers,
        reviewed,
        instances: inst.is_some(),
    }
}

/// Inside tier 5: unverified vendor facts and template premises first.
fn rank(i: &Item) -> u8 {
    if i.tier == 5
        && i.open
            .iter()
            .any(|r| r.starts_with("ASM-FACT-") || r.starts_with("ASM-TEMPLATE-"))
    {
        1
    } else {
        0
    }
}

fn outdated(r: &crate::tree::UnitReviewReport) -> bool {
    r.decisions.iter().any(|d| !d.current)
}

/// The tier of a unit nobody reviewed, or None when it is not listed.
fn unit_tier(u: &UnitReport, all: bool) -> Option<u8> {
    let rev = u.review.as_ref();
    let rejected = rev.is_some_and(|r| {
        r.decisions
            .iter()
            .any(|d| d.current && d.record.state == crate::review::State::Rejected)
    });
    let open = rev.map_or(
        matches!(u.status, Status::Assumptions | Status::Decision),
        |r| !r.open.is_empty(),
    );
    Some(if u.status >= Status::Differs || rejected {
        1
    } else if u.status == Status::Unproven {
        2
    } else if rev.is_some_and(|r| r.written_by == Some(By::Agent)) {
        3
    } else if open && u.status == Status::Decision {
        // The report does not say which open rule is the decision: the
        // higher tier.
        4
    } else if open {
        5
    } else if all {
        6
    } else {
        return None;
    })
}

/// The object a unit fails to bind on, when that is all that is wrong with
/// it: one missing table, routine or type.
fn missing_object(u: &UnitReport) -> Option<String> {
    if u.status != Status::Differs || u.rules.len() != 1 || !MISSING.contains(&u.rules[0].as_str())
    {
        return None;
    }
    if u.review
        .as_ref()
        .is_some_and(|r| r.written_by.is_some() || r.decisions.iter().any(|d| d.current))
    {
        return None;
    }
    // "Table `stock` does not exist ..."
    let kind = u.rules[0].trim_start_matches("bind.unknown-");
    let name = u.reason.split('`').nth(1)?;
    Some(format!("{kind} `{}`", name.to_ascii_lowercase()))
}

fn unit_item(f: &FileReport, u: &UnitReport, tier: u8, per_template: &PerTemplate<'_>) -> Item {
    let rev = u.review.as_ref();
    Item {
        key: u.id.clone(),
        tier,
        kind: Kind::Unit,
        path: f.path.clone(),
        unit: Some(u.id.clone()),
        src_line: u.src_line,
        out_line: u.out_line,
        status: u.status.label().to_string(),
        rules: u.rules.clone(),
        reason: u.reason.clone(),
        units: Vec::new(),
        instances: per_template.get(f.path.as_str()).map_or(0, |e| e.0),
        failing_instances: Vec::new(),
        open: match rev {
            Some(r) => r.open.clone(),
            None if matches!(u.status, Status::Assumptions | Status::Decision) => u.rules.clone(),
            None => Vec::new(),
        },
        written_by: rev.and_then(|r| r.written_by),
        outdated: rev.is_some_and(outdated),
    }
}

/// The queue as Markdown, for reading.
pub fn markdown(q: &Queue) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(s, "# Review queue\n");
    let _ = writeln!(
        s,
        "Worst first. Nothing here is safe to skip; reviewed units ({}) are not listed. {}\n",
        q.reviewed,
        if q.instances {
            "Templates are judged by their woven instances."
        } else {
            "No instance report was read: templates are judged on their own text only."
        }
    );
    for t in 1..=6u8 {
        let items: Vec<&Item> = q.items.iter().filter(|i| i.tier == t).collect();
        if items.is_empty() {
            continue;
        }
        let _ = writeln!(s, "## Tier {t}: {} ({})\n", tier_label(t), items.len());
        for i in items {
            let at = if i.out_line > 0 {
                format!(":{}", i.out_line)
            } else {
                String::new()
            };
            let _ = writeln!(
                s,
                "- `{}{at}` **{}**{}{} {}{}",
                i.path,
                i.status,
                if i.rules.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", i.rules.join(", "))
                },
                if i.instances > 0 {
                    format!(" [{} instances]", i.instances)
                } else {
                    String::new()
                },
                i.reason.lines().next().unwrap_or(""),
                if i.outdated {
                    " (a decision here is outdated)"
                } else {
                    ""
                }
            );
        }
        s.push('\n');
    }
    s
}

/// Reads `instance-report.json`, given as the file or the directory holding it.
pub fn read_instances(p: &std::path::Path) -> Result<Instances, String> {
    let file = if p.is_dir() {
        p.join("instance-report.json")
    } else {
        p.to_path_buf()
    };
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))
}

pub const QUEUE_JSON: &str = "review-queue.json";
pub const QUEUE_MD: &str = "review-queue.md";

/// Writes `review-queue.json` and `.md` into `out`.
pub fn write(
    q: &Queue,
    out: &std::path::Path,
) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let text = serde_json::to_string_pretty(q).map_err(|e| e.to_string())? + "\n";
    crate::tree::dest_under(out, QUEUE_JSON)?;
    crate::tree::dest_under(out, QUEUE_MD)?;
    let json = crate::tree::write_under(out, QUEUE_JSON, text.as_bytes())?;
    let md = crate::tree::write_under(out, QUEUE_MD, markdown(q).as_bytes())?;
    Ok((json, md))
}

/// One line per tier that has items.
pub fn summary(q: &Queue) -> String {
    let mut s = format!(
        "review queue: {} item(s), {} unit(s) reviewed\n",
        q.items.len(),
        q.reviewed
    );
    for (t, n) in &q.tiers {
        if *n > 0 {
            s.push_str(&format!("  tier {t} {:<28} {n:>6}\n", tier_label(*t)));
        }
    }
    s
}

fn weave_only_label(status: &str, sides: &[String]) -> String {
    if sides.is_empty() {
        format!("{status}, not checked on the generator's output")
    } else {
        format!(
            "{status} on SQLDiff's weave only (no generator output for the {} side)",
            sides.join(" and ")
        )
    }
}
