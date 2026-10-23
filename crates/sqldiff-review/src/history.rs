//! Change descriptions grouped by work item (design section 5.2, change
//! patterns), from an adapter's `get_history`.

use serde::{Deserialize, Serialize};
use sqldiff_source::protocol::HistoryEntry;
use sqldiff_source::Resolved;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeGroup {
    /// The work item, or `None` for changes that name none.
    pub work_item: Option<String>,
    /// Revisions in the order the source returned them.
    pub revisions: Vec<String>,
    pub descriptions: Vec<String>,
}

/// Groups history entries by work item. Entries without `work_items` get
/// them from the pack change patterns. An entry naming several work items
/// is listed in each group. Groups keep first-seen order; the group of
/// changes without a work item comes last.
pub fn group(entries: &[HistoryEntry], cfg: &Resolved) -> Vec<ChangeGroup> {
    let mut groups: Vec<ChangeGroup> = Vec::new();
    let mut none = ChangeGroup {
        work_item: None,
        revisions: Vec::new(),
        descriptions: Vec::new(),
    };
    for e in entries {
        let items = if e.work_items.is_empty() {
            cfg.work_items(&e.description)
        } else {
            e.work_items.clone()
        };
        if items.is_empty() {
            none.revisions.push(e.revision.clone());
            none.descriptions.push(e.description.clone());
            continue;
        }
        for w in items {
            let g = match groups
                .iter()
                .position(|g| g.work_item.as_deref() == Some(&w))
            {
                Some(i) => &mut groups[i],
                None => {
                    groups.push(ChangeGroup {
                        work_item: Some(w),
                        revisions: Vec::new(),
                        descriptions: Vec::new(),
                    });
                    groups.last_mut().expect("just pushed")
                }
            };
            g.revisions.push(e.revision.clone());
            g.descriptions.push(e.description.clone());
        }
    }
    if !none.revisions.is_empty() {
        groups.push(none);
    }
    groups
}
