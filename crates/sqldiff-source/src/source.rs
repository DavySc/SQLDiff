use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sqldiff_core::Artifact;

use crate::protocol::{
    ArtifactData, Catalog, CatalogScope, Composition, Describe, HistoryEntry, ObjectFilter,
    ObjectInfo, Pair, PairScope, Settings,
};
use crate::{Error, Resolved, Result};

/// A source of artifacts: the methods of the adapter protocol. The
/// reference adapters implement it in process; [`crate::ProcessSource`]
/// implements it by talking to an adapter program.
pub trait Source {
    fn describe(&self) -> Result<Describe>;
    fn list_objects(&self, filter: &ObjectFilter) -> Result<Vec<ObjectInfo>>;
    /// The whole text, verified against the adapter's length and hash.
    fn get_artifact(&self, id: &str) -> Result<ArtifactData>;
    fn get_pairs(&self, scope: &PairScope) -> Result<Vec<Pair>>;
    fn get_composition(&self, _id: &str) -> Result<Option<Composition>> {
        Ok(None)
    }
    fn get_catalog(&self, _scope: &CatalogScope) -> Result<Catalog> {
        Ok(Catalog::default())
    }
    fn get_history(&self, _id: &str) -> Result<Vec<HistoryEntry>> {
        Ok(Vec::new())
    }
    fn get_settings(&self, _scope: &CatalogScope) -> Result<Settings> {
        Ok(Settings::new())
    }

    /// The artifact in the engine's form. Fails if the source does not know
    /// its dialect, since a dialect is never guessed.
    fn load_artifact(&self, id: &str) -> Result<Artifact> {
        let data = self.get_artifact(id)?;
        to_artifact(&data)
    }
}

pub fn to_artifact(data: &ArtifactData) -> Result<Artifact> {
    let dialect = data
        .info
        .dialect
        .as_deref()
        .ok_or_else(|| Error::new(format!("{}: dialect unknown", data.info.id)))?
        .parse()
        .map_err(|e: String| Error::new(format!("{}: {e}", data.info.id)))?;
    let mut a = Artifact::new(data.info.id.clone(), dialect, data.text.clone());
    if let Some(v) = &data.info.version {
        a.version = v.clone();
    }
    // One object of a file with several: its own identity.
    if crate::split::split_id(&data.info.id).1.is_some() {
        a.object = crate::split::created(&a.text, dialect);
    }
    Ok(a)
}

/// Which source to open. In config: a `[source]` table with `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceSpec {
    /// A directory tree.
    Fs { root: PathBuf },
    /// A git repository at a revision, optionally limited to a subdirectory.
    Git {
        repo: PathBuf,
        #[serde(default = "head")]
        rev: String,
        #[serde(default)]
        subdir: Option<String>,
    },
    /// A snapshot bundle file.
    Bundle { path: PathBuf },
    /// An out-of-process adapter declared in a pack or config.
    Adapter {
        name: String,
        /// Credential reference passed to the adapter; never a secret.
        #[serde(default)]
        credential: Option<String>,
    },
}

fn head() -> String {
    "HEAD".to_string()
}

impl SourceSpec {
    /// Parses a command-line source:
    /// - `git:REPO[@REV][//SUBDIR]` for a git revision;
    /// - `adapter:NAME` for a configured adapter;
    /// - a `.json` file for a snapshot bundle;
    /// - any other path for a directory tree.
    pub fn parse(s: &str) -> Result<SourceSpec> {
        if let Some(rest) = s.strip_prefix("git:") {
            let (rest, subdir) = match rest.split_once("//") {
                Some((r, d)) => (r, Some(d.trim_matches('/').to_string())),
                None => (rest, None),
            };
            let (repo, rev) = match rest.rsplit_once('@') {
                Some((r, v)) if !v.is_empty() => (r, v.to_string()),
                _ => (rest, head()),
            };
            let repo = if repo.is_empty() { "." } else { repo };
            return Ok(SourceSpec::Git {
                repo: PathBuf::from(repo),
                rev,
                subdir: subdir.filter(|d| !d.is_empty()),
            });
        }
        if let Some(name) = s.strip_prefix("adapter:") {
            return Ok(SourceSpec::Adapter {
                name: name.to_string(),
                credential: None,
            });
        }
        let p = PathBuf::from(s);
        if p.is_file() {
            Ok(SourceSpec::Bundle { path: p })
        } else if p.is_dir() {
            Ok(SourceSpec::Fs { root: p })
        } else {
            Err(Error::new(format!("{s}: no such file or directory")))
        }
    }

    pub(crate) fn rebase(&mut self, base: &Path) {
        let fix = |p: &mut PathBuf| {
            if p.is_relative() {
                *p = base.join(&*p);
            }
        };
        match self {
            SourceSpec::Fs { root } => fix(root),
            SourceSpec::Git { repo, .. } => fix(repo),
            SourceSpec::Bundle { path } => fix(path),
            SourceSpec::Adapter { .. } => {}
        }
    }
}

/// Opens a source with the given configuration.
pub fn open(spec: &SourceSpec, config: &Resolved) -> Result<Box<dyn Source>> {
    Ok(match spec {
        SourceSpec::Fs { root } => Box::new(crate::FsSource::new(root, config)?),
        SourceSpec::Git { repo, rev, subdir } => {
            Box::new(crate::GitSource::new(repo, rev, subdir.as_deref(), config)?)
        }
        SourceSpec::Bundle { path } => Box::new(crate::BundleSource::open(path, config)?),
        SourceSpec::Adapter { name, credential } => {
            let spec = config.adapter(name).ok_or_else(|| {
                Error::new(format!("no adapter named `{name}` in packs or config"))
            })?;
            Box::new(crate::ProcessSource::spawn(spec, credential.as_deref())?)
        }
    })
}
