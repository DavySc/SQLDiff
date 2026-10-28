//! `sqldiff changes`: review the SQL a branch changed (the pull request
//! workflow). Only changed files are read and diffed, base against head.
//! SARIF results point at the head revision, so code scanning can show
//! every one of them on the pull request; the Markdown summary is the
//! pull request comment. The command and the app's runs both call
//! [`review`], so they write the same bytes.

use std::cell::OnceCell;
use std::fmt::Write as _;

use sqldiff_core::hooks::Hooks;
use sqldiff_core::model::{Location, Side};
use sqldiff_core::render::{self, line_anchor, Placement};
use sqldiff_core::{phrase, Dialect, Finding, Report, Verdict};
use sqldiff_source::changes::{self, ChangeScope, ChangeSet, HeadLine, LineMap, PairedBy};
use sqldiff_syntax::LineIndex;

use crate::pipeline::Pipeline;

/// What happened to one changed file.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Modified,
    Renamed(u32),
    PairedByName,
    Added,
}

struct Entry {
    status: Status,
    /// Base path; `None` for an added file.
    base: Option<String>,
    head: String,
    report: Report,
    /// Base and head texts, for line mapping; `None` for an added file.
    texts: Option<(String, String)>,
    head_dialect: Dialect,
    head_text: String,
    /// Base-to-head line map, computed on first use.
    map: OnceCell<Option<LineMap>>,
}

impl Entry {
    /// The verdict this entry contributes: an added file was not compared
    /// with anything, so it is at best unknown.
    fn verdict(&self) -> Verdict {
        match self.status {
            Status::Added => self.report.open_verdict().max(Verdict::Unknown),
            _ => self.report.open_verdict(),
        }
    }

    fn change(&self) -> String {
        match self.status {
            Status::Modified => "modified".to_string(),
            Status::Renamed(s) => format!("renamed ({s}% similar)"),
            Status::PairedByName => "replaced (same object)".to_string(),
            Status::Added => "added".to_string(),
        }
    }
}

/// Reads what changed in `scope`'s repository and runs each changed file
/// through `pipe`, most severe first. `hooks` gets each file checked; a
/// cancel stops before the next one with [`sqldiff_core::hooks::CANCELLED`].
/// Nothing is written.
pub fn review(scope: &ChangeScope, pipe: &Pipeline, hooks: &Hooks) -> Result<Reviewed, String> {
    if pipe.redacting() {
        return Err(
            "changes does not support --redact: its output is meant for the repository's own reviewers"
                .into(),
        );
    }
    let set = changes::changes(scope, &pipe.cfg).map_err(|e| e.0)?;
    let opts = pipe.options();
    let total = set.pairs.len() + set.added.len();
    let mut entries = Vec::new();
    for p in &set.pairs {
        hooks.stop_if_cancelled()?;
        let report = pipe.run(&p.base.artifact, Some(&p.head.artifact), &opts);
        entries.push(Entry {
            status: match &p.paired_by {
                PairedBy::Path => Status::Modified,
                PairedBy::Rename { similarity } => Status::Renamed(*similarity),
                PairedBy::Name { .. } => Status::PairedByName,
            },
            base: Some(p.base.path.clone()),
            head: p.head.path.clone(),
            report,
            texts: Some((p.base.artifact.text.clone(), p.head.artifact.text.clone())),
            head_dialect: p.head.artifact.dialect,
            head_text: p.head.artifact.text.clone(),
            map: OnceCell::new(),
        });
        hooks.report(entries.len(), total);
    }
    for v in &set.added {
        hooks.stop_if_cancelled()?;
        let report = pipe.run(&v.artifact, None, &opts);
        entries.push(Entry {
            status: Status::Added,
            base: None,
            head: v.path.clone(),
            report,
            texts: None,
            head_dialect: v.artifact.dialect,
            head_text: v.artifact.text.clone(),
            map: OnceCell::new(),
        });
        hooks.report(entries.len(), total);
    }
    // Most severe first, then by path.
    entries.sort_by(|x, y| {
        y.verdict()
            .cmp(&x.verdict())
            .then_with(|| x.head.cmp(&y.head))
    });
    Ok(Reviewed { set, entries })
}

/// The changed files of a branch by how they came out. `added` files were
/// checked on their own, not compared with anything; `not_compared` counts
/// what could not be compared (removed, ambiguous, no dialect, unreadable,
/// config changed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Counts {
    pub different: usize,
    pub not_proven: usize,
    pub equal_under_assumptions: usize,
    pub equal: usize,
    pub added: usize,
    pub not_compared: usize,
}

