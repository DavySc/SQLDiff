//! Settings: the config layers, the effective `[format]` value of each key
//! with the layer it comes from, and an editor for the workspace's
//! `sqldiff.toml` with a live preview.
//!
//! - `GET /settings`: the page (see `app`)
//! - `GET /api/settings?dir=`: the layers and effective values for the
//!   folder (or `sqldiff.toml`) `dir`
//! - `POST /api/settings/preview` `{"token", "dir", "draft", "dialect", "text"}`:
//!   `text` formatted as `sqldiff fmt` would with the draft in place of the
//!   workspace file's `[format]`, and the effective values for that draft
//! - `POST /api/settings/save` `{"token", "dir", "draft", "hash"}`: write the
//!   draft into the workspace `sqldiff.toml`
//!
//! The configuration is loaded, merged and formatted by the library code the
//! command line runs (`Resolved::load`, `format::format`); this module only
//! decides what the page may see and where a save may go.
//!
//! - Writes go to one file: a `sqldiff.toml` in the folder the user named,
//!   never a symlink and never a file that is also another layer. The file is
//!   replaced whole (written beside it, then renamed), and only if it still
//!   has the content the page last read. `[format]` is written by
//!   [`sqldiff_source::format_file::set_format`], which keeps everything else.
//! - Private config is shown by path only. The user config (which can be the
//!   private setup beside the app) and private packs are read to merge the
//!   layers, but nothing from them reaches the page: no value, no pack name,
//!   no parse error text. A key one of them sets shows as "set by", without
//!   its value.
//! - Every POST needs the per-launch token the settings page was served with.
//!   The routes answer only when the app listens on loopback (see `app`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqldiff_core::format::{self, FormatConfig, FormatKeys};
use sqldiff_core::Dialect;
use sqldiff_source::config::{
    self, format_sources, merge_format, FormatLayer, LayerKind, Sources, PROJECT_FILE,
};
use sqldiff_source::format_file::set_format;
use sqldiff_source::Resolved;

use crate::browse;
use crate::server::{bad, json, Request, Response};

/// Largest text the preview formats.
pub const MAX_PREVIEW: usize = 1 << 20;

/// What the routes need to know about the app.
pub struct Ctx<'a> {
    /// The app ignores the user config (tests, reproducible runs).
    pub isolated: bool,
    /// This launch's token.
    pub token: &'a str,
}

/// A new token: 128 random bits from the operating system.
pub fn new_token() -> String {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).expect("the operating system has no random numbers");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The settings routes, if `req` is one.
pub(crate) fn route(req: &Request, ctx: &Ctx) -> Option<Response> {
    Some(match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/api/settings") => {
            let dir = crate::server::param(&req.query, "dir").unwrap_or_default();
            match load(&dir, ctx.isolated).map(|w| w.view()) {
                Ok(v) => json(&v),
                Err(e) => bad(e),
            }
        }
        ("POST", "/api/settings/preview") => post(req, ctx, preview),
        ("POST", "/api/settings/save") => post(req, ctx, save),
        _ => return None,
    })
}

fn post<T: for<'de> Deserialize<'de> + Tokened>(
    req: &Request,
    ctx: &Ctx,
    f: impl FnOnce(T, &Ctx) -> Result<Value, (&'static str, String)>,
) -> Response {
    let body: T = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(e) => return bad(e),
    };
    if !same(body.token(), ctx.token) {
        return (
            "403 Forbidden",
            "text/plain",
            "missing or wrong token: reload the settings page".into(),
        );
    }
    match f(body, ctx) {
        Ok(v) => json(&v),
        Err((status, e)) => (status, "text/plain", e),
    }
}

trait Tokened {
    fn token(&self) -> &str;
}

/// Equal strings, compared without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |d, (x, y)| d | (x ^ y)) == 0
}

const BAD: &str = "400 Bad Request";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preview {
    token: String,
    dir: String,
    draft: FormatConfig,
    dialect: String,
    text: String,
}

