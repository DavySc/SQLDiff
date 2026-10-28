//! Which packs a view or run loaded, as the pages confirm it: a shared pack
//! by name, version and path; a private pack (configured `private_packs`,
//! and every `--pack`) by path only, as the settings page shows private
//! config. Nothing else from a pack reaches the page.

use serde::{Deserialize, Serialize};
use sqldiff_source::config::LayerKind;
use sqldiff_source::Resolved;

use crate::browse;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct LoadedPack {
    /// Loaded as a private pack: `private_packs` in a config file, or `--pack`.
    pub private: bool,
    /// A shared pack's name and version; `None` for a private pack.
    pub name: Option<String>,
    pub version: Option<String>,
    /// The `pack.toml` read, absolute.
    pub path: String,
}

/// The packs `cfg` loaded, in merge order (lowest first).
pub fn loaded(cfg: &Resolved) -> Vec<LoadedPack> {
    cfg.packs_by_kind()
        .map(|(kind, p)| {
            let shared = kind == LayerKind::SharedPack;
            LoadedPack {
                private: !shared,
                name: shared.then(|| p.meta.name.clone()),
                version: shared.then(|| p.meta.version.clone()),
                path: browse::absolute(&p.path).display().to_string(),
            }
        })
        .collect()
}
