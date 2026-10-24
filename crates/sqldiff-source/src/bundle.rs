//! Snapshot bundles (design sections 3 and 5.4): an immutable,
//! content-hashed capture of a source. One canonical JSON file serves as
//! the offline mode, the exchange format and the test fixture format.
//! The format is documented in `docs/snapshot-bundle.md`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::LayerRef;
use crate::protocol::{
    ArtifactData, Catalog, CatalogScope, Composition, Describe, HistoryEntry, ObjectFilter,
    ObjectInfo, Pair, PairScope, Provenance, Settings, TemplateInfo,
};
use crate::source::Source;
use crate::walk::sha256_hex;
use crate::{pairing, Error, Resolved, Result};

pub const FORMAT: &str = "sqldiff-bundle";
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    pub format: String,
    pub format_version: u32,
    /// SHA-256 of the canonical bundle with `snapshot_id` empty and
    /// `taken_at` absent. Findings cite it.
    pub snapshot_id: String,
    /// When the snapshot was taken; not part of the snapshot id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taken_at: Option<String>,
    /// The adapter that produced the snapshot.
    pub source: Describe,
    /// Packs and config in force when the snapshot was taken.
    #[serde(default)]
    pub provenance: Vec<LayerRef>,
    /// Sorted by id.
    pub objects: Vec<BundleObject>,
    /// Sorted.
    #[serde(default)]
    pub pairs: Vec<Pair>,
    #[serde(default)]
    pub catalog: Catalog,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub history: BTreeMap<String, Vec<HistoryEntry>>,
    #[serde(default)]
    pub compositions: BTreeMap<String, Composition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleObject {
    pub info: ObjectInfo,
    /// SHA-256 of the exact text bytes; `info.bytes` is their count.
    pub sha256: String,
    #[serde(default)]
    pub provenance: Provenance,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<TemplateInfo>,
    pub text: String,
}

impl BundleObject {
    pub fn new(data: ArtifactData) -> BundleObject {
        let mut info = data.info;
        info.bytes = data.text.len();
        info.content_hash = sqldiff_core::artifact::content_hash(&data.text);
        BundleObject {
            sha256: sha256_hex(data.text.as_bytes()),
            info,
            provenance: data.provenance,
            metadata: data.metadata,
            template: data.template,
            text: data.text,
        }
    }

    pub fn data(&self) -> ArtifactData {
        ArtifactData {
            info: self.info.clone(),
            text: self.text.clone(),
            provenance: self.provenance.clone(),
            metadata: self.metadata.clone(),
            template: self.template.clone(),
        }
    }
}

/// What to capture besides objects and pairs.
#[derive(Debug, Clone, Default)]
pub struct CaptureOptions {
    pub filter: ObjectFilter,
    pub history: bool,
    pub taken_at: Option<String>,
}

impl Bundle {
    /// A bundle of the given objects. Call [`Bundle::seal`] after changing it.
    pub fn new(source: Describe, objects: Vec<ArtifactData>) -> Bundle {
        let mut b = Bundle {
            format: FORMAT.to_string(),
            format_version: FORMAT_VERSION,
            snapshot_id: String::new(),
            taken_at: None,
            source,
            provenance: Vec::new(),
            objects: objects.into_iter().map(BundleObject::new).collect(),
            pairs: Vec::new(),
            catalog: Catalog::default(),
            settings: Settings::new(),
            history: BTreeMap::new(),
            compositions: BTreeMap::new(),
        };
        b.seal();
        b
    }

    /// Captures everything a source offers.
    pub fn capture(src: &dyn Source, config: &Resolved, opts: &CaptureOptions) -> Result<Bundle> {
        let describe = src.describe()?;
        let has = |m: &str| describe.capabilities.iter().any(|c| c == m);
        let mut objects = Vec::new();
        for info in src.list_objects(&opts.filter)? {
            objects.push(src.get_artifact(&info.id)?);
        }
        let mut b = Bundle::new(describe.clone(), objects);
        b.provenance = config.layers.clone();
        b.taken_at = opts.taken_at.clone();
        let ids: Vec<String> = b.objects.iter().map(|o| o.info.id.clone()).collect();
        if has("get_pairs") {
            b.pairs = src
                .get_pairs(&PairScope::default())?
                .into_iter()
                .filter(|p| ids.contains(&p.a) && ids.contains(&p.b))
                .collect();
        }
        if has("get_catalog") {
            b.catalog = src.get_catalog(&CatalogScope::default())?;
        }
        if has("get_settings") {
            b.settings = src.get_settings(&CatalogScope::default())?;
        }
        for id in &ids {
            if opts.history && has("get_history") {
                let h = src.get_history(id)?;
                if !h.is_empty() {
                    b.history.insert(id.clone(), h);
                }
            }
            if has("get_composition") {
                if let Some(c) = src.get_composition(id)? {
                    b.compositions.insert(id.clone(), c);
                }
            }
        }
        b.seal();
        Ok(b)
    }

    /// Sorts, recomputes object hashes and sets the snapshot id.
    pub fn seal(&mut self) {
        self.objects.sort_by(|a, b| a.info.id.cmp(&b.info.id));
        self.pairs.sort();
        self.pairs.dedup();
        for o in &mut self.objects {
            o.info.bytes = o.text.len();
            o.info.content_hash = sqldiff_core::artifact::content_hash(&o.text);
            o.sha256 = sha256_hex(o.text.as_bytes());
        }
        self.snapshot_id = self.compute_id();
    }

    fn compute_id(&self) -> String {
        let mut body = self.clone();
        body.snapshot_id = String::new();
        body.taken_at = None;
        let v = serde_json::to_value(&body).expect("bundle serializes");
        sha256_hex(v.to_string().as_bytes())
    }

