//! Binder false positives found on real public code (pg_partman, pgmq, IBM i
//! samples, WideWorldImporters, Ola Hallengren's maintenance solution), each
//! with a twin that still has the error, so a fix never hides a real one.
//! The whole repositories run in CI through scripts/real-code-gate.py.

use sqldiff_core::{check, diff, Artifact, Dialect, Options, Verdict};

fn bind_findings(d: Dialect, text: &str) -> Vec<(String, Verdict, String)> {
    let r = check(&Artifact::new("x.sql", d, text), &Options::default());
    r.findings
        .into_iter()
        .filter(|f| f.rule.starts_with("bind."))
        .map(|f| (f.rule, f.verdict, f.witness))
        .collect()
}

fn different(d: Dialect, text: &str) -> Vec<(String, Verdict, String)> {
    bind_findings(d, text)
        .into_iter()
        .filter(|f| f.1 == Verdict::Different)
        .collect()
}

#[track_caller]
fn clean(d: Dialect, text: &str) {
    let f = different(d, text);
    assert!(f.is_empty(), "false positive(s): {f:#?}\n--\n{text}");
}

#[track_caller]
fn caught(d: Dialect, text: &str, rule: &str, name: &str) {
    let f = different(d, text);
    assert!(
        f.iter().any(|x| x.0 == rule && x.2.contains(name)),
        "expected {rule} on `{name}`, got {f:#?}\n--\n{text}"
    );
}

const PG_T: &str = "CREATE TABLE t (id int PRIMARY KEY, pattern text, queue_name text, CONSTRAINT t_u UNIQUE (pattern));\nCREATE TABLE u (id int, v text);\n";

#[test]
fn pg_set_names_settings() {
    let pg = Dialect::Postgres;
    clean(pg, "SET search_path = public, pg_catalog;\nSET LOCAL search_path TO pg_catalog, pg_temp;\nSET ROLE someone;\nSET client_min_messages = warning;\nSET TIME ZONE 'UTC';\nSET SESSION AUTHORIZATION DEFAULT;\n");
    clean(pg, "CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$\nBEGIN\n  SET search_path = public;\nEND $$;");
    // An assignment of an undeclared variable is still an error.
    caught(
        pg,
        "CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$\nBEGIN\n  nope := 1;\nEND $$;",
        "bind.unknown-variable",
        "nope",
    );
}

#[test]
fn pg_named_arguments() {
    let pg = Dialect::Postgres;
    clean(pg, "CREATE FUNCTION h(fq text) RETURNS void LANGUAGE plpgsql AS $$\nBEGIN\n  PERFORM public.create_parent(\n    p_parent_table := fq,\n    p_type := case when fq = 'x' then 'range' else 'native' end);\n  PERFORM public.create_parent(p_parent_table => fq);\nEND $$;");
    // The argument values are still bound.
    caught(
        pg,
        "CREATE FUNCTION h(fq text) RETURNS void LANGUAGE plpgsql AS $$\nBEGIN\n  PERFORM public.create_parent(p_parent_table := fq_typo);\nEND $$;",
        "bind.unknown-variable",
        "fq_typo",
    );
}

#[test]
fn pg_overload_defined_later_and_defaults() {
    let pg = Dialect::Postgres;
    // pgmq: the two-argument version calls the one-argument version created after it.
    clean(pg, "CREATE FUNCTION q.drop_queue(queue_name text, partitioned boolean) RETURNS boolean LANGUAGE plpgsql AS $$\nBEGIN\n  PERFORM q.drop_queue(queue_name);\n  RETURN true;\nEND $$;\nCREATE FUNCTION q.drop_queue(queue_name text) RETURNS boolean LANGUAGE plpgsql AS $$\nBEGIN\n  RETURN true;\nEND $$;");
    clean(pg, "CREATE FUNCTION f(a text, b boolean DEFAULT false) RETURNS void LANGUAGE plpgsql AS $$ BEGIN END $$;\nCREATE FUNCTION g() RETURNS void LANGUAGE plpgsql AS $$\nBEGIN\n  PERFORM f('x');\nEND $$;");
    // No overload anywhere accepts three arguments.
    caught(
        pg,
        "CREATE FUNCTION f(a text) RETURNS void LANGUAGE plpgsql AS $$ BEGIN END $$;\nCREATE FUNCTION g() RETURNS void LANGUAGE plpgsql AS $$\nBEGIN\n  PERFORM f('x', 1, 2);\nEND $$;",
        "bind.signature",
        "f",
    );
    // At the top level a routine created later does not exist yet.
    caught(
        pg,
        "CREATE FUNCTION f(a text) RETURNS int LANGUAGE sql AS 'SELECT 1';\nSELECT f('x', 1);\nCREATE FUNCTION f(a text, b int) RETURNS int LANGUAGE sql AS 'SELECT 1';",
        "bind.signature",
        "f",
    );
}

