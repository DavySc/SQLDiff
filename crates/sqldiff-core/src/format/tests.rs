use super::*;

fn fmt(text: &str, d: Dialect) -> Formatted {
    format(text, d, &FormatOptions::default())
}

fn with(f: impl FnOnce(&mut FormatOptions)) -> FormatOptions {
    let mut o = FormatOptions::default();
    f(&mut o);
    o
}

fn fixture(name: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name);
    std::fs::read_to_string(p).unwrap()
}

#[test]
fn lays_out_a_procedure() {
    let r = fmt(&fixture("movestock.tsql.sql"), Dialect::Tsql);
    assert!(r.kept.is_empty(), "{:?}", r.kept);
    assert_eq!(
        r.text,
        "CREATE PROCEDURE dbo.MoveStock @Item int, @Qty int AS
BEGIN
    SET NOCOUNT ON;
    BEGIN TRAN;
    UPDATE Stock
    SET Qty = Qty - @Qty
    WHERE ItemId = @Item;
    IF @@ERROR <> 0
    BEGIN
        ROLLBACK;
        RETURN 1;
    END
    INSERT INTO StockLog (ItemId, Delta)
    VALUES (@Item, -@Qty);
    IF @@ERROR <> 0
    BEGIN
        ROLLBACK;
        RETURN 2;
    END
    COMMIT;
    RETURN 0;
END
"
    );
}

#[test]
fn lays_out_procedural_blocks_in_every_dialect() {
    let pg = "CREATE FUNCTION f(x int) RETURNS int LANGUAGE plpgsql AS $$
DECLARE v int := 0;
BEGIN
  IF x > 0 THEN v := 1; ELSIF x < 0 THEN v := -1; ELSE v := 0; END IF;
  RETURN v;
EXCEPTION WHEN others THEN RETURN -9;
END $$;
";
    let r = format(pg, Dialect::Postgres, &with(|o| o.postgres_bodies = true));
    assert!(r.kept.is_empty(), "{:?}", r.kept);
    assert_eq!(
        r.text,
        "CREATE FUNCTION f(x INT) RETURNS INT LANGUAGE plpgsql AS $$
DECLARE
    v INT := 0;
BEGIN
    IF x > 0 THEN
        v := 1;
    ELSIF x < 0 THEN
        v := -1;
    ELSE
        v := 0;
    END IF;
    RETURN v;
EXCEPTION
    WHEN others THEN
        RETURN -9;
END
$$;
"
    );
    let db2 = "CREATE PROCEDURE p (IN x INT) LANGUAGE SQL
BEGIN DECLARE v INT DEFAULT 0; WHILE v < 10 DO SET v = v + 1; END WHILE; END;
";
    let r = fmt(db2, Dialect::Db2i);
    assert!(r.kept.is_empty(), "{:?}", r.kept);
    assert_eq!(
        r.text,
        "CREATE PROCEDURE p (IN x INT) LANGUAGE SQL
BEGIN
    DECLARE v INT DEFAULT 0;
    WHILE v < 10 DO
        SET v = v + 1;
    END WHILE;
END;
"
    );
    let tsql = "IF @x = 1 SELECT 1 ELSE IF @x = 2 SELECT 2
BEGIN TRY SELECT 1 END TRY BEGIN CATCH THROW; END CATCH
";
    assert_eq!(
        fmt(tsql, Dialect::Tsql).text,
        "IF @x = 1
    SELECT 1
ELSE IF @x = 2
    SELECT 2
BEGIN TRY
    SELECT 1
END TRY
BEGIN CATCH
    THROW;
END CATCH
"
    );
}

