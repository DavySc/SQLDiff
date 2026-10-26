//! `sqldiff convert`: convert a whole source tree into another dialect.
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
//! converted through its fragment (`sqldiff_convert::rewrite_template`),
//! with one set of stand-in names for the whole tree so the catalogs bind
//! across files, and the output tree holds templates for the target.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use sqldiff_convert::{Conversion, FragmentKind, StandIns, Status, ValueKind};
use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{Dialect, Options};
use sqldiff_source::compose;
use sqldiff_syntax::template::Template;

pub struct Args {
    pub src: PathBuf,
    pub out: PathBuf,
    pub from: Dialect,
    pub to: Dialect,
    pub jobs: Option<usize>,
    pub overwrite: bool,
    pub no_catalog: bool,
    /// Every file is a template.
    pub template: Option<TemplateMode>,
    /// File extensions to read, without the dot, any letter case.
    pub extensions: Vec<String>,
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

/// The rule id of the assumption every unit of a template with generator
/// directives carries (`sqldiff_convert::check_template`).
const DIRECTIVE: &str = "ASM-TEMPLATE-DIRECTIVE";

/// One input file.
struct Input {
    rel: String,
    text: String,
}

#[derive(Serialize)]
struct FileReport {
    path: String,
    /// The worst unit status in the file; `differs` when an output finding
    /// points into no unit.
    status: Status,
    counts: sqldiff_convert::Counts,
    seconds: f64,
    /// Units that are not plainly converted, in output order.
    units: Vec<UnitReport>,
    /// Output findings that point into no unit.
    unplaced: Vec<String>,
    /// For a template: what it holds.
    #[serde(skip_serializing_if = "Option::is_none")]
    template: Option<sqldiff_convert::TemplateInfo>,
}

#[derive(Serialize)]
struct UnitReport {
    status: Status,
    /// 1-based lines in the input file and in the written output file.
    src_line: usize,
    out_line: usize,
    rules: Vec<String>,
    reason: String,
}

#[derive(Serialize)]
struct Report {
    from: String,
    to: String,
    src: String,
    out: String,
    catalogs: bool,
    /// Whether the files were read as templates.
    templates: bool,
    files: Vec<FileReport>,
    /// Files that could not be read as text.
    unreadable: Vec<String>,
    totals: Totals,
}

#[derive(Serialize, Default)]
struct Totals {
    files: usize,
    files_by_status: std::collections::BTreeMap<&'static str, usize>,
    units: std::collections::BTreeMap<&'static str, usize>,
    seconds: f64,
    /// Most frequent reasons a unit is not converted, with counts.
    top_reasons: Vec<(String, usize)>,
}

pub fn run(a: Args) -> Result<ExitCode, String> {
    let started = Instant::now();
    if !sqldiff_convert::supported(a.from, a.to) {
        return Err(format!(
            "no conversion rules from {} to {} yet",
            a.from.display_name(),
            a.to.display_name()
        ));
    }
    let (inputs, unreadable) = collect(&a.src, &a.extensions)?;
    if inputs.is_empty() {
        return Err(format!(
            "{}: no files with extension {}",
            a.src.display(),
            a.extensions.join(", ")
        ));
    }
    prepare_out(&a)?;
    let jobs = a
        .jobs
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
        .max(1);
    let results = match &a.template {
        None => convert_files(&a, &inputs, jobs),
        Some(tm) => convert_templates(&a, tm, &inputs, jobs)?,
    };

    let mut files = Vec::new();
    for (input, (c, secs)) in inputs.iter().zip(results) {
        let (text, lines) = annotate(&c);
        let dest = a.out.join(&input.rel);
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        std::fs::write(&dest, &text).map_err(|e| format!("{}: {e}", dest.display()))?;
        files.push(file_report(&input.rel, &c, &lines, secs));
    }

    let report = Report {
        from: a.from.id().to_string(),
        to: a.to.id().to_string(),
        src: a.src.display().to_string(),
        out: a.out.display().to_string(),
        catalogs: !a.no_catalog,
        templates: a.template.is_some(),
        totals: totals(&files, started.elapsed().as_secs_f64()),
        files,
        unreadable,
    };
    let json = a.out.join("conversion-report.json");
    std::fs::write(&json, serde_json::to_string_pretty(&report).unwrap() + "\n")
        .map_err(|e| format!("{}: {e}", json.display()))?;
    let md = a.out.join("conversion-report.md");
    std::fs::write(&md, markdown(&report)).map_err(|e| format!("{}: {e}", md.display()))?;
    print!("{}", summary(&report));
    eprintln!("wrote {} and {}", json.display(), md.display());
    let worst = report
        .files
        .iter()
        .map(|f| f.status)
        .max()
        .unwrap_or(Status::Converted);
    Ok(ExitCode::from(
        if worst <= Status::Assumptions && report.unreadable.is_empty() {
            0
        } else {
            1
        },
    ))
}

/// Converts plain SQL files: rules with the source catalog of the whole
/// tree, then a check of every draft against both trees' catalogs.
fn convert_files(a: &Args, inputs: &[Input], jobs: usize) -> Vec<(Conversion, f64)> {
    // Catalogs from the whole tree, so names defined in other files bind:
    // the source catalog also lets the rules see other files' procedures.
    let source = (!a.no_catalog).then(|| {
        Arc::new(catalog(
            inputs.iter().map(|i| i.text.as_str()),
            a.from,
            &a.src,
            None,
        ))
    });
    // Pass 1: rules only.
    let drafts = parallel(jobs, inputs, |i| {
        sqldiff_convert::rewrite_in(&i.text, a.from, a.to, source.as_deref())
    });
    let mut opts = Options::default();
    if let Some(source) = source {
        opts.facts_a = vec![source];
        opts.facts_b = vec![Arc::new(catalog(
            drafts.iter().map(|d| d.output()),
            a.to,
            &a.out,
            None,
        ))];
    }
    // Pass 2: check every draft.
    let work = slots(inputs.iter().zip(drafts));
    let progress = Progress::new(work.len());
    parallel(jobs, &work, |w| {
        let (input, draft) = w.lock().unwrap().take().unwrap();
        let t = Instant::now();
        let c = sqldiff_convert::check(&input.text, draft, &opts);
        progress.tick();
        (c, t.elapsed().as_secs_f64())
    })
}

/// Converts templates: every file is split by the placeholder rules, and
/// all fragments share one set of stand-in names, so a table a template
/// creates as `<%SCHEMA%>.<%TABLE%>` binds in the templates that use it.
fn convert_templates(
    a: &Args,
    tm: &TemplateMode,
    inputs: &[Input],
    jobs: usize,
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
    let mut stand = StandIns::new(parsed.iter().map(|(t, _)| t)).with_kinds(tm.kinds.clone());
    if !a.no_catalog {
        stand = stand.bound_across();
    }
    let stem = Some(stand.stem());
    let source = (!a.no_catalog).then(|| {
        let frags: Vec<String> = parsed
            .iter()
            .map(|(t, _)| sqldiff_convert::template_fragment(t, &stand))
            .collect();
        Arc::new(catalog(
            frags.iter().map(String::as_str),
            a.from,
            &a.src,
            stem,
        ))
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
            target: Target::Pair(h, f),
        });
    }
    for (k, (t, basis)) in parsed.into_iter().enumerate() {
        if paired[k] {
            continue;
        }
        let kind = tm
            .fragments
            .iter()
            .find(|(g, _)| g.is_match(&inputs[k].rel))
            .map_or(FragmentKind::Statements, |(_, kind)| *kind);
        jobs_list.push(Job {
            template: t,
            basis,
            kind,
            target: Target::File(k),
        });
    }

