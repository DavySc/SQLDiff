//! Applying rules: lifting to canonical nodes, emitting them in a dialect,
//! converting between dialects, lints and near misses.
//!
//! Conversion is lift-then-emit at each node, outermost first: a rule whose
//! source form matches builds its canonical node, and a rule with a target
//! form whose canon matches that node builds the target term. The pieces the
//! target term took from the source (metavariable values) are marked and
//! converted in turn; the rest is already target. A node no rule matches is
//! shared vocabulary and keeps its head, with its children converted. The
//! printer then refuses whatever has no target spelling.

use sqldiff_syntax::Dialect;

use super::pattern::{distance, matches, same, size, Binds, Bound, Conflict, Matcher};
use super::rule::{Cond, Form, Kind, Rule, RuleSet, Status};
use crate::equiv::schema::Schema;
use crate::equiv::term::{a, n, Term};

type R<T> = Result<T, String>;

/// Catalog facts a side condition can ask about.
pub trait Facts {
    /// `Some(true)` proven, `Some(false)` refuted, `None` unknown.
    fn holds(&self, fact: &Term) -> Option<bool>;
    /// A value such as `(identity-col TABLE)`, when the catalog knows it.
    fn value(&self, _query: &Term) -> Option<Term> {
        None
    }
}

pub struct NoFacts;

impl Facts for NoFacts {
    fn holds(&self, _: &Term) -> Option<bool> {
        None
    }
}

fn table_of(t: &Term) -> Option<String> {
    if t.is("name") {
        return t.kids().last().and_then(Term::atom).map(str::to_lowercase);
    }
    t.atom().map(str::to_lowercase)
}

impl Facts for Schema {
    fn holds(&self, f: &Term) -> Option<bool> {
        let k = f.kids();
        match f.head() {
            "unique" => {
                let t = self.table(&table_of(k.first()?)?)?;
                let cols: Vec<String> = k[1..]
                    .iter()
                    .flat_map(|c| {
                        if c.is("cols") {
                            c.kids().to_vec()
                        } else {
                            vec![c.clone()]
                        }
                    })
                    .filter_map(|c| c.atom().map(str::to_lowercase))
                    .collect();
                Some(t.is_unique(&cols))
            }
            "not-null" => {
                let t = self.table(&table_of(k.first()?)?)?;
                let c = k.get(1)?;
                let c = if c.is("col") { c.kids().last()? } else { c };
                Some(t.column(c.atom()?)?.not_null)
            }
            _ => None,
        }
    }

    fn value(&self, q: &Term) -> Option<Term> {
        match q.head() {
            "identity-col" => {
                let t = self.table(&table_of(q.kid(0)?)?)?;
                Some(a(t.identity()?.name.clone()))
            }
            _ => None,
        }
    }
}

/// What a rule application relied on or noted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub rule: String,
    pub status: Status,
    pub text: String,
}

#[derive(Default)]
pub struct Log {
    pub applied: Vec<Applied>,
    /// (assumption id, text).
    pub assumptions: Vec<(String, String)>,
    /// Facts proven from the catalog.
    pub discharged: Vec<String>,
    /// Ids of the rules that lifted or emitted, in order.
    pub rules: Vec<String>,
}

impl Log {
    fn note(&mut self, rule: &str, status: Status, text: String) {
        let x = Applied {
            rule: rule.to_string(),
            status,
            text,
        };
        if !self.applied.contains(&x) {
            self.applied.push(x);
        }
    }

    fn used(&mut self, id: &str) {
        if !self.rules.iter().any(|x| x == id) {
            self.rules.push(id.to_string());
        }
    }

    fn assume(&mut self, id: &str, text: &str) {
        if !self.assumptions.iter().any(|(i, _)| i == id) {
            self.assumptions.push((id.to_string(), text.to_string()));
        }
    }

    pub fn status(&self) -> Status {
        let mut s = Status::Converted;
        if !self.assumptions.is_empty() {
            s = Status::Assumptions;
        }
        self.applied.iter().map(|x| x.status).fold(s, Status::max)
    }
}

const SRC: &str = "@src";