#[test]
fn breaks_query_clauses() {
    let q = "select a.Id,count(*) as n from dbo.Orders a inner join dbo.C b on b.Id=a.CId \
             where a.Total>10 and exists(select 1 from y where y.id=a.id) group by a.Id order by n desc";
    assert_eq!(
        fmt(q, Dialect::Tsql).text,
        "SELECT a.Id, count(*) AS n
FROM dbo.Orders a
INNER JOIN dbo.C b ON b.Id = a.CId
WHERE a.Total > 10 AND EXISTS(SELECT 1 FROM y WHERE y.id = a.id)
GROUP BY a.Id
ORDER BY n DESC
"
    );
    let items = format(
        "SELECT a, b, f(c, d) FROM t",
        Dialect::Postgres,
        &with(|o| o.one_item_per_line = true),
    );
    assert_eq!(items.text, "SELECT a,\n    b,\n    f(c, d)\nFROM t\n");
    let flat = format(
        "select a from t where x = 1",
        Dialect::Postgres,
        &with(|o| o.clause_breaks = false),
    );
    assert_eq!(flat.text, "SELECT a FROM t WHERE x = 1\n");
    // `DELETE FROM`, `IS DISTINCT FROM`, `DEFAULT VALUES` stay together.
    assert_eq!(
        fmt(
            "DELETE FROM t WHERE a IS DISTINCT FROM b",
            Dialect::Postgres
        )
        .text,
        "DELETE FROM t\nWHERE a IS DISTINCT FROM b\n"
    );
    assert_eq!(
        fmt("INSERT INTO t DEFAULT VALUES", Dialect::Tsql).text,
        "INSERT INTO t DEFAULT VALUES\n"
    );
}

#[test]
fn options_change_only_layout() {
    let src = "select a from t where x = 1\n";
    let tabs = format(
        "BEGIN SELECT 1; END",
        Dialect::Tsql,
        &with(|o| o.indent_style = IndentStyle::Tabs),
    );
    assert_eq!(tabs.text, "BEGIN\n\tSELECT 1;\nEND\n");
    let two = format(
        "BEGIN SELECT 1; END",
        Dialect::Tsql,
        &with(|o| o.indent_width = 2),
    );
    assert_eq!(two.text, "BEGIN\n  SELECT 1;\nEND\n");
    let lower = format(
        "SELECT A FROM T",
        Dialect::Postgres,
        &with(|o| o.keyword_case = KeywordCase::Lower),
    );
    assert_eq!(lower.text, "select A\nfrom T\n");
    let keep = format(
        src,
        Dialect::Postgres,
        &with(|o| o.keyword_case = KeywordCase::Preserve),
    );
    assert_eq!(keep.text, "select a\nfrom t\nwhere x = 1\n");
    let blank = fmt("SELECT 1;\n\n\n\nSELECT 2;", Dialect::Postgres);
    assert_eq!(blank.text, "SELECT 1;\n\nSELECT 2;\n");
    let joined = format(
        "SELECT a,\n  b\nFROM t",
        Dialect::Postgres,
        &with(|o| o.join_lines = true),
    );
    assert_eq!(joined.text, "SELECT a, b\nFROM t\n");
    let kept_lines = fmt("SELECT a,\n  b\nFROM t", Dialect::Postgres);
    assert_eq!(kept_lines.text, "SELECT a,\n    b\nFROM t\n");
}

#[test]
fn tsql_recases_only_reserved_keywords() {
    // In a case-sensitive database `Name`, `Date` and `int` keep their case.
    let r = fmt(
        "select Name, Date from T where id = cast(x as int)",
        Dialect::Tsql,
    );
    assert_eq!(
        r.text,
        "SELECT Name, Date\nFROM T\nWHERE id = cast(x AS int)\n"
    );
    // Words next to a dot are names, whatever they spell.
    assert_eq!(
        fmt("SELECT t.select FROM t", Dialect::Postgres).text,
        "SELECT t.select\nFROM t\n"
    );
}

