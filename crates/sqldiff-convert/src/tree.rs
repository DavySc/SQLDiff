//! Whole-tree conversion: the engine behind `sqldiff convert` and the MCP
//! `convert` and `convert_tree` tools, so both give the same statuses and
//! reasons for the same input.
//!
//! Two passes. The rules convert every file first (in parallel); then each
//! file is checked against catalogs built from the whole tree, the source
//! catalog from every input file and the target catalog from every output
//! file, so a call, column or type defined in another file binds. A file is
//! only reported as converted when every unit in it is.
//!
//! The output tree mirrors the input tree. Every unit that is not plainly
//! converted gets a `-- [sqldiff: <status>]` comment line above it, so a
//! reviewer reading only the output still sees it. The report
//! (`conversion-report.json` and `.md`) points into the annotated files.
//!
//! With a template mode every file is a generator template: each is
//! converted through its fragment (`rewrite_template`), with one set of
//! stand-in names for the whole tree so the catalogs bind across files, and
//! the output tree holds templates for the target.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::{Conversion, FragmentKind, StandIns, Status, ValueKind};
use serde::{Deserialize, Serialize};
use sqldiff_core::catalog::builtins::Helper;
use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{Dialect, Options};
use sqldiff_source::compose;
use sqldiff_syntax::template::Template;

/// How to convert a tree.
pub struct Args {
    /// The source tree, or one file. Locations in the report are relative to it.
    pub src: PathBuf,
    pub from: Dialect,
    pub to: Dialect,
    pub jobs: Option<usize>,
    /// Check each file alone, without catalogs built from the whole tree.
    pub no_catalog: bool,
    /// Every file is a template.
    pub template: Option<TemplateMode>,
    /// File extensions to read, without the dot, any letter case.
    pub extensions: Vec<String>,
    /// Pack helper functions: routines the dialects provide that no file in
    /// the tree defines. The output's check resolves calls to them.
    pub helpers: Vec<Helper>,
    /// The deployment profile (`[deployment]` in packs and config): the
    /// report lists which assumptions it accepts. Statuses do not change.
    pub deployment: sqldiff_source::deployment::Profile,
    /// Base catalogs (`--catalog`): DDL files in the source dialect of
    /// objects that exist already. Bound against on both sides (the tree's
    /// own objects win), converted for the target catalog only, never
    /// written or reported as files (`base`).
    pub catalogs: Vec<PathBuf>,
    /// The target's base catalogs (`--catalog-b`): the same objects as
    /// `catalogs` as DDL in the target dialect. Side B binds against these,
    /// as given, instead of converting `catalogs`. Needs `catalogs`.
    pub catalogs_b: Vec<PathBuf>,
}

/// How templates are read: the placeholder rules of `diff --template`.
pub struct TemplateMode {
    pub placeholders: compose::Placeholders,
    /// `--parameter` names; when given, only these (and the pack's
    /// `known_parameters`) are placeholders.
    pub parameters: Option<std::collections::BTreeSet<String>>,
    /// The pack's value kinds, by placeholder name.
    pub kinds: Vec<(String, ValueKind)>,
    /// The pack's fragment kinds, by path glob; the first match wins.
    pub fragments: Vec<(sqldiff_source::glob::Glob, FragmentKind)>,
    /// A routine's opening and closing templates, by path.
    pub pairs: Vec<(String, String)>,
}

impl TemplateMode {
    /// The template mode of the `[template]` syntax in the loaded packs or
    /// config: the same placeholder rules as `diff --template`.
    pub fn configured(
        cfg: &sqldiff_source::Resolved,
        parameters: Option<std::collections::BTreeSet<String>>,
    ) -> Result<TemplateMode, String> {
        let def = cfg.policy.template.as_ref().ok_or(
            "--template needs a [template] syntax: give --pack or a sqldiff.toml with one",
        )?;
        TemplateMode::new(def, parameters)
    }

    /// The template mode a pack's `[template]` section describes.
    pub fn new(
        def: &sqldiff_source::pack::TemplateSyntax,
        parameters: Option<std::collections::BTreeSet<String>>,
    ) -> Result<TemplateMode, String> {
        let placeholders = compose::Placeholders::new(def).map_err(|e| e.0)?;
        let upper = def.ignore_case;
        Ok(TemplateMode {
            kinds: def
                .values
                .iter()
                .map(|(n, k)| {
                    let n = if upper { n.to_uppercase() } else { n.clone() };
                    (n, ValueKind::parse(k).expect("the pack checks value kinds"))
                })
                .collect(),
            fragments: def
                .fragments
                .iter()
                .map(|f| {
                    Ok((
                        sqldiff_source::glob::Glob::new(&f.glob).map_err(|e| e.0)?,
                        FragmentKind::parse(&f.kind).expect("the pack checks fragment kinds"),
                    ))
                })
                .collect::<Result<_, String>>()?,
            pairs: def
                .pairs
                .iter()
                .map(|p| (p.head.clone(), p.foot.clone()))
                .collect(),
            placeholders,
            parameters,
        })
    }
}

/// File extensions as given (`sql`, `.SQL`, ...) without their dots;
/// `sql` when none are given.
pub fn extensions(given: Vec<String>) -> Vec<String> {
    if given.is_empty() {
        return vec!["sql".to_string()];
    }
    given
        .into_iter()
        .map(|e| e.trim_start_matches('.').to_string())
        .collect()
}

/// The rule id of the assumption every unit of a template with generator
/// directives carries (`check_template`).
const DIRECTIVE: &str = "ASM-TEMPLATE-DIRECTIVE";

/// One input file.
pub struct Input {
    /// Path relative to the source tree, with `/` separators.
    pub rel: String,
    pub text: String,
}

#[derive(Serialize, Deserialize)]
pub struct FileReport {
    pub path: String,
    /// The worst unit status in the file; `differs` when an output finding
    /// points into no unit.
    pub status: Status,
    pub counts: crate::Counts,
    pub seconds: f64,
    /// Units that are not plainly converted, in output order.
    pub units: Vec<UnitReport>,
    /// Plainly converted units, in output order: nothing found, not proven.
    pub converted_units: Vec<UnitReport>,
    /// `review::file_hash` of the file as written (with its status lines):
    /// a later run with `--overwrite` refuses to replace a file edited since.
    pub output_hash: String,
    /// Output findings that point into no unit.
    pub unplaced: Vec<String>,
    /// What the engine says it did not compare, at no unit's line: the file
    /// is then at best `not proven`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unproven: Vec<String>,
    /// For a template: what it holds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<crate::TemplateInfo>,
    /// What the review ledger says about the file, when one was read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<FileReviewReport>,
}

/// The ledger applied to one file.
#[derive(Serialize, Deserialize)]
pub struct FileReviewReport {
    /// Every unit reviewed and nothing unplaced.
    pub reviewed: bool,
    pub units_reviewed: usize,
    /// Units with a decision that no longer applies (the file changed, or the
    /// unit no longer has the rule).
    pub units_outdated: usize,
    /// Records whose unit no longer exists (its text changed): unit ids.
    pub orphaned: Vec<String>,
}

/// The ledger applied to one unit (`review::UnitReview`, without the unit).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitReviewReport {
    /// Reviewed by people: shown as "accepted in review", never as converted.
    pub reviewed: bool,
    /// Assumptions and decisions nobody accepted or picked yet.
    pub open: Vec<String>,
    /// What no decision can discharge.
    pub undischargeable: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub written_by: Option<crate::review::By>,
    pub decisions: Vec<crate::review::Applied>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UnitReport {
    /// Stable id (`review::unit_ids`): what review decisions are keyed to.
    pub id: String,
    pub status: Status,
    /// 1-based lines in the input file and in the written output file.
    pub src_line: usize,
    pub out_line: usize,
    /// Rules of the unit's worst notes (what decides its status); empty for
    /// a converted unit, which has none. The rewrites themselves are `trace`.
    pub rules: Vec<String>,
    pub reason: String,
    /// Rules of the engine's findings (diff and output check) on this unit,
    /// gating or not.
    pub findings: Vec<String>,
    /// Every rewrite the rules made in this unit (rule id, input byte span,
    /// instantiation), converted units included: the certificate's trace.
    pub trace: Vec<crate::Step>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<UnitReviewReport>,
    /// The unit's assumptions the deployment profile accepts. The unit is
    /// still under assumptions; see [`Report::deployment`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepted: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct Report {
    pub from: String,
    pub to: String,
    pub src: String,
    /// The output directory; absent when nothing was written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub out: Option<String>,
    pub catalogs: bool,
    /// Whether the files were read as templates.
    pub templates: bool,
    /// The base catalogs bound against (`--catalog`), in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub base_catalogs: Vec<crate::base::CatalogFile>,
    /// The target's base catalogs (`--catalog-b`), in order: side B binds
    /// against these instead of the converted `base_catalogs`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub base_catalogs_b: Vec<crate::base::CatalogFile>,
    /// One hash over their contents (`base::hash`): review decisions keep it
    /// and apply only while it is the same.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_hash: Option<String>,
    /// Suspect base objects (`suspect: true`: every statement using one
    /// carries `ASM-BASE-SUSPECT`), then the others whose conversion is not
    /// plainly converted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub base_objects: Vec<crate::base::ObjectReport>,
    /// Base statements that define no object and did not convert.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub base_statements: Vec<crate::base::StatementReport>,
    pub files: Vec<FileReport>,
    /// Files that could not be read as text.
    pub unreadable: Vec<String>,
    pub totals: Totals,
    /// The review ledger that was applied, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger: Option<LedgerReport>,
    /// The deployment profile and what it covers, when a pack or config
    /// file has a `[deployment]` table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment: Option<DeploymentReport>,
}

