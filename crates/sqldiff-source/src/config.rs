//! Layered configuration (design section 5.1), lowest to highest precedence:
//! built-in defaults, shared packs, private packs, project config, user
//! overrides, command line.
//!
//! Project config is `sqldiff.toml` in the working directory or an ancestor.
//! User config is `$SQLDIFF_CONFIG`, else `config/config.toml` beside the
//! executable (a distribution's bundled config), else
//! `<config dir>/sqldiff/config.toml`.
//! Both hold the [`Policy`] sections plus the keys of [`ConfigFile`]. Pack
//! paths are relative to the file that lists them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};
use sqldiff_core::format::{FormatConfig, FormatKeys};
use sqldiff_core::Dialect;

use crate::glob::Glob;
use crate::pack::{
    parse_dialect, reject_secrets, AdapterSpec, AuthorMarker, ChangePattern, LimitRule, Override,
    Pack, PairingRule, Policy,
};
use crate::source::SourceSpec;
use crate::{Error, Result};

pub const PROJECT_FILE: &str = "sqldiff.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerKind {
    Builtin,
    SharedPack,
    PrivatePack,
    Project,
    User,
    CommandLine,
}

impl LayerKind {
    /// The kind as JSON spells it: `shared_pack`, `project`, ...
    pub fn label(self) -> &'static str {
        match self {
            LayerKind::Builtin => "builtin",
            LayerKind::SharedPack => "shared_pack",
            LayerKind::PrivatePack => "private_pack",
            LayerKind::Project => "project",
            LayerKind::User => "user",
            LayerKind::CommandLine => "command_line",
        }
    }
}

/// Names and hashes of what produced a result, for the provenance bar and exports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerRef {
    pub kind: LayerKind,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Content hash of the file, or of the tool version for built-ins.
    pub hash: String,
}

/// The `[format]` keys one layer set; layers merge in the order of
/// [`Resolved::format_layers`].
#[derive(Debug, Clone, PartialEq)]
pub struct FormatLayer {
    pub kind: LayerKind,
    /// The file the layer was read from; `None` for the command line.
    pub path: Option<PathBuf>,
    pub format: FormatConfig,
}

/// Where the effective value of one format key comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySource {
    /// Index into the layers; `None`: the built-in default.
    pub layer: Option<usize>,
    /// Set in the dialect's own table (`[format.postgres]`), not in `[format]`.
    pub dialect_table: bool,
}

/// Keys a config file has beyond the policy sections.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    /// Shared (public or team) packs, lowest precedence first.
    #[serde(default)]
    pub packs: Vec<PathBuf>,
    /// Organisation-specific packs, loaded by path, above shared packs.
    #[serde(default)]
    pub private_packs: Vec<PathBuf>,
    /// Cache, snapshots and the review store; outside any repository.
    #[serde(default)]
    pub data_dir: Option<PathBuf>,
    /// Default source when the command line names none.
    #[serde(default)]
    pub source: Option<SourceSpec>,
    #[serde(default)]
    pub excerpt_lines: Option<usize>,
    /// Key for encryption at rest of the data directory: `env:NAME` or
    /// `file:PATH`. A reference, never the key itself.
    #[serde(default)]
    pub data_key: Option<String>,
}

/// A parsed config file: its own keys plus its policy.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigLayer {
    pub file: ConfigFile,
    pub policy: Policy,
    pub path: Option<PathBuf>,
    pub hash: String,
}

const CONFIG_KEYS: &[&str] = &[
    "packs",
    "private_packs",
    "data_dir",
    "source",
    "excerpt_lines",
    "data_key",
];

