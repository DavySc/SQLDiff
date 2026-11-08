//! Measurement only: how many *pair-states* a lockstep exploration of two
//! routines would need (perf plan W6, `perf-plan/conversion-performance-plan.md`).
//!
//! Nothing here is called by `diff`, `check` or the converter; the explorer
//! and every outcome are untouched, so results stay byte-identical. The
//! entry is [`measure`], reached through `engine::pair_states` (on demand,
//! like `what_if`).
//!
//! Both sides are stepped with the production step function, each up to its
//! next data-dependent decision (`Test::Unknown`). Two sides that reach a
//! decision on the same condition fork together (true with true, false with
//! false), so a *pair-state* is a pair of side states at a pair of nodes
//! with the same signature prefix. Pair-states that agree under a key are
//! counted once (and expanded once): the question is how many distinct
//! keys there are, against how many signature prefixes (paths) reach them.
//!
//! Keys (see [`Variant`]):
//! * `strict`: both full side states (what a single-side merge would see);
//! * `agree`: per effect slot only whether the two sides agree when both
//!   are settled (durable, undone or never run); a pending slot stays exact
//!   because a later ROLLBACK changes its fate; everything else of each
//!   side's state stays (a "residual" state), history (`signature`, rows,
//!   assumptions, trace) is left out as it only reaches the outcome;
//! * `steps`: whether the step counter is part of the key (the budget is per
//!   path today; W6.5 would make it per statement);
//! * `deadloop`: loop summaries (`loop_seen`) of loops that cannot run again
//!   from the node are dropped from the key.
//!
//! The counts are what a merged exploration needs, not a proof that merging
//! is sound: the sufficiency of an `agree` key is the W6 proof obligation.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Variant {
    pub agree: bool,
    pub steps: bool,
    pub deadloop: bool,
    /// Match conditions of the two sides more leniently than `pairs` does
    /// (a `v_` local prefix is ignored too): what a better matcher would give.
    pub lenient: bool,
    /// With `agree`: when both sides' logs of uncommitted effects are the
    /// same sequence, the logs, their markers' positions and the pending
    /// slots are replaced by that one fact (a rollback then undoes the same
    /// entries on both sides, whichever they are). The W6 log abstraction.
    pub abstract_log: bool,
}

/// What one exploration of a pair of routines found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Measure {
    /// Signature prefixes (paths) of the lockstep exploration without any
    /// merging, saturating at `u128::MAX`; `None` when the state cap hit.
    pub paths: Option<u128>,
    /// Distinct pair-states at decision points (keys), and distinct pair
    /// outcomes (exit pair + per-effect agreement).
    pub states: usize,
    pub leaves: usize,
    /// Straight-line ticks executed (merged exploration).
    pub ticks: u64,
    /// Distinct decision node pairs ("joins") and the most states at one.
    pub joins: usize,
    pub max_per_join: usize,
    /// Distinct side states (full key) over all decision points, per side,
    /// and the most at one node of that side.
    pub single_a: usize,
    pub single_b: usize,
    pub max_single: usize,
    /// Pairs whose sides disagree on what is decided next (or one side ended
    /// first): not explored further, counted as leaves.
    pub desync: usize,
    /// The first desync, for the report (conditions, or which side ended).
    pub why: Option<String>,
    /// The state cap was reached: counts are lower bounds.
    pub capped: bool,
}

#[derive(Clone)]
enum Side {
    At(NodeId, Box<State>),
    Done(Box<Outcome>),
}

/// Whether `step` forks at `node` in `st`: a test whose answer the state
/// does not give (`Interp::step`, `Op::Test`).
fn decision(cfg: &Cfg, node: NodeId, st: &State) -> bool {
    match &cfg.node(node).op {
        Op::Test { test, .. } => match test {
            Test::Unknown(_) => true,
            Test::NoRows(_) => st.no_rows.is_none(),
            _ => false,
        },
        _ => false,
    }
}