/// The deployment profile applied to a converted tree (decision 14). It
/// changes no status: an accepted assumption is listed as accepted, and
/// `assumptions_within_accept` says whether all of them are.
#[derive(Serialize, Deserialize)]
pub struct DeploymentReport {
    /// Accepted ids, each with the layers (pack, file) that accept it.
    pub accept: Vec<sqldiff_source::deployment::Accepted>,
    /// Facts the user asserts: their own declarations, not checked against
    /// a catalog and not used for any status yet.
    pub facts: Vec<DeploymentFact>,
    /// Every assumption the tree carries is accepted: each unit's
    /// assumption notes and the engine's open assumptions on each file (its
    /// diff and its output check). Only one of the conditions for shipping
    /// a tree unreviewed: units that differ, are not converted or are not
    /// proven carry no assumption for that and are not counted here.
    pub assumptions_within_accept: bool,
    /// Every assumption id the tree carries, accepted or not.
    pub carried: Vec<CarriedAssumption>,
}

#[derive(Serialize, Deserialize)]
pub struct DeploymentFact {
    pub key: String,
    pub value: String,
    pub by: sqldiff_core::model::DeploymentSource,
    /// The registered assumptions the fact bears on.
    pub bears_on: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct CarriedAssumption {
    pub id: String,
    /// Units with an assumption note of this id.
    pub units: usize,
    /// Files whose engine reports carry it.
    pub files: usize,
    pub accepted: bool,
}

#[derive(Serialize, Deserialize)]
pub struct LedgerReport {
    pub path: String,
    pub records: usize,
    /// Lines that did not parse or are not allowed; never applied.
    pub rejected_lines: Vec<String>,
    pub units_reviewed: usize,
    pub files_reviewed: usize,
}

impl Report {
    /// Every file is converted or converted under assumptions, and every
    /// file could be read: the CLI's exit code 0.
    pub fn ok(&self) -> bool {
        let worst = self
            .files
            .iter()
            .map(|f| f.status)
            .max()
            .unwrap_or(Status::Converted);
        worst <= Status::Assumptions && self.unreadable.is_empty()
    }
}

/// Counts keyed by status label, read back: a label no status has is refused.
fn by_status<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<std::collections::BTreeMap<&'static str, usize>, D::Error> {
    let read = std::collections::BTreeMap::<String, usize>::deserialize(d)?;
    read.into_iter()
        .map(|(k, n)| match ORDER.iter().find(|s| s.label() == k) {
            Some(s) => Ok((s.label(), n)),
            None => Err(serde::de::Error::custom(format!("no status {k:?}"))),
        })
        .collect()
}

#[derive(Serialize, Deserialize, Default)]
pub struct Totals {
    pub files: usize,
    #[serde(deserialize_with = "by_status")]
    pub files_by_status: std::collections::BTreeMap<&'static str, usize>,
    #[serde(deserialize_with = "by_status")]
    pub units: std::collections::BTreeMap<&'static str, usize>,
    pub seconds: f64,
    /// Most frequent reasons a unit is not converted, with counts.
    pub top_reasons: Vec<(String, usize)>,
}

/// A converted tree: the report, and every file as it is written (the
/// output with its status lines), in report order.
#[derive(Serialize, Deserialize)]
pub struct Tree {
    pub report: Report,
    pub outputs: Vec<String>,
    /// Each file's conversion as the engine returned it, in report order.
    pub conversions: Vec<Conversion>,
}

/// Reads the source tree (`collect`) for `a`. Fails when the pair has no
/// rules or no file has one of the extensions.
pub fn read(a: &Args) -> Result<(Vec<Input>, Vec<String>), String> {
    if !crate::supported(a.from, a.to) {
        return Err(format!(
            "no conversion rules from {} to {} yet",
            a.from.display_name(),
            a.to.display_name()
        ));
    }
    // The base catalogs too, so a bad one fails before anything is written.
    crate::base::read(&a.catalogs, a.no_catalog)?;
    crate::base::read_b(&a.catalogs_b, &a.catalogs, a.no_catalog)?;
    let (inputs, unreadable) = collect(&a.src, &a.extensions)?;
    if inputs.is_empty() {
        return Err(format!(
            "{}: no files with extension {}",
            a.src.display(),
            a.extensions.join(", ")
        ));
    }
    Ok((inputs, unreadable))
}

pub use sqldiff_core::hooks::{Hooks, CANCELLED};

/// Converts and checks `inputs` (read by `read`, or given directly) and
/// builds the report. Writes nothing; `out` only labels the report.
pub fn convert(
    a: &Args,
    inputs: &[Input],
    unreadable: Vec<String>,
    out: Option<&Path>,
) -> Result<Tree, String> {
    convert_with(a, inputs, unreadable, out, &Hooks::default())
}

/// [`convert`], reporting progress to `hooks` and stopping when they say so.
pub fn convert_with(
    a: &Args,
    inputs: &[Input],
    unreadable: Vec<String>,
    out: Option<&Path>,
    hooks: &Hooks,
) -> Result<Tree, String> {
    let started = Instant::now();
    if !crate::supported(a.from, a.to) {
        return Err(format!(
            "no conversion rules from {} to {} yet",
            a.from.display_name(),
            a.to.display_name()
        ));
    }
    let jobs = a
        .jobs
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
        .max(1);
    // Names the target catalog in binding reasons.
    let label = out.map_or_else(
        || PathBuf::from(format!("{} (converted, not written)", a.src.display())),
        Path::to_path_buf,
    );
    let label = label.as_path();
    // Output written by hand, kept in the ledger next to the converted tree.
    let ledger = match out {
        Some(o) => read_ledger(o)?,
        None => None,
    };
    let ledger = ledger.as_ref();
    let base_files = crate::base::read(&a.catalogs, a.no_catalog)?;
    let base_files_b = crate::base::read_b(&a.catalogs_b, &a.catalogs, a.no_catalog)?;
    let base = match base_files.is_empty() {
        true => None,
        false => Some(crate::base::prepare(
            &base_files,
            &base_files_b,
            a.from,
            a.to,
            &a.helpers,
            jobs,
            hooks,
        )?),
    };
    let results = match &a.template {
        None => convert_files(a, label, inputs, jobs, ledger, base.as_ref(), hooks)?,
        Some(tm) => convert_templates(a, label, tm, inputs, jobs, ledger, base.as_ref(), hooks)?,
    };
    let mut results = results;
    if let Some(b) = &base {
        let files: Vec<(&str, &str)> = inputs
            .iter()
            .map(|i| (i.rel.as_str(), i.text.as_str()))
            .collect();
        b.flag(&files, results.iter_mut().map(|(c, _)| c), a.from);
    }
    // A call to a converted user function holds when the function's own
    // conversion does.
    let mut convs: Vec<&mut Conversion> = results.iter_mut().map(|(c, _)| c).collect();
    crate::callees::resolve(inputs, &mut convs, a.from);
    let mut files = Vec::new();
    let mut outputs = Vec::new();
    let mut conversions = Vec::new();
    for (input, (c, secs)) in inputs.iter().zip(results) {
        let (text, lines) = annotate(&c);
        let mut f = file_report(&input.rel, &input.text, &c, &lines, secs);
        f.output_hash = crate::review::file_hash(&text);
        mark_accepted(&mut f, input, &c, &a.deployment);
        files.push(f);
        outputs.push(text);
        conversions.push(c);
    }
    let report = Report {
        from: a.from.id().to_string(),
        to: a.to.id().to_string(),
        src: a.src.display().to_string(),
        out: out.map(|o| o.display().to_string()),
        catalogs: !a.no_catalog,
        templates: a.template.is_some(),
        base_hash: base.as_ref().and_then(|b| b.hash.clone()),
        base_catalogs: base.as_ref().map(|b| b.files.clone()).unwrap_or_default(),
        base_catalogs_b: base.as_ref().map(|b| b.files_b.clone()).unwrap_or_default(),
        base_objects: base.as_ref().map(|b| b.objects.clone()).unwrap_or_default(),
        base_statements: base
            .as_ref()
            .map(|b| b.statements.clone())
            .unwrap_or_default(),
        totals: totals(&files, started.elapsed().as_secs_f64()),
        deployment: deployment_report(&a.deployment, &conversions),
        files,
        unreadable,
        ledger: None,
    };
    Ok(Tree {
        report,
        outputs,
        conversions,
    })
}

/// Writes a converted tree into `out` (checked by `prepare_out` first):
/// every output file at its source path, `conversion-report.json` and
/// `conversion-report.md`. Returns the two report paths.
pub fn write(tree: &Tree, out: &Path) -> Result<(PathBuf, PathBuf), String> {
    // Every destination first, so a refused one leaves nothing half written.
    for f in &tree.report.files {
        dest_under(out, &f.path)?;
    }
    for name in ["conversion-report.json", "conversion-report.md"] {
        dest_under(out, name)?;
    }
    for (f, text) in tree.report.files.iter().zip(&tree.outputs) {
        write_under(out, &f.path, text.as_bytes())?;
    }
    let json = write_under(
        out,
        "conversion-report.json",
        (serde_json::to_string_pretty(&tree.report).unwrap() + "\n").as_bytes(),
    )?;
    let md = write_under(
        out,
        "conversion-report.md",
        markdown(&tree.report).as_bytes(),
    )?;
    Ok((json, md))
}