impl ConfigLayer {
    pub fn load(path: &Path) -> Result<ConfigLayer> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
        let mut layer = ConfigLayer::parse(&text).map_err(|e| e.context(path.display()))?;
        // Relative paths are relative to the file that names them.
        let base = path.parent().unwrap_or(Path::new("."));
        for p in layer
            .file
            .packs
            .iter_mut()
            .chain(layer.file.private_packs.iter_mut())
        {
            if p.is_relative() {
                *p = base.join(&*p);
            }
        }
        if let Some(d) = layer.file.data_dir.as_mut() {
            if d.is_relative() {
                *d = base.join(&*d);
            }
        }
        if let Some(s) = layer.file.source.as_mut() {
            s.rebase(base);
        }
        layer.path = Some(path.to_path_buf());
        Ok(layer)
    }

    pub fn parse(text: &str) -> Result<ConfigLayer> {
        let mut table: toml::Table = toml::from_str(text)?;
        reject_secrets(&toml::Value::Table(table.clone()), "")?;
        let mut own = toml::Table::new();
        for k in CONFIG_KEYS {
            if let Some(v) = table.remove(*k) {
                own.insert(k.to_string(), v);
            }
        }
        let file: ConfigFile = toml::Value::Table(own).try_into()?;
        let policy: Policy = toml::Value::Table(table).try_into()?;
        policy.validate()?;
        if let Some(SourceSpec::Adapter {
            credential: Some(c),
            ..
        }) = &file.source
        {
            crate::pack::check_credential_ref(c)?;
        }
        if let Some(k) = &file.data_key {
            let ok = matches!(k.split_once(':'), Some(("env" | "file", rest)) if !rest.is_empty());
            if !ok {
                return Err(Error::new(format!(
                    "data_key `{k}`: expected env:NAME or file:PATH, a reference, never the key"
                )));
            }
        }
        Ok(ConfigLayer {
            file,
            policy,
            path: None,
            hash: sqldiff_core::artifact::content_hash(text),
        })
    }
}

/// Where to find each layer. Missing optional files are skipped; named
/// files that are missing are errors.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    /// Explicit project config; otherwise searched from `cwd` upward.
    pub project: Option<PathBuf>,
    /// Explicit user config; otherwise the default location.
    pub user: Option<PathBuf>,
    /// Skip the project and user files (tests, reproducible runs).
    pub isolated: bool,
    pub cwd: Option<PathBuf>,
    /// `--pack` paths, loaded as private packs after configured ones.
    pub packs: Vec<PathBuf>,
    /// The command-line layer.
    pub cli: ConfigLayer,
}

/// The merged configuration, with every layer that contributed.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub layers: Vec<LayerRef>,
    pub policy: Policy,
    pub packs: Vec<Pack>,
    pub data_dir: PathBuf,
    pub source: Option<SourceSpec>,
    pub excerpt_lines: usize,
    /// Reference to the data key, if the data directory is encrypted.
    pub data_key: Option<String>,
    /// `[format]` of each layer that was read, in merge order (lowest first).
    pub format_layers: Vec<FormatLayer>,
    /// The proof gate (decision 3); a front end sets the checker.
    pub proof: sqldiff_core::proof::Settings,
    /// The `[deployment]` tables of every layer, with where each entry
    /// comes from (decision 14).
    pub deployment: crate::deployment::Profile,
    /// The layer of each of `packs`, in the same order.
    pack_kinds: Vec<LayerKind>,
    compiled: Compiled,
}

#[derive(Debug, Clone, Default)]
struct Compiled {
    ignore: Vec<Glob>,
    shared: Vec<Glob>,
    dialect_map: Vec<(Glob, Dialect, Option<String>)>,
    rules: Vec<(PairingRule, Option<Dialect>, Option<Glob>)>,
    overrides: Vec<(Glob, Override)>,
    change_patterns: Vec<(String, Regex)>,
}

