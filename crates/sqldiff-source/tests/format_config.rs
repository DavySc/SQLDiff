//! The `[format]` layers of a loaded config, where each key comes from, and
//! writing `[format]` into a config file.

use std::path::{Path, PathBuf};

use sqldiff_core::format::{FormatConfig, FormatKeys, IndentStyle, KeywordCase, LineEnding};
use sqldiff_core::Dialect;
use sqldiff_source::config::{format_sources, merge_format, LayerKind, Sources};
use sqldiff_source::format_file::set_format;
use sqldiff_source::Resolved;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sqldiff-fmtcfg-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(root: &Path, rel: &str, text: &str) -> PathBuf {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, text).unwrap();
    p
}

fn parse(text: &str) -> FormatConfig {
    sqldiff_source::config::ConfigLayer::parse(text)
        .unwrap()
        .policy
        .format
}

const PACK: &str = "[pack]
name = \"fmt-pack\"
version = \"1\"

[format]
keyword_case = \"lower\"
indent = 2

[format.postgres]
indent = 3
one_item_per_line = true
";

const WORKSPACE: &str = "[format]
keyword_case = \"upper\"
max_blank_lines = 2

[format.tsql]
indent_style = \"tabs\"
";

fn load(root: &Path, project: Option<&Path>) -> Resolved {
    Resolved::load(&Sources {
        isolated: true,
        project: project.map(Path::to_path_buf),
        packs: vec![root.join("pack.toml")],
        ..Sources::default()
    })
    .unwrap()
}

#[test]
fn layers_are_kept_in_merge_order() {
    let root = tmp("layers");
    write(&root, "pack.toml", PACK);
    let ws = write(&root, "ws/sqldiff.toml", WORKSPACE);
    let cfg = load(&root, Some(&ws));
    let kinds: Vec<_> = cfg.format_layers.iter().map(|l| l.kind).collect();
    assert_eq!(kinds, [LayerKind::PrivatePack, LayerKind::Project]);
    assert_eq!(cfg.format_layers[1].path.as_deref(), Some(ws.as_path()));
    // Merging the layers as read gives the policy's format; replacing the
    // project layer by itself changes nothing.
    assert_eq!(merge_format(&cfg.format_layers), cfg.policy.format);
    let same = cfg.format_layers_with_project(&cfg.format_layers[1].format);
    assert_eq!(same, cfg.format_layers);
}

#[test]
fn sources_say_which_layer_set_each_key() {
    let root = tmp("sources");
    write(&root, "pack.toml", PACK);
    let ws = write(&root, "ws/sqldiff.toml", WORKSPACE);
    let cfg = load(&root, Some(&ws));
    let (pack, project) = (Some(0), Some(1));
    for (d, indent, case) in [
        (Dialect::Tsql, (pack, false), (project, false)),
        // The pack's [format.postgres] outranks the project's [format].
        (Dialect::Postgres, (pack, true), (project, false)),
    ] {
        let s = format_sources(&cfg.format_layers, Some(d));
        assert_eq!((s["indent"].layer, s["indent"].dialect_table), indent);
        assert_eq!(
            (s["keyword_case"].layer, s["keyword_case"].dialect_table),
            case
        );
        assert_eq!(s["final_newline"].layer, None);
    }
    // Without a dialect only [format] counts.
    let s = format_sources(&cfg.format_layers, None);
    assert_eq!(
        (s["indent"].layer, s["indent"].dialect_table),
        (pack, false)
    );
    assert_eq!(s["indent_style"].layer, None);
    let s = format_sources(&cfg.format_layers, Some(Dialect::Tsql));
    assert_eq!(
        (s["indent_style"].layer, s["indent_style"].dialect_table),
        (project, true)
    );
    // Every key's source holds the value the dialect's options end up with.
    for d in Dialect::ALL {
        let options =
            serde_json::to_value(FormatKeys::from_options(&cfg.policy.format.options(d))).unwrap();
        for (key, src) in format_sources(&cfg.format_layers, Some(d)) {
            let Some(i) = src.layer else { continue };
            let f = &cfg.format_layers[i].format;
            let own = match d {
                Dialect::Tsql => &f.tsql,
                Dialect::Postgres => &f.postgres,
                Dialect::Db2i => &f.db2i,
            };
            let keys = if src.dialect_table { own } else { &f.all };
            assert_eq!(
                serde_json::to_value(keys).unwrap()[&key],
                options[&key],
                "{d:?} {key}"
            );
        }
    }
}