#[test]
fn derived_tables_read_their_own_relations() {
    let pg = Dialect::Postgres;
    clean(
        pg,
        &format!("{PG_T}SELECT 1 FROM (SELECT id FROM t) x;\nSELECT x.id FROM (SELECT id FROM t WHERE pattern = 'a') x JOIN u ON u.id = x.id;\n"),
    );
    caught(
        pg,
        &format!("{PG_T}SELECT x.nope FROM (SELECT id FROM t) x;\n"),
        "bind.unknown-column",
        "nope",
    );
    caught(
        pg,
        &format!("{PG_T}SELECT 1 FROM (SELECT nope FROM t) x;\n"),
        "bind.unknown-column",
        "nope",
    );
}

#[test]
fn pg_dump_parenthesized_joins() {
    let pg = Dialect::Postgres;
    clean(
        pg,
        &format!("{PG_T}CREATE VIEW v AS SELECT t.id, u.v, u2.v AS v2 FROM ((t JOIN u ON ((t.id = u.id))) LEFT JOIN u u2 ON ((u2.id = t.id)));\nSELECT pattern FROM (t JOIN u ON t.id = u.id), u u3;\n"),
    );
    caught(
        pg,
        &format!("{PG_T}CREATE VIEW v AS SELECT t.nope FROM ((t JOIN u ON ((t.id = u.id))) LEFT JOIN u u2 ON ((u2.id = t.id)));\n"),
        "bind.unknown-column",
        "nope",
    );
}

#[test]
fn pg_insert_values_and_conflict_constraint() {
    let pg = Dialect::Postgres;
    clean(
        pg,
        &format!("{PG_T}CREATE FUNCTION f(pattern text, queue_name text) RETURNS void LANGUAGE plpgsql AS $$\nBEGIN\n  INSERT INTO t (id, pattern, queue_name) VALUES (1, pattern, queue_name)\n    ON CONFLICT ON CONSTRAINT t_u DO NOTHING;\n  IF NOT (queue_name OPERATOR(pg_catalog.=) 'x'::pg_catalog.text) THEN\n    RETURN;\n  END IF;\nEND $$;"),
    );
    // In DO UPDATE the target's columns are visible: there the name is ambiguous.
    caught(
        pg,
        &format!("{PG_T}CREATE FUNCTION f(pattern text) RETURNS void LANGUAGE plpgsql AS $$\nBEGIN\n  INSERT INTO t (id, pattern) VALUES (1, 'a')\n    ON CONFLICT (id) DO UPDATE SET queue_name = pattern;\nEND $$;"),
        "bind.ambiguous-name",
        "pattern",
    );
    caught(
        pg,
        &format!("{PG_T}CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$\nBEGIN\n  INSERT INTO t (id, pattern) VALUES (1, nope);\nEND $$;"),
        "bind.unknown-variable",
        "nope",
    );
}

#[test]
fn db2_trigger_predicates_and_concat() {
    let db2 = Dialect::Db2i;
    let trigger = "CREATE TABLE s.c (id INT, name VARCHAR(10));\nCREATE OR REPLACE TRIGGER s.c_trg\n  AFTER UPDATE OR INSERT OR DELETE ON s.c\n  REFERENCING NEW AS n OLD AS o FOR EACH ROW\n  WHEN (INSERTING OR UPDATING OR DELETING)\nBEGIN ATOMIC\n  DECLARE operation VARCHAR(10);\n  IF INSERTING THEN\n    SET operation = 'INSERT';\n  END IF;\n  IF (DELETING OR UPDATING) THEN\n    SET operation = 'X' CONCAT n.name CONCAT 'Y';\n  END IF;\nEND";
    clean(db2, trigger);
    clean(db2, "VALUES 'a' CONCAT 'b';");
    caught(
        db2,
        "CREATE PROCEDURE p ()\nLANGUAGE SQL\nBEGIN\n  DECLARE x VARCHAR(10);\n  IF INSERTING THEN\n    SET x = 'a';\n  END IF;\nEND",
        "bind.unknown-variable",
        "INSERTING",
    );
    caught(
        db2,
        "CREATE PROCEDURE p ()\nLANGUAGE SQL\nBEGIN\n  DECLARE x VARCHAR(10);\n  SET x = 'a' CONCAT nope;\nEND",
        "bind.unknown-variable",
        "nope",
    );
}

