//! Pattern matching over terms.
//!
//! In a pattern:
//! - `?x` binds one term, `?x...` binds a run of siblings (zero or more);
//! - `_` and `_...` match without binding;
//! - a metavariable used twice must bind equal terms. Names are compared
//!   case-insensitively except inside `(str ...)`, and table aliases are
//!   already replaced by table names in surface terms, so this equality is
//!   alpha-equivalence on bound names;
//! - `(?or p q ...)` matches any alternative;
//! - `(?block p)` matches `p` or a `(block p)` holding only `p`;
//! - `(?opt p)` inside a list matches zero or one sibling;
//! - a list head `?h` binds the head atom.
//!
//! In a template the same `?x` and `?x...` substitute, and `(?fn name args...)`
//! calls a helper (see `rewrite::helper`).

use std::collections::BTreeMap;

use crate::equiv::term::Term;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bound {
    One(Term),
    Seq(Vec<Term>),
}

pub type Binds = BTreeMap<String, Bound>;

/// A metavariable bound twice to different terms (lenient matching).
#[derive(Debug, Clone)]
pub struct Conflict {
    pub var: String,
    pub first: Term,
    pub second: Term,
}

fn is_seq(a: &str) -> bool {
    a.len() > 3 && a.ends_with("...")
}

/// Atom equality: case-insensitive, except string literal contents.
pub fn atom_eq(x: &str, y: &str, in_str: bool) -> bool {
    if in_str {
        x == y
    } else {
        x.eq_ignore_ascii_case(y)
    }
}

/// Term equality under the matcher's rules.
pub fn same(x: &Term, y: &Term) -> bool {
    same_in(x, y, false)
}

fn same_in(x: &Term, y: &Term, in_str: bool) -> bool {
    match (x, y) {
        (Term::A(a), Term::A(b)) => atom_eq(a, b, in_str),
        (Term::N(h1, k1), Term::N(h2, k2)) => {
            let s = h1 == "str";
            h1.eq_ignore_ascii_case(h2)
                && k1.len() == k2.len()
                && k1.iter().zip(k2).all(|(a, b)| same_in(a, b, s))
        }
        _ => false,
    }
}

pub struct Matcher<'c> {
    /// When set, conflicting rebindings are recorded instead of failing.
    pub conflicts: Option<&'c mut Vec<Conflict>>,
}

