//! The converter's rule trace (docs/verified-conversion.md, C0 item 3):
//! every rewrite leaves a step naming its rule and the input it rewrote,
//! including rewrites that need no assumption.

use std::path::{Path, PathBuf};

use sqldiff_convert::{convert, trace_text, Conversion, Status, Step, Unit};
use sqldiff_core::Dialect;

fn pg(text: &str) -> Conversion {
    convert(text, Dialect::Tsql, Dialect::Postgres).unwrap()
}

fn db2(text: &str) -> Conversion {
    convert(text, Dialect::Tsql, Dialect::Db2i).unwrap()
}

/// The steps of rule `rule` in the conversion, with the input they name.
fn steps<'a>(c: &'a Conversion, input: &'a str, rule: &str) -> Vec<(&'a Step, &'a str)> {
    c.units
        .iter()
        .flat_map(|u| &u.trace)
        .filter(|s| s.rule == rule)
        .map(|s| (s, &input[s.start..s.end]))
        .collect()
}

/// The smallest unit whose input contains `needle`.
fn unit<'a>(c: &'a Conversion, input: &str, needle: &str) -> &'a Unit {
    let at = input
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} not in input"));
    c.units
        .iter()
        .filter(|u| u.src.start <= at && at < u.src.end.max(u.src.start + 1))
        .min_by_key(|u| u.src.end - u.src.start)
        .unwrap_or_else(|| panic!("no unit for {needle}"))
}

/// What is wrong with the trace of a conversion of `input`: a unit the rules
/// rewrote without a step, a step outside its unit's input, an id that is
/// not a rule id, or the rules stopping on an internal error (which a debug
/// build's own completeness check causes).
fn trace_problems(name: &str, input: &str, c: &Conversion) -> Vec<String> {
    let mut out = Vec::new();
    for u in &c.units {
        if u.notes.iter().any(|n| n.rule == "internal-error") {
            out.push(format!("{name}: internal error: {}", u.notes[0].text));
            continue;
        }
        let a = &input[u.src.start..u.src.end];
        let b = &c.output[u.out.start..u.out.end];
        if trace_text(a) != trace_text(b) && u.trace.is_empty() {
            out.push(format!("{name}: no step for {a:?} → {b:?}"));
        }
        for s in &u.trace {
            if s.start > s.end || s.start < u.src.start || s.end > u.src.end {
                out.push(format!(
                    "{name}: step {} at {}..{} outside its unit {}..{}",
                    s.rule, s.start, s.end, u.src.start, u.src.end
                ));
            }
            if s.rule.is_empty() || s.rule.contains(char::is_whitespace) {
                out.push(format!("{name}: bad rule id {:?}", s.rule));
            }
        }
    }
    out
}

fn tsql_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            tsql_files(&p, out);
            continue;
        }
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let tsql = name.contains("tsql") || name.contains("mssql") || name == "dateformat.sql";
        if tsql && name.ends_with(".sql") {
            out.push(p);
        }
    }
}