fn helper(name: &str, args: &[Bound]) -> R<Option<Bound>> {
    let one = |i: usize| -> R<&Term> {
        match args.get(i) {
            Some(Bound::One(t)) => Ok(t),
            _ => Err(format!("helper `{name}`: argument {i} must be one term")),
        }
    };
    let seq = |i: usize| -> R<&[Term]> {
        match args.get(i) {
            Some(Bound::Seq(t)) => Ok(t),
            _ => Err(format!("helper `{name}`: argument {i} must be a sequence")),
        }
    };
    Ok(match name {
        // (?fn key-cols PRED (cols...) (row...)): the key columns when PRED is a
        // conjunction of `col = value` over inserted columns with the inserted values.
        "key-cols" => {
            let pred = one(0)?;
            let (cols, vals) = (seq(1)?, seq(2)?);
            let mut conj = Vec::new();
            flatten_and(pred, &mut conj);
            let mut keys = Vec::new();
            for c in conj {
                if !c.is("=") {
                    return Ok(None);
                }
                let (l, r) = (&c.kids()[0], &c.kids()[1]);
                let (col, val) = if l.is("col") {
                    (l, r)
                } else if r.is("col") {
                    (r, l)
                } else {
                    return Ok(None);
                };
                let name = col.kids().last().unwrap();
                let Some(i) = cols.iter().position(|x| same(x, name)) else {
                    return Ok(None);
                };
                if !vals.get(i).is_some_and(|v| same(v, val)) {
                    return Ok(None);
                }
                keys.push(name.clone());
            }
            Some(Bound::Seq(keys))
        }
        // (?fn key-pred keys... cols... vals...): `k = value` for each key.
        "key-pred" => {
            let (keys, cols, vals) = (seq(0)?, seq(1)?, seq(2)?);
            let mut pred: Option<Term> = None;
            for k in keys {
                let Some(i) = cols.iter().position(|x| same(x, k)) else {
                    return Ok(None);
                };
                let eq = n("=", vec![n("col", vec![k.clone()]), vals[i].clone()]);
                pred = Some(match pred {
                    None => eq,
                    Some(p) => n("AND", vec![p, eq]),
                });
            }
            pred.map(Bound::One)
        }
        // (?fn inline ALIAS (items...) X) / (?fn inline-seq ALIAS (items...) xs...):
        // replaces `ALIAS.c` by the value the derived table selects as c.
        "inline" | "inline-seq" => {
            let alias = one(0)?;
            let items = seq(1)?;
            let mut map = Vec::new();
            for it in items {
                let (Some(e), Some(c)) = (it.kid(0), it.kid(1).and_then(Term::atom)) else {
                    return Ok(None);
                };
                if !it.is("item") {
                    return Ok(None);
                }
                map.push((c.to_string(), e.clone()));
            }
            let al = n("alias", vec![alias.clone()]);
            let is_ref = |x: &Term| x.is("col") && x.kids().len() == 2 && same(&x.kids()[0], &al);
            let lookup = |x: &Term| -> Option<Term> {
                let c = x.kids()[1].atom()?;
                map.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(c))
                    .map(|(_, e)| e.clone())
            };
            let sub = |t: &Term| -> Option<Term> {
                if t.any(&|x| is_ref(x) && lookup(x).is_none()) {
                    return None;
                }
                Some(t.map(&|x| if is_ref(x) { lookup(x) } else { None }))
            };
            if name == "inline" {
                match sub(one(2)?) {
                    Some(t) => Some(Bound::One(t)),
                    None => return Ok(None),
                }
            } else {
                let mut out = Vec::new();
                for x in seq(2)? {
                    match sub(x) {
                        Some(t) => out.push(t),
                        None => return Ok(None),
                    }
                }
                Some(Bound::Seq(out))
            }
        }
        // (?fn stringy A B): `yes` when A or B is visibly a character string.
        "stringy" => {
            fn stringy(t: &Term) -> bool {
                match t.head() {
                    "str" => true,
                    "+" | "||" => t.kids().iter().any(stringy),
                    "call" => matches!(
                        t.kid(0)
                            .and_then(Term::atom)
                            .map(str::to_lowercase)
                            .as_deref(),
                        Some(
                            "concat"
                                | "upper"
                                | "lower"
                                | "ltrim"
                                | "rtrim"
                                | "trim"
                                | "left"
                                | "right"
                                | "substring"
                                | "replace"
                                | "replicate"
                                | "format"
                        )
                    ),
                    "cast" => t
                        .kid(1)
                        .and_then(|ty| ty.kid(0))
                        .and_then(Term::atom)
                        .is_some_and(|n| {
                            matches!(n, "varchar" | "nvarchar" | "char" | "nchar" | "text")
                        }),
                    _ => false,
                }
            }
            (stringy(one(0)?) || stringy(one(1)?)).then(|| Bound::One(a("yes")))
        }
        _ => return Err(format!("unknown helper `{name}`")),
    })
}