impl Resolved {
    /// Engine options from this configuration: excerpt cap, declared facts (a
    /// declared Db2 for i commit level is used by the analysis) and pack helper
    /// functions for the built-in catalog.
    pub fn options(&self) -> sqldiff_core::Options {
        sqldiff_core::Options {
            excerpt_lines: self.excerpt_lines,
            declared: self.policy.assumptions.clone(),
            helpers: self
                .policy
                .helpers
                .iter()
                .filter_map(|h| {
                    Some(sqldiff_core::catalog::builtins::Helper {
                        name: h.name.clone(),
                        dialect: h.dialect.parse().ok()?,
                        returns: h.returns.clone(),
                    })
                })
                .collect(),
            proof: self.proof.clone(),
            observer: self.policy.observer.unwrap_or_default(),
            ..sqldiff_core::Options::default()
        }
    }

    /// Built-in defaults only: what the core runs with when no pack is loaded.
    pub fn builtin() -> Resolved {
        Resolved::from_layers(Vec::new(), Vec::new()).expect("built-in config is valid")
    }

    /// Loads and merges every layer.
    pub fn load(sources: &Sources) -> Result<Resolved> {
        Resolved::load_with(sources, true)
    }

    /// [`Resolved::load`], but the project config is only read when
    /// `sources.project` names it: no search upward from the working
    /// directory. For a folder that has no `sqldiff.toml` yet.
    pub fn load_without_search(sources: &Sources) -> Result<Resolved> {
        Resolved::load_with(sources, false)
    }

    fn load_with(sources: &Sources, search_project: bool) -> Result<Resolved> {
        let cwd = match &sources.cwd {
            Some(c) => c.clone(),
            None => std::env::current_dir()?,
        };
        let mut configs: Vec<(LayerKind, ConfigLayer)> = Vec::new();
        if let Some(p) = &sources.project {
            configs.push((LayerKind::Project, ConfigLayer::load(p)?));
        } else if !sources.isolated && search_project {
            if let Some(p) = find_project(&cwd) {
                configs.push((LayerKind::Project, ConfigLayer::load(&p)?));
            }
        }
        if let Some(p) = &sources.user {
            configs.push((LayerKind::User, ConfigLayer::load(p)?));
        } else if !sources.isolated {
            if let Some(p) = user_config_path().filter(|p| p.is_file()) {
                configs.push((LayerKind::User, ConfigLayer::load(&p)?));
            }
        }
        configs.push((LayerKind::CommandLine, sources.cli.clone()));

        let mut shared = Vec::new();
        let mut private = Vec::new();
        for (_, c) in &configs {
            shared.extend(c.file.packs.iter().cloned());
            private.extend(c.file.private_packs.iter().cloned());
        }
        private.extend(sources.packs.iter().cloned());
        let mut packs = Vec::new();
        for (kind, paths) in [
            (LayerKind::SharedPack, shared),
            (LayerKind::PrivatePack, private),
        ] {
            for p in paths {
                let pack = Pack::load(&p)?;
                if packs
                    .iter()
                    .any(|(_, q): &(LayerKind, Pack)| q.meta.name == pack.meta.name)
                {
                    return Err(Error::new(format!(
                        "pack `{}` is loaded twice ({})",
                        pack.meta.name,
                        p.display()
                    )));
                }
                packs.push((kind, pack));
            }
        }
        Resolved::from_layers(packs, configs)
    }

