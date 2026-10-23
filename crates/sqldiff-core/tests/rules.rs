//! Rule set gates (design section 8, M6): the shipped rules load, are
//! confluent, convert their examples, round-trip where they are two-way,
//! and every conversion is checked by the equivalence engine, which does not
//! use the rules (so a rule can never prove itself).

use std::path::{Path, PathBuf};

use sqldiff_core::equiv::schema::Schema;
use sqldiff_core::rules::convert::{convert_text, Options};
use sqldiff_core::rules::rewrite::overlaps;
use sqldiff_core::rules::rule::{Example, Kind, Rule, RuleSet};
use sqldiff_core::{diff, Artifact, Dialect, Verdict};

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus")
}

fn catalog(d: Dialect) -> Artifact {
    let file = match d {
        Dialect::Tsql => "tsql.sql",
        Dialect::Postgres => "pg.sql",
        Dialect::Db2i => "db2i.sql",
    };
    let path = corpus().join("catalog").join(file);
    Artifact::new(
        path.display().to_string(),
        d,
        std::fs::read_to_string(&path).unwrap(),
    )
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `@OrderId` → `p_order_id` (example variables are procedure parameters).
fn param(v: &str) -> String {
    format!("p{}", &sqldiff_core::rules::convert::default_var(v)[1..])
}

fn pg_type(t: &str) -> String {
    let l = t.to_lowercase();
    if l == "int" {
        "integer".into()
    } else if let Some(rest) = l.strip_prefix("nvarchar") {
        format!("varchar{rest}")
    } else if let Some(rest) = l.strip_prefix("decimal") {
        format!("numeric{rest}")
    } else {
        l
    }
}

/// Wraps an example's statements in a routine (or a view for a lone query).
fn wrap(sql: &str, d: Dialect, ex: &Example) -> String {
    let query = ex.params.is_empty() && sql.trim_start().to_uppercase().starts_with("SELECT");
    match (d, query) {
        (Dialect::Tsql, true) => format!(
            "CREATE VIEW dbo.rule_example AS {}",
            sql.trim_end_matches(';')
        ),
        (Dialect::Tsql, false) => {
            let ps: Vec<String> = ex.params.iter().map(|(n, t)| format!("@{n} {t}")).collect();
            format!(
                "CREATE PROCEDURE dbo.rule_example {} AS\nBEGIN\n{sql}\nEND\nGO\n",
                ps.join(", ")
            )
        }
        (_, true) => format!("CREATE VIEW rule_example AS {}", sql.trim_end_matches(';')),
        (_, false) => {
            let ps: Vec<String> = ex
                .params
                .iter()
                .map(|(n, t)| format!("{} {}", param(&format!("@{n}")), pg_type(t)))
                .collect();
            format!(
                "CREATE PROCEDURE rule_example({})\nLANGUAGE plpgsql\nAS $$\nBEGIN\n{sql}\nEND;\n$$;\n",
                ps.join(", ")
            )
        }
    }
}

fn verdict(s: &str) -> Verdict {
    match s {
        "equal" => Verdict::Equal,
        "equal-under-assumptions" => Verdict::EqualUnderAssumptions,
        "different" => Verdict::Different,
        _ => Verdict::Unknown,
    }
}

fn examples() -> Vec<(&'static Rule, &'static Example)> {
    RuleSet::builtin()
        .rules
        .iter()
        .flat_map(|r| r.examples.iter().map(move |e| (r, e)))
        .collect()
}

#[test]
fn builtin_rules_load_and_are_confluent() {
    let rs = RuleSet::builtin();
    assert!(rs.rules.len() >= 20, "{} rules", rs.rules.len());
    let o = overlaps(rs);
    assert!(
        o.is_empty(),
        "rules that can both apply with the same priority (give one a priority):\n{}",
        o.iter()
            .map(|(a, b, w)| format!("{a} / {b} ({w})"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn every_idiom_and_translation_has_a_form_or_example_per_direction() {
    for r in &RuleSet::builtin().rules {
        if r.kind == Kind::Lint {
            continue;
        }
        assert!(
            r.forms.iter().any(|f| f.lift) || r.forms.iter().any(|f| f.emit),
            "{}",
            r.id
        );
    }
}

#[test]
fn examples_convert_and_the_engine_agrees() {
    let schema = Schema::load(&[catalog(Dialect::Tsql)]);
    let o = Options {
        rules: RuleSet::builtin(),
        facts: &schema,
        vars: &param,
    };
    let mut failures = Vec::new();
    let mut unverified = Vec::new();
    for (r, ex) in examples() {
        let (Some((Dialect::Tsql, src)), Some((Dialect::Postgres, want))) =
            (ex.sides.first(), ex.sides.get(1))
        else {
            continue;
        };
        let (out, pieces) = convert_text(src, Dialect::Tsql, Dialect::Postgres, &o);
        if let Some(p) = pieces.iter().find(|p| p.output.is_err()) {
            failures.push(format!("{}: not converted: {:?}", r.id, p.output));
            continue;
        }
        if !pieces.iter().any(|p| p.rules.contains(&r.id)) {
            failures.push(format!(
                "{}: the example did not use the rule (used {:?})",
                r.id,
                pieces
                    .iter()
                    .flat_map(|p| p.rules.clone())
                    .collect::<Vec<_>>()
            ));
        }
        if norm(&out) != norm(want) {
            failures.push(format!(
                "{}:\n  got  {}\n  want {}",
                r.id,
                norm(&out),
                norm(want)
            ));
            continue;
        }
        // Independent check: the engine's equivalence lowering, with the catalog.
        let a = Artifact::new(
            "example.tsql.sql",
            Dialect::Tsql,
            wrap(src, Dialect::Tsql, ex),
        );
        let b = Artifact::new(
            "example.pg.sql",
            Dialect::Postgres,
            wrap(&out, Dialect::Postgres, ex),
        );
        let rep = diff(
            &a,
            &b,
            &sqldiff_core::Options {
                catalog_a: vec![catalog(Dialect::Tsql)],
                catalog_b: vec![catalog(Dialect::Postgres)],
                ..Default::default()
            },
        );
        let expect = ex.expect.as_deref().map(verdict);
        match expect {
            Some(v) if v == rep.verdict => {}
            Some(v) => failures.push(format!(
                "{}: the engine says {} but the rule's example expects {}\n{}\n{}",
                r.id,
                rep.verdict.label(),
                v.label(),
                rep.findings
                    .iter()
                    .map(|f| f.witness.clone())
                    .collect::<Vec<_>>()
                    .join("\n"),
                rep.unknown_reasons.join("\n")
            )),
            None => failures.push(format!("{}: example without (expect ...)", r.id)),
        }
        if rep.verdict == Verdict::Unknown {
            unverified.push(r.id.clone());
        }
        // A rule that claims no difference must never meet a "different" from the engine.
        let claims_decision = pieces
            .iter()
            .any(|p| p.status >= sqldiff_core::rules::rule::Status::Decision);
        if rep.verdict == Verdict::Different && !claims_decision {
            failures.push(format!(
                "{}: the engine finds a difference the rule does not declare",
                r.id
            ));
        }
    }
    eprintln!("examples the engine cannot verify yet: {unverified:?}");
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn two_way_rules_round_trip() {
    // PostgreSQL → T-SQL → PostgreSQL is stable for rules whose forms both emit and lift.
    let back = Options {
        vars: &|v: &str| format!("@{v}"),
        ..Options::default()
    };
    let mut failures = Vec::new();
    for (r, ex) in examples() {
        let two_way = r.form(Dialect::Tsql).any(|f| f.lift && f.emit)
            && r.form(Dialect::Postgres).any(|f| f.lift && f.emit);
        if !two_way {
            continue;
        }
        let Some((_, pg)) = ex.sides.iter().find(|(d, _)| *d == Dialect::Postgres) else {
            continue;
        };
        let (tsql, pieces) = convert_text(pg, Dialect::Postgres, Dialect::Tsql, &back);
        if let Some(p) = pieces.iter().find(|p| p.output.is_err()) {
            failures.push(format!(
                "{}: PostgreSQL → T-SQL failed: {:?}",
                r.id, p.output
            ));
            continue;
        }
        let (again, _) = convert_text(
            &tsql,
            Dialect::Tsql,
            Dialect::Postgres,
            &Options {
                vars: &|v: &str| v.trim_start_matches('@').to_string(),
                ..Options::default()
            },
        );
        if norm(&again) != norm(pg) {
            failures.push(format!(
                "{}:\n  T-SQL {}\n  back  {}\n  want  {}",
                r.id,
                norm(&tsql),
                norm(&again),
                norm(pg)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn near_miss_names_the_differing_part() {
    let a = Artifact::new(
        "near.tsql.sql",
        Dialect::Tsql,
        "IF EXISTS (SELECT 1 FROM dbo.customer WHERE customer_id = @Id) UPDATE dbo.customer SET last_name = @Name WHERE customer_id = @Other; ELSE INSERT INTO dbo.customer (customer_id, last_name) VALUES (@Id, @Name);",
    );
    let r = sqldiff_core::check(&a, &Default::default());
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == "rules.idiom.near-miss/upsert/if-exists")
        .unwrap_or_else(|| panic!("{:#?}", r.findings));
    assert!(f.witness.contains("customer_id = @Id"), "{}", f.witness);
    assert!(f.witness.contains("customer_id = @Other"), "{}", f.witness);
}

#[test]
fn upsert_race_and_unrestricted_update_are_linted() {
    let a = Artifact::new(
        "race.tsql.sql",
        Dialect::Tsql,
        "MERGE dbo.item_price AS t USING (SELECT @Item AS item_id, @Price AS unit_price) AS s ON t.item_id = s.item_id WHEN MATCHED THEN UPDATE SET unit_price = s.unit_price WHEN NOT MATCHED THEN INSERT (item_id, unit_price) VALUES (s.item_id, s.unit_price);\nUPDATE p SET unit_price = (SELECT f.unit_price FROM dbo.price_feed f WHERE f.item_id = p.item_id) FROM dbo.item_price p;",
    );
    let r = sqldiff_core::check(&a, &Default::default());
    let rules: Vec<&str> = r.findings.iter().map(|f| f.rule.as_str()).collect();
    assert!(
        rules.contains(&"rules.concurrency/upsert-race"),
        "{rules:?}"
    );
    // UPDATE ... FROM is not the plain correlated form; only the race is reported.
    assert!(
        !rules.contains(&"rules.cardinality/correlated-update-unrestricted"),
        "{rules:?}"
    );
}

#[test]
fn pack_rules_load_next_to_the_builtin_ones() {
    let mut rs = RuleSet::builtin().clone();
    rs.load(
        r#"(translate pack/getdate (form tsql (call getdate)) (form postgres (call clock_timestamp)))
           (functions postgres clock_timestamp)"#,
        "pack.rules",
    )
    .unwrap();
    let o = Options {
        rules: &rs,
        facts: &sqldiff_core::rules::rewrite::NoFacts,
        vars: &param,
    };
    let (out, _) = convert_text(
        "SELECT GETDATE() AS t;",
        Dialect::Tsql,
        Dialect::Postgres,
        &o,
    );
    assert_eq!(out, "SELECT clock_timestamp() AS t;");
    assert!(rs
        .load(
            "(translate pack/bad (form tsql (call x ?a)) (form postgres (call y ?b)))",
            "bad"
        )
        .is_err());
}
