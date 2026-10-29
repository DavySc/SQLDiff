//! Every tree the encoder writes for the repository's SQL is a value of
//! `encode::SCHEMA`, so the Lean decoder generated from the schema reads
//! everything the Rust side sends.

use std::path::{Path, PathBuf};

use sqldiff_syntax::abstract_syntax::{parse_stmt_ext, tokens};
use sqldiff_syntax::encode::{self, check, parse_line, schema_types, FieldTy};
use sqldiff_syntax::{parse, routine, Dialect, SyntaxKind};

fn sql_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        if p.is_dir() {
            if name != "target" && !name.starts_with('.') {
                sql_files(&p, out);
            }
        } else if name.ends_with(".sql") {
            out.push(p);
        }
    }
}

fn dialect_of(name: &str) -> Option<Dialect> {
    if name.starts_with("pg") || name.contains(".pg.") || name.contains("postgres") {
        Some(Dialect::Postgres)
    } else if name.starts_with("tsql") || name.contains(".tsql.") || name.contains("mssql") {
        Some(Dialect::Tsql)
    } else if name.contains("db2") {
        Some(Dialect::Db2i)
    } else {
        None
    }
}

#[test]
fn every_encoded_tree_is_a_value_of_the_schema() {
    let schema = schema_types();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for d in ["corpus", "eval", "tests"] {
        sql_files(&root.join(d), &mut files);
    }
    let (mut stmts, mut routines) = (0, 0);
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let Some(d) = dialect_of(&name) else {
            continue;
        };
        let text = std::fs::read_to_string(f).unwrap();
        let p = parse(&text, d);
        for n in p.syntax().descendants() {
            let (line, ty) = match n.kind() {
                SyntaxKind::STATEMENT => match parse_stmt_ext(&tokens(&n), d) {
                    Ok(s) => {
                        stmts += 1;
                        (encode::stmt(&s), "Stmt")
                    }
                    Err(_) => continue,
                },
                SyntaxKind::ROUTINE => match routine::read(&n, d) {
                    Ok(r) => {
                        routines += 1;
                        (encode::routine(&r), "Routine")
                    }
                    Err(_) => continue,
                },
                _ => continue,
            };
            let v =
                parse_line(&line).unwrap_or_else(|| panic!("{}: unreadable: {line}", f.display()));
            if let Err(e) = check(&v, &FieldTy::Named(ty.into()), &schema) {
                panic!("{}: {e}\n{line}", f.display());
            }
        }
    }
    eprintln!("{stmts} statements and {routines} routines checked");
    assert!(
        stmts > 300 && routines > 30,
        "{stmts} statements, {routines} routines"
    );
}

#[test]
fn malformed_lines_are_refused() {
    for bad in [
        "",
        "(",
        "(Null",
        "(Null) ",
        "[(Null)",
        "s:abc",
        "(Name [s:78 ])",
        "x",
        "(Null)(Null)",
    ] {
        assert_eq!(parse_line(bad), None, "{bad:?}");
    }
    assert!(parse_line("(Name [s:78])").is_some());
}