impl Interp<'_> {
    /// Runs until the next decision (not yet taken) or the end.
    fn to_decision(&self, mut node: NodeId, mut st: Box<State>, ticks: &mut u64) -> Side {
        loop {
            if decision(self.cfg, node, &st) {
                return Side::At(node, st);
            }
            *ticks += 1;
            st.steps += 1;
            if st.steps > MAX_STEPS {
                st.imprecise.push("step budget exhausted".into());
                st.cut.push(node);
                return Side::Done(Box::new(self.finish(Exit::Normal, *st)));
            }
            match self.step(node, st) {
                Step::Go(n, s) => {
                    node = n;
                    st = s;
                }
                Step::Done(o) => return Side::Done(o),
                Step::Fork(_) | Step::Pruned(..) => unreachable!("only a decision forks"),
            }
        }
    }

    /// Takes the decision at `node`: `Ok((cond, [(true), (false)]))`, or
    /// `Err(side)` when a loop is summarised instead of forking.
    #[allow(clippy::type_complexity)]
    fn decide(
        &self,
        node: NodeId,
        mut st: Box<State>,
        ticks: &mut u64,
    ) -> Result<(String, Vec<(NodeId, Box<State>)>), Side> {
        *ticks += 1;
        st.steps += 1;
        if st.steps > MAX_STEPS {
            st.imprecise.push("step budget exhausted".into());
            st.cut.push(node);
            return Err(Side::Done(Box::new(self.finish(Exit::Normal, *st))));
        }
        match self.step(node, st) {
            Step::Fork(v) => {
                let c = v[0]
                    .1
                    .signature
                    .last()
                    .map(|x| x.0.to_string())
                    .unwrap_or_default();
                Ok((c, v))
            }
            Step::Go(n, s) => Err(self.to_decision(n, s, ticks)),
            Step::Done(o) => Err(Side::Done(o)),
            Step::Pruned(..) => unreachable!("`step` always has room"),
        }
    }

    #[allow(clippy::disallowed_methods)] // hash order cannot reach an output: sorted right after
    fn side_key(
        &self,
        node: NodeId,
        st: &State,
        v: Variant,
        live: bool,
        reach: &mut HashMap<NodeId, BTreeSet<NodeId>>,
        out: &mut String,
    ) {
        let State {
            depth,
            caller_depth,
            owner,
            no_commit,
            doomed,
            xact_abort,
            xact_abort_local,
            implicit,
            live: lv,
            log,
            temp,
            scopes,
            savepoints,
            resume,
            caught,
            last_error,
            no_rows,
            entry_depth,
            loop_seen,
            ret,
            raising,
            durability_unknown,
            steps,
            partial: _,
            temp_reads: _,
            raised: _,
            signature: _,
            imprecise: _,
            cut: _,
            rows: _,
            assumptions: _,
            untested: _,
            trace: _,
            faults_done: _,
            before_fault: _,
            faulted: _,
            fault_index: _,
            fault_route: _,
            // Which statement a pending row-count test reads is a position,
            // like the step counter: it would split every path and is left
            // out of the measurement key.
            rows_stmt: _,
            rows_failed: _,
        } = st;
        let abs = v.abstract_log && v.agree && !live;
        let _ = write!(
            out,
            "{node}|{depth}|{caller_depth}|{owner:?}|{no_commit}|{doomed}|{xact_abort}|{xact_abort_local}|{implicit}|",
        );
        if abs {
            let sc: Vec<_> = scopes.iter().map(|x| x.0).collect();
            let sp: Vec<_> = savepoints.iter().map(|x| &x.0).collect();
            let _ = write!(out, "abs|{sc:?}|{sp:?}|");
        } else {
            let _ = write!(out, "{log:?}|{scopes:?}|{savepoints:?}|");
        }
        let _ = write!(
            out,
            "{temp:?}|{resume:?}|{caught:?}|{last_error:?}|{no_rows:?}|{entry_depth}|{ret:?}|{raising:?}|{durability_unknown:?}|",
        );
        if v.steps {
            let _ = write!(out, "{steps}|");
        }
        let mut ls: Vec<(&NodeId, &Vec<String>)> = loop_seen.iter().collect();
        ls.sort();
        if v.deadloop {
            let r = if resume.is_empty() {
                reach
                    .entry(node)
                    .or_insert_with(|| self.cfg.reachable_from(&[node]))
                    .clone()
            } else {
                let mut from = vec![node];
                from.extend(resume.iter().copied());
                self.cfg.reachable_from(&from)
            };
            ls.retain(|(id, _)| r.contains(id));
        }
        let _ = write!(out, "{ls:?}|");
        if live {
            let _ = write!(out, "{lv:?}");
        }
    }
}

