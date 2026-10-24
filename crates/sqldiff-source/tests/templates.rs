//! Templates from a source: what is a placeholder is decided by the
//! template's parameter names, not by the delimiters alone, because `[name]`
//! is also a quoted identifier in T-SQL.

use std::collections::BTreeSet;

use sqldiff_core::{Artifact, Dialect, Options, Report};
use sqldiff_source::bundle::{Bundle, BundleSource};
use sqldiff_source::config::Sources;
use sqldiff_source::protocol::{
    ArtifactData, Describe, ObjectInfo, Pair, PairScope, Provenance, TemplateInfo,
};
use sqldiff_source::{queue, tree, Resolved};

const PACK: &str = "[pack]\nname = \"t\"\nversion = \"1\"\n\n\
                    [template]\nopen = \"[\"\nclose = \"]\"\n\
                    name_pattern = \"[A-Z][A-Z0-9_]*\"\n\
                    known_parameters = [\"OBJECT_ID\"]\n";

fn cfg(name: &str) -> Resolved {
    cfg_with(PACK, name)
}

fn cfg_with(pack: &str, name: &str) -> Resolved {
    let d = std::env::temp_dir().join(format!("sqldiff-templates-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("pack.toml"), pack).unwrap();
    Resolved::load(&Sources {
        isolated: true,
        packs: vec![d],
        ..Sources::default()
    })
    .unwrap()
}

// `[TAB]` is a parameter; in T-SQL `[ID]` is a quoted column and `[qty]` a
// quoted name the pattern does not match.
const TSQL: &str = "update [TAB] set [qty] = 0 where [ID] = 1;\n";
const DB2: &str = "update [TAB] set qty = 0 where ID = 1;\n";

fn rules(r: &Report) -> Vec<&str> {
    r.findings.iter().map(|f| f.rule.as_str()).collect()
}

fn pattern_reasons(r: &Report) -> Vec<&String> {
    r.unknown_reasons
        .iter()
        .filter(|x| x.contains("taken as placeholders by the name pattern"))
        .collect()
}

fn tdiff(a: &str, b: &str, known: [Option<&BTreeSet<String>>; 2], name: &str) -> Report {
    tree::template_diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Db2i, b),
        known,
        &cfg(name),
        &Options::default(),
    )
    .unwrap()
}

#[test]
fn known_parameters_decide_what_is_a_placeholder() {
    let tab: BTreeSet<String> = ["TAB".to_string()].into();
    let r = tdiff(TSQL, DB2, [Some(&tab), Some(&tab)], "known");
    // `[ID]` matches the pattern but is not a parameter: kept as SQL and said so.
    let unresolved: Vec<&String> = r
        .findings
        .iter()
        .filter(|f| f.rule == "template.unresolved")
        .map(|f| &f.witness)
        .collect();
    assert_eq!(unresolved.len(), 1, "{unresolved:?}");
    assert!(unresolved[0].contains("`[ID]`"), "{}", unresolved[0]);
    assert!(pattern_reasons(&r).is_empty(), "{:?}", r.unknown_reasons);
    // The pack's generator-supplied names are known too.
    let r = tdiff(
        "exec [OBJECT_ID];\nupdate [TAB] set x = 1;\n",
        "call [OBJECT_ID];\nupdate [TAB] set x = 1;\n",
        [Some(&tab), Some(&tab)],
        "pack-known",
    );
    assert!(
        !rules(&r).contains(&"template.unresolved"),
        "{:?}",
        rules(&r)
    );
}

#[test]
fn without_a_list_the_pattern_decides_and_the_report_says_so() {
    let r = tdiff(TSQL, DB2, [None, None], "pattern");
    assert!(
        !rules(&r).contains(&"template.unresolved"),
        "{:?}",
        rules(&r)
    );
    // Only the T-SQL side, where `[...]` also quotes names.
    let reasons = pattern_reasons(&r);
    assert_eq!(reasons.len(), 1, "{:?}", r.unknown_reasons);
    assert!(
        reasons[0].starts_with("side A") && reasons[0].contains("[TAB], [ID]"),
        "{}",
        reasons[0]
    );
}