/// Whether `rel` (with `/`) names a path inside a tree: only plain names,
/// no `..`, `.`, root, drive or empty part. A source file name holding a
/// backslash becomes a separator here, so `a\..\..\x.sql` would climb out.
pub fn inside(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.contains(':')
        && rel
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..")
}

/// Writes `bytes` to `out/rel`, creating the directories between. Refuses a
/// path that leaves `out` and a symbolic link anywhere below `out`: a link
/// committed into the output tree would otherwise redirect the write, e.g.
/// onto the source file it was converted from.
pub(crate) fn write_under(out: &Path, rel: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let dest = dest_under(out, rel)?;
    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    std::fs::write(&dest, bytes).map_err(|e| format!("{}: {e}", dest.display()))?;
    Ok(dest)
}

/// `out/rel`, or why [`write_under`] would refuse it.
pub(crate) fn dest_under(out: &Path, rel: &str) -> Result<PathBuf, String> {
    if !inside(rel) {
        return Err(format!(
            "{}: `{rel}` is not a path inside the output directory",
            out.display()
        ));
    }
    let mut dest = out.to_path_buf();
    for part in rel.split('/') {
        dest.push(part);
        match std::fs::symlink_metadata(&dest) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(format!(
                    "{}: a symbolic link in the output directory; not writing through it",
                    dest.display()
                ))
            }
            _ => {}
        }
    }
    Ok(dest)
}

/// Converts plain SQL files: rules with the source catalog of the whole
/// tree, then a check of every draft against both trees' catalogs.
fn convert_files(
    a: &Args,
    out: &Path,
    inputs: &[Input],
    jobs: usize,
    ledger: Option<&crate::review::Ledger>,
    base: Option<&crate::base::Base>,
    hooks: &Hooks,
) -> Result<Vec<(Conversion, f64)>, String> {
    // Catalogs from the whole tree, so names defined in other files bind:
    // the source catalog also lets the rules see other files' procedures.
    // Base catalogs go under the tree's objects.
    let source = (!a.no_catalog).then(|| {
        let texts = inputs.iter().map(|i| i.text.as_str());
        Arc::new(match base {
            Some(b) => crate::base::over(
                &b.source,
                texts,
                a.from,
                crate::base::origin(&a.src.display().to_string(), &b.files),
                None,
            ),
            None => catalog(texts, a.from, &a.src, None),
        })
    });
    // The temporary table names the whole tree's table variables claim, so
    // no two routines that may be active in one session share one.
    let claims = crate::table_vars::Claims::of(
        inputs.iter().map(|i| (i.rel.as_str(), i.text.as_str())),
        a.from,
    );
    // Pass 1: rules only.
    let mut drafts = parallel(jobs, inputs, |i| {
        crate::rewrite_in_tree(&i.text, a.from, a.to, source.as_deref(), Some(&claims))
    });
    // Hand-written output replaces the rules' before anything is checked,
    // so it is checked like theirs and the target catalog has it.
    if let Some(l) = ledger {
        for (i, d) in inputs.iter().zip(drafts.iter_mut()) {
            hand_written_draft(l, i, d);
        }
    }
    let mut opts = Options {
        helpers: a.helpers.clone(),
        ..Options::default()
    };
    let files: Vec<(&str, &str)> = inputs
        .iter()
        .map(|i| (i.rel.as_str(), i.text.as_str()))
        .collect();
    let mut stand_ins = crate::refused::StandIns::default();
    if let Some(source) = source {
        let texts = drafts.iter().map(|d| d.output());
        let mut target = match base {
            Some(b) => crate::base::over(
                &b.target,
                texts,
                a.to,
                crate::base::origin(&out.display().to_string(), b.target_files()),
                None,
            ),
            None => catalog(texts, a.to, out, None),
        };
        // Tables whose CREATE TABLE was refused for a part of it stand in
        // for binding, without that part; what uses them is flagged below.
        stand_ins = crate::refused::StandIns::build(
            &files,
            drafts.iter().map(|d| d.output()),
            &mut target,
            a.from,
            a.to,
            Some(&source),
        );
        opts.facts_a = vec![source];
        opts.facts_b = vec![Arc::new(target)];
    }
    // Pass 2: check every draft.
    hooks.stop_if_cancelled()?;
    let work = slots(inputs.iter().zip(drafts));
    let progress = Progress::new(work.len(), hooks);
    let done = parallel(jobs, &work, |w| {
        let (input, draft) = w.lock().unwrap().take().unwrap();
        if hooks.cancelled() {
            return None;
        }
        let t = Instant::now();
        let c = crate::check(&input.text, draft, &opts);
        progress.tick();
        Some((c, t.elapsed().as_secs_f64()))
    });
    let mut done: Vec<(Conversion, f64)> = done
        .into_iter()
        .map(|d| d.ok_or_else(|| CANCELLED.to_string()))
        .collect::<Result<_, _>>()?;
    stand_ins.flag(&files, done.iter_mut().map(|(c, _)| c), a.from, a.to);
    Ok(done)
}