#[test]
fn every_rewrite_in_the_corpus_leaves_a_step() {
    let root = PathBuf::from(format!("{}/../..", env!("CARGO_MANIFEST_DIR")));
    let mut files = Vec::new();
    for d in ["corpus", "tests/fixtures", "eval"] {
        tsql_files(&root.join(d), &mut files);
    }
    assert!(files.len() >= 40, "{} T-SQL files found", files.len());
    let (mut problems, mut steps, mut rewritten) = (Vec::new(), 0, 0);
    for f in &files {
        let input = std::fs::read_to_string(f).unwrap();
        let name = f.strip_prefix(&root).unwrap_or(f).display().to_string();
        for (to, c) in [("pg", pg(&input)), ("db2i", db2(&input))] {
            problems.extend(trace_problems(&format!("{name} → {to}"), &input, &c));
            steps += c.units.iter().map(|u| u.trace.len()).sum::<usize>();
            rewritten += c
                .units
                .iter()
                .filter(|u| {
                    trace_text(&input[u.src.start..u.src.end])
                        != trace_text(&c.output[u.out.start..u.out.end])
                })
                .count();
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    assert!(
        rewritten > 100 && steps > rewritten,
        "{rewritten} units, {steps} steps"
    );
}

#[test]
fn a_rewrite_without_an_assumption_leaves_a_step() {
    let src = "SELECT LEN(name), NEWID() FROM t;";
    let c = pg(src);
    assert_eq!(
        c.output.trim(),
        "SELECT length(rtrim(name)), gen_random_uuid() FROM t;"
    );
    let len = steps(&c, src, "tsql.func.len");
    assert_eq!(len.len(), 1, "{:?}", c.units[0].trace);
    assert_eq!(len[0].1, "LEN(name)");
    let newid = steps(&c, src, "tsql.func.newid");
    assert_eq!(newid.len(), 1, "{:?}", c.units[0].trace);
    assert_eq!(newid[0].1, "NEWID()");
    // No note of the rules (the engine's own may follow): steps decide no status.
    let rules: Vec<_> = c.units[0]
        .notes
        .iter()
        .filter(|n| n.status != Status::Unproven)
        .collect();
    assert!(rules.is_empty(), "{rules:?}");
}

#[test]
fn a_rule_with_an_assumption_leaves_its_note_and_a_step() {
    let src = "SELECT ISNULL(a, 0) FROM t;";
    for c in [pg(src), db2(src)] {
        let u = &c.units[0];
        assert!(
            u.notes.iter().any(|n| n.rule == "ASM-ISNULL-TYPE"),
            "{:?}",
            u.notes
        );
        let s = steps(&c, src, "tsql.func.isnull");
        assert_eq!(s.len(), 1, "{:?}", u.trace);
        assert_eq!(s[0].1, "ISNULL(a, 0)");
    }
}

#[test]
fn statement_rules_name_what_they_rewrote() {
    let src = "CREATE PROCEDURE dbo.p @Qty int AS\nBEGIN\n  SET NOCOUNT ON;\n  UPDATE t SET a = 1 WHERE b = @Qty;\nEND\n";
    let c = pg(src);
    let set = steps(&c, src, "tsql.stmt.set-option");
    assert_eq!(set.len(), 1, "{:#?}", c.units);
    assert_eq!(set[0].1, "SET NOCOUNT ON");
    assert_eq!(set[0].0.inst.as_deref(), Some("NOCOUNT ON"));
    // The parameter's new name, in the header and where the body reads it.
    let param = steps(&c, src, "tsql.routine.param");
    assert_eq!(param.len(), 1);
    assert_eq!(param[0].1, "@Qty int");
    assert_eq!(param[0].0.inst.as_deref(), Some("p_qty integer"));
    let var = steps(&c, src, "tsql.expr.variable");
    assert_eq!(var.len(), 1);
    assert_eq!(var[0].1, "@Qty");
    assert_eq!(var[0].0.inst.as_deref(), Some("p_qty"));
    let ty = steps(&c, src, "tsql.type.int");
    assert_eq!(ty.len(), 1);
    assert_eq!(ty[0].0.inst.as_deref(), Some("integer"));
    // The header unit names the routine rule and the dropped schema.
    let header = unit(&c, src, "CREATE PROCEDURE");
    let rules: Vec<&str> = header.trace.iter().map(|s| s.rule.as_str()).collect();
    assert!(rules.contains(&"tsql.routine.procedure"), "{rules:?}");
    assert!(rules.contains(&"tsql.expr.dbo-schema"), "{rules:?}");
}

#[test]
fn top_becomes_a_row_limit_clause() {
    let src = "SELECT TOP 5 a FROM t ORDER BY a;";
    for (c, inst) in [(pg(src), "LIMIT 5"), (db2(src), "FETCH FIRST 5 ROWS ONLY")] {
        let top = steps(&c, src, "tsql.select.top");
        assert_eq!(top.len(), 1, "{:#?}", c.units);
        assert_eq!(top[0].1, "TOP 5");
        assert_eq!(top[0].0.inst.as_deref(), Some(inst));
    }
}

#[test]
fn made_up_tokens_name_the_rule_that_made_them() {
    // Db2 for i: a SELECT without FROM reads SYSIBM.SYSDUMMY1.
    let src = "DECLARE @n int;\nSELECT @n = 1 + 2;\nIF EXISTS (SELECT 1) SET @n = 0;\n";
    let c = db2(src);
    let dummy = steps(&c, src, "tsql.select.dummy-from");
    assert!(!dummy.is_empty(), "{}\n{:#?}", c.output, c.units);
    assert!(c.output.contains("SYSIBM.SYSDUMMY1"), "{}", c.output);
    // Empty spans, at the place the clause was added.
    assert!(dummy
        .iter()
        .all(|(s, text)| s.start == s.end && text.is_empty()));
}

#[test]
fn a_refused_unit_names_the_refusal() {
    let src = "MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN UPDATE SET a = s.a;";
    let c = pg(src);
    let u = &c.units[0];
    assert_eq!(u.status, Status::NotConverted);
    assert_eq!(u.trace.len(), 1);
    assert_eq!(u.trace[0].rule, u.notes[0].rule);
    assert_eq!((u.trace[0].start, u.trace[0].end), (u.src.start, u.src.end));
}

#[test]
fn the_trace_is_serialized_with_the_unit() {
    let src = "SELECT LEN(name) FROM t;";
    let c = pg(src);
    let v = serde_json::to_value(&c.units[0]).unwrap();
    assert_eq!(v["trace"][0]["rule"], "tsql.func.len");
    assert_eq!(v["trace"][0]["start"], 7);
    assert_eq!(v["trace"][0]["end"], 16);
    // No instantiation: the field is left out.
    assert!(v["trace"][0].get("inst").is_none(), "{v}");
}
