//! A source checked out with CRLF line endings (git on Windows) converts
//! exactly like the same source with LF: the same output, statuses and unit
//! ids, and the same file hashes in the review ledger. Otherwise a tree
//! converted and reviewed on one machine would lose its decisions, and look
//! edited by hand, on another.

use std::path::{Path, PathBuf};

use sqldiff_convert::{convert, review};
use sqldiff_core::Dialect;

fn corpus_tsql() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
    let mut files: Vec<PathBuf> = Vec::new();
    for dir in ["pairs", "mutations"] {
        for case in std::fs::read_dir(root.join(dir)).unwrap() {
            let case = case.unwrap().path();
            if !case.is_dir() {
                continue;
            }
            for f in std::fs::read_dir(&case).unwrap() {
                let f = f.unwrap().path();
                let name = f.file_name().unwrap().to_string_lossy().to_string();
                if name == "tsql.sql" || name.ends_with(".tsql.sql") {
                    files.push(f);
                }
            }
        }
    }
    files.sort();
    files
        .into_iter()
        .map(|f| {
            // LF whatever the checkout, so both forms below are what they say.
            let text = std::fs::read_to_string(&f).unwrap().replace("\r\n", "\n");
            (f.display().to_string(), text)
        })
        .collect()
}

#[test]
fn crlf_source_converts_like_lf() {
    let files = corpus_tsql();
    assert!(files.len() >= 20, "corpus T-SQL files: {}", files.len());
    for (path, lf) in &files {
        let crlf = lf.replace('\n', "\r\n");
        for to in [Dialect::Postgres, Dialect::Db2i] {
            let a = convert(lf, Dialect::Tsql, to).unwrap();
            let b = convert(&crlf, Dialect::Tsql, to).unwrap();
            assert_eq!(a.output, b.output, "{path} to {to:?}");
            assert!(
                !b.output.contains('\r'),
                "{path} to {to:?}: a CR in the output"
            );
            let units = |c: &sqldiff_convert::Conversion| -> Vec<String> {
                c.units
                    .iter()
                    .map(|u| format!("{:?} {:?} {:?}", u.status, u.checked, u.notes))
                    .collect()
            };
            assert_eq!(units(&a), units(&b), "{path} to {to:?}");
            assert_eq!(
                review::unit_ids("f.sql", lf, &a),
                review::unit_ids("f.sql", &crlf, &b),
                "{path} to {to:?}"
            );
            assert_eq!(review::file_hash(lf), review::file_hash(&crlf), "{path}");
        }
    }
}

#[test]
fn file_hash_ignores_only_the_cr_of_crlf() {
    let lf = "SELECT 'a\nb';\n";
    assert_eq!(
        review::file_hash(lf),
        review::file_hash("SELECT 'a\r\nb';\r\n")
    );
    // Any other change still changes the hash: a lone CR, a trailing blank.
    assert_ne!(review::file_hash(lf), review::file_hash("SELECT 'a\rb';\n"));
    assert_ne!(
        review::file_hash(lf),
        review::file_hash("SELECT 'a\nb';\r\r\n")
    );
    assert_ne!(
        review::file_hash(lf),
        review::file_hash("SELECT 'a \nb';\n")
    );
}

#[test]
fn dir_hash_reads_crlf_as_lf() {
    use sqldiff_convert::tree::dir_hash;
    let base = std::env::temp_dir().join(format!("sqldiff-eol-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let tree = |name: &str, text: &str| {
        let d = base.join(name);
        std::fs::create_dir_all(d.join("procs")).unwrap();
        std::fs::write(d.join("procs/p.sql"), text).unwrap();
        dir_hash(&d).unwrap()
    };
    let lf = tree("lf", "SELECT 1;\n-- x\n");
    assert_eq!(lf, tree("crlf", "SELECT 1;\r\n-- x\r\n"));
    assert_ne!(lf, tree("cr", "SELECT 1;\r-- x\n"));
    assert_ne!(lf, tree("changed", "SELECT 2;\n-- x\n"));
    let _ = std::fs::remove_dir_all(&base);
}