/// Reads the review ledger in `out`, if there is one.
fn read_ledger(out: &Path) -> Result<Option<crate::review::Ledger>, String> {
    let path = out.join(crate::review::LEDGER_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(crate::review::Ledger::parse(&text))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn hand_note(r: &crate::review::Record, unchecked: bool) -> crate::Note {
    let who = format!(
        "{} ({})",
        r.author,
        match r.by {
            crate::review::By::Human => "a person",
            crate::review::By::Agent => "an agent",
        }
    );
    if unchecked {
        crate::Note {
            status: Status::Unproven,
            rule: "hand-written.unchecked".into(),
            text: format!("Written by hand by {who}. A template unit written by hand is not checked by convert: the instance check (`sqldiff instances`) judges it."),
        }
    } else {
        crate::Note {
            status: Status::Assumptions,
            rule: "ASM-HAND-WRITTEN".into(),
            text: format!("Written by hand by {who}; checked like the rules' output. The check allows the same dialect mappings it allows the rules (a dropped `dbo` schema, mapped types), which a text written by hand may lean on without saying so; the rules' assumptions and decisions for this unit are kept. Accept this once the text has been read against the source."),
        }
    }
}

fn conflict_note() -> crate::Note {
    crate::Note {
        status: Status::Differs,
        rule: "review.conflicting-texts".into(),
        text: "The review ledger holds different hand-written texts for this unit from different authors; neither is applied. Keep one.".into(),
    }
}

/// Applies the ledger's hand-written texts for one file to its draft.
fn hand_written_draft(l: &crate::review::Ledger, i: &Input, d: &mut crate::Draft) {
    let (texts, conflicts) = l.texts(&i.rel);
    if texts.is_empty() && conflicts.is_empty() {
        return;
    }
    for (k, id) in d.unit_ids(&i.rel, &i.text).into_iter().enumerate() {
        if conflicts.contains(&id) {
            d.note_unit(k, conflict_note());
        } else if let Some(r) = texts.get(&id) {
            let w = crate::Written {
                by: r.by,
                author: r.author.clone(),
                origin: id.clone(),
            };
            let text = r.text.as_deref().unwrap_or("");
            if let Err(e) = d.replace_unit(k, text, w, hand_note(r, false)) {
                d.note_unit(
                    k,
                    crate::Note {
                        status: Status::Differs,
                        rule: "review.text-not-applied".into(),
                        text: format!("The hand-written text for this unit was not applied: {e}."),
                    },
                );
            }
        }
    }
}

/// Applies the ledger's hand-written texts to a checked template, unchecked.
fn hand_written_template(l: &crate::review::Ledger, i: &Input, c: &mut Conversion) {
    let (texts, conflicts) = l.texts(&i.rel);
    if texts.is_empty() && conflicts.is_empty() {
        return;
    }
    let ids = crate::review::unit_ids(&i.rel, &i.text, c);
    for (k, id) in ids.iter().enumerate() {
        let note = if conflicts.contains(id) {
            Some(conflict_note())
        } else if let Some(r) = texts.get(id) {
            let w = crate::Written {
                by: r.by,
                author: r.author.clone(),
                origin: id.clone(),
            };
            let text = r.text.as_deref().unwrap_or("");
            crate::splice_unit(c, k, text, w, hand_note(r, true))
                .err()
                .map(|e| crate::Note {
                    status: Status::Differs,
                    rule: "review.text-not-applied".into(),
                    text: format!("The hand-written text for this unit was not applied: {e}."),
                })
        } else {
            None
        };
        if let Some(n) = note {
            let u = &mut c.units[k];
            u.status = u.status.max(n.status);
            u.notes.insert(0, n);
        }
    }
}

/// The file at the root of a template tree that lists the parameters of the
/// routine each routine body is woven into.
pub const ROUTINE_PARAMETERS_FILE: &str = "template-parameters.toml";

/// Reads [`ROUTINE_PARAMETERS_FILE`] from a template tree: for a template (its
/// path from the tree root, with `/`), the parameters of the routine its body
/// goes into, as T-SQL declarations. A generator wraps a bare body in a
/// routine whose header it writes itself; this is how a conversion learns what
/// that header declares. A tree without the file, or a single file, has none.
///
/// ```toml
/// ["procs/update_item.sql"]
/// parameters = ["@mode int", "@item_name varchar(100) output"]
/// ```
pub fn read_routine_parameters(
    src: &Path,
) -> Result<std::collections::BTreeMap<String, Vec<String>>, String> {
    let path = src.join(ROUTINE_PARAMETERS_FILE);
    if !src.is_dir() || !path.is_file() {
        return Ok(Default::default());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse_routine_parameters(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn parse_routine_parameters(
    text: &str,
) -> Result<std::collections::BTreeMap<String, Vec<String>>, String> {
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    let mut out = std::collections::BTreeMap::new();
    for (key, value) in table {
        let rel = key.replace('\\', "/");
        let toml::Value::Table(entry) = value else {
            return Err(format!("`{key}`: expected a table with `parameters`"));
        };
        if let Some(other) = entry.keys().find(|k| k.as_str() != "parameters") {
            return Err(format!(
                "`{key}`: unknown key `{other}`; only `parameters` is read"
            ));
        }
        let list = match entry.get("parameters") {
            Some(toml::Value::Array(a)) => a,
            _ => return Err(format!("`{key}`: `parameters` must be a list of strings")),
        };
        let mut decls = Vec::new();
        let mut names = Vec::new();
        for item in list {
            let toml::Value::String(d) = item else {
                return Err(format!("`{key}`: `parameters` must be a list of strings"));
            };
            let d = d.trim();
            let Some(name) = declared_name(d) else {
                return Err(format!(
                    "`{key}`: `{d}` is not a parameter declaration (`@name type [OUTPUT]`, one line, no comment)"
                ));
            };
            // T-SQL names are case-insensitive: `@a` and `@A` are one parameter.
            if names.contains(&name) {
                return Err(format!("`{key}`: `{d}` declares a parameter listed before"));
            }
            names.push(name);
            decls.push(d.to_string());
        }
        out.insert(rel, decls);
    }
    Ok(out)
}

/// The name (upper case) a single T-SQL parameter declaration declares, or
/// `None` when the text is not one: it must start with a variable, lex
/// cleanly on one line, and hold no comment, `;`, comma or unbalanced
/// parenthesis outside its own parentheses. The declaration is pasted into a
/// stand-in header, so anything else could end the header early or hide the
/// body that follows it.
fn declared_name(d: &str) -> Option<String> {
    use sqldiff_syntax::SyntaxKind as K;
    if d.contains('\n') || d.contains('\r') {
        return None;
    }
    let lexed = sqldiff_syntax::lexer::tokenize(d, Dialect::Tsql);
    if !lexed.diagnostics.is_empty() {
        return None;
    }
    let mut toks = lexed.tokens.iter().filter(|t| t.kind != K::WHITESPACE);
    let first = toks.next().filter(|t| t.kind == K::VARIABLE)?;
    let mut depth = 0usize;
    for t in toks {
        match t.kind {
            K::LINE_COMMENT | K::BLOCK_COMMENT | K::SEMICOLON | K::ERROR_TOKEN => return None,
            K::L_PAREN => depth += 1,
            K::R_PAREN => depth = depth.checked_sub(1)?,
            K::COMMA if depth == 0 => return None,
            _ => {}
        }
    }
    (depth == 0).then(|| d[first.start..first.end].to_uppercase())
}

/// Converts templates: every file is split by the placeholder rules, and
/// all fragments share one set of stand-in names, so a table a template
/// creates as `<%SCHEMA%>.<%TABLE%>` binds in the templates that use it.
#[allow(clippy::too_many_arguments)]
fn convert_templates(
    a: &Args,
    out: &Path,
    tm: &TemplateMode,
    inputs: &[Input],
    jobs: usize,
    ledger: Option<&crate::review::Ledger>,
    base: Option<&crate::base::Base>,
    hooks: &Hooks,
) -> Result<Vec<(Conversion, f64)>, String> {
    let mut parsed: Vec<(Template, sqldiff_core::composition::PlaceholderBasis)> = Vec::new();
    for i in inputs {
        let (t, basis) = tm
            .placeholders
            .parse(&i.text, a.from, tm.parameters.as_ref());
        if t.print() != i.text {
            return Err(format!(
                "{}: round trip failed: template segments differ from the file",
                i.rel
            ));
        }
        parsed.push((t, basis));
    }
    let routine_parameters = read_routine_parameters(&a.src)?;
    let mut stand = StandIns::new(parsed.iter().map(|(t, _)| t)).with_kinds(tm.kinds.clone());
    if !a.no_catalog {
        stand = stand.bound_across();
    }
    let stem = Some(stand.stem());
    let source = (!a.no_catalog).then(|| {
        let frags: Vec<String> = parsed
            .iter()
            .map(|(t, _)| crate::template_fragment(t, &stand))
            .collect();
        let texts = frags.iter().map(String::as_str);
        // Base catalogs are plain DDL: no placeholders are read in them.
        Arc::new(match base {
            Some(b) => crate::base::over(
                &b.source,
                texts,
                a.from,
                crate::base::origin(&a.src.display().to_string(), &b.files),
                stem,
            ),
            None => catalog(texts, a.from, &a.src, stem),
        })
    });

    // What to convert: each file alone, as its fragment kind, except a
    // routine's opening and closing templates, which go together with a
    // marker line between them.
    let index = |rel: &str| inputs.iter().position(|i| i.rel == rel);
    let mut marker = String::from("sqldiffpair");
    while inputs
        .iter()
        .any(|i| i.text.to_ascii_lowercase().contains(&marker))
    {
        marker.push('x');
    }
    let marker = format!("/*{marker}*/");
    let mut paired = vec![false; inputs.len()];
    let mut jobs_list: Vec<Job> = Vec::new();
    for (head, foot) in &tm.pairs {
        let (Some(h), Some(f)) = (index(head), index(foot)) else {
            return Err(format!(
                "template pair {head} + {foot}: both must be files in {}",
                a.src.display()
            ));
        };
        paired[h] = true;
        paired[f] = true;
        let text = format!("{}\n{marker}\n{}", inputs[h].text, inputs[f].text);
        let (t, basis) = tm.placeholders.parse(&text, a.from, tm.parameters.as_ref());
        jobs_list.push(Job {
            template: t,
            basis,
            kind: FragmentKind::Statements,
            parameters: Vec::new(),
            target: Target::Pair(h, f),
        });
    }
    for (k, (t, basis)) in parsed.into_iter().enumerate() {
        if paired[k] {
            continue;
        }
        // A body with listed parameters is a routine body, unless the pack
        // says it is another kind of fragment.
        let parameters = routine_parameters
            .get(&inputs[k].rel)
            .cloned()
            .unwrap_or_default();
        let kind = tm
            .fragments
            .iter()
            .find(|(g, _)| g.is_match(&inputs[k].rel))
            .map_or(
                if parameters.is_empty() {
                    FragmentKind::Statements
                } else {
                    FragmentKind::RoutineBody
                },
                |(_, kind)| *kind,
            );
        jobs_list.push(Job {
            template: t,
            basis,
            kind,
            parameters,
            target: Target::File(k),
        });
    }

    let drafts = parallel(jobs, &jobs_list, |j| {
        crate::rewrite_template_with(
            &j.template,
            j.kind,
            &j.parameters,
            a.from,
            a.to,
            &stand,
            source.as_deref(),
        )
    });
    let mut opts = Options {
        helpers: a.helpers.clone(),
        ..Options::default()
    };
    if let Some(source) = source {
        opts.facts_a = vec![source];
        let texts = drafts.iter().map(|d| d.output());
        opts.facts_b = vec![Arc::new(match base {
            Some(b) => crate::base::over(
                &b.target,
                texts,
                a.to,
                crate::base::origin(&out.display().to_string(), b.target_files()),
                stem,
            ),
            None => catalog(texts, a.to, out, stem),
        })];
    }
    hooks.stop_if_cancelled()?;
    let work = slots(jobs_list.iter().zip(drafts));
    let progress = Progress::new(work.len(), hooks);
    let delimiters = tm.placeholders.delimiters();
    let done = parallel(jobs, &work, |w| {
        let (j, draft) = w.lock().unwrap().take().unwrap();
        if hooks.cancelled() {
            return None;
        }
        let t0 = Instant::now();
        let c = crate::check_template(&j.template, draft, &stand, &opts, j.basis, delimiters);
        progress.tick();
        Some((c, t0.elapsed().as_secs_f64()))
    });
    let done: Vec<(Conversion, f64)> = done
        .into_iter()
        .map(|d| d.ok_or_else(|| CANCELLED.to_string()))
        .collect::<Result<_, _>>()?;
    let mut out: Vec<Option<(Conversion, f64)>> = (0..inputs.len()).map(|_| None).collect();
    for (j, (c, secs)) in jobs_list.iter().zip(done) {
        match j.target {
            Target::File(k) => out[k] = Some((c, secs)),
            Target::Pair(h, f) => {
                let (ch, cf) = crate::split_pair(c, &inputs[h].text, &inputs[f].text, &marker);
                out[h] = Some((ch, secs / 2.0));
                out[f] = Some((cf, secs / 2.0));
            }
        }
    }
    let mut out: Vec<(Conversion, f64)> = out
        .into_iter()
        .map(|c| c.expect("every file is converted"))
        .collect();
    if let Some(l) = ledger {
        for (i, (c, _)) in inputs.iter().zip(out.iter_mut()) {
            hand_written_template(l, i, c);
        }
    }
    Ok(out)
}

/// One conversion: a file, or a routine's opening and closing templates.
struct Job {
    template: Template,
    basis: sqldiff_core::composition::PlaceholderBasis,
    kind: FragmentKind,
    /// Parameters of the routine a routine body is woven into.
    parameters: Vec<String>,
    target: Target,
}

enum Target {
    File(usize),
    Pair(usize, usize),
}

/// Items handed out once each to the worker threads.
fn slots<T>(items: impl Iterator<Item = T>) -> Vec<std::sync::Mutex<Option<T>>> {
    items.map(|w| std::sync::Mutex::new(Some(w))).collect()
}

struct Progress<'h> {
    done: AtomicUsize,
    total: usize,
    hooks: &'h Hooks,
}

impl<'h> Progress<'h> {
    fn new(total: usize, hooks: &'h Hooks) -> Progress<'h> {
        Progress {
            done: AtomicUsize::new(0),
            total,
            hooks,
        }
    }

    fn tick(&self) {
        let n = self.done.fetch_add(1, Ordering::Relaxed) + 1;
        match &self.hooks.progress {
            Some(f) => f(n, self.total),
            None if n % 50 == 0 || n == self.total => eprintln!("checked {n}/{}", self.total),
            None => {}
        }
    }
}

/// Every file with one of `extensions` under `src` (or `src` itself),
/// sorted by path.
pub fn collect(src: &Path, extensions: &[String]) -> Result<(Vec<Input>, Vec<String>), String> {
    let mut paths = Vec::new();
    if src.is_file() {
        paths.push(src.to_path_buf());
    } else {
        walk(src, extensions, &mut paths).map_err(|e| format!("{}: {e}", src.display()))?;
    }
    paths.sort();
    let base = if src.is_file() {
        src.parent().unwrap_or(Path::new(""))
    } else {
        src
    };
    let mut inputs = Vec::new();
    let mut unreadable = Vec::new();
    for p in paths {
        let rel = p
            .strip_prefix(base)
            .unwrap_or(&p)
            .to_string_lossy()
            .replace('\\', "/");
        // A name that climbs out of the tree once `\` reads as a separator
        // is not converted (and never written).
        if !inside(&rel) {
            unreadable.push(rel);
            continue;
        }
        let bytes = std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        match decode(&bytes) {
            Some(text) => inputs.push(Input { rel, text }),
            None => unreadable.push(rel),
        }
    }
    Ok((inputs, unreadable))
}

/// Paths, relative to `dir` and with `/`, of the files `collect` would read
/// there, in order. An absent directory has none.
pub fn collect_paths(dir: &Path, extensions: &[String]) -> Result<Vec<String>, String> {
    let mut paths = Vec::new();
    if dir.is_dir() {
        walk(dir, extensions, &mut paths).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut rels: Vec<String> = paths
        .iter()
        .map(|p| {
            p.strip_prefix(dir)
                .unwrap_or(p)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    rels.sort();
    Ok(rels)
}

fn walk(dir: &Path, extensions: &[String], out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let name = e.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let ty = e.file_type()?;
        if ty.is_dir() {
            walk(&e.path(), extensions, out)?;
        } else if ty.is_file()
            && Path::new(&name).extension().is_some_and(|x| {
                extensions
                    .iter()
                    .any(|e| x.eq_ignore_ascii_case(e.as_str()))
            })
        {
            out.push(e.path());
        }
    }
    Ok(())
}

/// UTF-8 (with or without BOM) or UTF-16 with a BOM, as SSDT projects save
/// some files. Anything else is not guessed.
pub fn decode(bytes: &[u8]) -> Option<String> {
    let utf16 = |b: &[u8], le: bool| {
        if b.len() % 2 != 0 {
            return None;
        }
        let units = b.chunks(2).map(|c| {
            if le {
                u16::from_le_bytes([c[0], c[1]])
            } else {
                u16::from_be_bytes([c[0], c[1]])
            }
        });
        char::decode_utf16(units)
            .collect::<Result<String, _>>()
            .ok()
    };
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8(rest.to_vec()).ok(),
        [0xFF, 0xFE, rest @ ..] => utf16(rest, true),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, false),
        _ => String::from_utf8(bytes.to_vec()).ok(),
    }
}

/// The output directory must not hold anything we did not ask to replace,
/// and must not be inside the source tree (it would be read next time).
/// Creates it when missing.
pub fn prepare_out(src: &Path, out: &Path, overwrite: bool) -> Result<(), String> {
    let src_c = std::fs::canonicalize(src).map_err(|e| format!("{}: {e}", src.display()))?;
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let out_c = std::fs::canonicalize(out).map_err(|e| format!("{}: {e}", out.display()))?;
    if src_c.is_dir() && out_c.starts_with(&src_c) {
        return Err(format!(
            "{}: the output directory is inside the source tree",
            out.display()
        ));
    }
    if src_c.starts_with(&out_c) {
        return Err(format!(
            "{}: the output directory contains the source",
            out.display()
        ));
    }
    let empty = std::fs::read_dir(&out_c)
        .map_err(|e| format!("{}: {e}", out.display()))?
        .next()
        .is_none();
    if !empty && !overwrite {
        return Err(format!(
            "{}: not empty; pass --overwrite to replace the files a conversion writes",
            out.display()
        ));
    }
    Ok(())
}

/// Runs `f` over `items` on `jobs` threads; results in input order.
pub fn parallel<T: Sync, R: Send>(jobs: usize, items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<Option<R>>> =
        items.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..jobs.min(items.len()) {
            std::thread::Builder::new()
                .stack_size(sqldiff_core::ENGINE_STACK)
                .spawn_scoped(s, || loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    let r = f(item);
                    *slots[i].lock().unwrap() = Some(r);
                })
                .expect("start a worker thread");
        }
    });
    slots
        .into_iter()
        .map(|m| m.into_inner().unwrap().unwrap())
        .collect()
}

/// The objects every text defines. With a stand-in `stem` (templates), an
/// object whose name holds a stand-in and that more than one file defines
/// is left out: those files are different instances' tables or routines,
/// and binding against one of them would be a guess.
pub fn catalog<'a>(
    texts: impl Iterator<Item = &'a str>,
    dialect: Dialect,
    origin: &Path,
    stem: Option<&str>,
) -> Catalog {
    let mut cat = Catalog {
        origin: origin.display().to_string(),
        ..Catalog::default()
    };
    let Some(stem) = stem else {
        for t in texts {
            let root = sqldiff_syntax::parse(t, dialect).syntax();
            ddl::load_tree(&mut cat, &root, dialect);
        }
        return cat;
    };
    let mut per_file: Vec<Catalog> = Vec::new();
    for t in texts {
        let mut c = Catalog::default();
        let root = sqldiff_syntax::parse(t, dialect).syntax();
        ddl::load_tree(&mut c, &root, dialect);
        per_file.push(c);
    }
    let templated = |schema: &Option<String>, name: &str| {
        let full = format!("{}.{name}", schema.as_deref().unwrap_or("")).to_ascii_lowercase();
        full.contains(stem).then_some(full)
    };
    let mut files: std::collections::HashMap<String, usize> = Default::default();
    for c in &per_file {
        let mut names: std::collections::BTreeSet<String> = Default::default();
        names.extend(
            c.tables
                .iter()
                .filter_map(|t| templated(&t.schema, &t.name)),
        );
        names.extend(
            c.routines
                .iter()
                .filter_map(|r| templated(&r.schema, &r.name).map(|n| format!("routine {n}"))),
        );
        for n in names {
            *files.entry(n).or_default() += 1;
        }
    }
    let shared = |key: Option<String>| key.is_some_and(|k| files.get(&k).copied().unwrap_or(0) > 1);
    for c in per_file {
        for t in c.tables {
            if !shared(templated(&t.schema, &t.name)) {
                cat.put_table(dialect, t);
            }
        }
        for r in c.routines {
            if !shared(templated(&r.schema, &r.name).map(|n| format!("routine {n}"))) {
                cat.put_routine(dialect, r);
            }
        }
        for sc in c.schemas {
            if !cat.schemas.contains(&sc) {
                cat.schemas.push(sc);
            }
        }
    }
    cat
}

/// The output with a status comment above every unit that is not plainly
/// converted, and each unit's 1-based first line in the annotated text.
fn annotate(c: &Conversion) -> (String, Vec<usize>) {
    let starts: Vec<usize> = c
        .units
        .iter()
        .map(|u| line_start(&c.output, u.out.start))
        .collect();
    let mut marks: Vec<(usize, String)> = Vec::new();
    for (u, &at) in c.units.iter().zip(&starts) {
        // A refused unit already opens with its own comment; a not
        // converted unit whose code was still written out (a construct with
        // no rule inside it) gets a status line like any other.
        let refused = c.output[at..]
            .trim_start()
            .starts_with("-- [not converted]");
        if u.status == Status::Converted || u.status == Status::NotConverted && refused {
            continue;
        }
        // The directive assumption is on every unit of the template; it is
        // said once, at the top of the file.
        let worst = u.notes.iter().filter(|n| n.status == u.status);
        if u.status == Status::Assumptions && worst.clone().all(|n| n.rule == DIRECTIVE) {
            continue;
        }
        let indent: String = c.output[at..]
            .chars()
            .take_while(|ch| *ch == ' ' || *ch == '\t')
            .collect();
        let why = u
            .notes
            .iter()
            .filter(|n| n.status == u.status && n.rule != DIRECTIVE)
            .map(|n| n.rule.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let why = if why.is_empty() {
            "engine finding".to_string()
        } else {
            why
        };
        marks.push((
            at,
            format!("{indent}-- [sqldiff: {}] {why}\n", u.status.label()),
        ));
    }
    marks.sort_by_key(|m| m.0);
    marks.dedup_by_key(|m| m.0);
    let mut text = String::with_capacity(c.output.len() + marks.len() * 60);
    let directives = c
        .template
        .as_ref()
        .map(|t| {
            let mut d: Vec<&str> = Vec::new();
            for f in &t.special_forms {
                if !d.contains(&f.as_str()) {
                    d.push(f);
                }
            }
            d
        })
        .unwrap_or_default();
    let header = !directives.is_empty()
        && c.units
            .iter()
            .any(|u| u.notes.iter().any(|n| n.rule == DIRECTIVE));
    if header {
        let _ = writeln!(
            text,
            "-- [sqldiff: under assumptions] {DIRECTIVE}: converted without the text of its directives ({}); check the woven instances",
            directives.join(", ")
        );
    }
    let mut last = 0;
    for (at, m) in &marks {
        text.push_str(&c.output[last..*at]);
        text.push_str(m);
        last = *at;
    }
    text.push_str(&c.output[last..]);
    // Lines: the unit's own line plus the marks inserted at or before it.
    let lines = c
        .units
        .iter()
        .zip(&starts)
        .map(|(u, &at)| {
            u.out.line + usize::from(header) + marks.iter().filter(|m| m.0 <= at).count()
        })
        .collect();
    (text, lines)
}

fn line_start(s: &str, at: usize) -> usize {
    s[..at.min(s.len())].rfind('\n').map_or(0, |i| i + 1)
}

fn file_report(rel: &str, input: &str, c: &Conversion, lines: &[usize], secs: f64) -> FileReport {
    let mut status = c
        .units
        .iter()
        .map(|u| u.status)
        .max()
        .unwrap_or(Status::Converted);
    if !c.unplaced.is_empty() {
        status = status.max(Status::Differs);
    }
    if !c.unproven.is_empty() {
        status = status.max(Status::Unproven);
    }
    let ids = crate::review::unit_ids(rel, input, c);
    let (mut units, mut converted_units) = (Vec::new(), Vec::new());
    for ((u, &l), id) in c.units.iter().zip(lines).zip(ids) {
        let worst: Vec<&crate::Note> = u.notes.iter().filter(|n| n.status == u.status).collect();
        let mut reason = worst.first().map(|n| n.text.clone()).unwrap_or_default();
        if reason.is_empty() && u.status == Status::Differs {
            // The engine's diff found it: say what.
            reason = u
                .findings
                .iter()
                .filter_map(|&i| c.report.findings.get(i))
                .find(|f| f.verdict == sqldiff_core::Verdict::Different)
                .map(|f| format!("{}: {}", f.rule, f.witness))
                .unwrap_or_default();
        }
        let findings = u
            .findings
            .iter()
            .filter_map(|&i| c.report.findings.get(i))
            .chain(
                u.output_findings
                    .iter()
                    .filter_map(|&i| c.output_check.findings.get(i)),
            )
            .map(|f| f.rule.clone())
            .collect();
        let r = UnitReport {
            id,
            status: u.status,
            src_line: u.src.line,
            out_line: l,
            rules: if u.status == Status::Converted {
                Vec::new()
            } else if worst.is_empty() {
                vec!["engine".to_string()]
            } else {
                worst.iter().map(|n| n.rule.clone()).collect()
            },
            reason,
            findings,
            trace: u.trace.clone(),
            review: None,
            accepted: Vec::new(),
        };
        if u.status == Status::Converted {
            converted_units.push(r);
        } else {
            units.push(r);
        }
    }
    FileReport {
        path: rel.to_string(),
        status,
        counts: c.counts.clone(),
        seconds: (secs * 1000.0).round() / 1000.0,
        units,
        converted_units,
        output_hash: String::new(),
        unplaced: c
            .unplaced
            .iter()
            .filter_map(|&i| c.output_check.findings.get(i))
            .map(|f| format!("{}: {}", f.rule, f.witness))
            .collect(),
        unproven: c.unproven.clone(),
        template: c.template.clone(),
        review: None,
    }
}

/// A `written` record that replaces the output of unit `unit` (its id in
/// this conversion, as the report and the queue give it) in file `path`
/// with `text`. Keyed to the unit as the rules wrote it, so it applies on
/// every later conversion while that unit exists. `by` is who calls: the
/// caller decides it, never its own caller.
#[allow(clippy::too_many_arguments)]
pub fn written_record(
    t: &Tree,
    inputs: &[Input],
    path: &str,
    unit: &str,
    text: &str,
    by: crate::review::By,
    author: &str,
    reason: &str,
    at: &str,
) -> Result<crate::review::Record, String> {
    let k = t
        .report
        .files
        .iter()
        .position(|f| f.path == path)
        .ok_or_else(|| format!("no file `{path}` in the converted tree"))?;
    let (c, input) = (&t.conversions[k], &inputs[k].text);
    let ids = crate::review::unit_ids(path, input, c);
    let i = ids.iter().position(|id| id == unit).ok_or_else(|| {
        format!("no unit `{unit}` in `{path}` (convert again: ids change with the text)")
    })?;
    let origin = c.units[i]
        .written
        .as_ref()
        .map_or_else(|| unit.to_string(), |w| w.origin.clone());
    if text.trim().is_empty() {
        return Err("the text is empty".into());
    }
    let r = crate::review::Record {
        unit: origin,
        path: path.to_string(),
        rule: String::new(),
        state: crate::review::State::Written,
        by,
        author: author.to_string(),
        reason: reason.to_string(),
        at: at.to_string(),
        input_hash: crate::review::file_hash(input),
        output_hash: crate::review::file_hash(&c.output),
        text: Some(text.to_string()),
        base_catalogs: t.report.base_hash.clone(),
    };
    crate::review::allowed(&r)?;
    Ok(r)
}

/// Files in `out` edited by hand since `sqldiff convert` wrote them, by the
/// `output_hash` the last report recorded: an `--overwrite` run would lose
/// those edits. Empty when there is no earlier report.
pub fn hand_edits(out: &Path) -> Result<Vec<String>, String> {
    let report = out.join("conversion-report.json");
    let text = match std::fs::read_to_string(&report) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", report.display())),
    };
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", report.display()))?;
    let mut edited = Vec::new();
    for f in v["files"].as_array().into_iter().flatten() {
        let Some(path) = f["path"].as_str() else {
            continue;
        };
        let now = std::fs::read(out.join(path)).ok();
        let now = now.map(|b| crate::review::file_hash(&String::from_utf8_lossy(&b)));
        match (f["output_hash"].as_str(), now) {
            // A report from before output hashes: nothing to compare with.
            (None, _) => {}
            // Deleted since: nothing to lose.
            (_, None) => {}
            (Some(h), Some(n)) if h == n => {}
            _ => edited.push(path.to_string()),
        }
    }
    Ok(edited)
}

/// Refuses to overwrite files edited by hand since the last run, unless
/// `discard` says to.
pub fn guard_hand_edits(out: &Path, discard: bool) -> Result<(), String> {
    let edited = hand_edits(out)?;
    if edited.is_empty() || discard {
        return Ok(());
    }
    Err(format!(
        "{} file(s) in {} were edited by hand since the last conversion and would be overwritten: {}. Record each edit as a `written` record with its text in {} (the GUI's or the MCP server's edit does that), so every later conversion applies and checks it, or pass --discard-edits to drop them",
        edited.len(),
        out.display(),
        edited.join(", "),
        crate::review::LEDGER_FILE
    ))
}

/// The rule ids of a unit's assumption notes, in note order without repeats.
fn unit_assumptions(u: &crate::Unit) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for n in u.notes.iter().filter(|n| n.status == Status::Assumptions) {
        debug_assert!(
            !n.rule.starts_with("ASM-") || sqldiff_core::assumptions::lookup(&n.rule).is_some(),
            "assumption id `{}` is not registered in crates/sqldiff-core/profiles/assumptions.toml",
            n.rule
        );
        if !out.contains(&n.rule.as_str()) {
            out.push(&n.rule);
        }
    }
    out
}

