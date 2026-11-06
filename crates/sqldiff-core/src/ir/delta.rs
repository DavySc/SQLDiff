//! Shared, delta-shaped parts of an [`Outcome`](super::interp::Outcome).
//!
//! A fault scenario that merges back into the fault-free run (see
//! `Explorer::explore`) ends in the same state as that run, so its outcome
//! is the fault-free outcome with a few entries changed. These types hold
//! such an outcome as the fault-free one (shared, never copied) plus the
//! changes, so that making and comparing it costs the size of the change
//! and not the size of the routine. Each type reads as the plain value it
//! stands for: two of them are equal when those values are.

use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

use super::cfg::NodeId;
use super::interp::Fate;

/// The statements a path ran, in order: concatenated ranges of shared lists.
#[derive(Clone, Default)]
pub struct Trace {
    parts: Vec<(Rc<Vec<NodeId>>, usize, usize)>,
}

impl Trace {
    pub fn new(v: Vec<NodeId>) -> Trace {
        let n = v.len();
        Trace {
            parts: if n == 0 {
                Vec::new()
            } else {
                vec![(Rc::new(v), 0, n)]
            },
        }
    }

    /// `self` followed by `tail[from..]`.
    pub(super) fn joined(self, tail: &Trace, from: usize) -> Trace {
        let mut parts = self.parts;
        let mut skip = from;
        for (v, a, b) in &tail.parts {
            let n = b - a;
            if skip >= n {
                skip -= n;
                continue;
            }
            parts.push((v.clone(), a + skip, *b));
            skip = 0;
        }
        Trace { parts }
    }

    pub fn len(&self) -> usize {
        self.parts.iter().map(|(_, a, b)| b - a).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    pub fn get(&self, mut i: usize) -> Option<&NodeId> {
        for (v, a, b) in &self.parts {
            let n = b - a;
            if i < n {
                return v.get(a + i);
            }
            i -= n;
        }
        None
    }

    pub fn iter(&self) -> impl Iterator<Item = &NodeId> {
        self.parts.iter().flat_map(|(v, a, b)| v[*a..*b].iter())
    }
}

impl PartialEq for Trace {
    fn eq(&self, o: &Trace) -> bool {
        self.iter().eq(o.iter())
    }
}

impl Eq for Trace {}

impl fmt::Debug for Trace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

/// Fates by code: 0 for an effect that did not run.
const FATES: [Fate; 4] = [Fate::Durable, Fate::Undone, Fate::Partial, Fate::Caller];

pub(super) fn fate_code(f: Option<Fate>) -> u8 {
    match f {
        None => 0,
        Some(Fate::Durable) => 1,
        Some(Fate::Undone) => 2,
        Some(Fate::Partial) => 3,
        Some(Fate::Caller) => 4,
    }
}

fn code_fate(c: u8) -> Option<&'static Fate> {
    c.checked_sub(1).map(|i| &FATES[usize::from(i)])
}

/// A dense array of small codes, one per effect key (the keys are `names`,
/// in key order), shared with every outcome that came from it, and the
/// entries on which this one differs from it.
#[derive(Clone, Default)]
struct Dense {
    names: Rc<Vec<Rc<str>>>,
    base: Rc<Vec<u8>>,
    /// (index, code), by index, differing from `base`.
    over: Vec<(u32, u8)>,
}

impl Dense {
    fn at(&self, i: usize) -> u8 {
        match self.over.binary_search_by_key(&(i as u32), |(j, _)| *j) {
            Ok(x) => self.over[x].1,
            Err(_) => self.base[i],
        }
    }

    fn index(&self, k: &str) -> Option<usize> {
        self.names.binary_search_by(|n| (**n).cmp(k)).ok()
    }