/// A reviewed branch: what changed, and the engine's report for each changed
/// file. Rendered as text, JSON, SARIF or the pull request comment.
pub struct Reviewed {
    set: ChangeSet,
    entries: Vec<Entry>,
}

/// Why something was not compared, one line each.
struct NotCompared {
    what: &'static str,
    path: String,
    why: String,
}

impl Reviewed {
    fn not_compared(&self) -> Vec<NotCompared> {
        let s = &self.set;
        let mut out = Vec::new();
        for v in &s.removed {
            out.push(NotCompared {
                what: "removed",
                path: v.path.clone(),
                why: "no file at head defines this object".to_string(),
            });
        }
        for a in &s.ambiguous {
            out.push(NotCompared {
                what: "ambiguous",
                path: a.name.clone(),
                why: format!(
                    "removed {} and added {} all define it; pair them with an explicit rename",
                    a.removed.join(", "),
                    a.added.join(", ")
                ),
            });
        }
        for p in &s.no_dialect {
            out.push(NotCompared {
                what: "no dialect",
                path: p.clone(),
                why: "map it with dialect_map, or pass --dialect".to_string(),
            });
        }
        for (p, why) in &s.unreadable {
            out.push(NotCompared {
                what: "unreadable",
                path: p.clone(),
                why: why.clone(),
            });
        }
        for p in &s.config_changed {
            out.push(NotCompared {
                what: "config changed",
                path: p.clone(),
                why: "this change edits SQLDiff configuration; results use the head's".to_string(),
            });
        }
        out
    }

    /// The worst verdict: anything not compared makes it at least unknown.
    /// Files the config ignores are reported but do not count.
    pub fn verdict(&self) -> Verdict {
        let worst = self
            .entries
            .iter()
            .map(Entry::verdict)
            .max()
            .unwrap_or(Verdict::Equal);
        if self.not_compared().is_empty() {
            worst
        } else {
            worst.max(Verdict::Unknown)
        }
    }

    /// How many changed files came out each way.
    pub fn counts(&self) -> Counts {
        let n = |v: Verdict| {
            self.entries
                .iter()
                .filter(|e| e.status != Status::Added && e.report.open_verdict() == v)
                .count()
        };
        Counts {
            different: n(Verdict::Different),
            not_proven: n(Verdict::Unknown),
            equal_under_assumptions: n(Verdict::EqualUnderAssumptions),
            equal: n(Verdict::Equal),
            added: self
                .entries
                .iter()
                .filter(|e| e.status == Status::Added)
                .count(),
            not_compared: self.not_compared().len(),
        }
    }

    /// Changed files in all: the ones read and the ones removed.
    pub fn files(&self) -> usize {
        self.entries.len() + self.set.removed.len()
    }

    fn counts_line(&self) -> String {
        let c = self.counts();
        [
            format!("{} different", c.different),
            format!("{} not proven", c.not_proven),
            format!("{} equal under assumptions", c.equal_under_assumptions),
            format!("{} equal", c.equal),
            format!("{} added", c.added),
            format!("{} not compared", c.not_compared),
        ]
        .join(" · ")
    }

    /// The primary location of a finding at head.
    fn place(&self, e: &Entry, f: &Finding) -> Placement {
        let head_start = || {
            line_anchor(
                Side::B,
                &e.head,
                first_code_line(&e.head_text, e.head_dialect),
            )
        };
        // An added file is checked on its own: its side A is the head file.
        if e.status == Status::Added {
            return Placement {
                primary: f.a.clone().unwrap_or_else(|| {
                    let mut l = head_start();
                    l.side = Side::A;
                    l
                }),
                note: None,
            };
        }
        if let Some(b) = &f.b {
            return Placement {
                primary: b.clone(),
                note: None,
            };
        }
        let (Some(a), Some((base, head))) = (&f.a, &e.texts) else {
            return Placement {
                primary: head_start(),
                note: Some("(Whole object.)".to_string()),
            };
        };
        let Some(map) = e.map.get_or_init(|| LineMap::new(base, head)) else {
            return Placement {
                primary: head_start(),
                note: Some(format!(
                    "(At base line {}; the file was rewritten too much to map lines.)",
                    a.start_line
                )),
            };
        };
        match (map.map(a.start_line), map.map(a.end_line)) {
            (HeadLine::Same(s), HeadLine::Same(t))
                if t >= s && t - s == a.end_line - a.start_line =>
            {
                let mut l: Location = a.clone();
                l.side = Side::B;
                l.path = e.head.clone();
                l.start_line = s;
                l.end_line = t;
                l.byte_start = 0;
                l.byte_end = 0;
                Placement {
                    primary: l,
                    note: Some(format!("(Base line {}, unchanged here.)", a.start_line)),
                }
            }
            (HeadLine::Same(s), _) => Placement {
                primary: line_anchor(Side::B, &e.head, s),
                note: Some(format!(
                    "(Base lines {}-{}, starting here.)",
                    a.start_line, a.end_line
                )),
            },
            (HeadLine::Gap { after, replaced }, _) => {
                let lines = map.head_lines();
                let (line, note) = if replaced {
                    (
                        after + 1,
                        format!("(Base line {} was replaced here.)", a.start_line),
                    )
                } else if after < lines {
                    (
                        after + 1,
                        format!(
                            "(Base line {} was removed just above this line.)",
                            a.start_line
                        ),
                    )
                } else {
                    (
                        after.max(1),
                        format!(
                            "(Base line {} was removed just below this line.)",
                            a.start_line
                        ),
                    )
                };
                Placement {
                    primary: line_anchor(Side::B, &e.head, line),
                    note: Some(note),
                }
            }
        }
    }

