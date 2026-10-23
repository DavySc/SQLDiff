//! Single-artifact lints: residue from other dialects and parse hazards.

use std::sync::OnceLock;

use serde::Deserialize;
use sqldiff_syntax::{ast, Dialect, SyntaxKind, SyntaxNode, SyntaxToken};

use crate::artifact::Analyzed;
use crate::model::{fingerprint, ChangeClass, Finding, FingerprintInput, Severity, Side, Verdict};

#[derive(Debug, Deserialize)]
struct RuleFile {
    rule: Vec<ResidueRule>,
}

#[derive(Debug, Deserialize)]
pub struct ResidueRule {
    pub id: String,
    pub origin: String,
    pub targets: Vec<String>,
    pub message: String,
    pub pattern: Vec<String>,
}

const BUILTIN_RESIDUE: &str = include_str!("../profiles/residue.toml");

pub fn residue_rules() -> &'static [ResidueRule] {
    static RULES: OnceLock<Vec<ResidueRule>> = OnceLock::new();
    RULES.get_or_init(|| {
        toml::from_str::<RuleFile>(BUILTIN_RESIDUE)
            .expect("built-in residue rules are valid")
            .rule
    })
}

struct Matcher {
    touch: bool,
    test: Test,
}

enum Test {
    Word(Vec<String>),
    Text(Vec<String>),
    Prefix(String),
    Kind(Vec<String>),
}

fn compile(item: &str) -> Matcher {
    let (touch, item) = match item.strip_prefix('+') {
        Some(rest) => (true, rest),
        None => (false, item),
    };
    let (kind, arg) = item.split_once(':').unwrap_or(("text", item));
    let alts = |s: &str| s.split('|').map(str::to_string).collect::<Vec<_>>();
    let test = match kind {
        "word" => Test::Word(
            alts(arg)
                .into_iter()
                .map(|s| s.to_ascii_uppercase())
                .collect(),
        ),
        "prefix" => Test::Prefix(arg.to_ascii_uppercase()),
        "kind" => Test::Kind(alts(arg)),
        // `text:@|@@` lists alternatives; a lone `|` is a literal.
        _ if arg.len() > 1 && arg.contains('|') => Test::Text(alts(arg)),
        _ => Test::Text(vec![arg.to_string()]),
    };
    Matcher { touch, test }
}

impl Test {
    fn matches(&self, t: &SyntaxToken) -> bool {
        match self {
            Test::Word(ws) => {
                t.kind() == SyntaxKind::IDENT && ws.contains(&t.text().to_ascii_uppercase())
            }
            Test::Text(xs) => xs.iter().any(|x| x == t.text()),
            Test::Prefix(p) => t.text().to_ascii_uppercase().starts_with(p.as_str()),
            Test::Kind(ks) => ks.iter().any(|k| format!("{:?}", t.kind()) == *k),
        }
    }
}

fn touches(prev: &SyntaxToken, next: &SyntaxToken) -> bool {
    prev.text_range().end() == next.text_range().start()
}

/// A lint hit, before it becomes a finding.
pub struct Hit {
    pub rule: String,
    pub class: ChangeClass,
    pub severity: Severity,
    pub message: String,
    pub start: usize,
    pub end: usize,
    /// Normalized matched text, used for fingerprints and old-vs-new comparison.
    pub key: String,
}