#[test]
fn keeps_the_hazards() {
    // PostgreSQL joins string constants across a line break, not a space.
    let r = format(
        "SELECT 'a'\n  'b' FROM t",
        Dialect::Postgres,
        &with(|o| o.join_lines = true),
    );
    assert_eq!(r.text, "SELECT 'a'\n  'b'\nFROM t\n");
    // A line comment keeps the line break after it.
    let r = format(
        "SELECT 1 -- one\n, 2",
        Dialect::Tsql,
        &with(|o| o.join_lines = true),
    );
    assert_eq!(r.text, "SELECT 1 -- one\n    , 2\n");
    // GO stays alone on its line; a column named go does not become one.
    let r = fmt("SELECT 1\nGO\nSELECT go FROM t\nGO 2", Dialect::Tsql);
    assert_eq!(r.text, "SELECT 1\nGO\nSELECT go\nFROM t\nGO 2\n");
    // `*=` is one operator, not a comparison: no space goes inside or around it.
    assert_eq!(fmt("SET @x*=2", Dialect::Tsql).text, "SET @x*=2\n");
    assert_eq!(
        fmt("SELECT 1 WHERE a=-1", Dialect::Tsql).text,
        "SELECT 1\nWHERE a = -1\n"
    );
    // Strings and comments are never touched.
    let src = "select 'a  from  b' /* x   y */ from t";
    assert_eq!(
        fmt(src, Dialect::Tsql).text,
        "SELECT 'a  from  b' /* x   y */\nFROM t\n"
    );
}

#[test]
fn postgres_bodies_are_kept_unless_asked() {
    let src = "CREATE FUNCTION f() RETURNS int LANGUAGE sql AS $$ select   1 $$;";
    assert_eq!(
        fmt(src, Dialect::Postgres).text,
        "CREATE FUNCTION f() RETURNS INT LANGUAGE sql AS $$ select   1 $$;\n"
    );
    let body = format(src, Dialect::Postgres, &with(|o| o.postgres_bodies = true));
    assert!(body.text.contains("$$\n    SELECT 1\n$$;"), "{}", body.text);
}

#[test]
fn line_endings() {
    let crlf = "select 1\r\nselect 2 from t\r\n";
    let r = fmt(crlf, Dialect::Tsql);
    assert_eq!(r.text, "SELECT 1\r\nSELECT 2\r\nFROM t\r\n");
    let lf = format(
        crlf,
        Dialect::Tsql,
        &with(|o| o.line_ending = LineEnding::Lf),
    );
    assert_eq!(lf.text, "SELECT 1\nSELECT 2\nFROM t\n");
    let to_crlf = format(
        "select 1 -- c\nfrom t",
        Dialect::Tsql,
        &with(|o| o.line_ending = LineEnding::Crlf),
    );
    assert_eq!(to_crlf.text, "SELECT 1 -- c\r\nFROM t\r\n");
}

#[test]
fn formatting_is_idempotent_on_the_fixtures() {
    for (f, d) in [
        ("movestock.tsql.sql", Dialect::Tsql),
        ("movestock_reformatted.tsql.sql", Dialect::Tsql),
        ("catch_rethrow.tsql.sql", Dialect::Tsql),
        ("movestock.pg.sql", Dialect::Postgres),
        ("movestock.db2i.sql", Dialect::Db2i),
    ] {
        let src = fixture(f);
        for o in [
            FormatOptions::default(),
            with(|o| {
                o.one_item_per_line = true;
                o.join_lines = true;
                o.postgres_bodies = true;
                o.keyword_case = KeywordCase::Lower;
            }),
        ] {
            let once = format(&src, d, &o);
            assert!(once.kept.is_empty(), "{f}: {:?}", once.kept);
            let twice = format(&once.text, d, &o);
            assert_eq!(once.text, twice.text, "{f}");
        }
    }
}