/// The engine's open assumptions on a file: its diff's and its output
/// check's, without facts proven from composed output.
fn engine_assumptions(c: &Conversion) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for a in c
        .report
        .assumptions
        .iter()
        .chain(&c.output_check.assumptions)
    {
        if a.source != sqldiff_core::model::AssumptionSource::ComposedOutput
            && !out.contains(&a.id.as_str())
        {
            out.push(&a.id);
        }
    }
    out
}

/// Fills each unit report's `accepted`: its assumptions the profile accepts.
fn mark_accepted(
    f: &mut FileReport,
    input: &Input,
    c: &Conversion,
    profile: &sqldiff_source::deployment::Profile,
) {
    if profile.accept.is_empty() {
        return;
    }
    let ids = crate::review::unit_ids(&input.rel, &input.text, c);
    for (u, id) in c.units.iter().zip(ids) {
        let accepted: Vec<String> = unit_assumptions(u)
            .into_iter()
            .filter(|r| !profile.accepting(r).is_empty())
            .map(str::to_string)
            .collect();
        if let Some(r) = f
            .units
            .iter_mut()
            .chain(f.converted_units.iter_mut())
            .find(|r| r.id == id)
        {
            r.accepted = accepted;
        }
    }
}

/// The deployment section of a tree's report; `None` when no layer has a
/// `[deployment]` table.
fn deployment_report(
    profile: &sqldiff_source::deployment::Profile,
    conversions: &[Conversion],
) -> Option<DeploymentReport> {
    if !profile.declared {
        return None;
    }
    let mut carried: Vec<CarriedAssumption> = Vec::new();
    let mut count = |id: &str, unit: bool| {
        let i = match carried.iter().position(|x| x.id == id) {
            Some(i) => i,
            None => {
                carried.push(CarriedAssumption {
                    id: id.to_string(),
                    units: 0,
                    files: 0,
                    accepted: !profile.accepting(id).is_empty(),
                });
                carried.len() - 1
            }
        };
        if unit {
            carried[i].units += 1;
        } else {
            carried[i].files += 1;
        }
    };
    for c in conversions {
        for u in &c.units {
            for id in unit_assumptions(u) {
                count(id, true);
            }
        }
        for id in engine_assumptions(c) {
            count(id, false);
        }
    }
    // Not accepted first.
    carried.sort_by(|a, b| a.accepted.cmp(&b.accepted).then(a.id.cmp(&b.id)));
    Some(DeploymentReport {
        accept: profile.accept.clone(),
        facts: profile
            .facts
            .iter()
            .map(|f| DeploymentFact {
                key: f.key.clone(),
                value: f.value.clone(),
                by: f.by.clone(),
                bears_on: sqldiff_core::assumptions::bearing_on(&f.key)
                    .iter()
                    .map(|e| e.id.clone())
                    .collect(),
            })
            .collect(),
        assumptions_within_accept: carried.iter().all(|x| x.accepted),
        carried,
    })
}