/// The condition as the two sides are matched on it: `pairs`' folding, and
/// with `lenient` also the spelling differences of a conversion (local and
/// parameter prefixes, underscores, case, casts and function spellings).
pub(crate) fn canon(c: &str, lenient: bool) -> String {
    let n = crate::ir::behaviour::norm_cond(c);
    if !lenient {
        return n;
    }
    const NOISE: [&str; 12] = [
        "int", "integer", "bigint", "varchar", "text", "cast", "as", "extract", "from", "isnull",
        "coalesce", "datepart",
    ];
    let mut toks: Vec<String> = Vec::new();
    for w in n.split(|ch: char| !(ch.is_alphanumeric() || ch == '_')) {
        let w = w
            .strip_prefix("v_")
            .or_else(|| w.strip_prefix("p_"))
            .unwrap_or(w);
        let w: String = w.chars().filter(|ch| *ch != '_').collect();
        if w.is_empty() || NOISE.contains(&w.as_str()) {
            continue;
        }
        toks.push(w);
    }
    toks.join(" ")
}

fn h128(s: &str) -> u128 {
    let (mut a, mut b) = (0xcbf29ce484222325u64, 0x84222325cbf29ce4u64);
    for x in s.bytes() {
        a = (a ^ u64::from(x)).wrapping_mul(0x100000001b3);
        b = (b ^ u64::from(x)).wrapping_mul(0x1000193).rotate_left(5);
    }
    (u128::from(a) << 64) | u128::from(b)
}

struct Frame {
    key: u128,
    kids: Vec<(Side, Side)>,
    next: usize,
    sum: u128,
}