impl Tokened for Preview {
    fn token(&self) -> &str {
        &self.token
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Save {
    token: String,
    dir: String,
    draft: FormatConfig,
    /// The hash of the file as the page read it; `None`: it did not exist.
    hash: Option<String>,
}

impl Tokened for Save {
    fn token(&self) -> &str {
        &self.token
    }
}

fn preview(p: Preview, ctx: &Ctx) -> Result<Value, (&'static str, String)> {
    let dialect: Dialect = p
        .dialect
        .parse()
        .map_err(|_| (BAD, format!("unknown dialect `{}`", p.dialect)))?;
    if p.text.len() > MAX_PREVIEW {
        return Err((
            BAD,
            format!("the preview formats up to {} KiB", MAX_PREVIEW >> 10),
        ));
    }
    let w = load(&p.dir, ctx.isolated).map_err(|e| (BAD, e))?;
    let layers = w.layers_with(&p.draft);
    // What `sqldiff fmt` does: the merged `[format]`, then the dialect's options.
    let options = merge_format(&layers).options(dialect);
    let out = format::format(&p.text, dialect, &options);
    let kept: Vec<_> = out
        .kept
        .iter()
        .map(|k| json!({"start_line": k.start_line, "end_line": k.end_line, "reason": k.reason}))
        .collect();
    Ok(json!({
        "text": out.text,
        "changed": out.changed(&p.text),
        "kept": kept,
        "effective": w.effective(&layers),
    }))
}

fn save(s: Save, ctx: &Ctx) -> Result<Value, (&'static str, String)> {
    let w = load(&s.dir, ctx.isolated).map_err(|e| (BAD, e))?;
    w.writable().map_err(|e| ("403 Forbidden", e))?;
    let text = match std::fs::read_to_string(&w.file) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err((BAD, format!("{}: {e}", w.file.display()))),
    };
    let now = text.as_deref().map(sqldiff_core::artifact::content_hash);
    if now != s.hash {
        return Err((
            "409 Conflict",
            format!(
                "{} changed since this page read it; reload the page's settings, then apply the edit again",
                w.file.display()
            ),
        ));
    }
    let new = set_format(text.as_deref().unwrap_or(""), &s.draft, cfg!(windows))
        .map_err(|e| (BAD, e.0))?;
    if text.as_deref() != Some(new.as_str()) {
        write_beside(&w.file, &new).map_err(|e| ("500 Internal Server Error", e))?;
    }
    let w = load(&s.dir, ctx.isolated).map_err(|e| (BAD, e))?;
    Ok(w.view())
}

/// Replaces `file` with `text` by writing a file beside it and renaming that
/// over it, so a failed write leaves the old file.
fn write_beside(file: &Path, text: &str) -> Result<(), String> {
    let tmp = file.with_file_name(format!("{PROJECT_FILE}.{}.tmp", std::process::id()));
    let fail = |e: std::io::Error| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", file.display())
    };
    std::fs::write(&tmp, text).map_err(fail)?;
    if let Ok(m) = std::fs::metadata(file) {
        std::fs::set_permissions(&tmp, m.permissions()).map_err(fail)?;
    }
    std::fs::rename(&tmp, file).map_err(fail)
}

/// The workspace file named by the page and the configuration around it.
struct Workspace {
    file: PathBuf,
    /// The file's text, `None` when it does not exist.
    text: Option<String>,
    cfg: Resolved,
    /// Pack names by index into `cfg.format_layers` (packs come first).
    pack_names: Vec<String>,
}

