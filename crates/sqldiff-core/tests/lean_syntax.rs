//! `formal/lean/Sqldiff/Syntax.lean` is generated from the Rust syntax
//! kinds (the AST boundary in docs/verification-roadmap.md). A kind added,
//! removed or renamed fails this test until the file is generated again:
//! `SQLDIFF_BLESS=1 cargo test -p sqldiff-core --test lean_syntax`.

use std::path::PathBuf;

#[test]
fn the_lean_syntax_module_is_generated_from_the_rust_kinds() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../formal/lean/Sqldiff/Syntax.lean");
    let want = sqldiff_core::proof::lean_syntax();
    if std::env::var_os("SQLDIFF_BLESS").is_some() {
        std::fs::write(&path, &want).unwrap();
    }
    // A CRLF checkout (Windows) is the same file.
    let got = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        got == want,
        "{} is stale: run SQLDIFF_BLESS=1 cargo test -p sqldiff-core --test lean_syntax",
        path.display()
    );
}

#[test]
fn the_lean_ast_module_is_generated_from_the_schema() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../formal/lean/Sqldiff/Ast.lean");
    let want = sqldiff_core::proof::lean_ast();
    if std::env::var_os("SQLDIFF_BLESS").is_some() {
        std::fs::write(&path, &want).unwrap();
    }
    // A CRLF checkout (Windows) is the same file.
    let got = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        got == want,
        "{} is stale: run SQLDIFF_BLESS=1 cargo test -p sqldiff-core --test lean_syntax",
        path.display()
    );
}