    pub fn sarif(&self) -> String {
        let reports: Vec<Report> = self.entries.iter().map(|e| e.report.clone()).collect();
        render::sarif_placed(&reports, &|i, f| self.place(&self.entries[i], f)) + "\n"
    }

    pub fn json(&self) -> String {
        let s = &self.set;
        let entries: Vec<serde_json::Value> = self
            .entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "change": e.change(),
                    "base": e.base,
                    "head": e.head,
                    "verdict": e.verdict().label(),
                    "report": e.report,
                })
            })
            .collect();
        let not: Vec<serde_json::Value> = self
            .not_compared()
            .iter()
            .map(|n| serde_json::json!({ "what": n.what, "path": n.path, "why": n.why }))
            .collect();
        let v = serde_json::json!({
            "verdict": self.verdict().label(),
            "base": s.base,
            "head": s.head,
            "merge_base": s.merge_base,
            "changed_files": s.changed_files,
            "entries": entries,
            "not_compared": not,
            "ignored": s.ignored,
        });
        serde_json::to_string_pretty(&v).expect("json") + "\n"
    }

    pub fn text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "{} changed SQL file(s) between {} and {}: {}",
            self.entries.len() + self.set.removed.len(),
            short(&self.set.merge_base),
            short(&self.set.head),
            self.counts_line()
        );
        let _ = writeln!(
            out,
            "{:<26} {:<24} {:>8}  file",
            "verdict", "change", "findings"
        );
        for e in &self.entries {
            let file = match &e.base {
                Some(b) if *b != e.head => format!("{} (was {b})", e.head),
                _ => e.head.clone(),
            };
            let _ = writeln!(
                out,
                "{:<26} {:<24} {:>8}  {file}",
                e.verdict().label(),
                e.change(),
                e.report.findings.len(),
            );
        }
        for n in self.not_compared() {
            let _ = writeln!(
                out,
                "{:<26} {:<24} {:>8}  {}: {}",
                "not compared", n.what, "-", n.path, n.why
            );
        }
        for p in &self.set.ignored {
            let _ = writeln!(
                out,
                "{:<26} {:<24} {:>8}  {p}",
                "ignored by config", "-", "-"
            );
        }
        let _ = writeln!(out, "Overall: {}", self.verdict().label());
        out
    }

    pub fn markdown(&self) -> String {
        markdown(self)
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// Line of the first token that is not whitespace or a comment.
fn first_code_line(text: &str, dialect: Dialect) -> usize {
    let root = sqldiff_syntax::parse(text, dialect).syntax();
    let at = root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| !t.kind().is_trivia() && !t.text().trim().is_empty())
        .map(|t| usize::from(t.text_range().start()))
        .unwrap_or(0);
    LineIndex::new(text).line_col(at).0
}

/// A path as Markdown code, safe inside a table cell.
fn code(s: &str) -> String {
    format!("`{}`", s.replace('`', "'").replace('|', "\\|"))
}

/// GitHub rejects comments over 65,536 characters.
const COMMENT_LIMIT: usize = 60_000;

/// Marks the comment so the Action updates it instead of adding another.
pub const MARKER: &str = "<!-- sqldiff:changes -->";