/// `dir` is a folder (its `sqldiff.toml`) or a `sqldiff.toml`.
fn workspace_file(dir: &str) -> Result<PathBuf, String> {
    if dir.trim().is_empty() {
        return Err("choose the workspace folder".into());
    }
    let p = browse::absolute(Path::new(dir.trim()));
    let file = if p.is_dir() {
        p.join(PROJECT_FILE)
    } else {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let ok = match cfg!(windows) {
            true => name.eq_ignore_ascii_case(PROJECT_FILE),
            false => name == PROJECT_FILE,
        };
        if !ok {
            return Err(format!(
                "{}: settings are saved to a file named {PROJECT_FILE}; choose its folder",
                p.display()
            ));
        }
        p
    };
    match file.parent() {
        Some(d) if d.is_dir() => Ok(file),
        _ => Err(format!("{}: folder not found", file.display())),
    }
}

fn load(dir: &str, isolated: bool) -> Result<Workspace, String> {
    let file = workspace_file(dir)?;
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    let sources = Sources {
        project: text.is_some().then(|| file.clone()),
        user: None,
        isolated,
        cwd: file.parent().map(Path::to_path_buf),
        packs: Vec::new(),
        cli: config::cli_layer(&[], &[], None).map_err(|e| e.0)?,
    };
    if text.is_some() {
        // An error in the workspace file itself is the user's to read; one in
        // another layer may quote a private config, so it is not passed on.
        config::ConfigLayer::load(&file).map_err(|e| e.0)?;
    }
    let cfg = Resolved::load_without_search(&sources).map_err(|_| {
        "another configuration layer (user config or a pack) could not be loaded; \
         run `sqldiff config` to see why"
            .to_string()
    })?;
    let pack_names = cfg.packs.iter().map(|p| p.meta.name.clone()).collect();
    Ok(Workspace {
        file,
        text,
        cfg,
        pack_names,
    })
}

/// A layer whose contents the page never gets.
fn private(l: &FormatLayer) -> bool {
    matches!(l.kind, LayerKind::PrivatePack | LayerKind::User)
}

#[derive(Serialize)]
struct LayerView {
    kind: LayerKind,
    label: String,
    /// Absent for the built-in defaults.
    path: Option<String>,
    /// False: the page gets the path only.
    shown: bool,
    /// Number of `[format]` keys the layer sets (shown layers only).
    format_keys: Option<usize>,
}

fn count(f: &FormatConfig) -> usize {
    [&f.all, &f.tsql, &f.postgres, &f.db2i]
        .iter()
        .map(|k| k.names().len())
        .sum()
}

