//! Statement-level conversion with the rules: surface terms of the source,
//! lift and emit, then print in the target. Used by the live conversion
//! view for the statements its token rules cannot handle (idioms), and by
//! the rule examples' tests.
//!
//! Conversion never checks itself: the caller diffs the output against the
//! input with the engine, whose equivalence lowering (`equiv`) does not use
//! these rules, so a wrong rule shows up as a difference instead of being
//! proved by itself.

use std::ops::Range;

use sqldiff_syntax::{ast as cst, Dialect, SyntaxNode};

use super::print::Printer;
use super::rewrite::{Applied, Engine, Facts, Log, NoFacts};
use super::rule::{RuleSet, Status};
use super::surface;
use crate::equiv::term::Term;

/// One converted run of source units.
#[derive(Debug, Clone)]
pub struct Piece {
    /// Indices of the source units it covers.
    pub units: Range<usize>,
    /// Byte range in the source text.
    pub src: (usize, usize),
    /// The target SQL, or why the run is not converted.
    pub output: Result<String, String>,
    pub status: Status,
    pub notes: Vec<Applied>,
    /// (assumption id, text).
    pub assumptions: Vec<(String, String)>,
    pub discharged: Vec<String>,
    /// Ids of the rules that matched.
    pub rules: Vec<String>,
}

pub struct Options<'a> {
    pub rules: &'a RuleSet,
    pub facts: &'a dyn Facts,
    /// Spelling of a source variable in the target.
    pub vars: &'a dyn Fn(&str) -> String,
}

/// `@CustomerId` → `v_customer_id`.
pub fn default_var(v: &str) -> String {
    let name = v.trim_start_matches('@');
    let mut out = String::from("v_");
    let mut prev_lower = false;
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            if prev_lower {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
            prev_lower = false;
        } else {
            out.push(c);
            prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        }
    }
    out
}

impl Default for Options<'static> {
    fn default() -> Self {
        Options {
            rules: RuleSet::builtin(),
            facts: &NoFacts,
            vars: &default_var,
        }
    }
}

/// Converts a list of source units (statements, IF, blocks).
pub fn convert_nodes(nodes: &[SyntaxNode], from: Dialect, to: Dialect, o: &Options) -> Vec<Piece> {
    let units = surface::units(nodes, from);
    let engine = Engine::new(o.rules, o.facts);
    let printer = Printer::new(to, o.vars, o.rules.funcs.get(&to));
    let terms: Vec<Term> = units.iter().map(|u| u.term.clone()).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < units.len() {
        let mut log = Log::default();
        let (len, res) = match engine.lift_seq(&terms[i..], from, &mut log) {
            Ok(Some((len, c))) => (
                len,
                engine
                    .emit_once(&c, to, &mut log)
                    .and_then(|e| {
                        e.ok_or_else(|| {
                            format!("no rule spells `{}` in {}", c.head(), to.display_name())
                        })
                    })
                    .and_then(|e| engine.target(&e, from, to, &mut log)),
            ),
            Ok(None) => (1, engine.convert(&terms[i], from, to, &mut log)),
            Err(e) => (1, Err(e)),
        };
        let printed = res.and_then(|t| {
            let items: Vec<Term> = if t.is("seq") {
                t.kids().to_vec()
            } else {
                vec![t]
            };
            items
                .iter()
                .map(|x| printer.stmt(x, 0))
                .collect::<Result<Vec<_>, _>>()
                .map(|v| v.join("\n"))
        });
        let printed = printed.map_err(|e| match &units[i].problem {
            Some(p) if len == 1 => format!("{e} (parse: {p})"),
            _ => e,
        });
        let start = cst::sig_range(&units[i].node).0;
        let end = cst::sig_range(&units[i + len - 1].node).1;
        out.push(Piece {
            units: i..i + len,
            src: (start, end),
            status: if printed.is_err() {
                Status::NotConverted
            } else {
                log.status()
            },
            output: printed,
            notes: log.applied,
            assumptions: log.assumptions,
            discharged: log.discharged,
            rules: log.rules,
        });
        i += len;
    }
    out
}

/// Converts the statements of a text (routine bodies are opened; routine
/// headers are the conversion view's job). Not-converted runs are kept as
/// comments.
pub fn convert_text(text: &str, from: Dialect, to: Dialect, o: &Options) -> (String, Vec<Piece>) {
    let root = sqldiff_syntax::parse(text, from).syntax();
    let nodes = surface::body_units(&root);
    let pieces = convert_nodes(&nodes, from, to, o);
    let mut out = Vec::new();
    for p in &pieces {
        match &p.output {
            Ok(s) => out.push(s.clone()),
            Err(e) => {
                let src = &text[p.src.0..p.src.1];
                let mut s = format!("-- not converted: {e}\n");
                for l in src.lines() {
                    s += &format!("-- {l}\n");
                }
                out.push(s.trim_end().to_string());
            }
        }
    }
    (out.join("\n"), pieces)
}
