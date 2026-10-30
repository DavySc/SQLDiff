//! Calls to converted user functions, checked against the callee's own
//! conversion (burndown X).
//!
//! The engine compares a call to a scalar user function inside an
//! expression as the same call on both sides, under `ASM-UDF-CALLEE:name`:
//! the function on side B is the conversion of the one on side A. Within a
//! converted tree that is checked here, after every file is converted: the
//! callee must be created by exactly one file of the tree, not take part in
//! a cycle of such calls, and that file must be converted or under
//! assumptions. Then the calling units also carry the callee file's
//! assumptions. Otherwise the calling units are not proven, and the note
//! names the callee and why.
//!
//! A unit is a calling unit when its source text calls the function by
//! name; when no unit of the file does (the name written in a form this
//! does not read), every unit with the assumption counts as one.

use std::collections::{BTreeMap, BTreeSet};

use sqldiff_core::catalog::{ddl, RoutineKind};
use sqldiff_core::Dialect;

use crate::tree::Input;
use crate::{Conversion, Note, Status};

/// Prefix of the engine's assumption ids for a called user function.
pub const UDF_CALLEE: &str = "ASM-UDF-CALLEE:";

/// Rule of the note on a call whose callee's conversion does not hold.
pub const CALLEE_RULE: &str = "equiv.udf-callee";