#[test]
fn the_check_pins_a_failure_to_its_statement() {
    let src = "SELECT a FROM t;\nSELECT b FROM u;\n";
    let parsed = parse(src, Dialect::Postgres);
    let root = parsed.syntax();
    let o = FormatOptions::default();
    let lay = layout::collect(src, &root, &o);
    let kw = layout::keyword_set(src, &lay.toks, Dialect::Postgres);
    let (out, starts) = layout::render(src, &lay, &o, &kw, &BTreeSet::new());
    let mut base = BTreeSet::new();
    let ok = verify(
        src,
        &out,
        Dialect::Postgres,
        &root,
        &parsed.diagnostics,
        &lay,
        &starts,
        &mut base,
    );
    assert!(ok.is_none());
    // A layout bug that changed a token of the second statement.
    let bad = out.replace("u;", "v;");
    let f = verify(
        src,
        &bad,
        Dialect::Postgres,
        &root,
        &parsed.diagnostics,
        &lay,
        &starts,
        &mut base,
    )
    .expect("caught");
    assert_eq!(f.tops, vec![1]);
    // A bug that splits a token: `<>` read as `<` `>`.
    let src = "SELECT 1 WHERE a <> b;";
    let parsed = parse(src, Dialect::Tsql);
    let root = parsed.syntax();
    let lay = layout::collect(src, &root, &o);
    let f = verify(
        src,
        "SELECT 1\nWHERE a < > b;\n",
        Dialect::Tsql,
        &root,
        &parsed.diagnostics,
        &lay,
        &starts,
        &mut BTreeSet::new(),
    )
    .expect("caught");
    assert_eq!(f.tops, vec![0]);
}

#[test]
fn never_changes_tokens_on_arbitrary_input() {
    // Random concatenations of awkward pieces: whatever the layout does, the
    // result reads as the input's tokens, or is the input.
    const PIECES: &[&str] = &[
        " ", "\n", "\r\n", "\t", "--c\n", "/*", "*/", "'", "''", "\"", "[", "]", "(", ")", ";",
        ",", ".", "=", "<", ">", "-", "*", "$$", "$a$", "GO", "go\n", "BEGIN", "END", "IF", "ELSE",
        "THEN", "SELECT", "FROM", "WHERE", "a", "1", "@x", "CASE", "WHEN", "WHILE", "LOOP",
        "DECLARE", "TRY", "CATCH", "UNION", "JOIN", "LEFT", "SET", "UPDATE", "VALUES", "N'x'",
        "E'\\n'",
    ];
    let mut seed: u64 = 0x5eed;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..400 {
        let n = (next() % 40) as usize;
        let text: String = (0..n)
            .map(|_| PIECES[(next() % PIECES.len() as u64) as usize])
            .collect();
        for d in [Dialect::Tsql, Dialect::Postgres, Dialect::Db2i] {
            let r = fmt(&text, d);
            if r.text == text {
                continue;
            }
            let a = tokens(&parse(&text, d).syntax());
            let b = tokens(&parse(&r.text, d).syntax());
            assert_eq!(a.len(), b.len(), "{d:?} {text:?} -> {:?}", r.text);
            for (x, y) in a.iter().zip(&b) {
                assert!(
                    x == y || (x.0 == SyntaxKind::IDENT && x.1.eq_ignore_ascii_case(&y.1)),
                    "{d:?} {text:?} -> {:?}",
                    r.text
                );
            }
        }
    }
}

#[test]
fn config_tables_override_by_dialect() {
    let t: toml::Table =
        toml::from_str("indent = 2\nkeyword_case = \"lower\"\n[tsql]\nkeyword_case = \"upper\"\n")
            .unwrap();
    let c = FormatConfig::try_from(t).unwrap();
    assert_eq!(
        c.options(Dialect::Postgres).keyword_case,
        KeywordCase::Lower
    );
    assert_eq!(c.options(Dialect::Tsql).keyword_case, KeywordCase::Upper);
    assert_eq!(c.options(Dialect::Tsql).indent_width, 2);
    let mut hi = FormatConfig::default();
    hi.all.indent = Some(8);
    let mut lo = c.clone();
    lo.merge(&hi);
    assert_eq!(lo.options(Dialect::Db2i).indent_width, 8);
    assert_eq!(lo.options(Dialect::Db2i).keyword_case, KeywordCase::Lower);
    // Round trip through TOML.
    let back = FormatConfig::try_from(toml::Table::from(c.clone())).unwrap();
    assert_eq!(back, c);
    // Unknown keys and bad values are errors.
    let bad: toml::Table = toml::from_str("indnet = 2").unwrap();
    assert!(FormatConfig::try_from(bad).is_err());
    let bad: toml::Table = toml::from_str("[postgres]\nindent = 0").unwrap();
    assert!(FormatConfig::try_from(bad)
        .unwrap_err()
        .contains("format.postgres.indent"));
}