#[test]
fn tsql_exec_goto_hints_and_static_methods() {
    let ts = Dialect::Tsql;
    let src = "CREATE TABLE dbo.d (BackupSize int, NumberOfDirectories int, MaxNumberOfFiles int);\nGO\nCREATE PROCEDURE dbo.p @n int AS\nBEGIN\n  DECLARE @x int, @sp nvarchar(100) = N'sp_executesql', @c nvarchar(max) = N'', @g geography;\n  DECLARE @s TABLE (ObjectID int);\n  SELECT @x = (SELECT MIN(F) FROM (SELECT (BackupSize / NumberOfDirectories) AS F UNION SELECT MaxNumberOfFiles / NumberOfDirectories) Files) * NumberOfDirectories\n  FROM dbo.d OPTION (RECOMPILE, MAXDOP 1);\n  EXECUTE @sp @stmt = @c;\n  INSERT INTO @s (ObjectID)\n  EXECUTE sp_executesql @stmt = @c, @params = N'@a int', @a = @n;\n  SET @g = geography::Point(37.7, -122.5, 4326);\n  IF @n > 0\n  BEGIN\n    GOTO Done\n  END\n  Done:\n  RETURN 0\nEND";
    clean(ts, src);
    caught(
        ts,
        &src.replace("@stmt = @c;", "@stmt = @nope;"),
        "bind.unknown-variable",
        "@nope",
    );
    caught(
        ts,
        &src.replace("GOTO Done", "GOTO Gone"),
        "bind.unknown-label",
        "Gone",
    );
    caught(
        ts,
        &src.replace("@a = @n;", "@a = @missing;"),
        "bind.unknown-variable",
        "@missing",
    );
}

/// Placeholders, client directives and unreadable headers make binding
/// errors unknown, never `different`, and never let a diff go `equal`.
#[test]
fn unreadable_input_degrades_to_unknown() {
    let pg = Dialect::Postgres;
    let ext = "CREATE FUNCTION @extschema@.apply(p_parent text) RETURNS void LANGUAGE plpgsql AS $$\nBEGIN\n  PERFORM 1 FROM @extschema@.part_config WHERE parent_table = p_parent;\nEND $$;";
    clean(pg, ext);
    let f = bind_findings(pg, ext);
    assert!(
        f.iter().all(|x| x.1 == Verdict::Unknown) && !f.is_empty(),
        "{f:#?}"
    );
    clean(
        pg,
        &format!("{PG_T}SELECT id FROM t WHERE id = ANY(ARRAY[:id1, :id2]);\n"),
    );
    clean(
        pg,
        &format!("{PG_T}\\set id 1\nSELECT id FROM t WHERE id = :'id';\n"),
    );
    // Array slices are not psql variables: an unknown name in one is still an error.
    caught(
        pg,
        "CREATE FUNCTION f(a int[]) RETURNS int[] LANGUAGE plpgsql AS $$\nBEGIN\n  RETURN a[1:nope];\nEND $$;",
        "bind.unknown-variable",
        "nope",
    );
    let ts = Dialect::Tsql;
    clean(ts, ":setvar db Sales\nGO\nSELECT x FROM $(db).dbo.t;\n");
    clean(
        ts,
        ":r .\\other.sql\nGO\nCREATE PROCEDURE p AS SELECT @declared_in_other;\n",
    );

    // A diff where B introduces an error next to a placeholder is not equal.
    let a = Artifact::new(
        "a.sql",
        pg,
        format!("{PG_T}CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$\nBEGIN\n  PERFORM 1 FROM @extschema@.t WHERE id = 1;\nEND $$;"),
    );
    let b = Artifact::new(
        "b.sql",
        pg,
        format!("{PG_T}CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$\nBEGIN\n  PERFORM 1 FROM @extschema@.t WHERE id = nope;\nEND $$;"),
    );
    let r = diff(&a, &b, &Options::default());
    assert_ne!(r.verdict, Verdict::Equal, "{:#?}", r.findings);
    // And B fixing an error A had is not silently dropped.
    let r = diff(&b, &a, &Options::default());
    assert_ne!(r.verdict, Verdict::Equal, "{:#?}", r.findings);
}