impl Workspace {
    /// Why the workspace file may not be written, if it may not.
    fn writable(&self) -> Result<(), String> {
        let f = &self.file;
        if std::fs::symlink_metadata(f).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!(
                "{}: is a link; settings are not saved through links",
                f.display()
            ));
        }
        let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| browse::absolute(p));
        let me = canon(f);
        let others = self
            .cfg
            .format_layers
            .iter()
            .filter(|l| l.kind != LayerKind::Project)
            .filter_map(|l| l.path.as_deref())
            .map(canon)
            .chain(config::user_config_path().into_iter().map(|p| canon(&p)));
        for o in others {
            if o == me {
                return Err(format!(
                    "{}: this file is also the user config or a pack; settings are saved only to a workspace {PROJECT_FILE}",
                    f.display()
                ));
            }
        }
        Ok(())
    }

    /// The layers with the workspace file's `[format]` replaced by `draft`.
    fn layers_with(&self, draft: &FormatConfig) -> Vec<FormatLayer> {
        let mut layers = self.cfg.format_layers_with_project(draft);
        layers.retain(|l| l.kind != LayerKind::CommandLine);
        for l in &mut layers {
            if l.kind == LayerKind::Project {
                l.path = Some(self.file.clone());
            }
        }
        layers
    }

    /// The `[format]` keys of the workspace file as it is.
    fn draft(&self) -> FormatConfig {
        self.cfg
            .format_layers
            .iter()
            .find(|l| l.kind == LayerKind::Project)
            .map(|l| l.format.clone())
            .unwrap_or_default()
    }

    /// Layers lowest first, the built-in defaults as index 0; `layers[i + 1]`
    /// is `layers_with(..)[i]`.
    fn layer_views(&self, layers: &[FormatLayer]) -> Vec<LayerView> {
        let mut v = vec![LayerView {
            kind: LayerKind::Builtin,
            label: "Built-in defaults".into(),
            path: None,
            shown: true,
            format_keys: None,
        }];
        let mut packs = self.pack_names.iter();
        for l in layers {
            // `..` in a pack path is resolved by name, so the list reads cleanly.
            let path = l
                .path
                .as_deref()
                .map(|p| browse::absolute(p).display().to_string());
            let shown = !private(l);
            let label = match l.kind {
                LayerKind::SharedPack => {
                    format!("Pack {}", packs.next().map(String::as_str).unwrap_or(""))
                }
                LayerKind::PrivatePack => {
                    packs.next();
                    "Private pack".to_string()
                }
                LayerKind::Project => "Workspace sqldiff.toml".to_string(),
                LayerKind::User => "User config".to_string(),
                LayerKind::Builtin | LayerKind::CommandLine => String::new(),
            };
            v.push(LayerView {
                kind: l.kind,
                label,
                path,
                shown,
                format_keys: shown.then(|| count(&l.format)),
            });
        }
        v
    }

    /// For each scope (`all`, then a dialect id) and key: the effective
    /// value, the layer it comes from (index as in `layer_views`) and
    /// whether that is a dialect's own table. A value from a private layer is
    /// `null` with `"hidden": true`.
    fn effective(&self, layers: &[FormatLayer]) -> Value {
        let merged = merge_format(layers);
        let mut out = serde_json::Map::new();
        let scopes: [(&str, Option<Dialect>); 4] = [
            ("all", None),
            (Dialect::Tsql.id(), Some(Dialect::Tsql)),
            (Dialect::Postgres.id(), Some(Dialect::Postgres)),
            (Dialect::Db2i.id(), Some(Dialect::Db2i)),
        ];
        for (scope, dialect) in scopes {
            let options = match dialect {
                Some(d) => merged.options(d),
                None => {
                    let mut o = format::FormatOptions::default();
                    merged.all.apply(&mut o);
                    o
                }
            };
            let values =
                serde_json::to_value(FormatKeys::from_options(&options)).unwrap_or_default();
            let mut keys = serde_json::Map::new();
            for (key, src) in format_sources(layers, dialect) {
                let hidden = src.layer.is_some_and(|i| private(&layers[i]));
                keys.insert(
                    key.clone(),
                    json!({
                        "value": if hidden { Value::Null } else { values[&key].clone() },
                        "hidden": hidden,
                        "layer": src.layer.map_or(0, |i| i + 1),
                        "dialect_table": src.dialect_table,
                    }),
                );
            }
            out.insert(scope.to_string(), Value::Object(keys));
        }
        Value::Object(out)
    }

    /// What the page shows for the workspace file as it is.
    fn view(&self) -> Value {
        let own = self.draft();
        let layers = self.layers_with(&own);
        json!({
            "dir": self.file.parent().map(|d| d.display().to_string()),
            "file": self.file.display().to_string(),
            "exists": self.text.is_some(),
            "hash": self.text.as_deref().map(sqldiff_core::artifact::content_hash),
            "draft": own,
            "layers": self.layer_views(&layers),
            "effective": self.effective(&layers),
            "writable": self.writable().err(),
        })
    }
}

/// The settings a key has, in the order the page lists them: name, what it
/// holds, its values (or range) and a line of help.
pub struct KeyInfo {
    pub name: &'static str,
    pub kind: &'static str,
    pub options: &'static [&'static str],
    pub min: i64,
    pub max: i64,
    pub help: &'static str,
}

const NONE: &[&str] = &[];