#[test]
fn a_draft_replaces_the_project_layer_or_becomes_one() {
    let root = tmp("draft");
    write(&root, "pack.toml", PACK);
    let ws = write(&root, "ws/sqldiff.toml", WORKSPACE);
    let draft = parse("[format]\nindent = 8\n");
    let with = merge_format(&load(&root, Some(&ws)).format_layers_with_project(&draft));
    // The pack's indent is overridden, the workspace's keyword_case is gone.
    assert_eq!(with.all.indent, Some(8));
    assert_eq!(with.all.keyword_case, Some(KeywordCase::Lower));
    assert_eq!(with.tsql, FormatKeys::default());
    // With no project config read, the draft goes above the packs.
    let without = Resolved::load_without_search(&Sources {
        isolated: true,
        cwd: Some(root.join("ws")),
        packs: vec![root.join("pack.toml")],
        ..Sources::default()
    })
    .unwrap();
    assert!(without
        .format_layers
        .iter()
        .all(|l| l.kind != LayerKind::Project));
    let with = merge_format(&without.format_layers_with_project(&draft));
    assert_eq!(with.all.indent, Some(8));
    assert_eq!(with.postgres.indent, Some(3));
}

#[test]
fn no_search_ignores_a_config_above() {
    let root = tmp("nosearch");
    write(&root, "sqldiff.toml", WORKSPACE);
    let sources = Sources {
        cwd: Some(root.join("deeper")),
        ..Sources::default()
    };
    std::fs::create_dir_all(root.join("deeper")).unwrap();
    // Not isolated, so the user config is read if there is one; only look at
    // the project layer.
    let found = |r: &Resolved| r.format_layers.iter().any(|l| l.kind == LayerKind::Project);
    assert!(found(&Resolved::load(&sources).unwrap()));
    assert!(!found(&Resolved::load_without_search(&sources).unwrap()));
}