fn flatten_and(t: &Term, out: &mut Vec<Term>) {
    if t.head().eq_ignore_ascii_case("AND") && t.kids().len() == 2 {
        flatten_and(&t.kids()[0], out);
        flatten_and(&t.kids()[1], out);
    } else {
        out.push(t.clone());
    }
}

/// Builds a template. With `mark`, substituted values are wrapped as source.
pub fn inst(t: &Term, b: &Binds, mark: bool) -> R<Option<Vec<Term>>> {
    let wrap = |x: &Term| {
        if mark {
            n(SRC, vec![x.clone()])
        } else {
            x.clone()
        }
    };
    match t {
        Term::A(v) if v.starts_with('?') && v.len() > 1 => {
            let (name, seq) = match v.strip_suffix("...") {
                Some(s) => (s, true),
                None => (v.as_str(), false),
            };
            match (b.get(name), seq) {
                (Some(Bound::One(x)), false) => Ok(Some(vec![wrap(x)])),
                (Some(Bound::Seq(xs)), true) => Ok(Some(xs.iter().map(wrap).collect())),
                (Some(_), _) => Err(format!("{v} used with the wrong arity")),
                (None, _) => Err(format!("{v} is unbound")),
            }
        }
        Term::A(v) if v == "_..." => Ok(Some(vec![])),
        Term::A(v) if v == "_" => Err("`_` cannot be built; bind it".into()),
        Term::A(_) => Ok(Some(vec![t.clone()])),
        // Match-only forms read as templates: the plain form, nothing, the first alternative.
        Term::N(h, k) if h == "?block" && k.len() == 1 => inst(&k[0], b, mark),
        // `(?opt p)` builds p when its variables are bound, else nothing.
        Term::N(h, k) if h == "?opt" && k.len() == 1 => match inst(&k[0], b, mark) {
            Ok(v) => Ok(v),
            Err(e) if e.ends_with("is unbound") => Ok(Some(vec![])),
            Err(e) => Err(e),
        },
        Term::N(h, k) if h == "?or" && !k.is_empty() => inst(&k[0], b, mark),
        Term::N(h, k) if h == "?fn" => Ok(call_helper(k, b)?.map(|r| match r {
            Bound::One(x) => vec![wrap(&x)],
            Bound::Seq(xs) => xs.iter().map(wrap).collect(),
        })),
        Term::N(h, k) => {
            let mut kids = Vec::new();
            for x in k {
                match inst(x, b, mark)? {
                    Some(v) => kids.extend(v),
                    None => return Ok(None),
                }
            }
            Ok(Some(vec![Term::N(h.clone(), kids)]))
        }
    }
}

fn inst1(t: &Term, b: &Binds, mark: bool) -> R<Option<Term>> {
    Ok(inst(t, b, mark)?.map(|mut v| {
        if v.len() == 1 {
            v.remove(0)
        } else {
            n("seq", v)
        }
    }))
}

/// Substitutes bound metavariables in a pattern, leaving the others.
fn subst(p: &Term, b: &Binds) -> Term {
    match p {
        Term::A(v) if v.starts_with('?') && !v.ends_with("...") => match b.get(v) {
            Some(Bound::One(x)) => x.clone(),
            _ => p.clone(),
        },
        Term::A(_) => p.clone(),
        Term::N(h, k) => {
            let mut kids = Vec::new();
            for x in k {
                match x {
                    Term::A(v) if v.starts_with('?') && v.ends_with("...") => {
                        match b.get(&v[..v.len() - 3]) {
                            Some(Bound::Seq(xs)) => kids.extend(xs.iter().cloned()),
                            _ => kids.push(x.clone()),
                        }
                    }
                    _ => kids.push(subst(x, b)),
                }
            }
            Term::N(h.clone(), kids)
        }
    }
}

/// Evaluates a `let` value: a helper result, or a built term.
fn eval(t: &Term, b: &Binds) -> R<Option<Bound>> {
    if let Term::N(h, k) = t {
        if h == "?fn" {
            return call_helper(k, b);
        }
    }
    Ok(inst1(t, b, false)?.map(Bound::One))
}

