//! Template layer and composition (design section 3 layers 1 and 4,
//! section 5.3): the m23 gate, check mode, staleness, wrapper facts and
//! the template round trip.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sqldiff_core::composition::{self, Composed};
use sqldiff_core::model::{AssumptionSource, Side};
use sqldiff_core::{Artifact, ChangeClass, Dialect, Options, Report, Verdict};
use sqldiff_source::compose::{self, TemplateDir};
use sqldiff_source::{protocol, Pack};
use sqldiff_syntax::template::{SegmentKind, Template};

fn corpus() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus")
}

fn pack() -> Pack {
    Pack::load(&corpus().join("packs/synthetic-generator.toml")).unwrap()
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sqldiff-compose-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn m23(side: &str) -> Artifact {
    let p = corpus().join(format!(
        "mutations/m23-composed-fragment-dropped/{side}.tsql.sql"
    ));
    Artifact::new(
        format!("{side}.tsql.sql"),
        Dialect::Tsql,
        std::fs::read_to_string(p).unwrap(),
    )
}

fn diff(a: &Artifact, b: &Artifact, pack: &Pack, templates: Option<&TemplateDir>) -> Report {
    let opts = Options::default();
    let mut r = sqldiff_core::diff(a, b, &opts);
    let ca = compose::read(&a.text, &pack.policy).unwrap();
    let cb = compose::read(&b.text, &pack.policy).unwrap();
    compose::annotate(
        &mut r,
        (a, ca.as_ref()),
        Some((b, cb.as_ref())),
        &pack.policy,
        templates,
        &opts,
    )
    .unwrap();
    r
}

/// Gate: m23 is detected for the right reason, the dropped composed part.
#[test]
fn m23_dropped_part_is_the_finding() {
    let (a, b) = (m23("original"), m23("mutated"));
    let r = diff(&a, &b, &pack(), None);
    assert_eq!(r.verdict, Verdict::Different);
    let part = r
        .findings
        .iter()
        .find(|f| f.rule == "composition.part-removed")
        .expect("composition finding");
    assert_eq!(part.class, ChangeClass::Transactional);
    assert_eq!(part.verdict, Verdict::Different);
    assert!(
        part.witness
            .contains("part 'session-options' (template xact-abort)")
            && part.witness.contains("missing from the side B output")
            && part.witness.contains("SET XACT_ABORT ON"),
        "{}",
        part.witness
    );
    assert_eq!(part.a.as_ref().unwrap().start_line, 7);
    // The part explains the removed statement, so it is not reported twice.
    assert!(
        !r.findings.iter().any(|f| f.rule == "stmt.removed"),
        "{:#?}",
        r.findings
    );
    // The behavioural consequence is still there.
    assert!(r
        .findings
        .iter()
        .any(|f| f.rule == "txn.atomicity" && f.verdict == Verdict::Different));
    // XACT_ABORT on side A is a wrapper fact, proven from composed output.
    let fact = r
        .assumptions
        .iter()
        .find(|x| x.id.starts_with("COMPOSED-A:session-options:"))
        .expect("wrapper fact");
    assert_eq!(fact.source, AssumptionSource::ComposedOutput);
    assert!(fact.text.contains("SET XACT_ABORT ON"), "{}", fact.text);
    // Everything is transactional except which statements run after the
    // error, which is control flow (error.control is semantic since WP9).
    let classes: Vec<(&str, ChangeClass)> = r
        .findings
        .iter()
        .map(|f| (f.rule.as_str(), f.class))
        .collect();
    assert!(
        classes
            .iter()
            .all(|(rule, c)| *c == ChangeClass::Transactional
                || (*rule == "error.control" && *c == ChangeClass::Semantic)),
        "{classes:?}"
    );
}

/// Without the pack the same pair is still different, but only from statements.
#[test]
fn m23_without_pack_has_no_composition() {
    let (a, b) = (m23("original"), m23("mutated"));
    let r = sqldiff_core::diff(&a, &b, &Options::default());
    assert!(!r
        .findings
        .iter()
        .any(|f| f.rule.starts_with("composition.")));
}

/// Wrapper facts are discharged: they do not weaken an equal verdict.
#[test]
fn composed_facts_do_not_weaken_equal() {
    let a = m23("original");
    let r = diff(&a, &a, &pack(), None);
    assert!(r
        .assumptions
        .iter()
        .all(|x| x.source == AssumptionSource::ComposedOutput));
    assert!(!r.assumptions.is_empty());
    assert_eq!(r.verdict, Verdict::Equal, "{r:#?}");
}

const GENERATED: &str = "\
-- [part:header template=hdr]
CREATE PROCEDURE dbo.purge AS
BEGIN
-- [/part]
-- [part:body template=purge]
  DELETE FROM dbo.orders WHERE closed = 1;
-- [/part]
-- [part:footer template=ftr]
END
-- [/part]
";

fn template_dir(body: &str) -> (PathBuf, TemplateDir) {
    let d = tmp(&format!("tpl-{}", body.len()));
    std::fs::write(
        d.join("hdr.sql"),
        "CREATE PROCEDURE {{SCHEMA}}.purge AS\nBEGIN\n",
    )
    .unwrap();
    std::fs::write(d.join("purge.sql"), body).unwrap();
    std::fs::write(d.join("ftr.sql"), "END\n").unwrap();
    let t = TemplateDir::new(&d).unwrap();
    (d, t)
}

fn check_mode(text: &str, dir: &TemplateDir, extra: &BTreeMap<String, String>) -> Report {
    let pack = pack();
    let art = Artifact::new("gen.tsql.sql", Dialect::Tsql, text);
    let opts = Options::default();
    let mut r = sqldiff_core::check(&art, &opts);
    // Extra values arrive as parameter source `cli`, merged by the pack's rule.
    let dir = TemplateDir::new(&dir.root)
        .unwrap()
        .with_params(extra.clone());
    compose::apply(&mut r, &art, None, &pack.policy, Some(&dir), &opts);
    r
}

#[test]
fn check_mode_matches_and_mismatches() {
    let params: BTreeMap<String, String> = [("SCHEMA".to_string(), "dbo".to_string())].into();
    let (_d, dir) = template_dir("  DELETE FROM {{SCHEMA}}.orders WHERE closed = 1;\n");
    let ok = check_mode(GENERATED, &dir, &params);
    assert!(ok.findings.is_empty(), "{:#?}", ok.findings);
    assert_eq!(ok.verdict, Verdict::Equal, "{:?}", ok.unknown_reasons);

    // Someone edited the generated output by hand.
    let edited = GENERATED.replace("closed = 1", "closed = 1 AND archived = 1");
    let bad = check_mode(&edited, &dir, &params);
    let f = bad
        .findings
        .iter()
        .find(|f| f.rule == "composition.substitution-mismatch")
        .expect("mismatch finding");
    assert!(
        f.witness.contains("part 'body' (template purge)"),
        "{}",
        f.witness
    );
    assert_eq!(f.a.as_ref().unwrap().start_line, 6);
    assert_eq!(bad.verdict, Verdict::Unknown);

    // No value for SCHEMA: the header cannot be checked, which is never hidden.
    let none = check_mode(GENERATED, &dir, &BTreeMap::new());
    assert!(
        none.unknown_reasons
            .iter()
            .any(|r| r.contains("part 'header'") && r.contains("no value for parameter `SCHEMA`")),
        "{:?}",
        none.unknown_reasons
    );
    assert_eq!(none.verdict, Verdict::Unknown);
}

/// A dropped part holding a compound with a write inside stays DIFFERENT.
#[test]
fn dropped_compound_part_is_different() {
    let tail = "-- [part:body template=b]\n  UPDATE dbo.t SET x = 1;\n-- [/part]\nEND\n";
    let a_text = format!(
        "CREATE PROCEDURE dbo.p AS\nBEGIN\n-- [part:guard template=g]\n  BEGIN TRY\n    INSERT INTO dbo.audit (x) VALUES (1);\n  END TRY\n  BEGIN CATCH\n    THROW;\n  END CATCH;\n-- [/part]\n{tail}"
    );
    let b_text = format!("CREATE PROCEDURE dbo.p AS\nBEGIN\n{tail}");
    let a = Artifact::new("a.sql", Dialect::Tsql, a_text);
    let b = Artifact::new("b.sql", Dialect::Tsql, b_text);
    let r = diff(&a, &b, &pack(), None);
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == "composition.part-removed")
        .expect("part finding");
    assert_eq!(f.verdict, Verdict::Different, "{}", f.witness);
    assert_eq!(r.verdict, Verdict::Different);
}

