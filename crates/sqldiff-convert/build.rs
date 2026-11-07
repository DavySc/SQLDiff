//! The engine fingerprint: what decides a conversion result, hashed from
//! sources instead of read off the running executable, so it is the same
//! for every binary that links the engine (CLI, GUI, MCP), does not change
//! when only a binary crate does, and can be told apart per crate.
//!
//! It covers the sources and data of the workspace crates the converter
//! depends on, every file they include at compile time (also outside the
//! crate), `Cargo.lock`, the compiler's version, the target, the profile and
//! the enabled features, the workspace manifest and cargo configuration of
//! the checkout, and the compiler flags and `CARGO_CFG_*` the target sets.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The workspace crates a conversion result is computed by: the closure of
/// `sqldiff-convert`'s path dependencies (a test checks it).
const CRATES: &[&str] = &[
    "sqldiff-syntax",
    "sqldiff-core",
    "sqldiff-source",
    "sqldiff-convert",
];

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            // Not compiled into the library.
            if matches!(name, "tests" | "examples" | "benches" | "target") {
                continue;
            }
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// Literal paths of `include_str!`, `include_bytes!` and `include!` in a
/// source file, resolved against the file's directory.
fn includes(file: &Path, text: &str, out: &mut Vec<PathBuf>) {
    for mac in ["include_str!(\"", "include_bytes!(\"", "include!(\""] {
        let mut rest = text;
        while let Some(i) = rest.find(mac) {
            rest = &rest[i + mac.len()..];
            if let Some(j) = rest.find('"') {
                if let Some(dir) = file.parent() {
                    out.push(dir.join(&rest[..j]));
                }
            }
        }
    }
}

/// `bytes` without carriage returns: a Windows checkout is the same engine.
fn lf(bytes: Vec<u8>) -> Vec<u8> {
    bytes.into_iter().filter(|b| *b != b'\r').collect()
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let crates_dir = manifest.parent().unwrap().to_path_buf();
    let root = crates_dir.parent().unwrap().to_path_buf();
    let mut h = Sha256::new();
    let mut put = |label: &str, bytes: &[u8]| {
        h.update((label.len() as u64).to_le_bytes());
        h.update(label.as_bytes());
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    };
    let mut files: Vec<PathBuf> = Vec::new();
    for c in CRATES {
        walk(&crates_dir.join(c), &mut files);
    }
    let mut extra: Vec<PathBuf> = Vec::new();
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
        if f.extension().is_some_and(|e| e == "rs") {
            if let Ok(t) = std::fs::read_to_string(f) {
                includes(f, &t, &mut extra);
            }
        }
    }
    // Compile-time includes that live outside the crates' own folders.
    extra.retain(|p| !CRATES.iter().any(|c| p.starts_with(crates_dir.join(c))));
    extra.sort();
    extra.dedup();
    for p in extra.iter() {
        println!("cargo:rerun-if-changed={}", p.display());
    }
    for p in files.iter().chain(extra.iter()) {
        let rel = p
            .strip_prefix(&root)
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| p.to_string_lossy().replace('\\', "/"));
        put(&rel, &lf(std::fs::read(p).unwrap_or_default()));
    }
    let lock = root.join("Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock.display());
    put("Cargo.lock", &lf(std::fs::read(&lock).unwrap_or_default()));
    // The workspace manifest (profiles, patches, shared dependency versions
    // and features) and the cargo configuration of the checkout (flags,
    // target settings) change the code that is generated.
    for rel in [
        "Cargo.toml",
        ".cargo/config.toml",
        ".cargo/config",
        "rust-toolchain.toml",
        "rust-toolchain",
    ] {
        let p = root.join(rel);
        println!("cargo:rerun-if-changed={}", p.display());
        // Absent files are hashed as absent, so adding one changes the key.
        match std::fs::read(&p) {
            Ok(b) => put(rel, &lf(b)),
            Err(_) => put(&format!("absent {rel}"), b""),
        }
    }
    // A new or removed file in a crate is not seen through the files that
    // exist: watch the folders too.
    for c in CRATES {
        println!("cargo:rerun-if-changed={}", crates_dir.join(c).display());
    }
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let version = std::process::Command::new(rustc)
        .arg("-vV")
        .output()
        .map(|o| o.stdout)
        .unwrap_or_default();
    put("rustc", &version);
    for key in [
        "TARGET",
        "PROFILE",
        "OPT_LEVEL",
        "DEBUG",
        "CARGO_CFG_TARGET_FEATURE",
    ] {
        put(key, std::env::var(key).unwrap_or_default().as_bytes());
    }
    // The flags the compiler gets (RUSTFLAGS, `build.rustflags`, target
    // flags, already merged by cargo), every `CARGO_CFG_*` the target sets,
    // and profile overrides from the environment: a result is never reused
    // across different code generation.
    for key in [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_BUILD_RUSTFLAGS",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
        put(key, std::env::var(key).unwrap_or_default().as_bytes());
    }
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k.starts_with("CARGO_CFG_") || k.starts_with("CARGO_PROFILE_"))
        .collect();
    env.sort();
    for (k, v) in env {
        put(&k, v.as_bytes());
    }
    let mut features: Vec<String> = std::env::vars()
        .filter(|(k, _)| k.starts_with("CARGO_FEATURE_"))
        .map(|(k, _)| k)
        .collect();
    features.sort();
    put("features", features.join(",").as_bytes());
    let digest: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("fingerprint.rs");
    let list: Vec<String> = CRATES.iter().map(|c| format!("{c:?}")).collect();
    std::fs::write(
        out,
        format!(
            "/// Hash of the sources and settings that decide a conversion result.\npub const ENGINE_FINGERPRINT: &str = {digest:?};\n/// The workspace crates it covers.\npub const FINGERPRINTED_CRATES: &[&str] = &[{}];\n",
            list.join(", ")
        ),
    )
    .unwrap();
}