fn call_helper(k: &[Term], b: &Binds) -> R<Option<Bound>> {
    let name = k
        .first()
        .and_then(Term::atom)
        .ok_or("?fn needs a helper name")?;
    let mut args = Vec::new();
    for x in &k[1..] {
        let is_seq = matches!(x, Term::A(s) if s.ends_with("..."));
        if is_seq {
            args.push(Bound::Seq(
                inst(x, b, false)?.ok_or("helper argument failed")?,
            ));
        } else {
            match eval(x, b)? {
                Some(v) => args.push(v),
                None => return Ok(None),
            }
        }
    }
    helper(name, &args)
}

fn let_all(lets: &[(String, Term)], b: &mut Binds, facts: &dyn Facts) -> R<bool> {
    for (v, e) in lets {
        if e.is("?fact") {
            let Some(q) = inst1(&n(e.kids()[0].head(), e.kids()[1..].to_vec()), b, false)? else {
                return Ok(false);
            };
            let val = facts.value(&q).unwrap_or_else(|| a("unknown"));
            b.insert(v.clone(), Bound::One(val));
            continue;
        }
        match eval(e, b)? {
            Some(x) => {
                b.insert(v.clone(), x);
            }
            None => return Ok(false),
        }
    }
    Ok(true)
}

/// Strips source marks (for analysis output and tests).
pub fn unmark(t: &Term) -> Term {
    match t {
        Term::N(h, k) if h == SRC && k.len() == 1 => unmark(&k[0]),
        Term::N(h, k) => Term::N(h.clone(), k.iter().map(unmark).collect()),
        x => x.clone(),
    }
}

pub struct Engine<'a> {
    pub rules: &'a RuleSet,
    pub facts: &'a dyn Facts,
    /// Idioms and translations, highest priority first.
    ordered: Vec<&'a Rule>,
}

impl<'a> Engine<'a> {
    pub fn new(rules: &'a RuleSet, facts: &'a dyn Facts) -> Self {
        let mut ordered: Vec<&Rule> = rules
            .rules
            .iter()
            .filter(|r| r.kind != Kind::Lint)
            .collect();
        ordered.sort_by_key(|r| -r.priority);
        Engine {
            rules,
            facts,
            ordered,
        }
    }

    /// Side conditions of `r` for dialect `d`. `Ok(false)` means the rule
    /// does not apply (a refuted fact or an `unless` pattern matched).
    fn conds(&self, r: &Rule, d: Dialect, b: &Binds, log: &mut Log) -> R<bool> {
        let mut pending = Vec::new();
        for (cd, c) in &r.conds {
            if cd.is_some_and(|x| x != d) {
                continue;
            }
            match c {
                Cond::Unless {
                    term,
                    pat,
                    anywhere,
                } => {
                    let Some(t) = inst1(term, b, false)? else {
                        continue;
                    };
                    let pat = &subst(pat, b);
                    let hit = if *anywhere {
                        t.any(&|x| matches(pat, x).is_some())
                    } else {
                        matches(pat, &t).is_some()
                    };
                    if hit {
                        return Ok(false);
                    }
                }
                Cond::IfContains {
                    term,
                    pat,
                    conjunct,
                } => {
                    let Some(t) = inst1(term, b, false)? else {
                        return Ok(false);
                    };
                    let pat = &subst(pat, b);
                    let hit = if *conjunct {
                        let mut conj = Vec::new();
                        flatten_and(&t, &mut conj);
                        conj.iter().any(|x| matches(pat, x).is_some())
                    } else {
                        t.any(&|x| matches(pat, x).is_some())
                    };
                    if !hit {
                        return Ok(false);
                    }
                }
                Cond::Require { fact, id, text } => {
                    let Some(f) = inst1(fact, b, false)? else {
                        return Ok(false);
                    };
                    match self.facts.holds(&f) {
                        Some(true) => log
                            .discharged
                            .push(format!("{} (catalog)", super::sexp::write(&f))),
                        Some(false) => return Ok(false),
                        None => pending.push((id.clone(), text.clone())),
                    }
                }
                Cond::Assume { id, text } => pending.push((id.clone(), text.clone())),
            }
        }
        for (id, text) in pending {
            log.assume(&id, &text);
        }
        Ok(true)
    }