/// Measures the lockstep exploration of `ca` and `cb` under `sa` and `sb`.
/// `max_states` caps distinct pair-states.
pub fn measure(
    ca: &Cfg,
    cb: &Cfg,
    keys: &KeyUniverse,
    sa: &Scenario,
    sb: &Scenario,
    v: Variant,
    max_states: usize,
) -> Measure {
    let ks = Keys::in_universe(keys);
    let sa = Scenario {
        faults: Vec::new(),
        ..sa.clone()
    };
    let sb = Scenario {
        faults: Vec::new(),
        ..sb.clone()
    };
    let ia = Interp {
        cfg: ca,
        keys: &ks,
        prof: Profile::of(ca.dialect),
        sc: &sa,
    };
    let ib = Interp {
        cfg: cb,
        keys: &ks,
        prof: Profile::of(cb.dialect),
        sc: &sb,
    };
    let conds = |c: &Cfg| -> HashSet<String> {
        c.nodes
            .iter()
            .filter_map(|n| match &n.op {
                Op::Test {
                    test: Test::Unknown(t),
                    ..
                } => Some(canon(t, v.lenient)),
                _ => None,
            })
            .collect()
    };
    let (conds_a, conds_b) = (conds(ca), conds(cb));
    let mut m = Measure::default();
    let mut ticks = 0u64;
    let mut memo: HashMap<u128, u128> = HashMap::new();
    let mut leaves: HashSet<u128> = HashSet::new();
    let mut per_join: BTreeMap<(NodeId, NodeId), usize> = BTreeMap::new();
    let mut singles_a: HashSet<u128> = HashSet::new();
    let mut singles_b: HashSet<u128> = HashSet::new();
    let mut per_a: BTreeMap<NodeId, usize> = BTreeMap::new();
    let mut per_b: BTreeMap<NodeId, usize> = BTreeMap::new();
    let mut reach_a = HashMap::new();
    let mut reach_b = HashMap::new();

    let first = |i: &Interp, ticks: &mut u64| {
        let st = i.start();
        let (n, s) = st.work.into_iter().next().unwrap();
        i.to_decision(n, s, ticks)
    };
    let root = (first(&ia, &mut ticks), first(&ib, &mut ticks));

    // Classifies a pair: a leaf (paths 1), a memo hit, or a new frame.
    let mut stack: Vec<Frame> = Vec::new();
    let mut result: Option<u128> = None;
    let mut pending = Some(root);
    let mut capped = false;
    loop {
        if let Some((a, b)) = pending.take() {
            let r: Option<u128> = match (a, b) {
                (Side::Done(oa), Side::Done(ob)) => {
                    let mut s = format!("{:?}|{:?}|", oa.exit, ob.exit);
                    let mut names: BTreeSet<&Rc<str>> = BTreeSet::new();
                    names.extend(oa.effects.iter().map(|x| x.0));
                    names.extend(ob.effects.iter().map(|x| x.0));
                    for k in names {
                        let _ = write!(s, "{}", u8::from(oa.effects.get(k) == ob.effects.get(k)));
                    }
                    leaves.insert(h128(&s));
                    Some(1)
                }
                (a, b) => {
                    let mut ka = String::new();
                    let mut kb = String::new();
                    let (na, nb) = (
                        if let Side::At(n, st) = &a {
                            ia.side_key(*n, st, v, true, &mut reach_a, &mut ka);
                            Some(*n)
                        } else {
                            None
                        },
                        if let Side::At(n, st) = &b {
                            ib.side_key(*n, st, v, true, &mut reach_b, &mut kb);
                            Some(*n)
                        } else {
                            None
                        },
                    );
                    if let Some(n) = na {
                        if singles_a.insert(h128(&ka)) {
                            *per_a.entry(n).or_default() += 1;
                        }
                    }
                    if let Some(n) = nb {
                        if singles_b.insert(h128(&kb)) {
                            *per_b.entry(n).or_default() += 1;
                        }
                    }
                    let mut key = String::new();
                    let part = |s: &Side,
                                i: &Interp,
                                reach: &mut HashMap<NodeId, BTreeSet<NodeId>>,
                                full: &str,
                                out: &mut String| match s {
                        Side::At(n, st) => {
                            if v.agree {
                                i.side_key(*n, st, v, false, reach, out)
                            } else {
                                out.push_str(full)
                            }
                        }
                        Side::Done(o) => {
                            let _ = write!(out, "done:{:?}:{:?}", o.exit, o.effects);
                        }
                    };
                    part(&a, &ia, &mut reach_a, &ka, &mut key);
                    key.push('#');
                    part(&b, &ib, &mut reach_b, &kb, &mut key);
                    if v.agree {
                        key.push('#');
                        match (&a, &b) {
                            (Side::At(_, x), Side::At(_, y)) => {
                                let same_log = |x: &State, y: &State| {
                                    x.log.len() == y.log.len()
                                        && x.log.iter().zip(&y.log).all(|(p, q)| match (p, q) {
                                            (Log::Effect(i), Log::Effect(j)) => i == j,
                                            (Log::Temp(s, a), Log::Temp(t, c)) => s == t && a == c,
                                            _ => false,
                                        })
                                        && x.scopes
                                            .iter()
                                            .map(|m| m.1)
                                            .eq(y.scopes.iter().map(|m| m.1))
                                        && x.savepoints
                                            .iter()
                                            .map(|m| m.1)
                                            .eq(y.savepoints.iter().map(|m| m.1))
                                };
                                let la = v.abstract_log && same_log(x, y);
                                if v.abstract_log {
                                    let _ = write!(key, "L{}|", u8::from(la));
                                    if !la {
                                        let _ = write!(key, "{:?}{:?}|", x.log, y.log);
                                    }
                                }
                                // Slot by slot over the keys either side ran
                                // (the others are `None` on both sides and
                                // equal).
                                let (mut ix, mut iy) =
                                    (x.live.iter().peekable(), y.live.iter().peekable());
                                loop {
                                    let k = match (ix.peek(), iy.peek()) {
                                        (None, None) => break,
                                        (Some(&(i, _)), Some(&(j, _))) => i.min(j),
                                        (Some(&(i, _)), None) => i,
                                        (None, Some(&(j, _))) => j,
                                    };
                                    let p = ix.next_if(|&(i, _)| i == k).map(|(_, l)| l);
                                    let q = iy.next_if(|&(j, _)| j == k).map(|(_, l)| l);
                                    // Only the slots that are not plainly equal are
                                    // written, with their index: a slot equal on both
                                    // sides (or `None` on both) says nothing.
                                    if (matches!(p, Some(Live::Pending))
                                        || matches!(q, Some(Live::Pending)))
                                        && !(la && p == q)
                                    {
                                        let _ = write!(key, "{k}:{p:?}/{q:?},");
                                    } else if p != q {
                                        let _ = write!(key, "{k}:0,");
                                    }
                                }
                            }
                            // One side has ended: its fates are final, the
                            // other side's slots are compared exactly.
                            (Side::At(_, x), Side::Done(_)) | (Side::Done(_), Side::At(_, x)) => {
                                let _ = write!(key, "{:?}", x.live);
                            }
                            _ => {}
                        }
                    }
                    let k = h128(&key);
                    if let Some(&p) = memo.get(&k) {
                        Some(p)
                    } else if stack.iter().any(|f| f.key == k) {
                        m.desync += 1;
                        Some(1)
                    } else if memo.len() + stack.len() >= max_states {
                        capped = true;
                        Some(1)
                    } else {
                        let ca_ = match &a {
                            Side::At(n, _) => match &ia.cfg.node(*n).op {
                                Op::Test {
                                    test: Test::Unknown(t),
                                    ..
                                } => canon(t, v.lenient),
                                _ => String::new(),
                            },
                            _ => String::new(),
                        };
                        let cb_ = match &b {
                            Side::At(n, _) => match &ib.cfg.node(*n).op {
                                Op::Test {
                                    test: Test::Unknown(t),
                                    ..
                                } => canon(t, v.lenient),
                                _ => String::new(),
                            },
                            _ => String::new(),
                        };
                        // Which side(s) decide next: both on the same
                        // condition; else the side whose condition the
                        // other never decides (a decision of one side only
                        // forks that side alone, the other waits).
                        #[derive(PartialEq)]
                        enum Who {
                            Both,
                            A,
                            B,
                        }
                        let who = match (&a, &b) {
                            (Side::At(..), Side::Done(_)) => Who::A,
                            (Side::Done(_), Side::At(..)) => Who::B,
                            _ if ca_ == cb_ => Who::Both,
                            _ if !conds_b.contains(&ca_) => Who::A,
                            _ if !conds_a.contains(&cb_) => Who::B,
                            _ => Who::A,
                        };
                        if who != Who::Both {
                            m.desync += 1;
                            if m.why.is_none() {
                                m.why = Some(format!("first solo fork: A `{ca_}` / B `{cb_}`"));
                            }
                        }
                        let one_sided = matches!(
                            (&a, &b),
                            (Side::At(..), Side::Done(_)) | (Side::Done(_), Side::At(..))
                        );
                        if one_sided {
                            let solo_a = matches!(a, Side::At(..));
                            let (side, other, i_) = if solo_a {
                                (a.clone(), b.clone(), &ia)
                            } else {
                                (b.clone(), a.clone(), &ib)
                            };
                            let Side::At(n, st) = side else {
                                unreachable!()
                            };
                            let pair = |x: Side| {
                                if solo_a {
                                    (x, other.clone())
                                } else {
                                    (other.clone(), x)
                                }
                            };
                            let kids = match i_.decide(n, st, &mut ticks) {
                                Ok((_, v2)) => v2
                                    .into_iter()
                                    .map(|(n1, s1)| pair(i_.to_decision(n1, s1, &mut ticks)))
                                    .collect(),
                                Err(x) => vec![pair(x)],
                            };
                            *per_join
                                .entry((na.unwrap_or(usize::MAX), nb.unwrap_or(usize::MAX)))
                                .or_default() += 1;
                            stack.push(Frame {
                                key: k,
                                kids,
                                next: 0,
                                sum: 0,
                            });
                            None
                        } else {
                            let (Some(nodea), Some(nodeb)) = (na, nb) else {
                                unreachable!()
                            };
                            let (Side::At(_, sa_), Side::At(_, sb_)) = (a.clone(), b.clone())
                            else {
                                unreachable!()
                            };
                            *per_join.entry((nodea, nodeb)).or_default() += 1;
                            let kids: Vec<(Side, Side)> = match who {
                                Who::Both => {
                                    let da = ia.decide(nodea, sa_, &mut ticks);
                                    let db = ib.decide(nodeb, sb_, &mut ticks);
                                    match (da, db) {
                                        (Ok((_, va)), Ok((_, vb))) => va
                                            .into_iter()
                                            .zip(vb)
                                            .map(|((n1, s1), (n2, s2))| {
                                                (
                                                    ia.to_decision(n1, s1, &mut ticks),
                                                    ib.to_decision(n2, s2, &mut ticks),
                                                )
                                            })
                                            .collect(),
                                        (Err(x), Err(y)) => vec![(x, y)],
                                        (Ok((_, va)), Err(y)) => va
                                            .into_iter()
                                            .map(|(n1, s1)| {
                                                (ia.to_decision(n1, s1, &mut ticks), y.clone())
                                            })
                                            .collect(),
                                        (Err(x), Ok((_, vb))) => vb
                                            .into_iter()
                                            .map(|(n2, s2)| {
                                                (x.clone(), ib.to_decision(n2, s2, &mut ticks))
                                            })
                                            .collect(),
                                    }
                                }
                                Who::A => match ia.decide(nodea, sa_, &mut ticks) {
                                    Ok((_, va)) => va
                                        .into_iter()
                                        .map(|(n1, s1)| {
                                            (ia.to_decision(n1, s1, &mut ticks), b.clone())
                                        })
                                        .collect(),
                                    Err(x) => vec![(x, b.clone())],
                                },
                                Who::B => match ib.decide(nodeb, sb_, &mut ticks) {
                                    Ok((_, vb)) => vb
                                        .into_iter()
                                        .map(|(n2, s2)| {
                                            (a.clone(), ib.to_decision(n2, s2, &mut ticks))
                                        })
                                        .collect(),
                                    Err(y) => vec![(a.clone(), y)],
                                },
                            };
                            stack.push(Frame {
                                key: k,
                                kids,
                                next: 0,
                                sum: 0,
                            });
                            None
                        }
                    }
                }
            };
            if r.is_some() {
                result = r;
            }
        }
        if let Some(r) = result.take() {
            match stack.last_mut() {
                None => {
                    m.paths = (!capped).then_some(r);
                    break;
                }
                Some(f) => f.sum = f.sum.saturating_add(r),
            }
        }
        let Some(f) = stack.last_mut() else {
            break;
        };
        if f.next < f.kids.len() {
            let (a, b) = std::mem::replace(
                &mut f.kids[f.next],
                (
                    Side::Done(Box::new(Outcome::empty())),
                    Side::Done(Box::new(Outcome::empty())),
                ),
            );
            f.next += 1;
            pending = Some((a, b));
        } else {
            let f = stack.pop().unwrap();
            memo.insert(f.key, f.sum);
            result = Some(f.sum);
        }
    }
    m.states = memo.len();
    m.leaves = leaves.len();
    m.ticks = ticks;
    m.joins = per_join.len();
    m.max_per_join = per_join.values().copied().max().unwrap_or(0);
    m.single_a = singles_a.len();
    m.single_b = singles_b.len();
    m.max_single = per_a
        .values()
        .chain(per_b.values())
        .copied()
        .max()
        .unwrap_or(0);
    m.capped = capped;
    m
}