/// Resolves every `ASM-UDF-CALLEE:name` in `convs` (parallel to `inputs`).
pub fn resolve(inputs: &[Input], convs: &mut [&mut Conversion], from: Dialect) {
    if from != Dialect::Tsql {
        return;
    }
    // The scalar functions each file creates, by the engine's key.
    let mut defs: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, input) in inputs.iter().enumerate() {
        if !input.text.to_ascii_lowercase().contains("function") {
            continue;
        }
        let cat = ddl::load(&input.text, from, &input.rel);
        for r in cat
            .routines
            .iter()
            .filter(|r| r.kind == RoutineKind::Function)
        {
            defs.entry(key(r.schema.as_deref(), &r.name))
                .or_default()
                .push(i);
        }
    }
    let wanted = |c: &Conversion| -> BTreeSet<String> {
        c.units
            .iter()
            .flat_map(|u| &u.notes)
            .filter_map(|n| n.rule.strip_prefix(UDF_CALLEE).map(str::to_string))
            .collect()
    };
    let deps: Vec<BTreeSet<String>> = convs.iter().map(|c| wanted(c)).collect();
    // Files whose calls lead back to themselves.
    let callee_file = |k: &str| match defs.get(k).map(Vec::as_slice) {
        Some([j]) => Some(*j),
        _ => None,
    };
    let cyclic: Vec<bool> = (0..convs.len())
        .map(|start| {
            let mut seen = BTreeSet::new();
            let mut todo: Vec<usize> = deps[start].iter().filter_map(|k| callee_file(k)).collect();
            while let Some(j) = todo.pop() {
                if j == start {
                    return true;
                }
                if seen.insert(j) {
                    todo.extend(deps[j].iter().filter_map(|k| callee_file(k)));
                }
            }
            false
        })
        .collect();
    // Statuses only get worse and notes are only added, so this ends.
    loop {
        let mut changed = false;
        for i in 0..convs.len() {
            for k in &deps[i] {
                let outcome = match defs.get(k).map(Vec::as_slice) {
                    None | Some([]) => Err(format!(
                        "the user function {k} is not created by a file of this conversion, so its own conversion was not compared"
                    )),
                    Some([j]) if cyclic[i] && cyclic[*j] => Err(format!(
                        "the user function {k} ({}) is called in a cycle of calls between converted functions, which is not proven",
                        inputs[*j].rel
                    )),
                    Some([j]) => {
                        let c = &*convs[*j];
                        let worst = file_status(c);
                        if worst <= Status::Assumptions {
                            Ok(carried(c))
                        } else {
                            Err(format!(
                                "the user function {k}'s own conversion ({}) is {}, so the call is not proven either",
                                inputs[*j].rel,
                                worst.label()
                            ))
                        }
                    }
                    Some(js) => Err(format!(
                        "the user function {k} is created by {} files of this conversion",
                        js.len()
                    )),
                };
                let c = &mut *convs[i];
                let at = calling_units(&inputs[i].text, c, k, outcome.is_err());
                for u in at {
                    let u = &mut c.units[u];
                    match &outcome {
                        Ok(notes) => {
                            for n in notes {
                                if !u.notes.iter().any(|x| x.rule == n.rule) {
                                    u.status = u.status.max(Status::Assumptions);
                                    let p = u
                                        .notes
                                        .partition_point(|x| x.status >= Status::Assumptions);
                                    u.notes.insert(p, n.clone());
                                    changed = true;
                                }
                            }
                        }
                        Err(why) => {
                            let text = format!("The engine did not prove this call: {why}");
                            if !u.notes.iter().any(|x| x.text == text) {
                                u.status = u.status.max(Status::Unproven);
                                u.checked = u.checked.max(Status::Unproven);
                                let p = u.notes.partition_point(|x| x.status >= Status::Unproven);
                                u.notes.insert(
                                    p,
                                    Note {
                                        status: Status::Unproven,
                                        rule: CALLEE_RULE.into(),
                                        text: text.into(),
                                    },
                                );
                                changed = true;
                            }
                        }
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    for c in convs.iter_mut() {
        crate::recount(c);
    }
}

/// The engine's key of a function name: lower case, without the default
/// schema (`dbo`).
fn key(schema: Option<&str>, name: &str) -> String {
    match schema.map(str::to_lowercase) {
        Some(s) if s != "dbo" => format!("{s}.{}", name.to_lowercase()),
        _ => name.to_lowercase(),
    }
}

/// The worst status in a converted file, counting what names no unit.
fn file_status(c: &Conversion) -> Status {
    let mut worst = c
        .units
        .iter()
        .map(|u| u.status)
        .max()
        .unwrap_or(Status::Converted);
    if !c.unproven.is_empty() || !c.unplaced.is_empty() {
        worst = worst.max(Status::Unproven);
    }
    worst
}

/// The assumption notes of a converted file, once each.
fn carried(c: &Conversion) -> Vec<Note> {
    let mut out: Vec<Note> = Vec::new();
    for n in c.units.iter().flat_map(|u| &u.notes) {
        if n.status == Status::Assumptions && !out.iter().any(|x| x.rule == n.rule) {
            out.push(n.clone());
        }
    }
    out
}

/// The units that call `k`: the innermost ones whose source text calls it
/// by name, else every unit with its assumption. `wide` (the call is not
/// proven) takes every unit whose text names the function at all, in a
/// comment or a spelling the match above misses (`dbo./**/f(`) too: a unit
/// it wrongly includes is only kept from counting.
fn calling_units(text: &str, c: &Conversion, k: &str, wide: bool) -> Vec<usize> {
    let rule = format!("{UDF_CALLEE}{k}");
    let with: Vec<usize> = (0..c.units.len())
        .filter(|&i| c.units[i].notes.iter().any(|n| n.rule == rule))
        .collect();
    let call = match (wide, k.contains('.')) {
        (true, _) => k.rsplit('.').next().unwrap_or(k).to_string(),
        (false, true) => format!("{k}("),
        (false, false) => format!("dbo.{k}("),
    };
    let calls = |i: usize| {
        let s = &c.units[i].src;
        text.get(s.start..s.end).is_some_and(|t| {
            let flat: String = t
                .chars()
                .filter(|ch| !ch.is_whitespace() && !matches!(ch, '[' | ']' | '"'))
                .flat_map(char::to_lowercase)
                .collect();
            flat.contains(&call)
        })
    };
    let hits: Vec<usize> = with.iter().copied().filter(|&i| calls(i)).collect();
    if hits.is_empty() {
        return with;
    }
    if wide {
        return hits;
    }
    let span = |i: usize| (c.units[i].src.start, c.units[i].src.end);
    hits.iter()
        .copied()
        .filter(|&i| {
            let (a, b) = span(i);
            !hits.iter().any(|&j| {
                let (x, y) = span(j);
                j != i && a <= x && y <= b && (x, y) != (a, b)
            })
        })
        .collect()
}
