//! `sqldiff dist`: assemble a ready-to-run folder for colleagues: the
//! executables, a README and, unless `--no-config`, the private config with
//! every pack it names copied into `config/` beside the executables, where
//! they find it without any setup (`config::bundled_config_path`).
//!
//! The release workflow runs it with `--no-config`, so a public archive never
//! holds a config. With a config, the output must lie outside every git work
//! tree: a private bundle is handed over directly, never committed.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args;
use sqldiff_source::config::{self, ConfigLayer, Sources, BUNDLED_DIR};
use sqldiff_source::{Pack, Resolved};

/// Executables a distribution carries, without the platform suffix.
const PROGRAMS: &[&str] = &["sqldiff", "sqldiff-gui", "sqldiff-mcp"];

const README: &str = include_str!("../../../docs/dist/README.md");

const PRIVATE_NOTE: &str = "\
This folder holds your organisation's private SQLDiff configuration.
Do not commit it to any repository, attach it to issues or tickets, or
upload it anywhere public. SQLDiff reads config.toml here automatically;
replace the folder when you are sent a new one.
";

#[derive(Args)]
pub struct DistArgs {
    /// Folder to create (must not exist yet, or be empty).
    #[arg(short, long, value_name = "DIR")]
    output: PathBuf,
    /// Config to bundle (default: the user config this command would load:
    /// $SQLDIFF_CONFIG, the bundled config, or the platform config file).
    #[arg(long, value_name = "FILE", conflicts_with = "no_config")]
    user_config: Option<PathBuf>,
    /// Also bundle this pack, as a private pack. Repeatable.
    #[arg(long = "pack", value_name = "PATH", conflicts_with = "no_config")]
    packs: Vec<PathBuf>,
    /// Bundle no config: a generic distribution (what a release archive holds).
    #[arg(long)]
    no_config: bool,
    /// Folder holding the executables (default: the folder of this executable).
    #[arg(long, value_name = "DIR")]
    from: Option<PathBuf>,
}

