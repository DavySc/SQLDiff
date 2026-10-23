//! Rule files: parsing and validation.
//!
//! ```text
//! (idiom ID                       ; a canonical node with one form per dialect
//!   (doc "...") (priority N)
//!   (canon TEMPLATE)              ; built from the form's bindings when lifting,
//!                                 ; matched as a pattern when emitting
//!   (form DIALECT PATTERN         ; two-way unless (lift-only) or (emit-only)
//!         [(bind ?v VALUE)...]    ; canon variables the form fixes
//!         [(emit-any ?v)...]      ; ... but accepts any value of when emitting
//!         [(let ?v EXPR)...]      ; computed after matching the form (lifting)
//!         [(emit-let ?v EXPR)...] ; computed after matching the canon (emitting)
//!         [(lift-only)] [(emit-only)])
//!   (require [DIALECT] FACT ASM-ID "text")  ; catalog fact, else an assumption
//!   (assume [DIALECT] ASM-ID "text")
//!   (unless [DIALECT] TERM PATTERN)        ; skip when TERM (built from the
//!   (unless-contains [DIALECT] TERM PATTERN) ; bindings) or a subterm matches
//!   (note DIALECT STATUS "text" [(if ?v PATTERN)])
//!   (example (DIALECT "sql")... [(params (name type)...)] [(expect VERDICT)]))
//! (translate ID (form tsql P) (form postgres Q) ...)   ; an idiom whose canon is
//!                                                       ; implied: (~ID vars...)
//! (lint ID (form DIALECT|any P) (class C) [(severity S)] (message "...")
//!   [(diff-verdict different)])   ; new in the second version: the pair differs
//! `(if-contains [DIALECT] TERM PATTERN)` applies a rule only when TERM has a
//! matching subterm; `(if-conjunct [DIALECT] TERM PATTERN)` only when one of
//! TERM's top-level AND operands matches. `(?fact NAME args...)` in a `let` asks the catalog for
//! a value (`unknown` when it cannot say).
//! (functions DIALECT name...)     ; functions printable in that dialect
//! ```
//! A `seq` pattern, `(seq p q ...)`, matches consecutive statements.

use std::collections::{HashMap, HashSet};

use sqldiff_syntax::Dialect;

use super::pattern::vars;
use super::sexp;
use crate::equiv::term::{a, n, Term};
use crate::model::{ChangeClass, Severity};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Status {
    Converted,
    Assumptions,
    Decision,
    Differs,
    /// No rule (or no target spelling) for some part; nothing is emitted.
    NotConverted,
}