    fn from_layers(
        packs: Vec<(LayerKind, Pack)>,
        configs: Vec<(LayerKind, ConfigLayer)>,
    ) -> Result<Resolved> {
        let mut layers = vec![LayerRef {
            kind: LayerKind::Builtin,
            name: "builtin".to_string(),
            version: Some(sqldiff_core::TOOL_VERSION.to_string()),
            hash: sqldiff_core::artifact::content_hash(&format!(
                "sqldiff {}",
                sqldiff_core::TOOL_VERSION
            )),
        }];
        let mut policy = Policy::default();
        let mut format_layers = Vec::new();
        let mut deployment = crate::deployment::Profile::default();
        for (kind, p) in &packs {
            if let Some(d) = &p.policy.deployment {
                deployment.add(
                    d,
                    sqldiff_core::model::DeploymentSource {
                        layer: kind.label().to_string(),
                        pack: Some(p.meta.name.clone()),
                        file: Some(p.path.display().to_string()),
                    },
                );
            }
            format_layers.push(FormatLayer {
                kind: *kind,
                path: Some(p.path.clone()),
                format: p.policy.format.clone(),
            });
            layers.push(LayerRef {
                kind: *kind,
                name: p.meta.name.clone(),
                version: Some(p.meta.version.clone()),
                hash: p.hash.clone(),
            });
            merge(&mut policy, &p.policy);
        }
        let mut file = ConfigFile::default();
        for (kind, c) in &configs {
            if *kind == LayerKind::CommandLine
                && c.policy == Policy::default()
                && c.file == ConfigFile::default()
            {
                continue;
            }
            layers.push(LayerRef {
                kind: *kind,
                name: match &c.path {
                    Some(p) => p.display().to_string(),
                    None => format!("{kind:?}").to_lowercase(),
                },
                version: None,
                hash: c.hash.clone(),
            });
            merge(&mut policy, &c.policy);
            if let Some(d) = &c.policy.deployment {
                deployment.add(
                    d,
                    sqldiff_core::model::DeploymentSource {
                        layer: kind.label().to_string(),
                        pack: None,
                        file: c.path.as_ref().map(|p| p.display().to_string()),
                    },
                );
            }
            format_layers.push(FormatLayer {
                kind: *kind,
                path: c.path.clone(),
                format: c.policy.format.clone(),
            });
            if c.file.data_dir.is_some() {
                file.data_dir = c.file.data_dir.clone();
            }
            if c.file.source.is_some() {
                file.source = c.file.source.clone();
            }
            if c.file.excerpt_lines.is_some() {
                file.excerpt_lines = c.file.excerpt_lines;
            }
            if c.file.data_key.is_some() {
                file.data_key = c.file.data_key.clone();
            }
        }
        let compiled = compile(&policy)?;
        let pack_kinds = packs.iter().map(|(k, _)| *k).collect();
        Ok(Resolved {
            layers,
            policy,
            packs: packs.into_iter().map(|(_, p)| p).collect(),
            pack_kinds,
            data_dir: file.data_dir.unwrap_or_else(default_data_dir),
            source: file.source,
            excerpt_lines: file.excerpt_lines.unwrap_or(3),
            data_key: file.data_key,
            format_layers,
            proof: sqldiff_core::proof::Settings::default(),
            deployment,
            compiled,
        })
    }

    /// The loaded packs with their layer: [`LayerKind::SharedPack`] or
    /// [`LayerKind::PrivatePack`] (configured `private_packs` and `--pack`).
    pub fn packs_by_kind(&self) -> impl Iterator<Item = (LayerKind, &Pack)> {
        self.pack_kinds.iter().copied().zip(&self.packs)
    }

    /// The `[format]` layers with the project layer's keys replaced by
    /// `draft`, or, when no project config was read, with `draft` added as
    /// that layer (above the packs, below the user config).
    pub fn format_layers_with_project(&self, draft: &FormatConfig) -> Vec<FormatLayer> {
        let mut layers = self.format_layers.clone();
        match layers.iter_mut().find(|l| l.kind == LayerKind::Project) {
            Some(l) => l.format = draft.clone(),
            None => {
                let at = layers
                    .iter()
                    .position(|l| l.kind > LayerKind::Project)
                    .unwrap_or(layers.len());
                layers.insert(
                    at,
                    FormatLayer {
                        kind: LayerKind::Project,
                        path: None,
                        format: draft.clone(),
                    },
                );
            }
        }
        layers
    }

    /// True if a filesystem or git source should skip this path.
    pub fn is_ignored(&self, path: &str) -> bool {
        crate::glob::any(&self.compiled.ignore, path)
    }

    pub fn is_shared(&self, id: &str) -> bool {
        crate::glob::any(&self.compiled.shared, id)
    }