/// `docs/formatter.md`'s keys.
#[rustfmt::skip]
pub const KEYS: [KeyInfo; 12] = [
    KeyInfo { name: "indent", kind: "int", options: NONE, min: 1, max: 16, help: "Spaces per indent level" },
    KeyInfo { name: "indent_style", kind: "enum", options: &["spaces", "tabs"], min: 0, max: 0, help: "Indent with spaces or tabs" },
    KeyInfo { name: "keyword_case", kind: "enum", options: &["upper", "lower", "preserve"], min: 0, max: 0, help: "Letter case of keywords; preserve leaves them as written" },
    KeyInfo { name: "clause_breaks", kind: "bool", options: NONE, min: 0, max: 0, help: "FROM, WHERE, JOIN, GROUP BY … each on their own line" },
    KeyInfo { name: "one_item_per_line", kind: "bool", options: NONE, min: 0, max: 0, help: "Each select-list, SET and ORDER BY item on its own line" },
    KeyInfo { name: "space_after_comma", kind: "bool", options: NONE, min: 0, max: 0, help: "A space after each comma" },
    KeyInfo { name: "space_around_comparisons", kind: "bool", options: NONE, min: 0, max: 0, help: "Spaces around =, <, >= and the like" },
    KeyInfo { name: "max_blank_lines", kind: "int", options: NONE, min: 0, max: 10, help: "Blank lines kept in a row" },
    KeyInfo { name: "line_ending", kind: "enum", options: &["keep", "lf", "crlf"], min: 0, max: 0, help: "Keep is what most of the file uses" },
    KeyInfo { name: "final_newline", kind: "bool", options: NONE, min: 0, max: 0, help: "End the file with a line break" },
    KeyInfo { name: "join_lines", kind: "bool", options: NONE, min: 0, max: 0, help: "Join line breaks inside a statement; off keeps them and only re-indents" },
    KeyInfo { name: "postgres_bodies", kind: "bool", options: NONE, min: 0, max: 0, help: "Also lay out PostgreSQL routine bodies" },
];

/// Text the preview starts with, per dialect.
pub fn sample(d: Dialect) -> &'static str {
    match d {
        Dialect::Tsql => "select o.id,sum(l.qty) as total from dbo.orders o join dbo.lines l on l.oid=o.id\nwhere o.dt>=@from and o.status in (1,2) group by o.id order by total desc;\n\nCREATE PROCEDURE dbo.Ship @id int AS\nBEGIN\nSET NOCOUNT ON;\nupdate dbo.orders set status=3,shipped=getdate() where id=@id;\nIF @@ROWCOUNT=0 BEGIN RAISERROR('no such order',16,1); END\nEND\n",
        Dialect::Postgres => "select o.id,sum(l.qty) as total from orders o join lines l on l.oid=o.id\nwhere o.dt>=$1 and o.status in (1,2) group by o.id order by total desc;\n\ninsert into audit(order_id,at) select id,now() from orders where status=3 on conflict do nothing;\n",
        Dialect::Db2i => "select o.id,sum(l.qty) as total from orders o join lines l on l.oid=o.id\nwhere o.dt>=? and o.status in (1,2) group by o.id order by total desc fetch first 10 rows only;\n\nupdate orders set status=3,shipped=current timestamp where id=?;\n",
    }
}

/// The folder of the project config a command run here would load, to start
/// the page on.
pub fn initial_dir() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    cwd.ancestors()
        .find(|d| d.join(PROJECT_FILE).is_file())
        .map(|d| d.display().to_string())
}

/// The page's payload.
pub fn payload(no_picking: Option<&str>, token: &str) -> Value {
    json!({
        "token": token,
        "no_picking": no_picking,
        "initial_dir": initial_dir(),
        "dialects": Dialect::ALL.iter().map(|d| (d.id(), d.display_name())).collect::<Vec<_>>(),
        "keys": KEYS.iter().map(|k| json!({
            "name": k.name, "kind": k.kind, "options": k.options,
            "min": k.min, "max": k.max, "help": k.help,
        })).collect::<Vec<_>>(),
        "samples": Dialect::ALL.iter().map(|d| (d.id(), sample(*d))).collect::<std::collections::BTreeMap<_, _>>(),
    })
}
