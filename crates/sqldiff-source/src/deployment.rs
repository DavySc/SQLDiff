//! The deployment profile (decision 14 in docs/verified-conversion.md): the
//! `[deployment]` tables of every loaded pack and config file, merged with
//! where each entry comes from.
//!
//! Accepting an assumption is a reviewed, reported fact, never a change of
//! verdict: an accepted assumption still makes a result "equal under
//! assumptions" (`Report::settle` is not involved). Reports name the profile
//! that accepted each assumption, and a converted tree says whether every
//! assumption it carries is accepted (`sqldiff_convert::tree`).

use serde::{Deserialize, Serialize};
use sqldiff_core::assumptions;
use sqldiff_core::model::{Assumption, AssumptionSource, DeploymentSource};
use sqldiff_core::Report;

/// One accepted id and every layer that accepts it, lowest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accepted {
    pub id: String,
    pub by: Vec<DeploymentSource>,
}

/// One asserted fact, from the highest layer that sets the key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fact {
    pub key: String,
    pub value: String,
    pub by: DeploymentSource,
}

/// The merged `[deployment]` tables. `accept` lists are joined across
/// layers; a fact key set in a higher layer replaces a lower one's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Profile {
    /// Some layer has a `[deployment]` table (possibly empty).
    pub declared: bool,
    pub accept: Vec<Accepted>,
    pub facts: Vec<Fact>,
}

impl Profile {
    /// Adds one layer's table, above the layers added before it.
    pub fn add(&mut self, d: &crate::pack::Deployment, by: DeploymentSource) {
        self.declared = true;
        for id in &d.accept {
            match self.accept.iter_mut().find(|a| &a.id == id) {
                Some(a) => {
                    if !a.by.contains(&by) {
                        a.by.push(by.clone());
                    }
                }
                None => self.accept.push(Accepted {
                    id: id.clone(),
                    by: vec![by.clone()],
                }),
            }
        }
        for (k, v) in &d.facts {
            let f = Fact {
                key: k.clone(),
                value: v.clone(),
                by: by.clone(),
            };
            match self.facts.iter_mut().find(|f| &f.key == k) {
                Some(x) => *x = f,
                None => self.facts.push(f),
            }
        }
    }

    /// The layers that accept an id a report carries (an accepted id covers
    /// its `:qualified` instances), in layer order without repeats.
    pub fn accepting(&self, id: &str) -> Vec<DeploymentSource> {
        let mut out: Vec<DeploymentSource> = Vec::new();
        for a in &self.accept {
            if assumptions::covers(&a.id, id) {
                for b in &a.by {
                    if !out.contains(b) {
                        out.push(b.clone());
                    }
                }
            }
        }
        out
    }

    /// Whether an assumption is one the profile accepts. Proven facts
    /// (composed output) are not assumptions any more and need no acceptance.
    pub fn accepts(&self, a: &Assumption) -> bool {
        a.source == AssumptionSource::ComposedOutput || !self.accepting(&a.id).is_empty()
    }

    /// Marks every assumption of a report that the profile accepts. The
    /// verdict is untouched.
    pub fn apply(&self, report: &mut Report) {
        for a in &mut report.assumptions {
            a.accepted_by = if a.source == AssumptionSource::ComposedOutput {
                Vec::new()
            } else {
                self.accepting(&a.id)
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::Deployment;

    fn src(name: &str) -> DeploymentSource {
        DeploymentSource {
            layer: "project".into(),
            pack: None,
            file: Some(name.into()),
        }
    }

    #[test]
    fn layers_join_accept_lists_and_replace_facts_by_key() {
        let mut p = Profile::default();
        let lo = Deployment {
            accept: vec!["ASM-OBSERVER-ROWCOUNT".into(), "ASM-BIND-COLUMN".into()],
            facts: [("collation".to_string(), "a".to_string())].into(),
        };
        let hi = Deployment {
            accept: vec!["ASM-OBSERVER-ROWCOUNT".into()],
            facts: [("collation".to_string(), "b".to_string())].into(),
        };
        p.add(&lo, src("lo"));
        p.add(&hi, src("hi"));
        assert_eq!(p.accepting("ASM-OBSERVER-ROWCOUNT"), [src("lo"), src("hi")]);
        assert_eq!(p.accepting("ASM-BIND-COLUMN:qty"), [src("lo")]);
        assert!(p.accepting("ASM-DBO-SCHEMA").is_empty());
        assert_eq!(p.facts.len(), 1);
        assert_eq!(p.facts[0].value, "b");
        assert_eq!(p.facts[0].by, src("hi"));
    }
}