/// Check mode asked for without composition markers says so.
#[test]
fn templates_without_markers_are_not_silent() {
    let (_d, dir) = template_dir("x");
    let pack = pack();
    let mut policy = pack.policy.clone();
    policy.composition.clear();
    let art = Artifact::new("gen.tsql.sql", Dialect::Tsql, GENERATED);
    let opts = Options::default();
    let mut r = sqldiff_core::check(&art, &opts);
    compose::apply(&mut r, &art, None, &policy, Some(&dir), &opts);
    assert!(
        r.unknown_reasons
            .iter()
            .any(|x| x.contains("check mode") && x.contains("did not run")),
        "{:?}",
        r.unknown_reasons
    );
    assert_eq!(r.verdict, Verdict::Unknown);
}

#[test]
fn stale_adapter_composition_caps_at_unknown() {
    let a = Artifact::new("gen.tsql.sql", Dialect::Tsql, GENERATED);
    let start = GENERATED.find("  DELETE").unwrap();
    let end = start + "  DELETE FROM dbo.orders WHERE closed = 1;\n".len();
    let c = protocol::Composition {
        id: "gen".into(),
        parts: vec![protocol::CompositionPart {
            name: "body".into(),
            start,
            end,
            template: Some("purge".into()),
            parameters: BTreeMap::new(),
            template_hash: None,
        }],
        stale: true,
    };
    let comp = compose::from_protocol(GENERATED, &c);
    assert!(comp.problems.is_empty());
    let opts = Options::default();
    let mut r = sqldiff_core::diff(&a, &a, &opts);
    composition::annotate(
        &mut r,
        Composed::new(&a, Some(&comp)),
        Some(Composed::new(&a, Some(&comp))),
        &opts,
    );
    assert_eq!(r.verdict, Verdict::Unknown);
    assert!(r.unknown_reasons.iter().any(|x| x.contains("stale")));
}