/// Applies the ledger committed in the output directory, if there is one
/// (`review::LEDGER_FILE`). Returns the lines it did not apply.
pub fn review_in(t: &mut Tree, inputs: &[Input], out: &Path) -> Result<Vec<String>, String> {
    let path = out.join(crate::review::LEDGER_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let l = crate::review::Ledger::parse(&text);
            review(t, inputs, &l, &path.display().to_string());
            Ok(l.rejected_lines)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Applies a review ledger to a converted tree: every unit and file gets
/// what the ledger says about it, and the report says which ledger it was.
/// `inputs` are the texts the tree was converted from, in report order.
/// Statuses are never changed.
pub fn review(t: &mut Tree, inputs: &[Input], ledger: &crate::review::Ledger, path: &str) {
    let (mut units_reviewed, mut files_reviewed) = (0, 0);
    let base = t.report.base_hash.clone();
    for ((f, c), input) in t.report.files.iter_mut().zip(&t.conversions).zip(inputs) {
        let fr = crate::review::review_file_in(&f.path, &input.text, c, ledger, base.as_deref());
        let mut by_id: std::collections::HashMap<String, crate::review::UnitReview> =
            fr.units.into_iter().map(|u| (u.id.clone(), u)).collect();
        let mut outdated = 0;
        for u in f.units.iter_mut().chain(f.converted_units.iter_mut()) {
            let Some(r) = by_id.remove(&u.id) else {
                continue;
            };
            if r.decisions.iter().any(|d| !d.current) {
                outdated += 1;
            }
            units_reviewed += usize::from(r.reviewed);
            u.review = Some(UnitReviewReport {
                reviewed: r.reviewed,
                open: r.open,
                undischargeable: r.undischargeable,
                written_by: r.written_by,
                decisions: r.decisions,
            });
        }
        let all = f
            .units
            .iter()
            .chain(&f.converted_units)
            .all(|u| u.review.as_ref().is_some_and(|r| r.reviewed));
        // A file without units has nothing a person accepted: never reviewed.
        let reviewed = all
            && fr.unplaced.is_empty()
            && f.unproven.is_empty()
            && !(f.units.is_empty() && f.converted_units.is_empty());
        files_reviewed += usize::from(reviewed);
        f.review = Some(FileReviewReport {
            reviewed,
            units_reviewed: f
                .units
                .iter()
                .chain(&f.converted_units)
                .filter(|u| u.review.as_ref().is_some_and(|r| r.reviewed))
                .count(),
            units_outdated: outdated,
            orphaned: {
                let mut o: Vec<String> = fr.orphaned.into_iter().map(|r| r.unit).collect();
                o.sort();
                o.dedup();
                o
            },
        });
    }
    t.report.ledger = Some(LedgerReport {
        path: path.to_string(),
        records: ledger.records().count(),
        rejected_lines: ledger.rejected_lines.clone(),
        units_reviewed,
        files_reviewed,
    });
}

const ORDER: [Status; 6] = [
    Status::Converted,
    Status::Assumptions,
    Status::Decision,
    Status::Unproven,
    Status::Differs,
    Status::NotConverted,
];

fn totals(files: &[FileReport], seconds: f64) -> Totals {
    let mut t = Totals {
        files: files.len(),
        seconds: (seconds * 100.0).round() / 100.0,
        ..Totals::default()
    };
    let mut reasons: std::collections::HashMap<String, usize> = Default::default();
    for s in ORDER {
        t.files_by_status.insert(s.label(), 0);
        t.units.insert(s.label(), 0);
    }
    for f in files {
        *t.files_by_status.entry(f.status.label()).or_default() += 1;
        for (s, n) in [
            (Status::Converted, f.counts.converted),
            (Status::Assumptions, f.counts.assumptions),
            (Status::Decision, f.counts.decision),
            (Status::Unproven, f.counts.unproven),
            (Status::Differs, f.counts.differs),
            (Status::NotConverted, f.counts.not_converted),
        ] {
            *t.units.entry(s.label()).or_default() += n;
        }
        for u in &f.units {
            if u.status >= Status::Unproven {
                let key = format!(
                    "{} | {}",
                    u.status.label(),
                    u.rules.first().map(String::as_str).unwrap_or("engine")
                );
                *reasons.entry(key).or_default() += 1;
            }
        }
    }
    let mut r: Vec<(String, usize)> = reasons.into_iter().collect();
    r.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    r.truncate(25);
    t.top_reasons = r;
    t
}

pub fn summary(r: &Report) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{} files, {} → {}, {:.1} s",
        r.totals.files, r.from, r.to, r.totals.seconds
    );
    let _ = writeln!(s, "{:<20} {:>6} {:>8}", "status", "files", "units");
    for st in ORDER {
        let _ = writeln!(
            s,
            "{:<20} {:>6} {:>8}",
            st.label(),
            r.totals
                .files_by_status
                .get(st.label())
                .copied()
                .unwrap_or(0),
            r.totals.units.get(st.label()).copied().unwrap_or(0)
        );
    }
    if !r.unreadable.is_empty() {
        let _ = writeln!(s, "{:<20} {:>6}", "unreadable", r.unreadable.len());
    }
    if !r.base_catalogs_b.is_empty() {
        let _ = writeln!(
            s,
            "base catalogs: {} file(s), and {} file(s) for the target side (--catalog-b), used as given",
            r.base_catalogs.len(),
            r.base_catalogs_b.len()
        );
    } else if !r.base_catalogs.is_empty() {
        let suspect = r.base_objects.iter().filter(|o| o.suspect).count();
        let _ = writeln!(
            s,
            "base catalogs: {} file(s); {suspect} suspect base object(s), flagged where used ({})",
            r.base_catalogs.len(),
            crate::base::BASE_SUSPECT
        );
    }
    if let Some(l) = &r.ledger {
        let units: usize = r
            .files
            .iter()
            .map(|f| f.units.len() + f.converted_units.len())
            .sum();
        let _ = writeln!(
            s,
            "review: {} of {} files and {} of {} units reviewed ({} records{})",
            l.files_reviewed,
            r.files.len(),
            l.units_reviewed,
            units,
            l.records,
            if l.rejected_lines.is_empty() {
                String::new()
            } else {
                format!(", {} lines not applied", l.rejected_lines.len())
            }
        );
    }
    if let Some(d) = &r.deployment {
        let open = d.carried.iter().filter(|x| !x.accepted).count();
        let _ = writeln!(
            s,
            "deployment profile: {}",
            if d.assumptions_within_accept {
                format!(
                    "every assumption accepted ({} ids; units stay under assumptions)",
                    d.carried.len()
                )
            } else {
                format!("{open} of {} assumption ids not accepted", d.carried.len())
            }
        );
    }
    s
}

