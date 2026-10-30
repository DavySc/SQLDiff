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
            Node::ForQuery { .. } => "for",
            Node::Call { .. } => "call",
            Node::Setting { .. } => "setting",
            Node::Try { .. } => "try",
            Node::Throw { .. } => "throw",
            Node::Raise { .. } => "raise",
            Node::Txn { .. } => "txn",
        })
        .collect();
    assert_eq!(shape, ["stmt", "if", "while", "call", "stmt"]);
    // T-SQL names and OUTPUT arguments have no PL/pgSQL spelling.
    assert!(routine::print_postgres(&r).is_err());
}

#[test]
fn syntax_the_tree_would_drop_is_refused() {
    // The checker sees only the tree: a result set from OUTPUT, a qualified
    // SET target (an error in PostgreSQL) or a table hint must not vanish.
    for (stmt, what) in [
        (
            "INSERT INTO dbo.t (a) OUTPUT inserted.a VALUES (@x);",
            "OUTPUT",
        ),
        (
            "UPDATE dbo.t SET a = @x OUTPUT inserted.a WHERE a = 1;",
            "OUTPUT",
        ),
        ("UPDATE dbo.t SET t.a = @x WHERE a = 1;", "qualifier"),
        ("INSERT INTO dbo.t WITH (TABLOCK) (a) VALUES (@x);", "hints"),
        (
            "IF EXISTS (SELECT a FROM dbo.t FOR JSON PATH) SET @x = 1;",
            "FOR JSON",
        ),
    ] {
        let text = format!("CREATE PROCEDURE dbo.p @x int AS\nBEGIN\n  {stmt}\nEND\n");
        let r = routines(&text, Dialect::Tsql).remove(0);
        let e = r.expect_err(stmt);
        assert!(e.contains(what), "{stmt}: {e}");
    }
    let plain =
        "CREATE PROCEDURE dbo.p @x int AS\nBEGIN\n  UPDATE dbo.t SET a = @x WHERE a = 1;\nEND\n";
    assert!(routines(plain, Dialect::Tsql).remove(0).is_ok());
    // PostgreSQL ONLY leaves inheritance children out; the tree would not
    // show it.
    let only = "CREATE PROCEDURE p(x integer) LANGUAGE plpgsql AS $$\nBEGIN\n  UPDATE ONLY t SET a = x WHERE a = 1;\nEND;\n$$;\n";
    let e = routines(only, Dialect::Postgres)
        .remove(0)
        .expect_err("ONLY");
    assert!(e.contains("ONLY"), "{e}");
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

const TSQL_C4: &str = "CREATE PROCEDURE dbo.p @id int AS
BEGIN
  SET XACT_ABORT ON;
  BEGIN TRY
    BEGIN TRAN;
    INSERT INTO dbo.t (id) VALUES (@id);
    COMMIT TRANSACTION;
  END TRY
  BEGIN CATCH
    IF @@TRANCOUNT > 0 ROLLBACK;
    THROW;
  END CATCH
  BEGIN TRY
    SET @id = @id / 0;
  END TRY
  BEGIN CATCH
    THROW 50001, 'failed', 1;
  END CATCH
END
";

const PG_C4: &str = "CREATE PROCEDURE p(p_id integer)
LANGUAGE plpgsql
AS $$
BEGIN
  INSERT INTO t (id) VALUES (p_id);
  COMMIT;
  BEGIN
    p_id := p_id / 0;
  EXCEPTION
    WHEN division_by_zero OR SQLSTATE '22003' THEN
      RAISE;
    WHEN OTHERS THEN
      RAISE EXCEPTION USING MESSAGE = 'failed', ERRCODE = 'P0001';
  END;
  RAISE NOTICE 'n = %', p_id;
END;
$$;
";

/// Phase C4: TRY/CATCH, THROW and transaction control read in T-SQL;
/// exception blocks, RAISE and COMMIT read, print and read back in
/// PL/pgSQL.
#[test]
fn error_handling_and_transactions_read() {
    let r = routines(TSQL_C4, Dialect::Tsql).remove(0).unwrap();
    let [Node::Setting { .. }, Node::Try { body, handlers }, Node::Try { handlers: h2, .. }] =
        r.body.body.as_slice()
    else {
        panic!("{:?}", r.body.body);
    };
    assert!(matches!(
        body.as_slice(),
        [Node::Txn { op, name: None }, Node::Stmt(_), Node::Txn { .. }] if op == "BEGIN"
    ));
    assert!(handlers.len() == 1 && handlers[0].conds.is_empty());
    assert!(matches!(
        handlers[0].body.as_slice(),
        [Node::If { .. }, Node::Throw { args }] if args.is_empty()
    ));
    assert!(matches!(h2[0].body.as_slice(), [Node::Throw { args }] if args.len() == 3));
    assert!(routine::print_postgres(&r).is_err());

    let r = routines(PG_C4, Dialect::Postgres).remove(0).unwrap();
    let Node::Try { handlers, .. } = &r.body.body[2] else {
        panic!("{:?}", r.body.body);
    };
    assert_eq!(
        handlers.iter().map(|h| h.conds.clone()).collect::<Vec<_>>(),
        [
            vec!["division_by_zero".to_string(), "sqlstate:22003".to_string()],
            vec!["others".to_string()]
        ]
    );
    assert!(matches!(&r.body.body[1], Node::Txn { op, name: None } if op == "COMMIT"));
    assert_eq!(reprint(&r), r);

    for (body, what) in [
        ("BEGIN DISTRIBUTED TRANSACTION;", "BEGIN"),
        ("SAVE TRANSACTION s;", "SAVE"),
        ("THROW 50001, 'x';", "THROW"),
        ("COMMIT WITH (DELAYED_DURABILITY = ON);", "COMMIT"),
    ] {
        let text = format!("CREATE PROCEDURE dbo.p AS\nBEGIN\n  {body}\nEND\n");
        let e = routines(&text, Dialect::Tsql).remove(0).expect_err(body);
        assert!(e.contains(what), "{body}: {e}");
    }
    for body in [
        "RAISE division_by_zero;",
        "COMMIT AND CHAIN;",
        "DECLARE x int; BEGIN x := 1; EXCEPTION WHEN OTHERS THEN x := 2; END;",
    ] {
        let text =
            format!("CREATE PROCEDURE p() LANGUAGE plpgsql AS $$\nBEGIN\n  {body}\nEND;\n$$;\n");
        assert!(
            routines(&text, Dialect::Postgres).remove(0).is_err(),
            "{body}"
        );
    }
    // An exception section on the routine's own block is not read.
    let top = "CREATE PROCEDURE p() LANGUAGE plpgsql AS $$\nBEGIN\n  NULL;\nEXCEPTION WHEN OTHERS THEN\n  RAISE;\nEND;\n$$;\n";
    assert!(routines(top, Dialect::Postgres).remove(0).is_err());
}

/// C3b: the PL/pgSQL block the converter writes for T-SQL `SELECT @x =
/// col` reads as a nested block around a `FOR` loop over the query, and
/// prints and reads back as the same tree; other `FOR` loops are refused.
#[test]
fn a_for_loop_over_a_query_reads() {
    let text = "CREATE PROCEDURE look(p_id integer, INOUT p_q integer)
LANGUAGE plpgsql
AS $$
BEGIN
  DECLARE
  sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT Qty AS sqldiff_c1 FROM Acct WHERE Id = p_id LOOP
    p_q := sqldiff_r.sqldiff_c1;
  END LOOP;
  END;
END;
$$;
";
    let r = routines(text, Dialect::Postgres).remove(0).unwrap();
    let [Node::Block(b)] = r.body.body.as_slice() else {
        panic!("{:?}", r.body.body);
    };
    assert_eq!(b.decls.len(), 1);
    let [Node::ForQuery { var, query, body }] = b.body.as_slice() else {
        panic!("{:?}", b.body);
    };
    assert_eq!(var, "sqldiff_r");
    assert_eq!(query.items.len(), 1);
    assert_eq!(body.len(), 1);
    assert_eq!(reprint(&r), r);
    for lp in [
        "FOR i IN 1..3 LOOP p_q := i; END LOOP;",
        "FOR sqldiff_r IN EXECUTE 'SELECT 1' LOOP p_q := 1; END LOOP;",
        "FOR i IN REVERSE 3..1 LOOP p_q := i; END LOOP;",
        "<<l>> FOR sqldiff_r IN SELECT 1 AS a LOOP p_q := 1; END LOOP;",
    ] {
        let text = format!(
            "CREATE PROCEDURE look(INOUT p_q integer) LANGUAGE plpgsql AS $$\nDECLARE i integer; sqldiff_r record;\nBEGIN\n  {lp}\nEND;\n$$;\n"
        );
        assert!(
            routines(&text, Dialect::Postgres).remove(0).is_err(),
            "{lp}"
        );
    }
}

/// C3d: a query over several tables reads with its joins, aliases and
/// qualified names in both dialects and prints back as the same tree; the
/// forms the checker has no rule for either do not read (`APPLY`, `USING`,
/// a parenthesised join, a block label) or keep a shape the checker
/// refuses (an outer join's kind).
#[test]
fn query_blocks_read_with_their_joins_and_aliases() {
    use sqldiff_syntax::abstract_syntax::{Expr, From, Stmt};
    let pg = "CREATE PROCEDURE look(p_id integer, INOUT p_q integer)
LANGUAGE plpgsql
AS $$
BEGIN
  DECLARE
  sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT c.Score AS sqldiff_c1 FROM Ord AS o JOIN Cust c ON c.Id = o.CustId, Two CROSS JOIN Pair WHERE o.Id = p_id LOOP
    p_q := sqldiff_r.sqldiff_c1;
  END LOOP;
  END;
END;
$$;
";
    let r = routines(pg, Dialect::Postgres).remove(0).unwrap();
    let [Node::Block(b)] = r.body.body.as_slice() else {
        panic!("{:?}", r.body.body);
    };
    let [Node::ForQuery { query, .. }] = b.body.as_slice() else {
        panic!("{:?}", b.body);
    };
    let [From::Join {
        kind,
        left,
        right,
        on: Some(_),
    }, From::Join {
        kind: k2, on: None, ..
    }] = query.from.as_slice()
    else {
        panic!("{:?}", query.from);
    };
    assert_eq!((kind.as_str(), k2.as_str()), ("INNER", "CROSS"));
    assert!(matches!(left.as_ref(), From::Table { alias: Some(a), .. } if a == "o"));
    assert!(matches!(right.as_ref(), From::Table { alias: Some(a), .. } if a == "c"));
    assert!(
        matches!(&query.items[0].expr, Expr::Name(p) if p == &["c".to_string(), "Score".to_string()])
    );
    assert_eq!(reprint(&r), r);
    // T-SQL: the same FROM, with `INNER JOIN` read as `INNER` too.
    let t = "CREATE PROCEDURE dbo.look @id int, @q int OUTPUT AS
BEGIN
  SELECT @q = c.Score FROM dbo.Ord AS o INNER JOIN dbo.Cust c ON c.Id = o.CustId WHERE o.Id = @id;
END
";
    let r = routines(t, Dialect::Tsql).remove(0).unwrap();
    let [Node::Stmt(Stmt::Select(q))] = r.body.body.as_slice() else {
        panic!("{:?}", r.body.body);
    };
    assert!(matches!(q.from.as_slice(), [From::Join { kind, .. }] if kind == "INNER"));
    // An outer join keeps its kind (the checker refuses it).
    let t = t.replace("INNER JOIN", "LEFT JOIN");
    let r = routines(&t, Dialect::Tsql).remove(0).unwrap();
    let [Node::Stmt(Stmt::Select(q))] = r.body.body.as_slice() else {
        panic!("{:?}", r.body.body);
    };
    assert!(matches!(q.from.as_slice(), [From::Join { kind, .. }] if kind == "LEFT"));
    // Forms with no rule do not read.
    for from in [
        "dbo.Ord AS o CROSS APPLY dbo.Cust c",
        "dbo.Ord AS o OUTER APPLY dbo.Cust c",
        "(dbo.Ord AS o JOIN dbo.Cust c ON c.Id = o.CustId)",
    ] {
        let t = format!(
            "CREATE PROCEDURE dbo.look @id int, @q int OUTPUT AS\nBEGIN\n  SELECT @q = c.Score FROM {from} WHERE o.Id = @id;\nEND\n"
        );
        assert!(routines(&t, Dialect::Tsql).remove(0).is_err(), "{from}");
    }
    for from in [
        "Ord AS o JOIN Cust c USING (Id)",
        "Ord AS o NATURAL JOIN Cust c",
        "(Ord AS o JOIN Cust c ON c.Id = o.CustId)",
    ] {
        let text = pg.replace(
            "Ord AS o JOIN Cust c ON c.Id = o.CustId, Two CROSS JOIN Pair",
            from,
        );
        assert!(
            routines(&text, Dialect::Postgres).remove(0).is_err(),
            "{from}"
        );
    }
    // A label on the block around the loop, or on the routine's block: a
    // label qualifies variables (`l.x`), so it is not read.
    for (from, to) in [
        ("  DECLARE\n  sqldiff_r", "  <<l>>\n  DECLARE\n  sqldiff_r"),
        ("AS $$\nBEGIN", "AS $$\n<<outer>>\nBEGIN"),
    ] {
        let text = pg.replacen(from, to, 1);
        assert!(
            routines(&text, Dialect::Postgres).remove(0).is_err(),
            "{to}"
        );
    }
}

/// C3d part 2: system-versioned tables are now read-only catalog tables, so
/// a query that reads another state of one (`FOR SYSTEM_TIME`) must keep a
/// shape the checker refuses: `AS OF` reads as a table hint (which
/// `readJoin` refuses, `stmt:select:from:hints`), and every other form does
/// not read at all. A plain `FROM` reads without hints.
#[test]
fn a_system_time_query_keeps_a_hint_or_does_not_read() {
    use sqldiff_syntax::abstract_syntax::{From, Stmt};
    let proc = |from: &str| {
        format!(
            "CREATE PROCEDURE dbo.c @id int, @n int OUTPUT AS
BEGIN
  SELECT @n = c.N FROM {from} WHERE c.Id = @id;
END
"
        )
    };
    let hints = |from: &str| -> Option<Vec<String>> {
        let r = routines(&proc(from), Dialect::Tsql).remove(0).ok()?;
        let [Node::Stmt(Stmt::Select(q))] = r.body.body.as_slice() else {
            panic!("{:?}", r.body.body);
        };
        let [From::Table { hints, .. }] = q.from.as_slice() else {
            panic!("{:?}", q.from);
        };
        Some(hints.clone())
    };
    assert_eq!(hints("dbo.Color AS c"), Some(Vec::new()));
    assert!(
        hints("dbo.Color FOR SYSTEM_TIME AS OF '2020-01-01' AS c").is_some_and(|h| !h.is_empty())
    );
    for from in [
        "dbo.Color FOR SYSTEM_TIME ALL AS c",
        "dbo.Color FOR SYSTEM_TIME BETWEEN '2020-01-01' AND '2021-01-01' AS c",
        "dbo.Color FOR SYSTEM_TIME FROM '2020-01-01' TO '2021-01-01' AS c",
        "dbo.Color FOR SYSTEM_TIME CONTAINED IN ('2020-01-01', '2021-01-01') AS c",
    ] {
        assert!(hints(from).map_or(true, |h| !h.is_empty()), "{from}");
    }
}