impl Status {
    pub fn parse(s: &str) -> Option<Status> {
        Some(match s {
            "converted" => Status::Converted,
            "assumptions" => Status::Assumptions,
            "decision" => Status::Decision,
            "differs" => Status::Differs,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Converted => "converted",
            Status::Assumptions => "assumptions",
            Status::Decision => "decision",
            Status::Differs => "differs",
            Status::NotConverted => "not converted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Idiom,
    Translate,
    Lint,
}

#[derive(Debug, Clone)]
pub struct Form {
    /// `None` for `any` (lints only).
    pub dialect: Option<Dialect>,
    pub pat: Term,
    pub binds: Vec<(String, Term)>,
    pub emit_any: Vec<String>,
    /// Computed bindings after matching the form (lifting).
    pub lets: Vec<(String, Term)>,
    /// Computed bindings after matching the canon (emitting).
    pub emit_lets: Vec<(String, Term)>,
    pub lift: bool,
    pub emit: bool,
}

#[derive(Debug, Clone)]
pub enum Cond {
    Require {
        fact: Term,
        id: String,
        text: String,
    },
    Assume {
        id: String,
        text: String,
    },
    /// Skip the rule when the instantiated term (or, with `anywhere`, any of its subterms) matches.
    Unless {
        term: Term,
        pat: Term,
        anywhere: bool,
    },
    /// Apply the rule only when the term or a subterm matches (with
    /// `conjunct`, only when one of its top-level AND operands matches).
    IfContains {
        term: Term,
        pat: Term,
        conjunct: bool,
    },
}

#[derive(Debug, Clone)]
pub struct Note {
    pub dialect: Dialect,
    pub status: Status,
    pub text: String,
    pub guard: Option<(String, Term)>,
}

#[derive(Debug, Clone)]
pub struct Example {
    pub sides: Vec<(Dialect, String)>,
    pub params: Vec<(String, String)>,
    pub expect: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LintSpec {
    /// In a same-dialect diff, a hit that only the new version has makes the pair different.
    pub diff_different: bool,
    pub class: ChangeClass,
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub id: String,
    pub kind: Kind,
    pub doc: String,
    pub priority: i32,
    pub canon: Option<Term>,
    pub forms: Vec<Form>,
    /// Conditions, each for one dialect's form or for all (`None`).
    pub conds: Vec<(Option<Dialect>, Cond)>,
    pub notes: Vec<Note>,
    pub examples: Vec<Example>,
    pub lint: Option<LintSpec>,
    /// Where the rule came from (built-in file or pack path).
    pub origin: String,
}

impl Rule {
    pub fn form(&self, d: Dialect) -> impl Iterator<Item = &Form> {
        self.forms.iter().filter(move |f| f.dialect == Some(d))
    }
}

#[derive(Debug, Clone, Default)]
pub struct RuleSet {
    pub rules: Vec<Rule>,
    pub funcs: HashMap<Dialect, HashSet<String>>,
}

pub fn dialect(s: &str) -> Result<Option<Dialect>, String> {
    match s {
        "any" => Ok(None),
        "pg" => Ok(Some(Dialect::Postgres)),
        _ => s
            .parse::<Dialect>()
            .map(Some)
            .map_err(|_| format!("unknown dialect `{s}`")),
    }
}

fn text(t: Option<&Term>, what: &str) -> Result<String, String> {
    t.and_then(Term::atom)
        .map(str::to_string)
        .ok_or_else(|| format!("{what}: expected a string"))
}

fn class(s: &str) -> Result<ChangeClass, String> {
    Ok(match s {
        "semantic" => ChangeClass::Semantic,
        "cardinality" => ChangeClass::Cardinality,
        "transactional" => ChangeClass::Transactional,
        "concurrency" => ChangeClass::Concurrency,
        "binding" => ChangeClass::Binding,
        "residue" => ChangeClass::Residue,
        "limit" => ChangeClass::Limit,
        "structural" => ChangeClass::Structural,
        _ => return Err(format!("unknown class `{s}`")),
    })
}

fn severity(s: &str) -> Result<Severity, String> {
    Ok(match s {
        "info" => Severity::Info,
        "low" => Severity::Low,
        "medium" => Severity::Medium,
        "high" => Severity::High,
        "critical" => Severity::Critical,
        _ => return Err(format!("unknown severity `{s}`")),
    })
}

/// Splits an optional leading dialect atom off a clause's arguments.
fn opt_dialect(k: &[Term]) -> (Option<Dialect>, &[Term]) {
    if let Some(Term::A(s)) = k.first() {
        if let Ok(Some(d)) = dialect(s) {
            if k.len() > 1 {
                return (Some(d), &k[1..]);
            }
        }
    }
    (None, k)
}

impl RuleSet {
    /// The shipped rules.
    pub fn builtin() -> &'static RuleSet {
        static RS: std::sync::OnceLock<RuleSet> = std::sync::OnceLock::new();
        RS.get_or_init(|| {
            let mut rs = RuleSet::default();
            for (name, src) in BUILTIN {
                rs.load(src, name).unwrap_or_else(|e| panic!("{name}: {e}"));
            }
            rs
        })
    }

    /// Adds the rules of one file (built-in or pack-supplied).
    pub fn load(&mut self, src: &str, origin: &str) -> Result<(), String> {
        for top in sexp::read_all(src)? {
            match top.head() {
                "functions" => {
                    let k = top.kids();
                    let d = dialect(k.first().and_then(Term::atom).unwrap_or(""))?
                        .ok_or("functions need a dialect")?;
                    let set = self.funcs.entry(d).or_default();
                    for f in &k[1..] {
                        set.insert(f.atom().ok_or("function names are atoms")?.to_lowercase());
                    }
                }
                "idiom" | "translate" | "lint" => {
                    let r = parse_rule(&top, origin)?;
                    if self.rules.iter().any(|x| x.id == r.id) {
                        return Err(format!("duplicate rule `{}`", r.id));
                    }
                    self.rules.push(r);
                }
                h => return Err(format!("unknown top-level form `{h}`")),
            }
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&Rule> {
        self.rules.iter().find(|r| r.id == id)
    }
}

pub const BUILTIN: &[(&str, &str)] = &[
    (
        "idioms.rules",
        include_str!("../../profiles/rules/idioms.rules"),
    ),
    (
        "tsql-pg.rules",
        include_str!("../../profiles/rules/tsql-pg.rules"),
    ),
    (
        "lints.rules",
        include_str!("../../profiles/rules/lints.rules"),
    ),
];

fn parse_rule(top: &Term, origin: &str) -> Result<Rule, String> {
    let kind = match top.head() {
        "idiom" => Kind::Idiom,
        "translate" => Kind::Translate,
        _ => Kind::Lint,
    };
    let id = text(top.kid(0), "rule id")?;
    let ctx = |e: String| format!("rule `{id}`: {e}");
    let mut r = Rule {
        id: id.clone(),
        kind,
        doc: String::new(),
        priority: 0,
        canon: None,
        forms: Vec::new(),
        conds: Vec::new(),
        notes: Vec::new(),
        examples: Vec::new(),
        lint: None,
        origin: origin.to_string(),
    };
    let mut lint_class = None;
    let mut lint_sev = None;
    let mut lint_msg = None;
    let mut diff_different = false;
    for c in &top.kids()[1..] {
        let k = c.kids();
        match c.head() {
            "doc" => r.doc = text(k.first(), "doc").map_err(ctx)?,
            "priority" => {
                r.priority = text(k.first(), "priority")
                    .and_then(|s| s.parse().map_err(|_| "priority: expected a number".into()))
                    .map_err(ctx)?
            }
            "canon" => {
                r.canon = Some(
                    k.first()
                        .cloned()
                        .ok_or_else(|| ctx("empty canon".into()))?,
                )
            }
            "form" => {
                let d = dialect(k.first().and_then(Term::atom).unwrap_or("")).map_err(ctx)?;
                let pat = k
                    .get(1)
                    .cloned()
                    .ok_or_else(|| ctx("form without a pattern".into()))?;
                let mut f = Form {
                    dialect: d,
                    pat,
                    binds: Vec::new(),
                    emit_any: Vec::new(),
                    lets: Vec::new(),
                    emit_lets: Vec::new(),
                    lift: true,
                    emit: true,
                };
                for o in &k[2..] {
                    match o.head() {
                        "bind" => f.binds.push((
                            text(o.kid(0), "bind").map_err(ctx)?,
                            o.kid(1)
                                .cloned()
                                .ok_or_else(|| ctx("bind without a value".into()))?,
                        )),
                        "let" | "emit-let" => {
                            let v = text(o.kid(0), "let").map_err(ctx)?;
                            let e = o
                                .kid(1)
                                .cloned()
                                .ok_or_else(|| ctx("let without a value".into()))?;
                            if o.is("let") {
                                f.lets.push((v, e));
                            } else {
                                f.emit_lets.push((v, e));
                            }
                        }
                        "emit-any" => f.emit_any.push(text(o.kid(0), "emit-any").map_err(ctx)?),
                        "lift-only" => f.emit = false,
                        "emit-only" => f.lift = false,
                        h => return Err(ctx(format!("unknown form option `{h}`"))),
                    }
                }
                if d.is_none() && kind != Kind::Lint {
                    return Err(ctx("only lints may use the `any` dialect".into()));
                }
                r.forms.push(f);
            }
            "require" => {
                let (d, k) = opt_dialect(k);
                r.conds.push((
                    d,
                    Cond::Require {
                        fact: k
                            .first()
                            .cloned()
                            .ok_or_else(|| ctx("require without a fact".into()))?,
                        id: text(k.get(1), "require id").map_err(ctx)?,
                        text: text(k.get(2), "require text").map_err(ctx)?,
                    },
                ));
            }
            "assume" => {
                let (d, k) = opt_dialect(k);
                r.conds.push((
                    d,
                    Cond::Assume {
                        id: text(k.first(), "assume id").map_err(ctx)?,
                        text: text(k.get(1), "assume text").map_err(ctx)?,
                    },
                ));
            }
            head @ ("if-contains" | "if-conjunct") => {
                let (d, k) = opt_dialect(k);
                if k.len() != 2 {
                    return Err(ctx(format!("{head}: ({head} [DIALECT] TERM PATTERN)")));
                }
                r.conds.push((
                    d,
                    Cond::IfContains {
                        term: k[0].clone(),
                        pat: k[1].clone(),
                        conjunct: head == "if-conjunct",
                    },
                ));
            }
            "unless" | "unless-contains" => {
                let (d, k) = opt_dialect(k);
                if k.len() != 2 {
                    return Err(ctx("unless: (unless [DIALECT] TERM PATTERN)".into()));
                }
                r.conds.push((
                    d,
                    Cond::Unless {
                        term: k[0].clone(),
                        pat: k[1].clone(),
                        anywhere: c.is("unless-contains"),
                    },
                ));
            }
            "note" => {
                let d = dialect(k.first().and_then(Term::atom).unwrap_or(""))
                    .map_err(ctx)?
                    .ok_or_else(|| ctx("note needs a dialect".into()))?;
                let status = text(k.get(1), "note status")
                    .ok()
                    .and_then(|s| Status::parse(&s))
                    .ok_or_else(|| {
                        ctx("note status: converted, assumptions, decision or differs".into())
                    })?;
                let mut note = Note {
                    dialect: d,
                    status,
                    text: text(k.get(2), "note text").map_err(ctx)?,
                    guard: None,
                };
                if let Some(g) = k.get(3) {
                    if !g.is("if") || g.kids().len() != 2 {
                        return Err(ctx("note guard: (if ?v PATTERN)".into()));
                    }
                    note.guard = Some((text(g.kid(0), "guard").map_err(ctx)?, g.kids()[1].clone()));
                }
                r.notes.push(note);
            }
            "example" => {
                let mut ex = Example {
                    sides: Vec::new(),
                    params: Vec::new(),
                    expect: None,
                };
                for s in k {
                    match s.head() {
                        "params" => {
                            for p in s.kids() {
                                ex.params.push((
                                    p.head().to_string(),
                                    text(p.kid(0), "param type").map_err(ctx)?,
                                ));
                            }
                        }
                        "expect" => ex.expect = Some(text(s.kid(0), "expect").map_err(ctx)?),
                        h => {
                            let d = dialect(h)
                                .map_err(ctx)?
                                .ok_or_else(|| ctx("example needs a dialect".into()))?;
                            ex.sides
                                .push((d, text(s.kid(0), "example sql").map_err(ctx)?));
                        }
                    }
                }
                r.examples.push(ex);
            }
            "class" => {
                lint_class = Some(class(&text(k.first(), "class").map_err(ctx)?).map_err(ctx)?)
            }
            "severity" => {
                lint_sev = Some(severity(&text(k.first(), "severity").map_err(ctx)?).map_err(ctx)?)
            }
            "diff-verdict" => {
                diff_different = text(k.first(), "diff-verdict").map_err(ctx)? == "different"
            }
            "message" => lint_msg = Some(text(k.first(), "message").map_err(ctx)?),
            h => return Err(ctx(format!("unknown clause `{h}`"))),
        }
    }
    if r.forms.is_empty() {
        return Err(ctx("no forms".into()));
    }
    match kind {
        Kind::Lint => {
            let class = lint_class.ok_or_else(|| ctx("lint without a class".into()))?;
            r.lint = Some(LintSpec {
                diff_different,
                class,
                severity: lint_sev.unwrap_or(class.default_severity()),
                message: lint_msg.ok_or_else(|| ctx("lint without a message".into()))?,
            });
        }
        Kind::Translate => {
            if r.canon.is_some() {
                return Err(ctx("translate rules have an implied canon".into()));
            }
            let mut vs: Vec<String> = Vec::new();
            vars(&r.forms[0].pat, &mut vs);
            vs.sort();
            for f in &r.forms[1..] {
                let mut w = Vec::new();
                vars(&f.pat, &mut w);
                w.sort();
                if w != vs {
                    return Err(ctx(
                        "all forms of a translate rule use the same variables".into()
                    ));
                }
            }
            // The implied canon keeps each variable's arity, and variables that a
            // form may leave unbound (inside ?opt) stay optional.
            let mut shape = Vec::new();
            var_shapes(&r.forms[0].pat, false, &mut shape);
            let kids = vs
                .iter()
                .map(|v| {
                    let (seq, opt) = shape
                        .iter()
                        .filter(|(n, _, _)| n == v)
                        .fold((false, false), |(s, o), (_, s2, o2)| (s || *s2, o || *o2));
                    let atom = a(if seq { format!("{v}...") } else { v.clone() });
                    if opt {
                        n("?opt", vec![atom])
                    } else {
                        atom
                    }
                })
                .collect();
            r.canon = Some(n(&format!("~{id}"), kids));
        }
        Kind::Idiom => {
            if r.canon.is_none() {
                return Err(ctx("idiom without a canon".into()));
            }
        }
    }
    validate(&r).map_err(ctx)?;
    Ok(r)
}

/// (variable, used as a sequence, inside ?opt) for each metavariable occurrence.
fn var_shapes(p: &Term, opt: bool, out: &mut Vec<(String, bool, bool)>) {
    match p {
        Term::A(x) if x.starts_with('?') && x.len() > 1 => match x.strip_suffix("...") {
            Some(v) => out.push((v.to_string(), true, opt)),
            None => out.push((x.clone(), false, opt)),
        },
        Term::A(_) => {}
        Term::N(h, k) => {
            if h == "?fn" || h == "?fact" {
                return;
            }
            for x in k {
                var_shapes(x, opt || h == "?opt", out);
            }
        }
    }
}

/// Every variable a template needs is bound by the pattern it is built from.
fn validate(r: &Rule) -> Result<(), String> {
    let Some(canon) = &r.canon else { return Ok(()) };
    let mut cv = Vec::new();
    vars(canon, &mut cv);
    for f in &r.forms {
        let mut fv = Vec::new();
        vars(&f.pat, &mut fv);
        let fixed: Vec<&String> = f.binds.iter().map(|(v, _)| v).collect();
        let lets: Vec<&String> = f.lets.iter().map(|(v, _)| v).collect();
        let emit_lets: Vec<&String> = f.emit_lets.iter().map(|(v, _)| v).collect();
        if f.lift {
            if let Some(v) = cv
                .iter()
                .find(|v| !fv.contains(v) && !fixed.contains(v) && !lets.contains(v))
            {
                return Err(format!(
                    "lifting the {} form leaves {v} of the canon unbound",
                    f.dialect.map(|d| d.id()).unwrap_or("any")
                ));
            }
        }
        if f.emit {
            if let Some(v) = fv
                .iter()
                .find(|v| !cv.contains(v) && !fixed.contains(v) && !emit_lets.contains(v))
            {
                return Err(format!(
                    "emitting the {} form needs {v}, which the canon does not bind",
                    f.dialect.map(|d| d.id()).unwrap_or("any")
                ));
            }
        }
    }
    Ok(())
}