pub fn markdown(r: &Report) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# Conversion report\n");
    let _ = writeln!(
        s,
        "`{}` → `{}`, {} to {}. Cross-file catalogs: {}.\n",
        r.src,
        r.out.as_deref().unwrap_or("(not written)"),
        r.from,
        r.to,
        if r.catalogs { "yes" } else { "no" }
    );
    let _ = writeln!(
        s,
        "Base catalogs: {}.\n",
        match r.base_catalogs.len() {
            0 => "none".to_string(),
            n => match r.base_catalogs_b.len() {
                0 => format!("{n} file(s), listed below; bound against, not converted into this tree"),
                m => format!("{n} file(s), and {m} for the target side, listed below; bound against, not converted into this tree"),
            },
        }
    );
    let _ = writeln!(
        s,
        "A file counts as converted only when every unit in it is: no assumption, no decision, nothing found by the engine's diff or by checking the output on its own (binding, leftover source syntax, target limits).\n"
    );
    let _ = writeln!(s, "```\n{}```\n", summary(r));
    if let Some(d) = &r.deployment {
        deployment_markdown(&mut s, d);
    }
    base_markdown(&mut s, r);
    if !r.totals.top_reasons.is_empty() {
        let _ = writeln!(
            s,
            "## Most frequent blockers\n\n| units | status, rule |\n|---:|---|"
        );
        for (k, n) in &r.totals.top_reasons {
            let _ = writeln!(s, "| {n} | {k} |");
        }
        s.push('\n');
    }
    if !r.unreadable.is_empty() {
        let _ = writeln!(s, "## Unreadable files (not converted)\n");
        for p in &r.unreadable {
            let _ = writeln!(s, "- `{p}`");
        }
        s.push('\n');
    }
    let _ = writeln!(s, "## Files\n");
    let mut files: Vec<&FileReport> = r.files.iter().collect();
    files.sort_by(|a, b| b.status.cmp(&a.status).then(a.path.cmp(&b.path)));
    for f in files {
        let reviewed = match &f.review {
            Some(r) if r.reviewed => ", accepted in review",
            _ => "",
        };
        let _ = writeln!(s, "### `{}`: {}{reviewed}\n", f.path, f.status.label());
        for u in f.units.iter().filter(|u| u.status >= Status::Decision) {
            let _ = writeln!(
                s,
                "- line {} → {}: **{}** ({}) {}",
                u.src_line,
                u.out_line,
                u.status.label(),
                u.rules.join(", "),
                u.reason.lines().next().unwrap_or("")
            );
        }
        for p in &f.unplaced {
            let _ = writeln!(s, "- outside any unit: **differs** {p}");
        }
        for p in &f.unproven {
            let _ = writeln!(s, "- outside any unit: **not proven** {p}");
        }
        let assumed = f
            .units
            .iter()
            .filter(|u| u.status == Status::Assumptions)
            .count();
        if assumed > 0 {
            let _ = writeln!(
                s,
                "- {assumed} unit(s) under assumptions (see the JSON report)"
            );
        }
        s.push('\n');
    }
    s
}