    /// The canonical text: sorted keys, two-space indent, LF, final newline.
    pub fn to_canonical_json(&self) -> String {
        let v = serde_json::to_value(self).expect("bundle serializes");
        let mut s = serde_json::to_string_pretty(&v).expect("bundle serializes");
        s.push('\n');
        s
    }

    pub fn parse(text: &str) -> Result<Bundle> {
        let b: Bundle = serde_json::from_str(text)?;
        b.verify()?;
        Ok(b)
    }

    pub fn load(path: &Path) -> Result<Bundle> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
        Bundle::parse(&text).map_err(|e| e.context(path.display()))
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        std::fs::write(path, self.to_canonical_json())
            .map_err(|e| Error::new(format!("{}: {e}", path.display())))
    }

    /// Checks format, version, every object's length and hash, and the snapshot id.
    pub fn verify(&self) -> Result<()> {
        if self.format != FORMAT {
            return Err(Error::new(format!(
                "not a snapshot bundle (format `{}`)",
                self.format
            )));
        }
        if self.format_version > FORMAT_VERSION {
            return Err(Error::new(format!(
                "bundle format version {} is newer than this build reads ({FORMAT_VERSION})",
                self.format_version
            )));
        }
        let mut prev: Option<&str> = None;
        for o in &self.objects {
            let id = o.info.id.as_str();
            if prev.is_some_and(|p| p >= id) {
                return Err(Error::new(format!(
                    "objects not sorted or duplicated at `{id}`"
                )));
            }
            prev = Some(id);
            if o.text.len() != o.info.bytes || sha256_hex(o.text.as_bytes()) != o.sha256 {
                return Err(Error::new(format!(
                    "{id}: text does not match its length or hash"
                )));
            }
            if sqldiff_core::artifact::content_hash(&o.text) != o.info.content_hash {
                return Err(Error::new(format!("{id}: content hash mismatch")));
            }
        }
        if self.compute_id() != self.snapshot_id {
            return Err(Error::new("snapshot id does not match the bundle contents"));
        }
        Ok(())
    }

    fn object(&self, id: &str) -> Option<&BundleObject> {
        self.objects
            .binary_search_by(|o| o.info.id.as_str().cmp(id))
            .ok()
            .map(|i| &self.objects[i])
    }
}

/// Snapshot-bundle reference adapter.
pub struct BundleSource {
    bundle: Bundle,
    config: Resolved,
}

impl BundleSource {
    pub fn open(path: &Path, config: &Resolved) -> Result<BundleSource> {
        Ok(BundleSource::new(Bundle::load(path)?, config))
    }

    pub fn new(bundle: Bundle, config: &Resolved) -> BundleSource {
        BundleSource {
            bundle,
            config: config.clone(),
        }
    }

    pub fn bundle(&self) -> &Bundle {
        &self.bundle
    }
}

impl Source for BundleSource {
    fn describe(&self) -> Result<Describe> {
        Ok(Describe {
            name: "bundle".to_string(),
            version: sqldiff_core::TOOL_VERSION.to_string(),
            protocol: crate::protocol::PROTOCOL_VERSION,
            capabilities: [
                "list_objects",
                "get_artifact",
                "get_pairs",
                "get_composition",
                "get_catalog",
                "get_history",
                "get_settings",
            ]
            .map(String::from)
            .to_vec(),
            source: format!(
                "snapshot {} of {} ({})",
                &self.bundle.snapshot_id[..12.min(self.bundle.snapshot_id.len())],
                self.bundle.source.name,
                self.bundle.source.source
            ),
        })
    }

    fn list_objects(&self, filter: &ObjectFilter) -> Result<Vec<ObjectInfo>> {
        Ok(self
            .bundle
            .objects
            .iter()
            .map(|o| o.info.clone())
            .filter(|o| filter.matches(o))
            .collect())
    }

    fn get_artifact(&self, id: &str) -> Result<ArtifactData> {
        self.bundle
            .object(id)
            .map(BundleObject::data)
            .ok_or_else(|| Error::new(format!("{id}: not in snapshot")))
    }

    /// The pairs captured with the snapshot; if none were, pairs by the
    /// current pack rules.
    fn get_pairs(&self, scope: &PairScope) -> Result<Vec<Pair>> {
        let objects = self.list_objects(&ObjectFilter::default())?;
        if self.bundle.pairs.is_empty() {
            return Ok(pairing::pair(&objects, &self.config, scope)?.pairs);
        }
        let dialect = |id: &str| {
            objects
                .iter()
                .find(|o| o.id == id)
                .and_then(|o| o.dialect.clone())
        };
        let norm = |d: &Option<String>| {
            d.as_deref()
                .and_then(|d| d.parse::<sqldiff_core::Dialect>().ok())
                .map(|d| d.id().to_string())
        };
        let (sa, sb) = (norm(&scope.a_dialect), norm(&scope.b_dialect));
        Ok(self
            .bundle
            .pairs
            .iter()
            .filter(|p| sa.is_none() || dialect(&p.a) == sa)
            .filter(|p| sb.is_none() || dialect(&p.b) == sb)
            .cloned()
            .collect())
    }

    fn get_composition(&self, id: &str) -> Result<Option<Composition>> {
        Ok(self.bundle.compositions.get(id).cloned())
    }

    fn get_catalog(&self, _scope: &CatalogScope) -> Result<Catalog> {
        Ok(self.bundle.catalog.clone())
    }

    fn get_history(&self, id: &str) -> Result<Vec<HistoryEntry>> {
        Ok(self.bundle.history.get(id).cloned().unwrap_or_default())
    }

    fn get_settings(&self, _scope: &CatalogScope) -> Result<Settings> {
        Ok(self.bundle.settings.clone())
    }
}