impl Matcher<'_> {
    pub fn strict() -> Matcher<'static> {
        Matcher { conflicts: None }
    }

    fn bind(&mut self, b: &mut Binds, name: &str, v: Bound) -> bool {
        match b.get(name) {
            None => {
                b.insert(name.to_string(), v);
                true
            }
            Some(old) => {
                let eq = match (old, &v) {
                    (Bound::One(x), Bound::One(y)) => same(x, y),
                    (Bound::Seq(x), Bound::Seq(y)) => {
                        x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b))
                    }
                    _ => false,
                };
                if eq {
                    return true;
                }
                match (&mut self.conflicts, old, &v) {
                    (Some(c), Bound::One(x), Bound::One(y)) => {
                        c.push(Conflict {
                            var: name.to_string(),
                            first: x.clone(),
                            second: y.clone(),
                        });
                        true
                    }
                    _ => false,
                }
            }
        }
    }

    /// Matches `p` against `t`, extending `b`. On failure `b` may hold partial bindings.
    pub fn m(&mut self, p: &Term, t: &Term, b: &mut Binds) -> bool {
        self.m_in(p, t, b, false)
    }

    fn m_in(&mut self, p: &Term, t: &Term, b: &mut Binds, in_str: bool) -> bool {
        match p {
            Term::A(a) if a == "_" => true,
            Term::A(a) if a.starts_with('?') && a.len() > 1 && !is_seq(a) => {
                self.bind(b, a, Bound::One(t.clone()))
            }
            Term::A(a) => matches!(t, Term::A(x) if atom_eq(a, x, in_str)),
            Term::N(h, alts) if h == "?or" => {
                for alt in alts {
                    let mut b2 = b.clone();
                    let n = self.conflict_len();
                    if self.m_in(alt, t, &mut b2, in_str) {
                        *b = b2;
                        return true;
                    }
                    self.truncate(n);
                }
                false
            }
            Term::N(h, k) if h == "?block" && k.len() == 1 => {
                let mut b2 = b.clone();
                let n = self.conflict_len();
                if self.m_in(&k[0], t, &mut b2, in_str) {
                    *b = b2;
                    return true;
                }
                self.truncate(n);
                match t {
                    Term::N(th, tk) if th == "block" && tk.len() == 1 => {
                        self.m_in(&k[0], &tk[0], b, in_str)
                    }
                    _ => false,
                }
            }
            Term::N(h, k) if h == "?opt" && k.len() == 1 => self.m_in(&k[0], t, b, in_str),
            Term::N(h, pk) => {
                let Term::N(th, tk) = t else { return false };
                if h.starts_with('?') && h.len() > 1 {
                    if !self.bind(b, h, Bound::One(Term::A(th.clone()))) {
                        return false;
                    }
                } else if !h.eq_ignore_ascii_case(th) {
                    return false;
                }
                self.list(pk, tk, b, th == "str")
            }
        }
    }

    fn conflict_len(&self) -> usize {
        self.conflicts.as_ref().map_or(0, |c| c.len())
    }

    fn truncate(&mut self, n: usize) {
        if let Some(c) = self.conflicts.as_mut() {
            c.truncate(n);
        }
    }

    /// Matches a pattern child list against a term child list, with backtracking.
    fn list(&mut self, ps: &[Term], ts: &[Term], b: &mut Binds, in_str: bool) -> bool {
        let Some((p, rest)) = ps.split_first() else {
            return ts.is_empty();
        };
        if let Term::A(a) = p {
            if is_seq(a) {
                let name = &a[..a.len() - 3];
                // Shortest first, so a later sibling pattern gets the first chance.
                for k in 0..=ts.len() {
                    let mut b2 = b.clone();
                    let n = self.conflict_len();
                    let ok = (name == "_"
                        || self.bind(&mut b2, name, Bound::Seq(ts[..k].to_vec())))
                        && self.list(rest, &ts[k..], &mut b2, in_str);
                    if ok {
                        *b = b2;
                        return true;
                    }
                    self.truncate(n);
                }
                return false;
            }
        }
        if let Term::N(h, k) = p {
            if h == "?opt" && k.len() == 1 {
                if let Some((t, trest)) = ts.split_first() {
                    let mut b2 = b.clone();
                    let n = self.conflict_len();
                    if self.m_in(&k[0], t, &mut b2, in_str)
                        && self.list(rest, trest, &mut b2, in_str)
                    {
                        *b = b2;
                        return true;
                    }
                    self.truncate(n);
                }
                return self.list(rest, ts, b, in_str);
            }
        }
        let Some((t, trest)) = ts.split_first() else {
            return false;
        };
        let mut b2 = b.clone();
        let n = self.conflict_len();
        if self.m_in(p, t, &mut b2, in_str) && self.list(rest, trest, &mut b2, in_str) {
            *b = b2;
            return true;
        }
        self.truncate(n);
        false
    }
}

/// Matches strictly; the bindings on success.
pub fn matches(p: &Term, t: &Term) -> Option<Binds> {
    let mut b = Binds::new();
    Matcher::strict().m(p, t, &mut b).then_some(b)
}

/// Metavariables a pattern binds (or a template uses), without the `...`.
pub fn vars(p: &Term, out: &mut Vec<String>) {
    match p {
        Term::A(a) if a.starts_with('?') && a.len() > 1 => {
            let v = a.strip_suffix("...").unwrap_or(a).to_string();
            if !out.contains(&v) {
                out.push(v);
            }
        }
        Term::A(_) => {}
        Term::N(h, k) => {
            if h == "?fn" || h == "?fact" {
                // The helper name is not a variable.
                for x in k.iter().skip(1) {
                    vars(x, out);
                }
                return;
            }
            if h.starts_with('?') && !matches!(h.as_str(), "?or" | "?block" | "?opt") {
                vars(&Term::A(h.clone()), out);
            }
            for x in k {
                vars(x, out);
            }
        }
    }
}

/// Whether two patterns can match a common term (conservative: may say
/// yes when they cannot, never no when they can).
pub fn overlap(p: &Term, q: &Term) -> bool {
    let var = |t: &Term| matches!(t, Term::A(a) if a == "_" || (a.starts_with('?') && a.len() > 1));
    if var(p) || var(q) {
        return true;
    }
    match (p, q) {
        (Term::N(h, alts), _) if h == "?or" => alts.iter().any(|a| overlap(a, q)),
        (_, Term::N(h, _)) if h == "?or" => overlap(q, p),
        (Term::N(h, k), _) if h == "?block" && k.len() == 1 => overlap(&k[0], q) || q.is("block"),
        (_, Term::N(h, _)) if h == "?block" => overlap(q, p),
        (Term::A(a), Term::A(b)) => a.eq_ignore_ascii_case(b),
        (Term::N(h1, k1), Term::N(h2, k2)) => {
            let hv = |h: &str| h.starts_with('?') && h.len() > 1;
            (hv(h1) || hv(h2) || h1.eq_ignore_ascii_case(h2)) && overlap_list(k1, k2)
        }
        _ => false,
    }
}