    let drafts = parallel(jobs, &jobs_list, |j| {
        sqldiff_convert::rewrite_template_as(
            &j.template,
            j.kind,
            a.from,
            a.to,
            &stand,
            source.as_deref(),
        )
    });
    let mut opts = Options::default();
    if let Some(source) = source {
        opts.facts_a = vec![source];
        opts.facts_b = vec![Arc::new(catalog(
            drafts.iter().map(|d| d.output()),
            a.to,
            &a.out,
            stem,
        ))];
    }
    let work = slots(jobs_list.iter().zip(drafts));
    let progress = Progress::new(work.len());
    let delimiters = tm.placeholders.delimiters();
    let done = parallel(jobs, &work, |w| {
        let (j, draft) = w.lock().unwrap().take().unwrap();
        let t0 = Instant::now();
        let c =
            sqldiff_convert::check_template(&j.template, draft, &stand, &opts, j.basis, delimiters);
        progress.tick();
        (c, t0.elapsed().as_secs_f64())
    });
    let mut out: Vec<Option<(Conversion, f64)>> = (0..inputs.len()).map(|_| None).collect();
    for (j, (c, secs)) in jobs_list.iter().zip(done) {
        match j.target {
            Target::File(k) => out[k] = Some((c, secs)),
            Target::Pair(h, f) => {
                let (ch, cf) =
                    sqldiff_convert::split_pair(c, &inputs[h].text, &inputs[f].text, &marker);
                out[h] = Some((ch, secs / 2.0));
                out[f] = Some((cf, secs / 2.0));
            }
        }
    }
    Ok(out
        .into_iter()
        .map(|c| c.expect("every file is converted"))
        .collect())
}

/// One conversion: a file, or a routine's opening and closing templates.
struct Job {
    template: Template,
    basis: sqldiff_core::composition::PlaceholderBasis,
    kind: FragmentKind,
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

struct Progress {
    done: AtomicUsize,
    total: usize,
}

impl Progress {
    fn new(total: usize) -> Progress {
        Progress {
            done: AtomicUsize::new(0),
            total,
        }
    }