fn markdown(r: &Reviewed) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{MARKER}");
    let files = r.entries.len() + r.set.removed.len();
    let _ = writeln!(
        out,
        "### SQLDiff: {}\n",
        match r.verdict() {
            Verdict::Different => "behaviour differs",
            Verdict::Unknown => "needs review: not everything is proven",
            Verdict::EqualUnderAssumptions => "equal under assumptions",
            Verdict::Equal => "no behaviour change found",
        }
    );
    let _ = writeln!(
        out,
        "{files} changed SQL file{}: {}.\n",
        if files == 1 { "" } else { "s" },
        r.counts_line()
    );
    if r.verdict() == Verdict::Unknown {
        let _ = writeln!(
            out,
            "\"Not proven\" means SQLDiff could neither show that the behaviour is the same nor find a difference. A reviewer has to read those changes.\n"
        );
    }
    if !r.entries.is_empty() || !r.set.removed.is_empty() {
        let _ = writeln!(
            out,
            "| Verdict | File | Change | Findings |\n|---|---|---|---|"
        );
        for e in &r.entries {
            let file = match &e.base {
                Some(b) if *b != e.head => format!("{} (was {})", code(&e.head), code(b)),
                _ => code(&e.head),
            };
            let highest = e
                .report
                .highest_class()
                .map(|c| format!(" (highest: {})", c.label()))
                .unwrap_or_default();
            let verdict = match (e.status, e.verdict()) {
                (Status::Added, _) => "added, not compared".to_string(),
                (_, Verdict::Unknown) => "**not proven**".to_string(),
                (_, Verdict::Different) => "**different**".to_string(),
                (_, v) => v.label().to_string(),
            };
            let _ = writeln!(
                out,
                "| {verdict} | {file} | {} | {}{highest} |",
                e.change(),
                e.report.findings.len()
            );
        }
        for v in &r.set.removed {
            let _ = writeln!(
                out,
                "| removed, not compared | {} | removed | - |",
                code(&v.path)
            );
        }
        out.push('\n');
    }

    // Findings in the phrase grammar, one group per file.
    let groups: Vec<phrase::Group> = r
        .entries
        .iter()
        .map(|e| phrase::Group {
            label: match &e.base {
                Some(b) if *b != e.head => format!("{} (was {b})", e.head),
                _ => e.head.clone(),
            },
            findings: e.report.findings.iter().collect(),
            assumptions: &e.report.assumptions,
            denied: &[],
        })
        .collect();
    let any = groups.iter().any(|g| !g.findings.is_empty());
    if any {
        let mut text = phrase::comment(
            &groups,
            &phrase::Style {
                excerpt_lines: 1,
                fingerprints: true,
            },
        );
        let room = COMMENT_LIMIT.saturating_sub(out.len() + 2_000);
        if text.len() > room {
            let mut cut = room;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            let cut = text[..cut].rfind('\n').map_or(0, |i| i + 1);
            let left = text[cut..].lines().count();
            text.truncate(cut);
            let _ = writeln!(
                text,
                "… {left} more lines are not shown here (comment size limit). The job log and the SARIF upload have every finding."
            );
        }
        let _ = writeln!(
            out,
            "<details open><summary>Findings (A is the base, B is the head)</summary>\n\n~~~text\n{}~~~\n\n</details>\n",
            text.replace("~~~", "～～～")
        );
    }

    // Why each unproven file is unproven, when its findings do not say.
    let reasons: Vec<(&Entry, &String)> = r
        .entries
        .iter()
        .filter(|e| e.status != Status::Added && e.verdict() == Verdict::Unknown)
        .flat_map(|e| e.report.unknown_reasons.iter().map(move |x| (e, x)))
        .collect();
    if !reasons.is_empty() {
        let _ = writeln!(out, "**Why not proven**\n");
        for (e, why) in reasons {
            let _ = writeln!(out, "- {}: {}", code(&e.head), why.replace('\n', " "));
        }
        out.push('\n');
    }
    let not = r.not_compared();
    if !not.is_empty() {
        let _ = writeln!(out, "**Not compared**\n");
        for n in not {
            let _ = writeln!(out, "- {} {}: {}", n.what, code(&n.path), n.why);
        }
        out.push('\n');
    }
    if !r.set.ignored.is_empty() {
        let _ = writeln!(
            out,
            "Ignored by the config's `ignore` patterns: {}.\n",
            r.set
                .ignored
                .iter()
                .map(|p| code(p))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let _ = writeln!(
        out,
        "<sub>sqldiff {} · base {} · head {} · only files changed since the merge base were read.</sub>",
        sqldiff_core::TOOL_VERSION,
        short(&r.set.merge_base),
        short(&r.set.head)
    );
    out
}