    /// Dialect and version declared for a path by `dialect_map`.
    pub fn mapped_dialect(&self, path: &str) -> Option<(Dialect, Option<String>)> {
        self.compiled
            .dialect_map
            .iter()
            .find(|(g, _, _)| g.is_match(path))
            .map(|(_, d, v)| (*d, v.clone()))
    }

    /// Target version for a dialect: declared, else the dialect default.
    pub fn dialect_version(&self, d: Dialect) -> String {
        self.policy
            .dialect_versions
            .iter()
            .find(|(k, _)| k.parse::<Dialect>().ok() == Some(d))
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| d.default_version().to_string())
    }

    /// The pairing rule for an object; the built-in default if none matches.
    pub fn pairing_rule(&self, id: &str, dialect: Option<Dialect>) -> PairingRule {
        self.compiled
            .rules
            .iter()
            .find(|(_, d, g)| {
                d.map_or(true, |d| Some(d) == dialect)
                    && g.as_ref().map_or(true, |g| g.is_match(id))
            })
            .map(|(r, _, _)| r.clone())
            .unwrap_or_default()
    }

    /// The first override whose glob matches a finding rule id.
    pub fn override_for(&self, rule: &str) -> Option<&Override> {
        self.compiled
            .overrides
            .iter()
            .find(|(g, _)| g.is_match(rule))
            .map(|(_, o)| o)
    }

    /// Work-item identifiers found in a change description.
    pub fn work_items(&self, description: &str) -> Vec<String> {
        let mut out = Vec::new();
        for (_, re) in &self.compiled.change_patterns {
            for c in re.captures_iter(description) {
                let m = c
                    .get(1)
                    .or_else(|| c.get(0))
                    .map(|m| m.as_str().to_string());
                if let Some(m) = m {
                    if !out.contains(&m) {
                        out.push(m);
                    }
                }
            }
        }
        out
    }

    pub fn adapter(&self, name: &str) -> Option<&AdapterSpec> {
        self.policy.adapters.iter().find(|a| a.name == name)
    }
}

/// `[format]` of `layers` merged, lowest layer first, as [`Resolved::policy`]
/// has it.
pub fn merge_format(layers: &[FormatLayer]) -> FormatConfig {
    let mut merged = FormatConfig::default();
    for l in layers {
        merged.merge(&l.format);
    }
    merged
}

/// Where each format key gets its value, by key name as the config file
/// spells it. For a dialect: the last layer that set the key in the dialect's
/// own table, else the last that set it in `[format]`, else the built-in
/// default, which is the merge [`FormatConfig::options`] makes. With `None`,
/// only `[format]` counts.
pub fn format_sources(
    layers: &[FormatLayer],
    dialect: Option<Dialect>,
) -> BTreeMap<String, KeySource> {
    let mut out: BTreeMap<String, KeySource> = FormatKeys::from_options(&Default::default())
        .names()
        .into_iter()
        .map(|k| {
            let s = KeySource {
                layer: None,
                dialect_table: false,
            };
            (k, s)
        })
        .collect();
    for (i, l) in layers.iter().enumerate() {
        for k in l.format.all.names() {
            let s = KeySource {
                layer: Some(i),
                dialect_table: false,
            };
            out.insert(k, s);
        }
    }
    for (i, l) in layers.iter().enumerate() {
        let own = match dialect {
            None => continue,
            Some(Dialect::Tsql) => &l.format.tsql,
            Some(Dialect::Postgres) => &l.format.postgres,
            Some(Dialect::Db2i) => &l.format.db2i,
        };
        for k in own.names() {
            let s = KeySource {
                layer: Some(i),
                dialect_table: true,
            };
            out.insert(k, s);
        }
    }
    out
}

