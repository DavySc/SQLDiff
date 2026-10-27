//! The run sheet's quoting: each argument reaches `sqldiff` as one argument,
//! unchanged, in PowerShell and in a POSIX shell. (The GUI-vs-CLI parity
//! test in sqldiff-cli runs the commands through this platform's shell.)

use sqldiff_gui::runs::{powershell_quote, sh_quote};

#[test]
fn plain_arguments_stay_bare() {
    for a in [
        "convert",
        "-o",
        "--from",
        "postgres",
        "/home/d/src",
        "C:/x/out",
    ] {
        assert_eq!(powershell_quote(a), a);
        assert_eq!(sh_quote(a), a);
    }
    assert_eq!(powershell_quote(r"C:\Users\d\src"), r"C:\Users\d\src");
    assert_eq!(sh_quote(r"C:\Users\d\src"), r"'C:\Users\d\src'");
}

#[test]
fn powershell_single_quotes_and_doubles_quotes() {
    assert_eq!(
        powershell_quote(r"C:\Users\d\it's a [tree] $HOME"),
        r"'C:\Users\d\it''s a [tree] $HOME'"
    );
    // PowerShell reads the typographic single quotes as `'` too.
    assert_eq!(powershell_quote("a\u{2019}b"), "'a\u{2019}\u{2019}b'");
    // Splatting, arrays, subexpressions, comments and the stop-parsing token.
    for a in ["@x", "a,b", "$(x)", "#x", "--%", "a;b", "a b", "`x", ""] {
        let q = powershell_quote(a);
        assert!(q.starts_with('\'') && q.ends_with('\''), "{a} -> {q}");
    }
}

#[test]
fn sh_single_quotes_and_closes_around_quotes() {
    assert_eq!(sh_quote("it's a [tree] $HOME"), r"'it'\''s a [tree] $HOME'");
    for a in ["a b", "$x", "`x`", "a;b", "*", "~", "a\"b", "!x", ""] {
        let q = sh_quote(a);
        assert!(q.starts_with('\'') && q.ends_with('\''), "{a} -> {q}");
    }
}