    fn with(&self, changes: impl IntoIterator<Item = (usize, u8)>) -> Dense {
        let mut all: BTreeMap<u32, u8> = self.over.iter().copied().collect();
        for (i, c) in changes {
            all.insert(i as u32, c);
        }
        Dense {
            names: self.names.clone(),
            base: self.base.clone(),
            over: all
                .into_iter()
                .filter(|(i, c)| self.base[*i as usize] != *c)
                .collect(),
        }
    }

    fn live(&self) -> impl Iterator<Item = (usize, u8)> + '_ {
        let mut over = self.over.iter().peekable();
        (0..self.base.len()).filter_map(move |i| {
            let c = match over.peek() {
                Some(&&(j, c)) if j as usize == i => {
                    over.next();
                    c
                }
                _ => self.base[i],
            };
            (c != 0).then_some((i, c))
        })
    }
}

/// The fate of each effect that ran, by effect key: a dense array shared
/// with the outcomes it came from and the keys on which this outcome
/// differs from it.
#[derive(Clone, Default)]
pub struct Effects(Dense);

impl Effects {
    pub(super) fn dense(names: Rc<Vec<Rc<str>>>, fates: Vec<u8>) -> Effects {
        Effects(Dense {
            names,
            base: Rc::new(fates),
            over: Vec::new(),
        })
    }

    /// `self` with the fate (`None`: did not run) of some keys changed.
    pub(super) fn with(&self, changes: impl IntoIterator<Item = (usize, Option<Fate>)>) -> Effects {
        Effects(
            self.0
                .with(changes.into_iter().map(|(i, f)| (i, fate_code(f)))),
        )
    }

    pub fn get(&self, k: &str) -> Option<&Fate> {
        code_fate(self.0.at(self.0.index(k)?))
    }

    pub fn contains_key(&self, k: &str) -> bool {
        self.get(k).is_some()
    }

    pub fn get_key_value(&self, k: &str) -> Option<(&Rc<str>, &Fate)> {
        let i = self.0.index(k)?;
        code_fate(self.0.at(i)).map(|f| (&self.0.names[i], f))
    }

    pub fn is_empty(&self) -> bool {
        self.0.live().next().is_none()
    }

    pub fn len(&self) -> usize {
        self.0.live().count()
    }

    /// In key order.
    pub fn iter(&self) -> impl Iterator<Item = (&Rc<str>, &Fate)> {
        self.0
            .live()
            .filter_map(|(i, c)| Some((&self.0.names[i], code_fate(c)?)))
    }

    pub(super) fn name(&self, i: usize) -> &Rc<str> {
        &self.0.names[i]
    }

    pub(super) fn index_of(&self, k: &str) -> Option<usize> {
        self.0.index(k)
    }

    /// The fate in slot `i` (`None`: did not run).
    pub(super) fn fate_at(&self, i: usize) -> Option<Fate> {
        code_fate(self.0.at(i)).copied()
    }

    /// The indices of the keys on which `self` and `o` differ, if they are
    /// over the same keys. `cache` holds the shared arrays' differences.
    pub(super) fn differing(&self, o: &Effects, cache: ArrayDiff) -> Option<Vec<usize>> {
        if !Rc::ptr_eq(&self.0.names, &o.0.names) {
            return None;
        }
        let mut out: Vec<usize> = cache(&self.0.base, &o.0.base)
            .iter()
            .map(|&i| i as usize)
            .filter(|&i| {
                self.0
                    .over
                    .binary_search_by_key(&(i as u32), |(j, _)| *j)
                    .is_err()
                    && o.0
                        .over
                        .binary_search_by_key(&(i as u32), |(j, _)| *j)
                        .is_err()
            })
            .collect();
        for &(i, _) in self.0.over.iter().chain(&o.0.over) {
            let i = i as usize;
            if self.0.at(i) != o.0.at(i) {
                out.push(i);
            }
        }
        out.sort_unstable();
        out.dedup();
        Some(out)
    }
}