/// Merges `hi` (higher precedence) into `acc`.
fn merge(acc: &mut Policy, hi: &Policy) {
    // Ordered rule lists: the higher layer's rules are tried first.
    prepend(&mut acc.pairing.rules, &hi.pairing.rules);
    prepend(&mut acc.dialect_map, &hi.dialect_map);
    prepend(&mut acc.overrides, &hi.overrides);
    // Additive lists.
    acc.pairing.pairs.extend(hi.pairing.pairs.iter().cloned());
    acc.pairing.shared.extend(hi.pairing.shared.iter().cloned());
    acc.ignore.extend(hi.ignore.iter().cloned());
    acc.composition.extend(hi.composition.iter().cloned());
    extend_unique(
        &mut acc.author_markers,
        &hi.author_markers,
        |m: &AuthorMarker| m.pattern.clone(),
    );
    extend_unique(
        &mut acc.change_patterns,
        &hi.change_patterns,
        |c: &ChangePattern| c.name.clone(),
    );
    for t in &hi.deny_list {
        if !acc.deny_list.contains(t) {
            acc.deny_list.push(t.clone());
        }
    }
    // Keyed lists and maps: the higher layer replaces by key.
    replace_by(&mut acc.limits, &hi.limits, |l: &LimitRule| l.id.clone());
    replace_by(&mut acc.adapters, &hi.adapters, |a: &AdapterSpec| {
        a.name.clone()
    });
    replace_by(&mut acc.helpers, &hi.helpers, |h| {
        (h.dialect.clone(), h.name.to_lowercase())
    });
    acc.assumptions.extend(hi.assumptions.clone());
    acc.dialect_versions.extend(hi.dialect_versions.clone());
    if hi.template.is_some() {
        acc.template = hi.template.clone();
    }
    if hi.observer.is_some() {
        acc.observer = hi.observer;
    }
    // `[deployment]`: accept lists join, facts are replaced by key.
    if let Some(d) = &hi.deployment {
        let acc = acc.deployment.get_or_insert_with(Default::default);
        for id in &d.accept {
            if !acc.accept.contains(id) {
                acc.accept.push(id.clone());
            }
        }
        acc.facts.extend(d.facts.clone());
    }
    // Format keys: the higher layer replaces key by key.
    acc.format.merge(&hi.format);
}

fn prepend<T: Clone>(acc: &mut Vec<T>, hi: &[T]) {
    let mut v = hi.to_vec();
    v.append(acc);
    *acc = v;
}

fn extend_unique<T: Clone, K: PartialEq>(acc: &mut Vec<T>, hi: &[T], key: impl Fn(&T) -> K) {
    for x in hi {
        if !acc.iter().any(|y| key(y) == key(x)) {
            acc.push(x.clone());
        }
    }
}

fn replace_by<T: Clone, K: PartialEq>(acc: &mut Vec<T>, hi: &[T], key: impl Fn(&T) -> K) {
    for x in hi {
        match acc.iter_mut().find(|y| key(y) == key(x)) {
            Some(y) => *y = x.clone(),
            None => acc.push(x.clone()),
        }
    }
}

fn compile(p: &Policy) -> Result<Compiled> {
    let globs = |v: &[String]| v.iter().map(|g| Glob::new(g)).collect::<Result<Vec<_>>>();
    Ok(Compiled {
        ignore: globs(&p.ignore)?,
        shared: globs(&p.pairing.shared)?,
        dialect_map: p
            .dialect_map
            .iter()
            .map(|m| {
                Ok((
                    Glob::new(&m.glob)?,
                    parse_dialect(&m.dialect, &m.glob)?,
                    m.version.clone(),
                ))
            })
            .collect::<Result<_>>()?,
        rules: p
            .pairing
            .rules
            .iter()
            .map(|r| {
                Ok((
                    r.clone(),
                    r.dialect
                        .as_deref()
                        .map(|d| parse_dialect(d, &r.name))
                        .transpose()?,
                    r.path.as_deref().map(Glob::new).transpose()?,
                ))
            })
            .collect::<Result<_>>()?,
        overrides: p
            .overrides
            .iter()
            .map(|o| Ok((Glob::new(&o.rule)?, o.clone())))
            .collect::<Result<_>>()?,
        change_patterns: p
            .change_patterns
            .iter()
            .map(|c| Ok((c.name.clone(), Regex::new(&c.pattern)?)))
            .collect::<Result<_>>()?,
    })
}