    fn tick(&self) {
        let n = self.done.fetch_add(1, Ordering::Relaxed) + 1;
        if n % 50 == 0 || n == self.total {
            eprintln!("checked {n}/{}", self.total);
        }
    }
}

/// Every file with one of `extensions` under `src` (or `src` itself),
/// sorted by path.
fn collect(src: &Path, extensions: &[String]) -> Result<(Vec<Input>, Vec<String>), String> {
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
        let bytes = std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        match decode(&bytes) {
            Some(text) => inputs.push(Input { rel, text }),
            None => unreadable.push(rel),
        }
    }
    Ok((inputs, unreadable))
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
pub(crate) fn decode(bytes: &[u8]) -> Option<String> {
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
fn prepare_out(a: &Args) -> Result<(), String> {
    let src = std::fs::canonicalize(&a.src).map_err(|e| format!("{}: {e}", a.src.display()))?;
    std::fs::create_dir_all(&a.out).map_err(|e| format!("{}: {e}", a.out.display()))?;
    let out = std::fs::canonicalize(&a.out).map_err(|e| format!("{}: {e}", a.out.display()))?;
    if src.is_dir() && out.starts_with(&src) {
        return Err(format!(
            "{}: the output directory is inside the source tree",
            a.out.display()
        ));
    }
    if src.starts_with(&out) {
        return Err(format!(
            "{}: the output directory contains the source",
            a.out.display()
        ));
    }
    let empty = std::fs::read_dir(&out)
        .map_err(|e| format!("{}: {e}", a.out.display()))?
        .next()
        .is_none();
    if !empty && !a.overwrite {
        return Err(format!(
            "{}: not empty; pass --overwrite to replace the files a conversion writes",
            a.out.display()
        ));
    }
    Ok(())
}

/// Runs `f` over `items` on `jobs` threads; results in input order.
pub(crate) fn parallel<T: Sync, R: Send>(
    jobs: usize,
    items: &[T],
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<Option<R>>> =
        items.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..jobs.min(items.len()) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(item) = items.get(i) else { break };
                let r = f(item);
                *slots[i].lock().unwrap() = Some(r);
            });
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
pub(crate) fn catalog<'a>(
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

fn file_report(rel: &str, c: &Conversion, lines: &[usize], secs: f64) -> FileReport {
    let mut status = c
        .units
        .iter()
        .map(|u| u.status)
        .max()
        .unwrap_or(Status::Converted);
    if !c.unplaced.is_empty() {
        status = status.max(Status::Differs);
    }
    let units = c
        .units
        .iter()
        .zip(lines)
        .filter(|(u, _)| u.status != Status::Converted)
        .map(|(u, &l)| {
            let worst: Vec<&sqldiff_convert::Note> =
                u.notes.iter().filter(|n| n.status == u.status).collect();
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
            UnitReport {
                status: u.status,
                src_line: u.src.line,
                out_line: l,
                rules: if worst.is_empty() {
                    vec!["engine".to_string()]
                } else {
                    worst.iter().map(|n| n.rule.clone()).collect()
                },
                reason,
            }
        })
        .collect();
    FileReport {
        path: rel.to_string(),
        status,
        counts: c.counts.clone(),
        seconds: (secs * 1000.0).round() / 1000.0,
        units,
        unplaced: c
            .unplaced
            .iter()
            .filter_map(|&i| c.output_check.findings.get(i))
            .map(|f| format!("{}: {}", f.rule, f.witness))
            .collect(),
        template: c.template.clone(),
    }
}

const ORDER: [Status; 5] = [
    Status::Converted,
    Status::Assumptions,
    Status::Decision,
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
            (Status::Differs, f.counts.differs),
            (Status::NotConverted, f.counts.not_converted),
        ] {
            *t.units.entry(s.label()).or_default() += n;
        }
        for u in &f.units {
            if u.status >= Status::Differs {
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

fn summary(r: &Report) -> String {
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
    s
}

fn markdown(r: &Report) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# Conversion report\n");
    let _ = writeln!(
        s,
        "`{}` → `{}`, {} to {}. Cross-file catalogs: {}.\n",
        r.src,
        r.out,
        r.from,
        r.to,
        if r.catalogs { "yes" } else { "no" }
    );
    let _ = writeln!(
        s,
        "A file counts as converted only when every unit in it is: no assumption, no decision, nothing found by the engine's diff or by checking the output on its own (binding, leftover source syntax, target limits).\n"
    );
    let _ = writeln!(s, "```\n{}```\n", summary(r));
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
        let _ = writeln!(s, "### `{}`: {}\n", f.path, f.status.label());
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