pub fn residue_hits(root: &SyntaxNode, dialect: Dialect) -> Vec<Hit> {
    let toks: Vec<SyntaxToken> = ast::sig_tokens(root).collect();
    let mut hits = Vec::new();
    for rule in residue_rules() {
        if !rule.targets.iter().any(|t| t == dialect.id()) {
            continue;
        }
        let ms: Vec<Matcher> = rule.pattern.iter().map(|p| compile(p)).collect();
        let mut i = 0;
        while i + ms.len() <= toks.len() {
            let ok = ms.iter().enumerate().all(|(k, m)| {
                let t = &toks[i + k];
                m.test.matches(t) && (!m.touch || k == 0 || touches(&toks[i + k - 1], t))
            });
            if ok {
                let last = &toks[i + ms.len() - 1];
                let text: Vec<&str> = toks[i..i + ms.len()].iter().map(|t| t.text()).collect();
                let origin: Dialect = rule.origin.parse().unwrap_or(Dialect::Tsql);
                hits.push(Hit {
                    rule: format!("residue.{}", rule.id),
                    class: ChangeClass::Residue,
                    severity: Severity::Medium,
                    message: format!(
                        "{} (from {}) `{}` in a {} artifact.",
                        rule.message,
                        origin.display_name(),
                        text.join(" "),
                        dialect.display_name()
                    ),
                    start: toks[i].text_range().start().into(),
                    end: last.text_range().end().into(),
                    key: text.join(" ").to_ascii_uppercase(),
                });
                i += ms.len();
            } else {
                i += 1;
            }
        }
    }
    hits
}

/// T-SQL: a statement without `;` directly before THROW.
pub fn terminator_hits(root: &SyntaxNode, dialect: Dialect) -> Vec<Hit> {
    if dialect != Dialect::Tsql {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for node in root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::STATEMENT)
    {
        if !ast::starts_with_kw(&node, "THROW") {
            continue;
        }
        let Some(prev) = node.prev_sibling() else {
            continue;
        };
        let prev_stmt = last_statement(&prev);
        let Some(prev_stmt) = prev_stmt else { continue };
        if !ast::ends_with_inferred_terminator(&prev_stmt) {
            continue;
        }
        let prev_words = ast::first_words(&prev_stmt, 2);
        let bare_throw = ast::sig_tokens(&node)
            .filter(|t| t.kind() != SyntaxKind::SEMICOLON)
            .count()
            == 1;
        let txn = matches!(
            prev_words.first().map(String::as_str),
            Some("ROLLBACK" | "COMMIT" | "SAVE" | "BEGIN")
        );
        let message = if bare_throw && txn {
            format!(
                "`{}` is not terminated before THROW: SQL Server reads THROW as the transaction or savepoint name, so the error is not re-raised.",
                prev_words.join(" ")
            )
        } else {
            "The statement before THROW is not terminated with `;`; SQL Server requires it, and the THROW may be read as part of the previous statement.".to_string()
        };
        let (s, _) = ast::sig_range(&prev_stmt);
        let (_, e) = ast::sig_range(&node);
        hits.push(Hit {
            rule: "hazard.terminator-before-throw".to_string(),
            class: ChangeClass::Semantic,
            severity: Severity::High,
            message,
            start: s,
            end: e,
            key: format!("{} THROW", prev_words.join(" ")),
        });
    }
    hits
}

fn last_statement(node: &SyntaxNode) -> Option<SyntaxNode> {
    if node.kind() == SyntaxKind::STATEMENT {
        return Some(node.clone());
    }
    // `IF @@TRANCOUNT > 0 ROLLBACK` followed by THROW: the ROLLBACK is the last statement.
    node.descendants()
        .filter(|n| n.kind() == SyntaxKind::STATEMENT)
        .last()
}

pub fn all_hits(a: &Analyzed) -> Vec<Hit> {
    let mut hits = residue_hits(&a.root, a.dialect());
    hits.extend(terminator_hits(&a.root, a.dialect()));
    hits.sort_by_key(|h| h.start);
    hits
}

pub fn to_finding(a: &Analyzed, h: &Hit) -> Finding {
    let loc = a.location(h.start, h.end);
    let (line, _) = a.lines.line_col(h.start);
    Finding {
        fingerprint: fingerprint(&FingerprintInput {
            object: &a.object,
            class: h.class,
            rule: &h.rule,
            anchor_a: if a.side == Side::A { &h.key } else { "" },
            anchor_b: if a.side == Side::B { &h.key } else { "" },
        }),
        rule: h.rule.clone(),
        class: h.class,
        severity: h.severity,
        verdict: Verdict::Unknown,
        witness: format!("Side {:?} line {line}: {}", a.side, h.message),
        a: (a.side == Side::A).then(|| loc.clone()),
        b: (a.side == Side::B).then_some(loc),
        assumptions: Vec::new(),
    }
}