/// `sqldiff.toml` in `cwd` or the nearest ancestor that has one.
pub fn find_project(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .map(|d| d.join(PROJECT_FILE))
        .find(|p| p.is_file())
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Folder beside the executables that holds a distribution's bundled config
/// (`sqldiff dist`); never part of a build or a release archive.
pub const BUNDLED_DIR: &str = "config";

/// `config/config.toml` beside the running executable, if it exists.
pub fn bundled_config_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // A symlink on PATH points at the real install; look beside that.
    let exe = exe.canonicalize().unwrap_or(exe);
    let p = exe.parent()?.join(BUNDLED_DIR).join("config.toml");
    p.is_file().then_some(p)
}

/// `$SQLDIFF_CONFIG`, else the bundled config beside the executable, else
/// the platform config directory.
pub fn user_config_path() -> Option<PathBuf> {
    if let Some(p) = env_path("SQLDIFF_CONFIG") {
        return Some(p);
    }
    if let Some(p) = bundled_config_path() {
        return Some(p);
    }
    let dir = if cfg!(windows) {
        env_path("APPDATA")
    } else {
        env_path("XDG_CONFIG_HOME").or_else(|| env_path("HOME").map(|h| h.join(".config")))
    }?;
    Some(dir.join("sqldiff").join("config.toml"))
}

/// `$SQLDIFF_DATA_DIR`, else the platform data directory.
pub fn default_data_dir() -> PathBuf {
    if let Some(p) = env_path("SQLDIFF_DATA_DIR") {
        return p;
    }
    let dir = if cfg!(windows) {
        env_path("LOCALAPPDATA")
    } else {
        env_path("XDG_DATA_HOME")
            .or_else(|| env_path("HOME").map(|h| h.join(".local").join("share")))
    };
    dir.unwrap_or_else(std::env::temp_dir).join("sqldiff")
}

/// Builds the command-line layer from `--assume ID=TEXT` and
/// `--dialect-version DIALECT=VERSION` style pairs.
pub fn cli_layer(
    assumptions: &[(String, String)],
    versions: &[(String, String)],
    excerpt_lines: Option<usize>,
) -> Result<ConfigLayer> {
    let mut policy = Policy::default();
    for (k, v) in assumptions {
        policy.assumptions.insert(k.clone(), v.clone());
    }
    for (d, v) in versions {
        let dialect = parse_dialect(d, "--dialect-version")?;
        crate::pack::check_version(dialect, v, "--dialect-version")?;
        policy.dialect_versions.insert(d.clone(), v.clone());
    }
    let file = ConfigFile {
        excerpt_lines,
        ..ConfigFile::default()
    };
    let text = format!(
        "{:?}{:?}{:?}",
        policy.assumptions, policy.dialect_versions, excerpt_lines
    );
    Ok(ConfigLayer {
        file,
        policy,
        path: None,
        hash: sqldiff_core::artifact::content_hash(&text),
    })
}

/// Serializable summary for `sqldiff config` and the provenance bar.
pub fn summary(r: &Resolved) -> serde_json::Value {
    serde_json::json!({
        "layers": r.layers,
        "data_dir": r.data_dir,
        "source": r.source,
        "excerpt_lines": r.excerpt_lines,
        "data_key": r.data_key,
        "policy": r.policy,
        "dialect_versions": Dialect::ALL
            .iter()
            .map(|d| (d.id().to_string(), r.dialect_version(*d)))
            .collect::<BTreeMap<_, _>>(),
    })
}