    fn notes(&self, r: &Rule, d: Dialect, b: &Binds, log: &mut Log) {
        for note in r.notes.iter().filter(|x| x.dialect == d) {
            let on = match &note.guard {
                None => true,
                Some((v, p)) => match b.get(v) {
                    Some(Bound::One(t)) => matches(p, t).is_some(),
                    _ => false,
                },
            };
            if on {
                log.note(&r.id, note.status, note.text.clone());
            }
        }
    }

    /// Tries to lift `t` with any rule's `d` form: (rule, canonical node).
    pub fn lift_once(&self, t: &Term, d: Dialect, log: &mut Log) -> R<Option<(&'a Rule, Term)>> {
        for r in self.ordered.iter().copied() {
            for f in r.form(d).filter(|f| f.lift && !f.pat.is("seq")) {
                if let Some(c) = self.try_lift(r, f, t, d, log)? {
                    return Ok(Some((r, c)));
                }
            }
        }
        Ok(None)
    }

    fn try_lift(&self, r: &Rule, f: &Form, t: &Term, d: Dialect, log: &mut Log) -> R<Option<Term>> {
        let Some(mut b) = matches(&f.pat, t) else {
            return Ok(None);
        };
        for (v, val) in &f.binds {
            b.insert(v.clone(), Bound::One(val.clone()));
        }
        if !let_all(&f.lets, &mut b, self.facts)? {
            return Ok(None);
        }
        let mut l2 = Log::default();
        if !self.conds(r, d, &b, &mut l2)? {
            return Ok(None);
        }
        let Some(c) = inst1(r.canon.as_ref().unwrap(), &b, false)? else {
            return Ok(None);
        };
        merge(log, l2);
        self.notes(r, d, &b, log);
        log.used(&r.id);
        Ok(Some(c))
    }

    /// Emits a canonical node in dialect `d` (values marked as source).
    pub fn emit_once(&self, c: &Term, d: Dialect, log: &mut Log) -> R<Option<Term>> {
        for r in self.ordered.iter().copied() {
            let Some(canon) = &r.canon else { continue };
            let Some(b0) = matches(canon, c) else {
                continue;
            };
            for f in r.form(d).filter(|f| f.emit) {
                let mut b = b0.clone();
                let mut fixed_ok = true;
                for (v, val) in &f.binds {
                    match b.get(v) {
                        None => {
                            b.insert(v.clone(), Bound::One(val.clone()));
                        }
                        Some(Bound::One(x)) if same(x, val) || f.emit_any.contains(v) => {}
                        Some(_) => fixed_ok = false,
                    }
                }
                if !fixed_ok || !let_all(&f.emit_lets, &mut b, self.facts)? {
                    continue;
                }
                let mut l2 = Log::default();
                if !self.conds(r, d, &b, &mut l2)? {
                    continue;
                }
                let Some(out) = inst1(&f.pat, &b, true)? else {
                    continue;
                };
                merge(log, l2);
                self.notes(r, d, &b, log);
                log.used(&r.id);
                return Ok(Some(out));
            }
        }
        Ok(None)
    }

    /// Converts a surface term from `from` to `to`.
    pub fn convert(&self, t: &Term, from: Dialect, to: Dialect, log: &mut Log) -> R<Term> {
        if let Some((r, c)) = self.lift_once(t, from, log)? {
            let Some(out) = self.emit_once(&c, to, log)? else {
                return Err(format!(
                    "rule `{}` recognises this, but no rule spells `{}` in {}",
                    r.id,
                    c.head(),
                    to.display_name()
                ));
            };
            return self.target(&out, from, to, log);
        }
        match t {
            Term::A(_) => Ok(t.clone()),
            Term::N(h, k) if h == "block" => Ok(n("block", self.convert_list(k, from, to, log)?)),
            Term::N(h, k) => Ok(Term::N(
                h.clone(),
                k.iter()
                    .map(|x| self.convert(x, from, to, log))
                    .collect::<R<_>>()?,
            )),
        }
    }

    /// Converts the source-marked parts of an emitted term.
    pub fn target(&self, t: &Term, from: Dialect, to: Dialect, log: &mut Log) -> R<Term> {
        match t {
            Term::N(h, k) if h == SRC && k.len() == 1 => self.convert(&k[0], from, to, log),
            Term::N(h, k) => Ok(Term::N(
                h.clone(),
                k.iter()
                    .map(|x| self.target(x, from, to, log))
                    .collect::<R<_>>()?,
            )),
            x => Ok(x.clone()),
        }
    }