fn deployment_markdown(s: &mut String, d: &DeploymentReport) {
    let _ = writeln!(s, "## Deployment profile\n");
    let _ = writeln!(
        s,
        "{}\n",
        if d.assumptions_within_accept {
            "Every assumption the tree carries is accepted by the deployment profile. Acceptance is listed for review; it changes no status, so these units stay under assumptions."
        } else {
            "Not every assumption the tree carries is accepted by the deployment profile (marked below)."
        }
    );
    if !d.carried.is_empty() {
        let _ = writeln!(
            s,
            "| assumption | units | files (engine) | accepted by |\n|---|---:|---:|---|"
        );
        for x in &d.carried {
            let mut by: Vec<String> = Vec::new();
            for a in &d.accept {
                if sqldiff_core::assumptions::covers(&a.id, &x.id) {
                    for b in &a.by {
                        if !by.contains(&b.label()) {
                            by.push(b.label());
                        }
                    }
                }
            }
            let by = if by.is_empty() {
                "**not accepted**".to_string()
            } else {
                by.join("; ")
            };
            let _ = writeln!(s, "| `{}` | {} | {} | {by} |", x.id, x.units, x.files);
        }
        s.push('\n');
    }
    for f in &d.facts {
        let _ = writeln!(
            s,
            "- fact `{}` = `{}`, declared by {} (the user's own declaration, not checked)",
            f.key,
            f.value,
            f.by.label()
        );
    }
    if !d.facts.is_empty() {
        s.push('\n');
    }
}

/// The base catalogs section of the Markdown report.
fn base_markdown(s: &mut String, r: &Report) {
    if r.base_catalogs.is_empty() {
        return;
    }
    let table = |s: &mut String, files: &[crate::base::CatalogFile]| {
        let _ = writeln!(
            s,
            "| file | sha256 | tables and views | routines |\n|---|---|---:|---:|"
        );
        for c in files {
            let _ = writeln!(
                s,
                "| `{}` | `{}` | {} | {} |",
                c.path, c.sha256, c.tables, c.routines
            );
        }
        s.push('\n');
    };
    if !r.base_catalogs_b.is_empty() {
        let _ = writeln!(
            s,
            "## Base catalogs\n\nDDL of objects that exist already: `--catalog` in the source dialect for the source side, `--catalog-b` in the target dialect for the target side, each used as given (nothing converted). An object the tree defines replaces the base object of that name. They are not written into this tree and not counted above.\n\nSource side:\n"
        );
        table(s, &r.base_catalogs);
        let _ = writeln!(s, "Target side:\n");
        table(s, &r.base_catalogs_b);
        return;
    }
    let _ = writeln!(
        s,
        "## Base catalogs\n\nDDL of objects that exist already (`--catalog`), in the source dialect. Their objects bind on both sides; an object the tree defines replaces the base object of that name. For the target side they are converted with the same rules and checked against the base catalogs alone; they are not written into this tree and not counted above.\n"
    );
    table(s, &r.base_catalogs);
    let line = |s: &mut String, o: &crate::base::ObjectReport| {
        let _ = writeln!(
            s,
            "- {} `{}` (`{}` line {}): **{}** ({}) {}",
            o.kind,
            o.name,
            o.file,
            o.line,
            o.status.label(),
            o.rules.join(", "),
            o.reason.lines().next().unwrap_or("")
        );
    };
    let (out, kept): (Vec<_>, Vec<_>) = r.base_objects.iter().partition(|o| o.suspect);
    if !out.is_empty() {
        let _ = writeln!(
            s,
            "### Suspect base objects\n\nTheir conversion is not plain (or they use one that is not); the target catalog holds it all the same. Every statement that uses one carries `{}`, to accept once checked.\n",
            crate::base::BASE_SUSPECT
        );
        for o in out {
            line(s, o);
        }
        s.push('\n');
    }
    if !kept.is_empty() {
        let _ = writeln!(
            s,
            "### Base objects converted under assumptions\n\nStatements that use them bind to their converted form; their assumptions are not carried to those statements.\n"
        );
        for o in kept {
            line(s, o);
        }
        s.push('\n');
    }
    if !r.base_statements.is_empty() {
        let _ = writeln!(
            s,
            "### Base statements that did not convert (they define no object)\n"
        );
        for st in &r.base_statements {
            let _ = writeln!(
                s,
                "- `{}` line {}: **{}** ({}) {}",
                st.file,
                st.line,
                st.status.label(),
                st.rules.join(", "),
                st.reason.lines().next().unwrap_or("")
            );
        }
        s.push('\n');
    }
}

/// Files `sqldiff convert` writes into the converted tree besides the
/// converted files, and the review ledger kept there.
pub fn is_report(rel: &str) -> bool {
    matches!(
        rel,
        "conversion-report.json"
            | "conversion-report.md"
            | crate::queue::QUEUE_JSON
            | crate::queue::QUEUE_MD
            | crate::review::LEDGER_FILE
    )
}

/// SHA-256 over every file of a tree (or one file): its path and the hash
/// of its exact bytes, CRLF read as LF (a git checkout on Windows rewrites
/// line endings; see `review::file_hash`), in path order. Names starting with a dot and, at the
/// top, the reports and the ledger (`is_report`) are left out, so recording
/// a review decision does not change it.
pub fn dir_hash(dir: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    fn files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> std::io::Result<()> {
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let ty = e.file_type()?;
            let p = e.path();
            if ty.is_dir() {
                files(root, &p, out)?;
            } else if ty.is_file() {
                let rel = p.strip_prefix(root).unwrap_or(&p);
                out.push((rel.to_string_lossy().replace('\\', "/"), p));
            }
        }
        Ok(())
    }
    let mut list = Vec::new();
    if dir.is_file() {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        list.push((name, dir.to_path_buf()));
    } else {
        files(dir, dir, &mut list).map_err(|e| format!("{}: {e}", dir.display()))?;
        list.retain(|(rel, _)| !is_report(rel));
    }
    list.sort();
    let mut h = Sha256::new();
    for (rel, p) in list {
        let mut bytes = std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        if bytes.windows(2).any(|w| w == b"\r\n") {
            let mut lf = Vec::with_capacity(bytes.len());
            for (i, &b) in bytes.iter().enumerate() {
                if !(b == b'\r' && bytes.get(i + 1) == Some(&b'\n')) {
                    lf.push(b);
                }
            }
            bytes = lf;
        }
        h.update(rel.as_bytes());
        h.update([0]);
        h.update(Sha256::digest(&bytes));
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