fn flexible(t: &Term) -> bool {
    matches!(t, Term::A(a) if is_seq(a)) || t.is("?opt")
}

fn overlap_list(a: &[Term], b: &[Term]) -> bool {
    if a.iter().any(flexible) || b.iter().any(flexible) {
        // Compare the fixed prefixes and suffixes up to the first flexible element.
        let pre = |x: &[Term]| x.iter().take_while(|t| !flexible(t)).count();
        let suf = |x: &[Term]| x.iter().rev().take_while(|t| !flexible(t)).count();
        let n = pre(a).min(pre(b));
        let m = suf(a).min(suf(b));
        return a[..n].iter().zip(&b[..n]).all(|(x, y)| overlap(x, y))
            && a[a.len() - m..]
                .iter()
                .zip(&b[b.len() - m..])
                .all(|(x, y)| overlap(x, y));
    }
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| overlap(x, y))
}

pub fn size(t: &Term) -> usize {
    1 + t.kids().iter().map(size).sum::<usize>()
}

/// Ordered tree edit distance (insert and delete cost the subtree size,
/// relabel costs 1), used to rank near misses.
pub fn distance(x: &Term, y: &Term) -> usize {
    if same(x, y) {
        return 0;
    }
    let relabel = usize::from(!x.head().eq_ignore_ascii_case(y.head()));
    let (a, b) = (x.kids(), y.kids());
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in 1..=a.len() {
        d[i][0] = d[i - 1][0] + size(&a[i - 1]);
    }
    for j in 1..=b.len() {
        d[0][j] = d[0][j - 1] + size(&b[j - 1]);
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            d[i][j] = (d[i - 1][j] + size(&a[i - 1]))
                .min(d[i][j - 1] + size(&b[j - 1]))
                .min(d[i - 1][j - 1] + distance(&a[i - 1], &b[j - 1]));
        }
    }
    relabel + d[a.len()][b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::sexp::read;

    fn t(s: &str) -> Term {
        read(s).unwrap()
    }

    #[test]
    fn metavars_and_sequences() {
        let b = matches(&t("(f ?a _ ?rest...)"), &t("(f x y z w)")).unwrap();
        assert_eq!(b["?a"], Bound::One(t("x")));
        assert_eq!(b["?rest"], Bound::Seq(vec![t("z"), t("w")]));
        assert!(matches(&t("(f ?a ?a)"), &t("(f (g X) (g x))")).is_some());
        assert!(matches(&t("(f ?a ?a)"), &t("(f (str X) (str x))")).is_none());
        assert!(matches(&t("(f _... (k ?v) _...)"), &t("(f a (k 1) b)")).is_some());
        assert!(matches(&t("(f (?opt (w ?w)) z)"), &t("(f z)")).is_some());
        assert!(matches(&t("(f (?opt (w ?w)) z)"), &t("(f (w 1) z)")).is_some());
        assert!(matches(&t("(?block (u ?x))"), &t("(block (u 1))")).is_some());
        assert!(matches(&t("(?or (a) (b))"), &t("(b)")).is_some());
        assert!(matches(&t("(f)"), &t("(f x)")).is_none());
    }

    #[test]
    fn lenient_records_conflicts() {
        let mut c = Vec::new();
        let mut b = Binds::new();
        assert!(Matcher {
            conflicts: Some(&mut c)
        }
        .m(&t("(f ?a ?a)"), &t("(f 1 2)"), &mut b));
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn overlap_is_conservative() {
        assert!(overlap(&t("(call isnull ?a ?b)"), &t("(call ?f ?x...)")));
        assert!(!overlap(&t("(call isnull ?a ?b)"), &t("(call len ?a)")));
        assert!(!overlap(&t("(call isnull ?a ?b)"), &t("(call isnull ?a)")));
    }

    #[test]
    fn edit_distance() {
        assert_eq!(distance(&t("(f a b)"), &t("(f a b)")), 0);
        assert_eq!(distance(&t("(f a b)"), &t("(f a c)")), 1);
        assert_eq!(distance(&t("(f a)"), &t("(f a (g x))")), 2);
    }
}