const COMMENTED: &str = "# Workspace settings, edit with care.
excerpt_lines = 5   # lines around a finding
ignore = [\"vendor/**\"]

[format]
# Layout of `sqldiff fmt`.
indent = 4          # spaces per level
keyword_case = \"upper\"
join_lines = true   # off by default

[format.tsql]
indent_style = \"tabs\"
";

fn draft(text: &str) -> FormatConfig {
    parse(text)
}

#[test]
fn saving_keeps_comments_and_other_keys() {
    let mut f = draft(COMMENTED);
    f.all.indent = Some(2);
    f.all.keyword_case = None;
    f.all.max_blank_lines = Some(3);
    f.tsql = FormatKeys::default();
    f.postgres.keyword_case = Some(KeywordCase::Lower);
    let out = set_format(COMMENTED, &f, false).unwrap();
    for kept in [
        "# Workspace settings, edit with care.\n",
        "excerpt_lines = 5   # lines around a finding\n",
        "ignore = [\"vendor/**\"]\n",
        "# Layout of `sqldiff fmt`.\n",
        "join_lines = true   # off by default\n",
    ] {
        assert!(out.contains(kept), "{kept:?} lost:\n{out}");
    }
    // A changed value keeps the comment after it; a removed key is gone.
    assert!(
        out.contains("indent = 2          # spaces per level\n"),
        "{out}"
    );
    assert!(!out.contains("keyword_case = \"upper\""), "{out}");
    assert!(!out.contains("indent_style"), "{out}");
    assert!(out.contains("max_blank_lines = 3"), "{out}");
    assert!(
        out.contains("[format.postgres]\nkeyword_case = \"lower\"\n"),
        "{out}"
    );
    assert_eq!(parse(&out), f);
    // The non-format keys still read the same.
    let layer = sqldiff_source::config::ConfigLayer::parse(&out).unwrap();
    assert_eq!(layer.file.excerpt_lines, Some(5));
    assert_eq!(layer.policy.ignore, ["vendor/**"]);
}

#[test]
fn saving_what_is_there_changes_nothing() {
    let out = set_format(COMMENTED, &parse(COMMENTED), false).unwrap();
    assert_eq!(out, COMMENTED);
}

#[test]
fn saving_into_crlf_keeps_crlf() {
    let crlf = COMMENTED.replace('\n', "\r\n");
    let mut f = draft(&crlf);
    f.all.indent = Some(2);
    f.all.space_after_comma = Some(false);
    f.db2i.indent_style = Some(IndentStyle::Tabs);
    let out = set_format(&crlf, &f, false).unwrap();
    assert!(
        !out.replace("\r\n", "").contains('\n'),
        "a bare LF:\n{out:?}"
    );
    assert!(out.contains("# Layout of `sqldiff fmt`.\r\n"));
    assert!(out.contains("indent = 2          # spaces per level\r\n"));
    assert!(
        out.contains("[format.db2i]\r\nindent_style = \"tabs\"\r\n"),
        "{out:?}"
    );
    assert_eq!(parse(&out), f);
    // LF stays LF.
    let out = set_format(COMMENTED, &f, true).unwrap();
    assert!(!out.contains('\r'));
}

#[test]
fn saving_keeps_a_byte_order_mark() {
    let text = format!("\u{feff}{COMMENTED}");
    let mut f = draft(COMMENTED);
    f.all.line_ending = Some(LineEnding::Crlf);
    let out = set_format(&text, &f, false).unwrap();
    assert!(out.starts_with('\u{feff}'));
    assert!(out.contains("line_ending = \"crlf\""));
    assert_eq!(parse(out.trim_start_matches('\u{feff}')), f);
}

#[test]
fn saving_into_a_new_file() {
    let mut f = FormatConfig::default();
    f.all.indent = Some(2);
    f.postgres.keyword_case = Some(KeywordCase::Lower);
    assert_eq!(
        set_format("", &f, false).unwrap(),
        "[format]\nindent = 2\n\n[format.postgres]\nkeyword_case = \"lower\"\n"
    );
    let crlf = set_format("", &f, true).unwrap();
    assert!(crlf.contains("\r\n") && !crlf.replace("\r\n", "").contains('\n'));
    // Only a dialect table: no empty [format] header.
    let mut g = FormatConfig::default();
    g.tsql.indent = Some(3);
    assert_eq!(
        set_format("", &g, false).unwrap(),
        "[format.tsql]\nindent = 3\n"
    );
    // Nothing to write, nothing written.
    assert_eq!(set_format("", &FormatConfig::default(), false).unwrap(), "");
    // Added after other content that lacks a final newline.
    let out = set_format("excerpt_lines = 4", &f, false).unwrap();
    assert!(out.starts_with("excerpt_lines = 4\n"), "{out:?}");
    assert_eq!(parse(&out), f);
}

#[test]
fn saving_into_an_inline_table() {
    let text = "format = { indent = 2, keyword_case = \"lower\" }\n";
    let mut f = draft(text);
    f.all.indent = Some(6);
    let out = set_format(text, &f, false).unwrap();
    assert_eq!(parse(&out), f);
}

#[test]
fn saving_refuses_what_would_not_read_back() {
    let mut f = FormatConfig::default();
    f.all.indent = Some(99);
    assert!(set_format("", &f, false).unwrap_err().0.contains("indent"));
    // A file that is not TOML is not touched.
    assert!(set_format("[format\nindent = 2", &FormatConfig::default(), false).is_err());
    // [format] that is not a table.
    let e = set_format("format = 3\n", &draft("[format]\nindent = 2\n"), false).unwrap_err();
    assert!(e.0.contains("format"), "{e}");
}
