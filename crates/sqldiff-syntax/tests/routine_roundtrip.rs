//! Routines as abstract syntax: PL/pgSQL routines print and read back as
//! the same tree, and T-SQL routines read into the same shape.

use std::path::{Path, PathBuf};

use sqldiff_syntax::routine::{self, Mode, Node, Routine, RoutineKind, Security};
use sqldiff_syntax::{parse, Dialect, SyntaxKind};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn files(dir: &Path, pick: &dyn Fn(&str) -> bool, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        if p.is_dir() {
            if name != "target" && !name.starts_with('.') {
                files(&p, pick, out);
            }
        } else if name.ends_with(".sql") && pick(&name) {
            out.push(p);
        }
    }
}

fn routines(text: &str, d: Dialect) -> Vec<Result<Routine, String>> {
    parse(text, d)
        .syntax()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ROUTINE)
        .map(|n| routine::read(&n, d))
        .collect()
}

const PG: &str = "CREATE PROCEDURE p(a integer, INOUT b integer DEFAULT 2)
LANGUAGE plpgsql
AS $$
DECLARE
  x integer := 1;
BEGIN
  IF a > 1 THEN
    x := 2;
  ELSIF a < 0 THEN
    x := 3;
  ELSE
    x := 4;
  END IF;
  WHILE x < 10 LOOP
    x := x + 1;
  END LOOP;
  CALL q(x, b);
  RETURN;
END;
$$;
";

const TSQL: &str = "CREATE PROCEDURE dbo.p @a int, @b int = 2 OUTPUT
AS
BEGIN
  DECLARE @x int = 1;
  IF @a > 1
    SET @x = 2;
  ELSE IF @a < 0
  BEGIN
    SET @x = 3;
  END
  ELSE
    SET @x = 4;
  WHILE @x < 10
    SET @x = @x + 1;
  EXEC dbo.q @x, @b OUTPUT;
  RETURN @x;
END
";

fn reprint(r: &Routine) -> Routine {
    let text = routine::print_postgres(r).unwrap();
    let back = routines(&text, Dialect::Postgres);
    assert_eq!(back.len(), 1, "{text}");
    back.into_iter()
        .next()
        .unwrap()
        .unwrap_or_else(|e| panic!("{e}\n{text}"))
}

#[test]
fn a_plpgsql_procedure_reads_prints_and_reads_back() {
    let r = routines(PG, Dialect::Postgres).remove(0).unwrap();
    assert_eq!(r.kind, RoutineKind::Procedure);
    assert_eq!(r.params[1].mode, Mode::InOut);
    assert_eq!(r.body.decls.len(), 1);
    assert_eq!(r.body.body.len(), 4);
    let Node::If { els, .. } = &r.body.body[0] else {
        panic!("{:?}", r.body.body[0]);
    };
    assert!(matches!(els.as_slice(), [Node::If { .. }]));
    assert_eq!(reprint(&r), r);
}

#[test]
fn a_tsql_procedure_reads_into_the_same_shape() {
    let r = routines(TSQL, Dialect::Tsql).remove(0).unwrap();
    assert_eq!(r.name, ["dbo", "p"]);
    assert_eq!(r.params[0].mode, Mode::In);
    assert_eq!(r.params[1].mode, Mode::InOut);
    assert_eq!(r.security, Security::Caller);
    assert!(r.body.decls.is_empty());
    let shape: Vec<&str> = r
        .body
        .body
        .iter()
        .map(|n| match n {
            Node::Stmt(_) => "stmt",
            Node::Block(_) => "block",
            Node::If { .. } => "if",
            Node::While { .. } => "while",
            Node::Call { .. } => "call",
            Node::Setting { .. } => "setting",
        })
        .collect();
    assert_eq!(shape, ["stmt", "if", "while", "call", "stmt"]);
    // T-SQL names and OUTPUT arguments have no PL/pgSQL spelling.
    assert!(routine::print_postgres(&r).is_err());
}

#[test]
fn every_plpgsql_routine_in_the_repository_round_trips() {
    let mut fs = Vec::new();
    let pick = |n: &str| n.starts_with("pg") || n.contains(".pg.") || n.contains("postgres");
    for d in ["corpus", "eval", "tests"] {
        files(&root().join(d), &pick, &mut fs);
    }
    let (mut read, mut refused) = (0, Vec::new());
    for f in &fs {
        let text = std::fs::read_to_string(f).unwrap();
        for r in routines(&text, Dialect::Postgres) {
            match r {
                Ok(r) => {
                    read += 1;
                    match routine::print_postgres(&r) {
                        Ok(_) => assert_eq!(reprint(&r), r, "{}", f.display()),
                        Err(e) => refused.push(format!("{}: print: {e}", f.display())),
                    }
                }
                Err(e) => refused.push(format!("{}: {e}", f.display())),
            }
        }
    }
    eprintln!("{read} PL/pgSQL routines read");
    for r in &refused {
        eprintln!("not read: {r}");
    }
    assert!(read >= 20, "only {read} routines read");
}

#[test]
fn tsql_routines_in_the_repository_read() {
    let mut fs = Vec::new();
    let pick = |n: &str| n.starts_with("tsql") || n.contains(".tsql.") || n.contains("mssql");
    for d in ["corpus", "eval", "tests"] {
        files(&root().join(d), &pick, &mut fs);
    }
    let (mut read, mut refused) = (0, Vec::new());
    for f in &fs {
        let text = std::fs::read_to_string(f).unwrap();
        for r in routines(&text, Dialect::Tsql) {
            match r {
                Ok(_) => read += 1,
                Err(e) => refused.push(format!("{}: {e}", f.display())),
            }
        }
    }
    eprintln!("{read} T-SQL routines read");
    for r in &refused {
        eprintln!("not read: {r}");
    }
    assert!(read >= 10, "only {read} routines read");
}