/// Differences between two shared fate arrays, computed once per pair.
pub(super) type ArrayDiff<'a> = &'a dyn Fn(&Rc<Vec<u8>>, &Rc<Vec<u8>>) -> Rc<Vec<u32>>;

/// The indices at which two dense arrays of equal length differ.
pub(super) fn scan_differing(a: &[u8], b: &[u8]) -> Vec<u32> {
    let mut out = Vec::new();
    let n = a.len().min(b.len());
    let mut i = 0;
    while i + 8 <= n {
        let x = u64::from_ne_bytes(a[i..i + 8].try_into().expect("8 bytes"));
        let y = u64::from_ne_bytes(b[i..i + 8].try_into().expect("8 bytes"));
        if x != y {
            for j in i..i + 8 {
                if a[j] != b[j] {
                    out.push(j as u32);
                }
            }
        }
        i += 8;
    }
    for j in i..n {
        if a[j] != b[j] {
            out.push(j as u32);
        }
    }
    out
}

impl PartialEq for Effects {
    fn eq(&self, o: &Effects) -> bool {
        self.iter().eq(o.iter())
    }
}

impl Eq for Effects {}

impl fmt::Debug for Effects {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

/// A set of effect keys, as a dense array of flags.
#[derive(Clone, Default)]
pub struct KeySet(Dense);

impl KeySet {
    pub(super) fn dense(names: Rc<Vec<Rc<str>>>, flags: Vec<u8>) -> KeySet {
        KeySet(Dense {
            names,
            base: Rc::new(flags),
            over: Vec::new(),
        })
    }

    /// `self` with some keys put in or taken out.
    pub(super) fn with(&self, changes: impl IntoIterator<Item = (usize, bool)>) -> KeySet {
        KeySet(
            self.0
                .with(changes.into_iter().map(|(i, b)| (i, u8::from(b)))),
        )
    }

    pub fn contains(&self, k: &str) -> bool {
        self.0.index(k).is_some_and(|i| self.0.at(i) != 0)
    }

    pub fn is_empty(&self) -> bool {
        self.0.live().next().is_none()
    }

    pub fn len(&self) -> usize {
        self.0.live().count()
    }

    /// In key order.
    pub fn iter(&self) -> impl Iterator<Item = &Rc<str>> {
        self.0.live().map(|(i, _)| &self.0.names[i])
    }
}

impl PartialEq for KeySet {
    fn eq(&self, o: &KeySet) -> bool {
        self.iter().eq(o.iter())
    }
}

impl Eq for KeySet {}

impl fmt::Debug for KeySet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

/// The effect keys that had run when the first fault hit: the fates of a
/// snapshot of the run at that point, which is shared with every outcome of
/// the scenario that came from it.
#[derive(Clone, Default)]
pub struct BeforeFault {
    /// Key names in key order and, aligned with them, whether each had run.
    names: Rc<Vec<Rc<str>>>,
    ran: Rc<Vec<bool>>,
}

impl BeforeFault {
    pub(super) fn new(names: Rc<Vec<Rc<str>>>, ran: Vec<bool>) -> BeforeFault {
        BeforeFault {
            names,
            ran: Rc::new(ran),
        }
    }

    pub fn contains(&self, k: &str) -> bool {
        self.names
            .binary_search_by(|n| (**n).cmp(k))
            .is_ok_and(|i| self.ran[i])
    }

    pub fn is_empty(&self) -> bool {
        !self.ran.iter().any(|r| *r)
    }

    /// In key order.
    pub fn iter(&self) -> impl Iterator<Item = &Rc<str>> {
        self.names
            .iter()
            .zip(self.ran.iter())
            .filter(|(_, r)| **r)
            .map(|(n, _)| n)
    }
}

impl PartialEq for BeforeFault {
    fn eq(&self, o: &BeforeFault) -> bool {
        self.iter().eq(o.iter())
    }
}

impl Eq for BeforeFault {}

impl fmt::Debug for BeforeFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}
