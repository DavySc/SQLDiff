//! LIKE on a fixed-length `char(n)` value. Both dialects blank-pad it, and
//! the witness evaluator keeps values unpadded, so it would answer for the
//! unpadded text (`'abc'::char(6) LIKE 'abc'` is false on PostgreSQL). A
//! subject or pattern that may be `char(n)` is not evaluated: the pair is
//! never shown Different (or Equal) on such a witness. `varchar(n)` is the
//! control: there the verdict is right.

use sqldiff_core::{diff, Artifact, Dialect, Options, Verdict};

fn verdict(ddl: &str, a: &str, b: &str) -> Verdict {
    let opts = Options {
        catalog_a: vec![Artifact::new("catalog.sql", Dialect::Postgres, ddl)],
        catalog_b: vec![Artifact::new("catalog.sql", Dialect::Postgres, ddl)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", Dialect::Postgres, a),
        &Artifact::new("b.sql", Dialect::Postgres, b),
        &opts,
    )
    .verdict()
}

fn pair(ty: &str, body: impl Fn(&str) -> String) -> (Verdict, Verdict) {
    let ddl = format!("CREATE TABLE t (id integer PRIMARY KEY, c {ty} NOT NULL);");
    (
        verdict(&ddl, &body("'abc'"), &body("'zzzzzzz'")),
        verdict(&ddl, &body("'abc'"), &body("'abc'")),
    )
}

fn routine(params: &str, decl: &str, cond: &str) -> impl Fn(&str) -> String {
    let (params, decl, cond) = (params.to_string(), decl.to_string(), cond.to_string());
    move |pat| {
        format!(
            "CREATE FUNCTION g({params}) RETURNS integer LANGUAGE plpgsql AS $$ {decl} BEGIN IF {} THEN RETURN 1; END IF; RETURN 0; END $$;",
            cond.replace("PAT", pat)
        )
    }
}

fn row(pat: &str) -> String {
    format!("CREATE FUNCTION g(p_id integer) RETURNS integer LANGUAGE plpgsql AS $$ DECLARE r integer; BEGIN SELECT count(*) INTO r FROM t WHERE id = p_id AND c = 'abc' AND c LIKE {pat}; RETURN r; END $$;")
}

fn not_different(v: Verdict, what: &str) {
    assert!(
        !matches!(v, Verdict::Different),
        "{what}: char(n) is blank-padded, LIKE must not be evaluated unpadded"
    );
}

#[test]
fn rows_of_a_char_column() {
    let (d, _) = pair("char(6)", row);
    not_different(d, "row");
    // varchar(6) control: the difference is real.
    let (d, _) = pair("varchar(6)", row);
    assert!(matches!(d, Verdict::Different), "{d:?}");
}

#[test]
fn char_parameters() {
    let body = |ty: &str| routine(&format!("p {ty}"), "", "p = 'abc' AND p LIKE PAT");
    let ddl = "CREATE TABLE t (id integer PRIMARY KEY);";
    let f = body("char(6)");
    not_different(verdict(ddl, &f("'abc'"), &f("'zzzzzzz'")), "param");
    let f = body("varchar(6)");
    let v = verdict(ddl, &f("'abc'"), &f("'zzzzzzz'"));
    assert!(matches!(v, Verdict::Different), "{v:?}");
}

#[test]
fn char_variables() {
    let body = |ty: &str| {
        routine(
            "p integer",
            &format!("DECLARE v {ty} := 'abc';"),
            "v LIKE PAT",
        )
    };
    let ddl = "CREATE TABLE t (id integer PRIMARY KEY);";
    let f = body("char(6)");
    not_different(verdict(ddl, &f("'abc'"), &f("'zzzzzzz'")), "variable");
    let f = body("varchar(6)");
    let v = verdict(ddl, &f("'abc'"), &f("'zzzzzzz'"));
    assert!(matches!(v, Verdict::Different), "{v:?}");
}

#[test]
fn a_char_column_written_short_is_still_proven() {
    // Nothing reads the padding: the write itself is not refused.
    let ddl = "CREATE TABLE t (id integer PRIMARY KEY, c char(6) NOT NULL);";
    let a = "INSERT INTO t (id, c) VALUES (1, 'abc');";
    let v = verdict(ddl, a, a);
    assert!(
        matches!(v, Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{v:?}"
    );
}

fn query_verdict(d: Dialect, ty: &str, a: &str, b: &str) -> Verdict {
    let ddl = format!("CREATE TABLE t (id integer PRIMARY KEY, c {ty} NOT NULL);");
    let opts = Options {
        catalog_a: vec![Artifact::new("catalog.sql", d, &ddl)],
        catalog_b: vec![Artifact::new("catalog.sql", d, &ddl)],
        ..Options::default()
    };
    let q = |w: &str| format!("SELECT id FROM t WHERE c = 'abc' AND {w};");
    diff(
        &Artifact::new("a.sql", d, q(a)),
        &Artifact::new("b.sql", d, q(b)),
        &opts,
    )
    .verdict()
}

/// Every wrapper that keeps the type of a `char(n)` value is as unsafe as
/// the bare column: the verdict must not be Different, while the same
/// condition on `varchar(6)` is judged (the control).
#[test]
fn wrappers_that_keep_the_char_type() {
    let pg = [
        "COALESCE(c, 'x') LIKE",
        "NULLIF(c, 'q') LIKE",
        "CASE WHEN id > 0 THEN c END LIKE",
        "(SELECT c FROM t t2 WHERE t2.id = t.id) LIKE",
        "GREATEST(c, 'a') LIKE",
    ];
    let mut bad = Vec::new();
    for w in pg {
        let v = query_verdict(
            Dialect::Postgres,
            "char(6)",
            &format!("{w} 'abc'"),
            &format!("{w} 'zzzzzzz'"),
        );
        if matches!(v, Verdict::Different) {
            bad.push(format!("pg {w}"));
        }
    }
    let ms = [
        "c + '%'",
        "COALESCE(c, 'x')",
        "ISNULL(c, 'x')",
        "CASE WHEN id > 0 THEN c END",
        "(SELECT c FROM t t2 WHERE t2.id = t.id)",
        "CONCAT(c, '')",
        "c",
    ];
    for w in ms {
        let v = query_verdict(
            Dialect::Tsql,
            "char(6)",
            &format!("'abc' LIKE {w}"),
            "1 = 0",
        );
        if matches!(v, Verdict::Different) {
            bad.push(format!("tsql {w}"));
        }
    }
    let v = query_verdict(Dialect::Tsql, "char(6)", "c + 'x' LIKE 'abcx'", "1 = 0");
    if matches!(v, Verdict::Different) {
        bad.push("tsql c + 'x' LIKE".into());
    }
    assert!(bad.is_empty(), "shown Different:\n{}", bad.join("\n"));
}