/// Round trip through the template layer on every corpus SQL file, with
/// and without placeholders.
#[test]
fn template_round_trip_over_corpus() {
    let pack = pack();
    let syntax = compose::compile(pack.policy.template.as_ref().unwrap()).unwrap();
    let mut files = Vec::new();
    collect(&corpus(), &mut files);
    assert!(files.len() > 40, "{}", files.len());
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        let t = Template::parse(&text, &syntax, None);
        assert_eq!(t.print(), text, "{}", f.display());
        assert_eq!(t.fragment().text, text, "{}", f.display());
        // Turn every identifier-ish word `stock` into a placeholder and back.
        let templ = text.replace("stock", "{{STOCK}}");
        let t = Template::parse(&templ, &syntax, None);
        assert_eq!(t.print(), templ);
        let params: BTreeMap<String, String> = [("STOCK".to_string(), "stock".to_string())].into();
        let out = t.substitute(&params).unwrap();
        assert_eq!(out.text, text, "{}", f.display());
        // Offsets map back: each placeholder's output span is its value.
        for s in &t.segments {
            if let SegmentKind::Placeholder { .. } = s.kind {
                let at = out.map.to_dst(s.start);
                assert_eq!(&out.text[at..at + 5], "stock");
                assert_eq!(out.map.to_src(at), s.start);
            }
        }
        // The fragment parses like the substituted output, statement for statement.
        let frag = t.fragment();
        let dialect = dialect_of(&f);
        let n = |text: &str| {
            sqldiff_syntax::parse(text, dialect)
                .syntax()
                .descendants()
                .filter(|n| n.kind().is_unit())
                .count()
        };
        assert_eq!(n(&frag.text), n(&text), "{}", f.display());
        assert!(!frag.text.contains("{{") || text.contains("{{"));
    }
}

fn dialect_of(p: &Path) -> Dialect {
    let s = p.to_string_lossy();
    if s.contains("db2i") {
        Dialect::Db2i
    } else if s.contains("pg") {
        Dialect::Postgres
    } else {
        Dialect::Tsql
    }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "sql") {
            out.push(p);
        }
    }
}

/// Template-level diff: findings point into the templates and quote placeholders.
#[test]
fn template_level_diff_locates_in_templates() {
    let pack = pack();
    let syntax = compose::compile(pack.policy.template.as_ref().unwrap()).unwrap();
    let ta = Template::parse(
        "UPDATE {{T}} SET qty = qty - 1 WHERE id = {{ID}};\nINSERT INTO {{T}}_log VALUES ({{ID}});\n",
        &syntax,
        None,
    );
    let tb = Template::parse(
        "UPDATE {{T}} SET qty = qty - 1 WHERE id = {{ID}};\n",
        &syntax,
        None,
    );
    let (fa, fb) = (ta.fragment(), tb.fragment());
    let a = Artifact::new("a.sql", Dialect::Postgres, fa.text.clone());
    let b = Artifact::new("b.sql", Dialect::Postgres, fb.text.clone());
    let opts = Options::default();
    let mut r = sqldiff_core::diff(&a, &b, &opts);
    composition::remap_to_template(&mut r, Side::A, &ta, &fa, Dialect::Postgres, 3);
    composition::remap_to_template(&mut r, Side::B, &tb, &fb, Dialect::Postgres, 3);
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == "stmt.removed")
        .expect("removed");
    let loc = f.a.as_ref().unwrap();
    assert_eq!(loc.start_line, 2);
    assert_eq!(
        &ta.text[loc.byte_start..loc.byte_end],
        "INSERT INTO {{T}}_log VALUES ({{ID}});"
    );
    assert!(f.witness.contains("{{T}}"), "{}", f.witness);
}