pub fn run(args: DistArgs) -> Result<ExitCode, String> {
    let from = match &args.from {
        Some(d) => d.clone(),
        None => {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let exe = exe.canonicalize().unwrap_or(exe);
            exe.parent().ok_or("no executable folder")?.to_path_buf()
        }
    };
    // Resolve the config first: nothing is written when it does not load.
    let bundle = if args.no_config {
        None
    } else {
        Some(plan(&args)?)
    };

    let out = &args.output;
    if out.exists()
        && std::fs::read_dir(out)
            .map_err(|e| format!("{}: {e}", out.display()))?
            .next()
            .is_some()
    {
        return Err(format!("{}: exists and is not empty", out.display()));
    }
    if bundle.is_some() {
        if let Some(repo) = work_tree_of(out) {
            return Err(format!(
                "{}: inside the git work tree {}; a private config is never written \
                 into a repository. Choose a folder outside it.",
                out.display(),
                repo.display()
            ));
        }
    }
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;

    // `--from` may hold another platform's build (a Windows bundle made elsewhere).
    let suffix = if from.join("sqldiff.exe").is_file() && !from.join("sqldiff").is_file() {
        ".exe"
    } else {
        std::env::consts::EXE_SUFFIX
    };
    let mut copied = Vec::new();
    for name in PROGRAMS {
        let file = format!("{name}{suffix}");
        let src = from.join(&file);
        if !src.is_file() {
            if *name == "sqldiff" {
                return Err(format!("{}: not found", src.display()));
            }
            eprintln!("note: {} not found; not included", src.display());
            continue;
        }
        let dst = out.join(&file);
        std::fs::copy(&src, &dst).map_err(|e| format!("{}: {e}", dst.display()))?;
        copied.push(file);
    }
    write(&out.join("README.md"), README)?;

    match bundle {
        None => eprintln!("{}: {} (no config)", out.display(), copied.join(", ")),
        Some(b) => {
            let dir = out.join(BUNDLED_DIR);
            for (name, text) in &b.packs {
                write(&dir.join("packs").join(name).join("pack.toml"), text)?;
            }
            write(&dir.join("config.toml"), &b.config)?;
            write(&dir.join("PRIVATE.txt"), PRIVATE_NOTE)?;
            verify(&b, &dir.join("config.toml"))?;
            for n in &b.notes {
                eprintln!("note: {n}");
            }
            eprintln!(
                "{}: {} with config/ ({} pack(s)). Hand it over directly; do not publish it.",
                out.display(),
                copied.join(", "),
                b.packs.len()
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// What goes into `config/`.
struct Bundle {
    config: String,
    /// Folder name under `packs/` and the pack's text.
    packs: Vec<(String, String)>,
    /// The resolved source config, to compare the bundle against.
    expected: Resolved,
    notes: Vec<String>,
}

fn plan(args: &DistArgs) -> Result<Bundle, String> {
    let user = match &args.user_config {
        Some(p) => Some(p.clone()),
        None => config::user_config_path().filter(|p| p.is_file()),
    };
    if user.is_none() && args.packs.is_empty() {
        return Err(
            "no config to bundle: give --user-config FILE or --pack PATH, \
                    or --no-config for a distribution without one"
                .into(),
        );
    }
    let sources = |user: Option<PathBuf>, packs: Vec<PathBuf>| -> Result<Sources, String> {
        Ok(Sources {
            project: None,
            user,
            isolated: true,
            cwd: None,
            packs,
            cli: config::cli_layer(&[], &[], None).map_err(|e| e.0)?,
        })
    };
    let expected = Resolved::load(&sources(user.clone(), args.packs.clone())?).map_err(|e| e.0)?;

    let mut notes = Vec::new();
    let (mut table, layer) = match &user {
        Some(p) => {
            let text = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
            let table: toml::Table =
                toml::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))?;
            (table, ConfigLayer::load(p).map_err(|e| e.0)?)
        }
        None => (toml::Table::new(), ConfigLayer::default()),
    };

    // Packs by their resolved paths, renamed to packs/<name> in the bundle.
    let mut packs: Vec<(String, String)> = Vec::new();
    let mut add = |path: &Path| -> Result<toml::Value, String> {
        let pack = Pack::load(path).map_err(|e| e.0)?;
        let name = folder_name(&pack.meta.name);
        if packs.iter().any(|(n, _)| *n == name) {
            return Err(format!(
                "two packs map to the folder name `{name}`; rename one of them"
            ));
        }
        let text = std::fs::read_to_string(&pack.path)
            .map_err(|e| format!("{}: {e}", pack.path.display()))?;
        packs.push((name.clone(), text));
        Ok(toml::Value::String(format!("packs/{name}")))
    };
    let shared = layer
        .file
        .packs
        .iter()
        .map(|p| add(p))
        .collect::<Result<Vec<_>, _>>()?;
    let private = layer
        .file
        .private_packs
        .iter()
        .chain(&args.packs)
        .map(|p| add(p))
        .collect::<Result<Vec<_>, _>>()?;
    for (key, list) in [("packs", shared), ("private_packs", private)] {
        if list.is_empty() {
            table.remove(key);
        } else {
            table.insert(key.into(), toml::Value::Array(list));
        }
    }

    // Keys that name places on this machine do not travel.
    if table.remove("data_dir").is_some() {
        notes.push("data_dir left out: each colleague uses their own data directory".into());
    }
    let keep_source = matches!(
        table
            .get("source")
            .and_then(|s| s.get("kind"))
            .and_then(|k| k.as_str()),
        Some("adapter")
    );
    if !keep_source && table.remove("source").is_some() {
        notes.push(
            "[source] left out: it names a folder, repository or file on this machine".into(),
        );
    }
    if let Some(k) = table.get("data_key").and_then(|k| k.as_str()) {
        if k.starts_with("file:") {
            table.remove("data_key");
            notes.push("data_key left out: a file: reference names a file on this machine".into());
        }
    }
    for a in &expected.policy.adapters {
        if let Some(cmd) = a.command.first() {
            if cmd.contains('/') || cmd.contains('\\') {
                notes.push(format!(
                    "adapter `{}` runs {cmd}; that program must exist at that path on each \
                     colleague's machine",
                    a.name
                ));
            }
        }
    }

    let body = toml::to_string(&table).map_err(|e| e.to_string())?;
    let config = format!(
        "# Bundled by `sqldiff dist` ({}). Private: never commit or publish this file.\n\n{body}",
        sqldiff_core::TOOL_VERSION
    );
    Ok(Bundle {
        config,
        packs,
        expected,
        notes,
    })
}

/// The bundle must load to the same packs and policy as its source.
fn verify(b: &Bundle, config: &Path) -> Result<(), String> {
    let got = Resolved::load(&Sources {
        project: None,
        user: Some(config.to_path_buf()),
        isolated: true,
        cwd: None,
        packs: Vec::new(),
        cli: config::cli_layer(&[], &[], None).map_err(|e| e.0)?,
    })
    .map_err(|e| format!("the bundled config does not load: {}", e.0))?;
    let ids = |r: &Resolved| -> Vec<(String, String)> {
        r.packs
            .iter()
            .map(|p| (p.meta.name.clone(), p.hash.clone()))
            .collect()
    };
    if ids(&got) != ids(&b.expected) || got.policy != b.expected.policy {
        return Err(
            "the bundled config does not load to the same packs and settings as its source".into(),
        );
    }
    Ok(())
}

/// A pack name as a folder name: letters, digits, `.`, `-` and `_`.
fn folder_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    match s.trim_matches('.') {
        "" => "pack".to_string(),
        t => t.to_string(),
    }
}

/// The git work tree holding `path` (or where it would be created), if any.
fn work_tree_of(path: &Path) -> Option<PathBuf> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    // Resolve the deepest part that exists, so `..` and symlinks count.
    let mut base = abs.as_path();
    while !base.exists() {
        base = base.parent()?;
    }
    let base = base.canonicalize().ok()?;
    base.ancestors()
        .find(|d| d.join(".git").exists())
        .map(Path::to_path_buf)
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names_are_plain() {
        assert_eq!(folder_name("team-conventions"), "team-conventions");
        assert_eq!(folder_name("a/b c"), "a_b_c");
        assert_eq!(folder_name(".."), "pack");
    }
}