    /// Sequence rules over consecutive statements, then each statement.
    pub fn convert_list(
        &self,
        items: &[Term],
        from: Dialect,
        to: Dialect,
        log: &mut Log,
    ) -> R<Vec<Term>> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < items.len() {
            if let Some((len, c)) = self.lift_seq(&items[i..], from, log)? {
                let Some(e) = self.emit_once(&c, to, log)? else {
                    return Err(format!(
                        "no rule spells `{}` in {}",
                        c.head(),
                        to.display_name()
                    ));
                };
                let e = self.target(&e, from, to, log)?;
                if e.is("seq") {
                    out.extend(e.kids().iter().cloned());
                } else {
                    out.push(e);
                }
                i += len;
                continue;
            }
            out.push(self.convert(&items[i], from, to, log)?);
            i += 1;
        }
        Ok(out)
    }

    /// A sequence rule matching a prefix of `items`: (statements used, canon).
    pub fn lift_seq(&self, items: &[Term], d: Dialect, log: &mut Log) -> R<Option<(usize, Term)>> {
        for r in self.ordered.iter().copied() {
            for f in r.form(d).filter(|f| f.lift && f.pat.is("seq")) {
                let len = f.pat.kids().len();
                if items.len() < len {
                    continue;
                }
                let window = n("seq", items[..len].to_vec());
                if let Some(c) = self.try_lift(r, f, &window, d, log)? {
                    return Ok(Some((len, c)));
                }
            }
        }
        Ok(None)
    }

    /// Lifts every idiom in a surface tree (analysis; no emission).
    pub fn lift(&self, t: &Term, d: Dialect, log: &mut Log) -> R<Term> {
        if let Some((_, c)) = self.lift_once(t, d, log)? {
            if c.head().starts_with('~') {
                // Translations are for conversion only.
                return self.lift_kids(t, d, log);
            }
            return self.lift_kids(&c, d, log);
        }
        self.lift_kids(t, d, log)
    }

    fn lift_kids(&self, t: &Term, d: Dialect, log: &mut Log) -> R<Term> {
        match t {
            Term::A(_) => Ok(t.clone()),
            Term::N(h, k) if h == "block" => {
                let mut out = Vec::new();
                let mut i = 0;
                while i < k.len() {
                    if let Some((len, c)) = self.lift_seq(&k[i..], d, log)? {
                        out.push(self.lift_kids(&c, d, log)?);
                        i += len;
                    } else {
                        out.push(self.lift(&k[i], d, log)?);
                        i += 1;
                    }
                }
                Ok(n("block", out))
            }
            Term::N(h, k) => Ok(Term::N(
                h.clone(),
                k.iter().map(|x| self.lift(x, d, log)).collect::<R<_>>()?,
            )),
        }
    }

    /// Lint hits in a (lifted) tree: (rule, matched subterm).
    pub fn lints(&self, t: &Term, d: Dialect) -> Vec<(&'a Rule, Term)> {
        let mut out = Vec::new();
        let lints: Vec<&Rule> = self
            .rules
            .rules
            .iter()
            .filter(|r| r.kind == Kind::Lint)
            .collect();
        walk(t, &mut |x| {
            for r in &lints {
                for f in &r.forms {
                    if f.dialect.is_some_and(|fd| fd != d) {
                        continue;
                    }
                    if let Some(b) = matches(&f.pat, x) {
                        let mut log = Log::default();
                        if self.conds(r, d, &b, &mut log).unwrap_or(false) {
                            out.push((*r, x.clone()));
                        }
                    }
                }
            }
        });
        out
    }

    /// Patterns that match except for metavariables bound to different terms
    /// that are still alike (see `near`), closest first.
    pub fn near_misses(&self, t: &Term, d: Dialect) -> Vec<NearMiss> {
        let mut out = Vec::new();
        let rules: Vec<&Rule> = self
            .rules
            .rules
            .iter()
            .filter(|r| r.kind == Kind::Idiom)
            .collect();
        let mut check = |x: &Term| {
            for r in &rules {
                for f in r.form(d).filter(|f| f.lift) {
                    let mut c: Vec<Conflict> = Vec::new();
                    let mut b = Binds::new();
                    let ok = Matcher {
                        conflicts: Some(&mut c),
                    }
                    .m(&f.pat, x, &mut b);
                    if ok && !c.is_empty() {
                        if let Some(dist) = near(&c) {
                            out.push(NearMiss {
                                rule: r.id.clone(),
                                conflicts: c,
                                distance: dist,
                                at: x.clone(),
                            });
                        }
                    }
                }
            }
        };
        walk(t, &mut check);
        if t.is("block") || t.is("seq") {
            // Sequence forms over consecutive statements.
            let k = t.kids();
            for r in &rules {
                for f in r.form(d).filter(|f| f.lift && f.pat.is("seq")) {
                    let len = f.pat.kids().len();
                    for i in 0..k.len().saturating_sub(len - 1) {
                        let w = n("seq", k[i..i + len].to_vec());
                        let mut c = Vec::new();
                        let mut b = Binds::new();
                        if (Matcher {
                            conflicts: Some(&mut c),
                        })
                        .m(&f.pat, &w, &mut b)
                            && !c.is_empty()
                        {
                            let Some(dist) = near(&c) else { continue };
                            out.push(NearMiss {
                                rule: r.id.clone(),
                                conflicts: c,
                                distance: dist,
                                at: w,
                            });
                        }
                    }
                }
            }
        }
        out.sort_by_key(|m| m.distance);
        out
    }
}