fn object(id: &str, dialect: &str, text: &str, template: Option<TemplateInfo>) -> ArtifactData {
    ArtifactData {
        info: ObjectInfo {
            id: id.into(),
            kind: "script".into(),
            dialect: Some(dialect.into()),
            version: None,
            name: "t".into(),
            content_hash: String::new(),
            bytes: 0,
            last_changed: None,
            stale: false,
        },
        text: text.into(),
        provenance: Provenance::default(),
        metadata: Default::default(),
        template,
    }
}

fn run(template: Option<TemplateInfo>, name: &str) -> Report {
    let mut b = Bundle::new(
        Describe {
            name: "test".into(),
            version: "1".into(),
            protocol: 1,
            capabilities: vec![],
            source: String::new(),
        },
        vec![
            object("a", "tsql", TSQL, template.clone()),
            object("b", "db2i", DB2, template),
        ],
    );
    b.pairs = vec![Pair {
        a: "a".into(),
        b: "b".into(),
        key: "t".into(),
        rule: "explicit".into(),
    }];
    b.seal();
    let cfg = cfg(name);
    let run = queue::run(&BundleSource::new(b, &cfg), &cfg, &PairScope::default()).unwrap();
    run.pairs.into_iter().next().unwrap().report
}

#[test]
fn pairs_from_a_source_are_compared_as_templates() {
    let marked = Some(TemplateInfo {
        parameters: Some(vec!["TAB".into()]),
    });
    let r = run(marked, "queue-template");
    // `[TAB]` is a placeholder on both sides, so the two UPDATEs pair up and
    // the Db2 side has no T-SQL bracket residue.
    assert!(
        !rules(&r)
            .iter()
            .any(|x| *x == "write.unpaired" || x.starts_with("residue.tsql.bracket")),
        "{:?}",
        rules(&r)
    );
    assert!(
        rules(&r).contains(&"template.unresolved"),
        "{:?}",
        rules(&r)
    );
    // The same objects without the template marker are plain SQL: the Db2
    // `update [TAB]` is not readable as a write.
    let r = run(None, "queue-plain");
    assert!(rules(&r).contains(&"write.unpaired"), "{:?}", rules(&r));
}

