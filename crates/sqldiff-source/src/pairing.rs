//! Pairing by pack rules (design section 3, layer 5 step 1): which object in
//! one dialect corresponds to which in another.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sqldiff_core::Dialect;

use crate::pack::{Case, PairingRule};
use crate::protocol::{ObjectInfo, Pair, PairScope};
use crate::{Error, Resolved, Result};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pairing {
    pub pairs: Vec<Pair>,
    /// Objects with a dialect in scope that found no partner.
    pub unpaired: Vec<String>,
    /// Shared artifacts, never paired.
    pub shared: Vec<String>,
    /// Objects whose dialect is unknown.
    pub no_dialect: Vec<String>,
    /// Several objects of one dialect with the same key; none of them is paired.
    pub ambiguous: Vec<Ambiguity>,
    /// Explicit pairs naming objects the source does not have.
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ambiguity {
    pub dialect: String,
    pub key: String,
    pub ids: Vec<String>,
}

/// Applies a rule to a name.
pub fn key(name: &str, rule: &PairingRule) -> String {
    let fold = |s: &str| match rule.case {
        Case::Lower => s.to_lowercase(),
        Case::Upper => s.to_uppercase(),
        Case::Preserve => s.to_string(),
    };
    let mut k = fold(name);
    if let Some(p) = rule
        .strip_prefix
        .iter()
        .map(|p| fold(p))
        .find(|p| k.starts_with(p.as_str()))
    {
        k = k[p.len()..].to_string();
    }
    if let Some(s) = rule
        .strip_suffix
        .iter()
        .map(|s| fold(s))
        .find(|s| k.ends_with(s.as_str()))
    {
        k.truncate(k.len() - s.len());
    }
    k.chars()
        .filter(|c| !rule.ignore_chars.contains(*c))
        .collect()
}

fn dialect_of(o: &ObjectInfo) -> Option<Dialect> {
    o.dialect.as_deref().and_then(|d| d.parse().ok())
}

fn scope_dialect(s: &Option<String>) -> Result<Option<Dialect>> {
    s.as_deref()
        .map(|d| d.parse::<Dialect>().map_err(Error::new))
        .transpose()
}

/// Pairs objects across dialects. With no scope, every combination of
/// dialects present is paired (T-SQL before PostgreSQL before Db2 for i).
pub fn pair(objects: &[ObjectInfo], config: &Resolved, scope: &PairScope) -> Result<Pairing> {
    let mut out = Pairing::default();
    let sa = scope_dialect(&scope.a_dialect)?;
    let sb = scope_dialect(&scope.b_dialect)?;
    let by_id: BTreeMap<&str, &ObjectInfo> = objects.iter().map(|o| (o.id.as_str(), o)).collect();
    let in_scope = |d: Option<Dialect>| match (sa, sb, d) {
        (None, None, _) => true,
        (_, _, None) => false,
        (a, b, Some(d)) => a == Some(d) || b == Some(d) || (a.is_none() != b.is_none()),
    };

    let mut taken: BTreeSet<&str> = BTreeSet::new();
    for e in &config.policy.pairing.pairs {
        let (Some(a), Some(b)) = (by_id.get(e.a.as_str()), by_id.get(e.b.as_str())) else {
            for id in [&e.a, &e.b] {
                if !by_id.contains_key(id.as_str()) {
                    out.missing.push(id.clone());
                }
            }
            continue;
        };
        taken.insert(&a.id);
        taken.insert(&b.id);
        let (da, db) = (dialect_of(a), dialect_of(b));
        let (a, b) = if sa.is_some() && db == sa || sb.is_some() && da == sb {
            (b, a)
        } else {
            (a, b)
        };
        let (da, db) = (dialect_of(a), dialect_of(b));
        if sa.map_or(true, |s| Some(s) == da) && sb.map_or(true, |s| Some(s) == db) {
            out.pairs.push(Pair {
                a: a.id.clone(),
                b: b.id.clone(),
                key: format!("{} = {}", a.name, b.name),
                rule: "explicit".to_string(),
            });
        }
    }

    // (dialect, key) -> objects
    let mut groups: BTreeMap<(Dialect, String), Vec<&ObjectInfo>> = BTreeMap::new();
    let mut rule_of: BTreeMap<&str, String> = BTreeMap::new();
    for o in objects {
        if taken.contains(o.id.as_str()) {
            continue;
        }
        if config.is_shared(&o.id) {
            out.shared.push(o.id.clone());
            continue;
        }
        let Some(d) = dialect_of(o) else {
            out.no_dialect.push(o.id.clone());
            continue;
        };
        if !in_scope(Some(d)) {
            continue;
        }
        let rule = config.pairing_rule(&o.id, Some(d));
        rule_of.insert(&o.id, rule.name.clone());
        groups.entry((d, key(&o.name, &rule))).or_default().push(o);
    }
    let mut unique: BTreeMap<(Dialect, String), &ObjectInfo> = BTreeMap::new();
    for ((d, k), v) in groups {
        if v.len() == 1 {
            unique.insert((d, k), v[0]);
        } else {
            out.ambiguous.push(Ambiguity {
                dialect: d.id().to_string(),
                key: k,
                ids: v.iter().map(|o| o.id.clone()).collect(),
            });
        }
    }

    let present: BTreeSet<Dialect> = unique.keys().map(|(d, _)| *d).collect();
    let combos: Vec<(Dialect, Dialect)> = match (sa, sb) {
        (Some(a), Some(b)) => vec![(a, b)],
        (Some(a), None) => present
            .iter()
            .filter(|d| **d != a)
            .map(|d| (a, *d))
            .collect(),
        (None, Some(b)) => present
            .iter()
            .filter(|d| **d != b)
            .map(|d| (*d, b))
            .collect(),
        (None, None) => {
            let p: Vec<Dialect> = present.iter().copied().collect();
            let mut v = Vec::new();
            for i in 0..p.len() {
                for j in i + 1..p.len() {
                    v.push((p[i], p[j]));
                }
            }
            v
        }
    };
    let mut paired: BTreeSet<&str> = BTreeSet::new();
    for (da, db) in combos {
        for ((d, k), a) in unique.range((da, String::new())..) {
            if *d != da {
                break;
            }
            if let Some(b) = unique.get(&(db, k.clone())) {
                paired.insert(&a.id);
                paired.insert(&b.id);
                out.pairs.push(Pair {
                    a: a.id.clone(),
                    b: b.id.clone(),
                    key: k.clone(),
                    rule: rule_of.get(a.id.as_str()).cloned().unwrap_or_default(),
                });
            }
        }
    }
    out.unpaired = unique
        .values()
        .filter(|o| !paired.contains(o.id.as_str()))
        .map(|o| o.id.clone())
        .collect();
    out.pairs.sort();
    out.unpaired.sort();
    Ok(out)
}

/// Pairs two snapshots of the same source by object id (version review).
pub fn pair_versions(a: &[ObjectInfo], b: &[ObjectInfo]) -> (Vec<Pair>, Vec<String>, Vec<String>) {
    let ids_b: BTreeSet<&str> = b.iter().map(|o| o.id.as_str()).collect();
    let ids_a: BTreeSet<&str> = a.iter().map(|o| o.id.as_str()).collect();
    let pairs = a
        .iter()
        .filter(|o| ids_b.contains(o.id.as_str()))
        .map(|o| Pair {
            a: o.id.clone(),
            b: o.id.clone(),
            key: o.id.clone(),
            rule: "same-id".to_string(),
        })
        .collect();
    let only_a = ids_a.difference(&ids_b).map(|s| s.to_string()).collect();
    let only_b = ids_b.difference(&ids_a).map(|s| s.to_string()).collect();
    (pairs, only_a, only_b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        let r = PairingRule {
            strip_prefix: vec!["usp_".into(), "sp_".into()],
            strip_suffix: vec!["_pg".into()],
            ..PairingRule::default()
        };
        assert_eq!(key("usp_Get_Stock", &r), "getstock");
        assert_eq!(key("get_stock_pg", &r), "getstock");
        assert_eq!(key("Move-Stock", &PairingRule::default()), "movestock");
    }
}