#[derive(Debug, Clone)]
pub struct NearMiss {
    pub rule: String,
    pub conflicts: Vec<Conflict>,
    pub distance: usize,
    pub at: Term,
}

fn merge(log: &mut Log, other: Log) {
    for x in other.applied {
        if !log.applied.contains(&x) {
            log.applied.push(x);
        }
    }
    for (id, t) in other.assumptions {
        log.assume(&id, &t);
    }
    log.discharged.extend(other.discharged);
    for r in other.rules {
        log.used(&r);
    }
}

pub fn walk(t: &Term, f: &mut dyn FnMut(&Term)) {
    f(t);
    for k in t.kids() {
        walk(k, f);
    }
}

/// Pairs of rules that can both apply to one term in the same direction
/// with equal priority (the shipped set must have none).
pub fn overlaps(rs: &RuleSet) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let rules: Vec<&Rule> = rs.rules.iter().filter(|r| r.kind != Kind::Lint).collect();
    for (i, r1) in rules.iter().enumerate() {
        for r2 in &rules[i + 1..] {
            if r1.priority != r2.priority {
                continue;
            }
            for f1 in &r1.forms {
                for f2 in &r2.forms {
                    if f1.dialect != f2.dialect {
                        continue;
                    }
                    let d = f1.dialect.map(|d| d.id()).unwrap_or("any");
                    if f1.lift && f2.lift && super::pattern::overlap(&f1.pat, &f2.pat) {
                        out.push((r1.id.clone(), r2.id.clone(), format!("lifting {d}")));
                    }
                    if f1.emit && f2.emit {
                        if let (Some(c1), Some(c2)) = (&r1.canon, &r2.canon) {
                            let fixed = |f: &Form, c: &Term| -> Term {
                                c.map(&|x| {
                                    let v = x.atom()?;
                                    if f.emit_any.iter().any(|e| e == v) {
                                        return None;
                                    }
                                    f.binds
                                        .iter()
                                        .find(|(b, _)| b == v)
                                        .map(|(_, val)| val.clone())
                                })
                            };
                            if super::pattern::overlap(&fixed(f1, c1), &fixed(f2, c2)) {
                                out.push((r1.id.clone(), r2.id.clone(), format!("emitting {d}")));
                            }
                        }
                    }
                }
            }
        }
    }
    out.dedup();
    out
}

/// The edit distance of the conflicts when it is at most half of what
/// differs: `@Id` against `@Other` is a near miss, `a = 1` against `b > @z`
/// is just different code.
fn near(c: &[Conflict]) -> Option<usize> {
    let dist = c
        .iter()
        .map(|k| distance(&k.first, &k.second))
        .sum::<usize>();
    let budget = c
        .iter()
        .map(|k| size(&k.first).max(size(&k.second)))
        .sum::<usize>();
    // A single renamed atom (`@rc` against `@other`) is the closest miss there is.
    (dist * 2 <= budget.max(2)).then_some(dist)
}