#[test]
fn a_declared_pattern_convention_decides_and_is_an_assumption() {
    let pack = format!("{PACK}placeholders = \"pattern\"\n");
    let tab: BTreeSet<String> = ["TAB".to_string()].into();
    let r = tree::template_diff(
        &Artifact::new("a.sql", Dialect::Tsql, TSQL),
        &Artifact::new("b.sql", Dialect::Db2i, DB2),
        [Some(&tab), Some(&tab)],
        &cfg_with(&pack, "declared"),
        &Options::default(),
    )
    .unwrap();
    // Upper-case names are placeholders by convention, list or not; `[qty]`
    // does not match the pattern and stays a quoted name.
    assert!(
        !rules(&r).contains(&"template.unresolved"),
        "{:?}",
        rules(&r)
    );
    assert!(pattern_reasons(&r).is_empty(), "{:?}", r.unknown_reasons);
    let declared: Vec<&String> = r
        .assumptions
        .iter()
        .filter(|a| a.id.starts_with("ASM-TEMPLATE-PLACEHOLDER-PATTERN"))
        .map(|a| &a.id)
        .collect();
    // Only on the T-SQL side, where `[...]` also quotes names.
    assert_eq!(
        declared,
        ["ASM-TEMPLATE-PLACEHOLDER-PATTERN-A"],
        "{:?}",
        r.assumptions
    );
    // An unknown setting is refused when the pack loads.
    let bad = format!("{PACK}placeholders = \"guess\"\n");
    let d = std::env::temp_dir().join(format!("sqldiff-templates-{}-bad", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("pack.toml"), bad).unwrap();
    let e = sqldiff_source::Pack::load(&d).unwrap_err();
    assert!(e.0.contains("template.placeholders"), "{e}");
}

#[test]
fn where_brackets_are_not_sql_a_wider_name_pattern_applies() {
    let pack = format!(
        "{PACK}placeholders = \"pattern\"\nunquoted_name_pattern = \"[A-Za-z_][A-Za-z0-9_]*\"\n"
    );
    let cfg = cfg_with(&pack, "unquoted");
    let side = |text: &str, d: Dialect| {
        let r = tree::template_diff(
            &Artifact::new("a.sql", d, text),
            &Artifact::new("b.sql", d, text),
            [None, None],
            &cfg,
            &Options::default(),
        )
        .unwrap();
        r.assumptions
            .iter()
            .filter(|a| a.id.starts_with("ASM-TEMPLATE-PLACEHOLDER-PATTERN"))
            .map(|a| a.text.clone())
            .collect::<Vec<_>>()
    };
    let text = "update [TAB] set x = 1 where [pk_col] = 2;\n";
    // T-SQL: only the upper-case name is a placeholder; `[pk_col]` is a quoted name.
    let t = side(text, Dialect::Tsql);
    assert!(t.iter().all(|x| x.contains("([TAB])")), "{t:?}");
    // Db2 for i: brackets are not SQL, so `[pk_col]` is a placeholder too, and
    // no assumption is needed.
    assert!(side(text, Dialect::Db2i).is_empty());
    let r = tree::template_diff(
        &Artifact::new("a.sql", Dialect::Db2i, text),
        &Artifact::new(
            "b.sql",
            Dialect::Db2i,
            "update [TAB] set x = 1 where [PK_COL] = 2;\n",
        ),
        [None, None],
        &cfg,
        &Options::default(),
    )
    .unwrap();
    // Different placeholders, not T-SQL bracket residue.
    assert!(
        !rules(&r).iter().any(|x| x.starts_with("residue.")),
        "{:?}",
        rules(&r)
    );
}

#[test]
fn a_convention_plus_the_item_list_decides_in_t_sql() {
    let pack = format!("{PACK}placeholders = \"pattern\"\nignore_case = true\n");
    let cfg = cfg_with(&pack, "convention-list");
    let listed: BTreeSet<String> = ["COMMA".to_string()].into();
    let text = "select a[comma] b from [TAB] where [read] = 1;\n";
    let r = tree::template_diff(
        &Artifact::new("a.sql", Dialect::Tsql, text),
        &Artifact::new("b.sql", Dialect::Tsql, text),
        [Some(&listed), Some(&listed)],
        &cfg,
        &Options::default(),
    )
    .unwrap();
    assert!(
        !rules(&r).contains(&"template.unresolved"),
        "{:?}",
        rules(&r)
    );
    let a = r
        .assumptions
        .iter()
        .find(|a| a.id == "ASM-TEMPLATE-PLACEHOLDER-PATTERN-A")
        .expect("declared convention");
    // `[TAB]` by convention, `[comma]` because the items list it; `[read]`
    // stays a quoted name.
    assert!(a.text.contains("([COMMA], [TAB])"), "{}", a.text);
    assert!(!a.text.contains("READ"), "{}", a.text);
}

#[test]
fn glued_names_are_placeholders_without_a_list() {
    let pack = format!("{PACK}placeholders = \"pattern\"\n");
    let cfg = cfg_with(&pack, "glued");
    let text = "set @[parmtr_id][comma] = 1;\nexecute exh_[add_chg_del]_item;\n\
                select [read] from [dbo].[t];\n";
    let r = tree::template_diff(
        &Artifact::new("a.sql", Dialect::Tsql, text),
        &Artifact::new("b.sql", Dialect::Tsql, text),
        [None, None],
        &cfg,
        &Options::default(),
    )
    .unwrap();
    let a = r
        .assumptions
        .iter()
        .find(|a| a.id == "ASM-TEMPLATE-PLACEHOLDER-PATTERN-A")
        .expect("declared convention");
    // Lower-case, but glued to `@`, an identifier or another delimited name.
    for p in ["[parmtr_id]", "[comma]", "[add_chg_del]"] {
        assert!(a.text.contains(p), "{p}: {}", a.text);
    }
    // Free-standing lower-case names stay quoted names.
    for q in ["[read]", "[dbo]", "[t]"] {
        assert!(!a.text.contains(q), "{q}: {}", a.text);
    }
}
