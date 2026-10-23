//! Canonical terms: the dialect-neutral form statements are lowered to.
//!
//! A term is an atom or a head with children. Two statements are equivalent
//! when their terms are equal, up to the few hooks in `compare`. Every place
//! where the dialects differ in behaviour (comparison collation, NULL
//! ordering, cardinality contract, time source, concurrency) is an explicit
//! child, so comparing terms compares behaviour.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Term {
    A(String),
    N(String, Vec<Term>),
}

pub fn a(s: impl Into<String>) -> Term {
    Term::A(s.into())
}

pub fn n(head: &str, kids: Vec<Term>) -> Term {
    Term::N(head.to_string(), kids)
}

impl Term {
    pub fn head(&self) -> &str {
        match self {
            Term::A(s) => s,
            Term::N(h, _) => h,
        }
    }

    pub fn kids(&self) -> &[Term] {
        match self {
            Term::A(_) => &[],
            Term::N(_, k) => k,
        }
    }

    pub fn is(&self, head: &str) -> bool {
        matches!(self, Term::N(h, _) if h == head)
    }

    pub fn kid(&self, i: usize) -> Option<&Term> {
        self.kids().get(i)
    }

    pub fn atom(&self) -> Option<&str> {
        match self {
            Term::A(s) => Some(s),
            Term::N(..) => None,
        }
    }

    /// Whether `pred` holds for this term or any subterm.
    pub fn any(&self, pred: &dyn Fn(&Term) -> bool) -> bool {
        pred(self) || self.kids().iter().any(|k| k.any(pred))
    }

    pub fn count(&self, pred: &dyn Fn(&Term) -> bool) -> usize {
        usize::from(pred(self)) + self.kids().iter().map(|k| k.count(pred)).sum::<usize>()
    }

    /// Replaces every subterm for which `f` returns Some.
    pub fn map(&self, f: &dyn Fn(&Term) -> Option<Term>) -> Term {
        if let Some(t) = f(self) {
            return t;
        }
        match self {
            Term::A(_) => self.clone(),
            Term::N(h, k) => Term::N(h.clone(), k.iter().map(|x| x.map(f)).collect()),
        }
    }
}

impl fmt::Display for Term {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Term::A(s) => write!(f, "{s}"),
            Term::N(h, k) => {
                write!(f, "({h}")?;
                for x in k {
                    write!(f, " {x}")?;
                }
                write!(f, ")")
            }
        }
    }
}
