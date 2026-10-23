//! M4 gate: every row of the dialect semantic tables
//! (`profiles/semantics/*.toml`) has a test. Behavioural rows are exercised
//! by running a small routine through lowering and the transaction
//! interpreter and checking both the outcome and that the row was used;
//! descriptive rows (DDL, prerequisites, cardinality contracts, ...) have
//! value tests. `every_row_has_a_test` fails when a row is added without one.

use std::collections::BTreeSet;

use sqldiff_core::artifact::Analyzed;
use sqldiff_core::ir::cfg::Action;
use sqldiff_core::ir::{self, ErrorKind, Exit, Fate, Fault, Outcome, Profile, Scenario};
use sqldiff_core::model::Side;
use sqldiff_core::{Artifact, Dialect};

use Dialect::{Db2i, Postgres as Pg, Tsql};

/// Where a case injects its fault.
#[derive(Clone, Copy)]
enum At {
    None,
    /// The statement that writes this table.
    Write(&'static str, ErrorKind),
    /// The first COMMIT.
    Commit(ErrorKind),
}

struct Case {
    rows: &'static [&'static str],
    d: Dialect,
    sql: &'static str,
    caller_txn: bool,
    at: At,
    expect: &'static str,
}

const fn case(
    rows: &'static [&'static str],
    d: Dialect,
    sql: &'static str,
    at: At,
    expect: &'static str,
) -> Case {
    Case {
        rows,
        d,
        sql,
        caller_txn: false,
        at,
        expect,
    }
}

const fn caller(
    rows: &'static [&'static str],
    d: Dialect,
    sql: &'static str,
    at: At,
    expect: &'static str,
) -> Case {
    Case {
        rows,
        d,
        sql,
        caller_txn: true,
        at,
        expect,
    }
}

use At::{Commit, None as NoFault, Write};
use ErrorKind::{Cancel, Constraint, Conversion, Lock, NameResolution, NotFound};

fn tag(key: &str) -> String {
    key.split(':')
        .nth(1)
        .unwrap_or(key)
        .split('#')
        .next()
        .unwrap_or(key)
        .to_lowercase()
}

fn summary(cfg: &ir::Cfg, o: &Outcome) -> String {
    let mut parts = vec![match &o.exit {
        Exit::Normal => "normal".to_string(),
        Exit::Code(c) => format!("code {c}"),
        Exit::Raised(_) => "raised".to_string(),
    }];
    if o.raised && o.exit == Exit::Normal {
        parts.push("error-sent".into());
    }
    for (name, f) in [
        ("durable", Fate::Durable),
        ("undone", Fate::Undone),
        ("caller", Fate::Caller),
        ("partial", Fate::Partial),
    ] {
        let mut v: Vec<String> = o
            .effects
            .iter()
            .filter(|(_, x)| **x == f)
            .map(|(k, _)| tag(k))
            .collect();
        v.sort();
        if !v.is_empty() {
            parts.push(format!("{name}=[{}]", v.join(",")));
        }
    }
    if o.doomed {
        parts.push("doomed".into());
    }
    for (n, t, s) in &o.temp_reads {
        let by = cfg
            .node(*n)
            .stmt()
            .and_then(|s| s.durable_effect())
            .map(|e| tag(&e.key))
            .unwrap_or_default();
        parts.push(format!("{by} reads {}={:?}", t.to_lowercase(), s).to_lowercase());
    }
    parts.join(" ")
}

fn run(c: &Case) -> (String, BTreeSet<String>, Outcome) {
    let art = Artifact::new("case.sql", c.d, c.sql);
    let a = Analyzed::new(&art, Side::A, 3);
    assert!(
        a.parse.diagnostics.is_empty(),
        "{:?}\n{}",
        a.parse.diagnostics,
        c.sql
    );
    let cfg = ir::lower(&a);
    let faults = match c.at {
        At::None => Vec::new(),
        At::Write(t, kind) => {
            let node = cfg
                .effect_nodes()
                .into_iter()
                .find(|(_, e)| tag(&e.key) == t)
                .unwrap_or_else(|| panic!("no write to {t}\n{}", cfg.dump()))
                .0;
            vec![Fault { node, kind }]
        }
        At::Commit(kind) => {
            let node = (0..cfg.nodes.len())
                .find(|n| {
                    cfg.node(*n)
                        .stmt()
                        .is_some_and(|s| s.action == Action::Commit)
                })
                .expect("a COMMIT");
            vec![Fault { node, kind }]
        }
    };
    let sc = Scenario {
        caller_txn: c.caller_txn,
        faults,
        ..Scenario::default()
    };
    let outs = ir::explore(&cfg, &sc);
    assert_eq!(outs.len(), 1, "{}\n{}", c.sql, cfg.dump());
    let o = outs.into_iter().next().unwrap();
    (summary(&cfg, &o), o.rows.clone(), o)
}

const CASES: &[Case] = &[
    // ---------------------------------------------------------------- T-SQL
    case(
        &["tsql.unhandled.statement-only", "tsql.atomicity.statement", "tsql.transactions.routine_mode.procedure", "tsql.transactions.nesting"],
        Tsql,
        "CREATE PROCEDURE p AS BEGIN BEGIN TRAN; INSERT INTO a VALUES (1); UPDATE b SET x = 1; INSERT INTO c VALUES (1); COMMIT; END",
        Write("b", Constraint),
        "normal error-sent durable=[a,c]",
    ),
    case(
        &["tsql.unhandled.xact-abort-on", "tsql.transactions.session_switch"],
        Tsql,
        "CREATE PROCEDURE p AS BEGIN SET XACT_ABORT ON; BEGIN TRAN; INSERT INTO a VALUES (1); INSERT INTO b VALUES (1); COMMIT; END",
        Write("b", Constraint),
        "raised undone=[a]",
    ),
    case(
        &["tsql.unhandled.raiserror"],
        Tsql,
        "CREATE PROCEDURE p AS BEGIN SET XACT_ABORT ON; BEGIN TRAN; INSERT INTO a VALUES (1); RAISERROR('x', 16, 1); INSERT INTO b VALUES (1); COMMIT; END",
        NoFault,
        "normal error-sent durable=[a,b]",
    ),
    case(
        &["tsql.unhandled.throw", "tsql.transactions.trancount_mismatch_at_exit"],
        Tsql,
        "CREATE PROCEDURE p AS BEGIN BEGIN TRAN; INSERT INTO a VALUES (1); THROW 50001, 'x', 1; INSERT INTO b VALUES (1); END",
        NoFault,
        "raised caller=[a]",
    ),
    case(
        &["tsql.unhandled.cancel"],
        Tsql,
        "CREATE PROCEDURE p AS BEGIN BEGIN TRY INSERT INTO a VALUES (1); INSERT INTO b VALUES (1); END TRY BEGIN CATCH INSERT INTO h VALUES (1); END CATCH END",
        Write("b", Cancel),
        "raised durable=[a]",
    ),
    case(
        &["tsql.unhandled.batch-abort-lock"],
        Tsql,
        "CREATE PROCEDURE p AS BEGIN BEGIN TRAN; INSERT INTO a VALUES (1); INSERT INTO b VALUES (1); COMMIT; END",
        Write("b", Lock),
        "raised undone=[a]",
    ),
    case(
        &["tsql.unhandled.batch-abort-conversion"],
        Tsql,
        "CREATE PROCEDURE p AS BEGIN BEGIN TRAN; INSERT INTO a VALUES (1); INSERT INTO b VALUES (1); COMMIT; END",
        Write("b", Conversion),
        "raised undone=[a]",
    ),
    case(
        &["tsql.unhandled.batch-abort-name-resolution"],
        Tsql,
        "CREATE PROCEDURE p AS BEGIN BEGIN TRAN; INSERT INTO a VALUES (1); INSERT INTO b VALUES (1); COMMIT; END",
        Write("b", NameResolution),
        "raised caller=[a]",
    ),
    case(
        &["tsql.caught.try-catch-xact-abort-on", "tsql.transactions.doomed_commit"],
        Tsql,
        "CREATE PROCEDURE p AS BEGIN SET XACT_ABORT ON; BEGIN TRY BEGIN TRAN; INSERT INTO a VALUES (1); INSERT INTO b VALUES (1); COMMIT; END TRY BEGIN CATCH COMMIT; END CATCH END",
        Write("b", Constraint),
        "raised undone=[a]",
    ),
    case(
        &["tsql.transactions.doomed_writes"],
        Tsql,
        "CREATE PROCEDURE p AS BEGIN SET XACT_ABORT ON; BEGIN TRY BEGIN TRAN; INSERT INTO a VALUES (1); INSERT INTO b VALUES (1); COMMIT; END TRY BEGIN CATCH INSERT INTO h VALUES (1); END CATCH END",
        Write("b", Constraint),
        "raised undone=[a]",
    ),
    case(
        &["tsql.caught.try-catch-deadlock"],
        Tsql,
        "CREATE PROCEDURE p AS BEGIN BEGIN TRY BEGIN TRAN; INSERT INTO a VALUES (1); INSERT INTO b VALUES (1); COMMIT; END TRY BEGIN CATCH INSERT INTO h VALUES (1); END CATCH END",
        Write("b", Lock),
        "normal durable=[h] undone=[a]",
    ),
    case(
        &["tsql.caught.try-catch", "tsql.transactions.rollback_scope"],
        Tsql,
        "CREATE PROCEDURE p AS BEGIN BEGIN TRY BEGIN TRAN; INSERT INTO a VALUES (1); INSERT INTO b VALUES (1); COMMIT; END TRY BEGIN CATCH IF @@TRANCOUNT > 0 ROLLBACK; RETURN 7; END CATCH END",
        Write("b", Constraint),
        "code 7 undone=[a]",
    ),
    case(
        &["tsql.temp.table-variable"],
        Tsql,
        "CREATE PROCEDURE p AS BEGIN DECLARE @t TABLE (c int); BEGIN TRAN; INSERT INTO @t VALUES (1); ROLLBACK; INSERT INTO a SELECT c FROM @t; END",
        NoFault,
        "normal durable=[a] a reads t=rows",
    ),
    case(
        &["tsql.temp.local-temp-table"],
        Tsql,
        "CREATE PROCEDURE p AS BEGIN CREATE TABLE #t (c int); BEGIN TRAN; INSERT INTO #t VALUES (1); ROLLBACK; INSERT INTO a SELECT c FROM #t; END",
        NoFault,
        "normal durable=[a] a reads t=empty",
    ),
    case(
        &["tsql.transactions.commit_without_txn"],
        Tsql,
        "CREATE PROCEDURE p AS BEGIN INSERT INTO a VALUES (1); COMMIT; END",
        NoFault,
        "normal error-sent durable=[a]",
    ),
    case(
        &["tsql.transactions.rollback_without_txn"],
        Tsql,
        "CREATE PROCEDURE p AS BEGIN INSERT INTO a VALUES (1); ROLLBACK; END",
        NoFault,
        "normal error-sent durable=[a]",
    ),
    case(
        &["tsql.transactions.savepoints"],
        Tsql,
        "CREATE PROCEDURE p AS BEGIN BEGIN TRAN; INSERT INTO a VALUES (1); SAVE TRAN s; INSERT INTO b VALUES (1); ROLLBACK TRAN s; COMMIT; END",
        NoFault,
        "normal durable=[a] undone=[b]",
    ),
    caller(
        &["tsql.transactions.commit_in_caller_txn"],
        Tsql,
        "CREATE PROCEDURE p AS BEGIN BEGIN TRAN; INSERT INTO a VALUES (1); COMMIT; END",
        NoFault,
        "normal caller=[a,caller]",
    ),
    case(
        &["tsql.transactions.routine_mode.trigger"],
        Tsql,
        "CREATE TRIGGER tr ON x AFTER INSERT AS BEGIN INSERT INTO a VALUES (1); END",
        NoFault,
        "normal durable=[a]",
    ),
    case(
        &["tsql.transactions.routine_mode.function"],
        Tsql,
        "CREATE FUNCTION f() RETURNS int AS BEGIN RETURN 1; END",
        NoFault,
        "normal",
    ),
    case(
        &["tsql.transactions.routine_mode.script"],
        Tsql,
        "BEGIN TRAN;\nINSERT INTO a VALUES (1);\nCOMMIT;\nINSERT INTO b VALUES (1);",
        NoFault,
        "normal durable=[a,b]",
    ),
    case(
        &["tsql.atomicity.merge"],
        Tsql,
        "CREATE PROCEDURE p AS BEGIN MERGE INTO m USING s ON m.id = s.id WHEN MATCHED THEN UPDATE SET x = 1; END",
        Write("m", Constraint),
        "normal error-sent",
    ),
    // ----------------------------------------------------------- PostgreSQL
    case(
        &["postgres.unhandled.routine", "postgres.transactions.routine_mode.procedure", "postgres.transactions.nesting", "postgres.atomicity.statement"],
        Pg,
        "CREATE PROCEDURE p() LANGUAGE plpgsql AS $$ BEGIN INSERT INTO a VALUES (1); COMMIT; INSERT INTO b VALUES (1); UPDATE c SET x = 1; END; $$;",
        Write("c", Constraint),
        "raised durable=[a] undone=[b]",
    ),
    caller(
        &["postgres.unhandled.routine-in-caller-txn"],
        Pg,
        "CREATE PROCEDURE p() LANGUAGE plpgsql AS $$ BEGIN INSERT INTO a VALUES (1); INSERT INTO b VALUES (1); END; $$;",
        Write("b", Constraint),
        "raised undone=[a,caller] doomed",
    ),
    case(
        &["postgres.unhandled.transaction-block", "postgres.transactions.doomed_writes", "postgres.transactions.doomed_commit", "postgres.transactions.routine_mode.script"],
        Pg,
        "BEGIN;\nINSERT INTO a VALUES (1);\nINSERT INTO b VALUES (1);\nINSERT INTO c VALUES (1);\nCOMMIT;",
        Write("b", Constraint),
        "normal error-sent undone=[a]",
    ),
    case(
        &["postgres.unhandled.autocommit"],
        Pg,
        "INSERT INTO a VALUES (1);\nINSERT INTO b VALUES (1);\nINSERT INTO c VALUES (1);",
        Write("b", Constraint),
        "normal error-sent durable=[a,c]",
    ),
    case(
        &["postgres.caught.exception-block", "postgres.atomicity.atomic_compound"],
        Pg,
        "CREATE PROCEDURE p() LANGUAGE plpgsql AS $$ BEGIN INSERT INTO a VALUES (1); BEGIN INSERT INTO b VALUES (1); INSERT INTO c VALUES (1); EXCEPTION WHEN unique_violation THEN INSERT INTO h VALUES (1); END; END; $$;",
        Write("c", Constraint),
        "normal durable=[a,h] undone=[b]",
    ),
    case(
        &["postgres.temp.temporary-table"],
        Pg,
        "CREATE PROCEDURE p() LANGUAGE plpgsql AS $$ BEGIN CREATE TEMP TABLE t (c int); BEGIN INSERT INTO t VALUES (1); INSERT INTO a VALUES (1); EXCEPTION WHEN OTHERS THEN INSERT INTO b SELECT c FROM t; END; END; $$;",
        Write("a", Constraint),
        "normal durable=[b] b reads t=empty",
    ),
    case(
        &["postgres.transactions.commit_in_function", "postgres.transactions.routine_mode.function"],
        Pg,
        "CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$ BEGIN INSERT INTO a VALUES (1); COMMIT; END; $$;",
        NoFault,
        "raised undone=[a]",
    ),
    case(
        &["postgres.transactions.commit_in_exception_block"],
        Pg,
        "CREATE PROCEDURE p() LANGUAGE plpgsql AS $$ BEGIN BEGIN INSERT INTO a VALUES (1); COMMIT; EXCEPTION WHEN OTHERS THEN INSERT INTO h VALUES (1); END; END; $$;",
        NoFault,
        "normal durable=[h] undone=[a]",
    ),
    caller(
        &["postgres.transactions.commit_in_caller_txn"],
        Pg,
        "CREATE PROCEDURE p() LANGUAGE plpgsql AS $$ BEGIN INSERT INTO a VALUES (1); COMMIT; END; $$;",
        NoFault,
        "raised undone=[a,caller] doomed",
    ),
    case(
        &["postgres.transactions.commit_without_txn"],
        Pg,
        "COMMIT;\nINSERT INTO a VALUES (1);",
        NoFault,
        "normal durable=[a]",
    ),
    case(
        &["postgres.transactions.rollback_without_txn"],
        Pg,
        "ROLLBACK;\nINSERT INTO a VALUES (1);",
        NoFault,
        "normal durable=[a]",
    ),
    case(
        &["postgres.transactions.savepoints", "postgres.transactions.rollback_scope"],
        Pg,
        "BEGIN;\nINSERT INTO a VALUES (1);\nSAVEPOINT s;\nINSERT INTO b VALUES (1);\nROLLBACK TO SAVEPOINT s;\nCOMMIT;\nBEGIN;\nINSERT INTO c VALUES (1);\nROLLBACK;",
        NoFault,
        "normal durable=[a] undone=[b,c]",
    ),
    case(
        &["postgres.transactions.deferred_constraints"],
        Pg,
        "CREATE PROCEDURE p() LANGUAGE plpgsql AS $$ BEGIN INSERT INTO a VALUES (1); COMMIT; END; $$;",
        Commit(Constraint),
        "raised undone=[a]",
    ),
    case(
        &["postgres.transactions.routine_mode.anonymous"],
        Pg,
        "DO $$ BEGIN INSERT INTO a VALUES (1); COMMIT; INSERT INTO b VALUES (1); END; $$;",
        NoFault,
        "normal durable=[a,b]",
    ),
    case(
        &["postgres.atomicity.merge"],
        Pg,
        "CREATE PROCEDURE p() LANGUAGE plpgsql AS $$ BEGIN MERGE INTO m USING s ON m.id = s.id WHEN MATCHED THEN UPDATE SET x = 1; END; $$;",
        Write("m", Constraint),
        "raised",
    ),
    // ------------------------------------------------------------ Db2 for i
    case(
        &["db2i.unhandled.warning", "db2i.transactions.routine_mode.procedure"],
        Db2i,
        "CREATE PROCEDURE p () LANGUAGE SQL SET OPTION COMMIT = *CHG BEGIN UPDATE a SET x = 1; INSERT INTO b VALUES (1); END;",
        Write("a", NotFound),
        "normal durable=[b]",
    ),
    case(
        &["db2i.unhandled.no-commit", "db2i.atomicity.statement", "db2i.transactions.no_commit_levels"],
        Db2i,
        "CREATE PROCEDURE p () LANGUAGE SQL SET OPTION COMMIT = *NONE BEGIN INSERT INTO a VALUES (1); UPDATE b SET x = 1; END;",
        Write("b", Constraint),
        "raised durable=[a] partial=[b]",
    ),
    case(
        &["db2i.unhandled.exception"],
        Db2i,
        "CREATE PROCEDURE p () LANGUAGE SQL SET OPTION COMMIT = *CHG BEGIN INSERT INTO a VALUES (1); INSERT INTO b VALUES (1); END;",
        Write("b", Constraint),
        "raised caller=[a]",
    ),
    case(
        &["db2i.caught.undo-handler-no-commit", "db2i.atomicity.atomic_under_no_commit"],
        Db2i,
        "CREATE PROCEDURE p () LANGUAGE SQL SET OPTION COMMIT = *NONE BEGIN ATOMIC DECLARE UNDO HANDLER FOR SQLEXCEPTION INSERT INTO h VALUES (1); INSERT INTO a VALUES (1); INSERT INTO b VALUES (1); END;",
        Write("b", Constraint),
        "normal durable=[a,h]",
    ),
    case(
        &["db2i.caught.no-commit"],
        Db2i,
        "CREATE PROCEDURE p () LANGUAGE SQL SET OPTION COMMIT = *NONE BEGIN DECLARE CONTINUE HANDLER FOR SQLEXCEPTION SET v = 1; UPDATE a SET x = 1; INSERT INTO b VALUES (1); END;",
        Write("a", Constraint),
        "normal durable=[b] partial=[a]",
    ),
    case(
        &["db2i.caught.continue-handler"],
        Db2i,
        "CREATE PROCEDURE p () LANGUAGE SQL SET OPTION COMMIT = *CHG BEGIN DECLARE CONTINUE HANDLER FOR SQLSTATE '23505' INSERT INTO h VALUES (1); INSERT INTO a VALUES (1); INSERT INTO b VALUES (1); INSERT INTO c VALUES (1); END;",
        Write("b", Constraint),
        "normal durable=[a,c,h]",
    ),
    case(
        &["db2i.caught.exit-handler"],
        Db2i,
        "CREATE PROCEDURE p () LANGUAGE SQL SET OPTION COMMIT = *CHG BEGIN BEGIN DECLARE EXIT HANDLER FOR SQLEXCEPTION INSERT INTO h VALUES (1); INSERT INTO a VALUES (1); INSERT INTO b VALUES (1); INSERT INTO c VALUES (1); END; INSERT INTO d VALUES (1); END;",
        Write("b", Constraint),
        "normal durable=[a,d,h]",
    ),
    case(
        &["db2i.caught.undo-handler"],
        Db2i,
        "CREATE PROCEDURE p () LANGUAGE SQL SET OPTION COMMIT = *CHG BEGIN BEGIN ATOMIC DECLARE UNDO HANDLER FOR SQLEXCEPTION INSERT INTO h VALUES (1); INSERT INTO a VALUES (1); INSERT INTO b VALUES (1); END; INSERT INTO d VALUES (1); END;",
        Write("b", Constraint),
        "normal durable=[d,h] undone=[a]",
    ),
    case(
        &["db2i.temp.dgtt", "db2i.transactions.rollback_scope"],
        Db2i,
        "CREATE PROCEDURE p () LANGUAGE SQL SET OPTION COMMIT = *CHG BEGIN DECLARE GLOBAL TEMPORARY TABLE t (c INT) NOT LOGGED; INSERT INTO SESSION.t VALUES (1); ROLLBACK; INSERT INTO a SELECT c FROM SESSION.t; END;",
        NoFault,
        "normal durable=[a] a reads t=empty",
    ),
    case(
        &["db2i.temp.dgtt"],
        Db2i,
        "CREATE PROCEDURE p () LANGUAGE SQL SET OPTION COMMIT = *CHG BEGIN DECLARE GLOBAL TEMPORARY TABLE t (c INT) NOT LOGGED ON ROLLBACK PRESERVE ROWS; INSERT INTO SESSION.t VALUES (1); ROLLBACK; INSERT INTO a SELECT c FROM SESSION.t; END;",
        NoFault,
        "normal durable=[a] a reads t=rows",
    ),
    case(
        &["db2i.transactions.commit_in_atomic", "db2i.atomicity.atomic_compound"],
        Db2i,
        "CREATE PROCEDURE p () LANGUAGE SQL SET OPTION COMMIT = *CHG BEGIN ATOMIC INSERT INTO a VALUES (1); COMMIT; END;",
        NoFault,
        "raised undone=[a]",
    ),
    caller(
        &["db2i.transactions.commit_in_caller_txn"],
        Db2i,
        "CREATE PROCEDURE p () LANGUAGE SQL SET OPTION COMMIT = *CHG BEGIN INSERT INTO a VALUES (1); COMMIT; END;",
        NoFault,
        "normal durable=[a,caller]",
    ),
    case(
        &["db2i.transactions.commit_without_txn"],
        Db2i,
        "CREATE PROCEDURE p () LANGUAGE SQL SET OPTION COMMIT = *NONE BEGIN INSERT INTO a VALUES (1); COMMIT; END;",
        NoFault,
        "normal durable=[a]",
    ),
    case(
        &["db2i.transactions.rollback_without_txn"],
        Db2i,
        "CREATE PROCEDURE p () LANGUAGE SQL SET OPTION COMMIT = *NONE BEGIN INSERT INTO a VALUES (1); ROLLBACK; END;",
        NoFault,
        "normal durable=[a]",
    ),
    case(
        &["db2i.transactions.savepoints"],
        Db2i,
        "CREATE PROCEDURE p () LANGUAGE SQL SET OPTION COMMIT = *CHG BEGIN INSERT INTO a VALUES (1); SAVEPOINT s ON ROLLBACK RETAIN CURSORS; INSERT INTO b VALUES (1); ROLLBACK TO SAVEPOINT s; END;",
        NoFault,
        "normal durable=[a] undone=[b]",
    ),
    case(
        &["db2i.transactions.savepoints_need_commitment_control"],
        Db2i,
        "CREATE PROCEDURE p () LANGUAGE SQL SET OPTION COMMIT = *NONE BEGIN INSERT INTO a VALUES (1); SAVEPOINT s ON ROLLBACK RETAIN CURSORS; INSERT INTO b VALUES (1); END;",
        NoFault,
        "raised durable=[a]",
    ),
    case(
        &["db2i.atomicity.merge"],
        Db2i,
        "CREATE PROCEDURE p () LANGUAGE SQL SET OPTION COMMIT = *CHG BEGIN MERGE INTO m USING s ON m.id = s.id WHEN MATCHED THEN UPDATE SET x = 1; END;",
        Write("m", Constraint),
        "raised",
    ),
    case(
        &["db2i.atomicity.merge_not_atomic"],
        Db2i,
        "CREATE PROCEDURE p () LANGUAGE SQL SET OPTION COMMIT = *CHG BEGIN MERGE INTO m USING s ON m.id = s.id WHEN MATCHED THEN UPDATE SET x = 1 NOT ATOMIC CONTINUE ON SQLEXCEPTION; END;",
        Write("m", Constraint),
        "raised caller=[m]",
    ),
];

#[test]
fn behavioural_rows() {
    let mut failures = Vec::new();
    for c in CASES {
        let (got, rows, _) = run(c);
        if got != c.expect {
            failures.push(format!(
                "{:?}\n  sql:  {}\n  want: {}\n  got:  {got}",
                c.rows, c.sql, c.expect
            ));
        }
        for r in c.rows {
            if !rows.contains(*r) {
                failures.push(format!("{r}: not used by its case (used: {rows:?})"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Rows that describe a dialect without driving the interpreter, with the
/// value each is expected to hold.
fn value_rows() -> Vec<(&'static str, String, &'static str)> {
    let t = |d: Dialect| Profile::of(d);
    let card = |d: Dialect, form: &str| {
        let c = t(d).cardinality(form).unwrap();
        format!("{}/{}", c.many, c.none)
    };
    vec![
        (
            "tsql.atomicity.atomic_compound",
            t(Tsql).atomicity.atomic_compound.value.clone(),
            "none",
        ),
        (
            "tsql.atomicity.routine_body",
            t(Tsql).atomicity.routine_body.value.clone(),
            "none",
        ),
        (
            "tsql.transactions.commit_in_atomic",
            t(Tsql).transactions.commit_in_atomic.value.clone(),
            "allowed",
        ),
        (
            "tsql.transactions.commit_in_exception_block",
            t(Tsql).transactions.commit_in_exception_block.value.clone(),
            "allowed",
        ),
        (
            "tsql.transactions.ddl",
            t(Tsql).transactions.ddl.value.clone(),
            "transactional",
        ),
        (
            "tsql.transactions.no_commit_levels",
            format!("{:?}", t(Tsql).transactions.no_commit_levels.value),
            "[]",
        ),
        (
            "tsql.transactions.savepoints_need_commitment_control",
            t(Tsql)
                .transactions
                .savepoints_need_commitment_control
                .value
                .to_string(),
            "false",
        ),
        (
            "tsql.transactions.survives_rollback",
            t(Tsql).transactions.survives_rollback.value.join(","),
            "table_variable,identity",
        ),
        (
            "tsql.transactions.routine_mode.anonymous",
            t(Tsql).routine_mode("anonymous").into(),
            "autocommit",
        ),
        (
            "tsql.card.select-assign",
            card(Tsql, "select_into"),
            "last_silent/unchanged",
        ),
        (
            "tsql.card.scalar-subquery",
            card(Tsql, "scalar_subquery"),
            "error/null",
        ),
        (
            "tsql.card.update-from-join",
            card(Tsql, "update_from"),
            "any_silent/unchanged",
        ),
        (
            "tsql.card.merge-many-sources",
            card(Tsql, "merge"),
            "error/unchanged",
        ),
        ("tsql.card.dml-no-rows", card(Tsql, "dml"), "ok/silent"),
        (
            "postgres.atomicity.routine_body",
            t(Pg).atomicity.routine_body.value.clone(),
            "caller",
        ),
        (
            "postgres.transactions.commit_in_atomic",
            t(Pg).transactions.commit_in_atomic.value.clone(),
            "allowed",
        ),
        (
            "postgres.transactions.ddl",
            t(Pg).transactions.ddl.value.clone(),
            "transactional",
        ),
        (
            "postgres.transactions.no_commit_levels",
            format!("{:?}", t(Pg).transactions.no_commit_levels.value),
            "[]",
        ),
        (
            "postgres.transactions.savepoints_need_commitment_control",
            t(Pg)
                .transactions
                .savepoints_need_commitment_control
                .value
                .to_string(),
            "false",
        ),
        (
            "postgres.transactions.survives_rollback",
            t(Pg).transactions.survives_rollback.value.join(","),
            "sequence,session_advisory_lock",
        ),
        (
            "postgres.transactions.routine_mode.trigger",
            t(Pg).routine_mode("trigger").into(),
            "caller",
        ),
        (
            "postgres.card.select-into",
            card(Pg, "select_into"),
            "first_silent/null",
        ),
        (
            "postgres.card.select-into-strict",
            card(Pg, "select_into_strict"),
            "error/error",
        ),
        (
            "postgres.card.scalar-subquery",
            card(Pg, "scalar_subquery"),
            "error/null",
        ),
        (
            "postgres.card.update-from-join",
            card(Pg, "update_from"),
            "any_silent/unchanged",
        ),
        (
            "postgres.card.merge-many-sources",
            card(Pg, "merge"),
            "error/unchanged",
        ),
        ("postgres.card.dml-no-rows", card(Pg, "dml"), "ok/silent"),
        (
            "db2i.atomicity.routine_body",
            t(Db2i).atomicity.routine_body.value.clone(),
            "none",
        ),
        (
            "db2i.transactions.commit_in_exception_block",
            t(Db2i).transactions.commit_in_exception_block.value.clone(),
            "allowed",
        ),
        (
            "db2i.transactions.commit_level_default",
            t(Db2i)
                .transactions
                .commit_level_default
                .as_ref()
                .unwrap()
                .value
                .clone(),
            "unknown",
        ),
        (
            "db2i.transactions.ddl",
            t(Db2i).transactions.ddl.value.clone(),
            "verify",
        ),
        (
            "db2i.transactions.doomed_commit",
            t(Db2i).transactions.doomed_commit.value.clone(),
            "error",
        ),
        (
            "db2i.transactions.doomed_writes",
            t(Db2i).transactions.doomed_writes.value.clone(),
            "allowed",
        ),
        (
            "db2i.transactions.nesting",
            t(Db2i).transactions.nesting.value.clone(),
            "flat",
        ),
        (
            "db2i.transactions.prerequisite",
            t(Db2i)
                .transactions
                .prerequisite
                .as_ref()
                .unwrap()
                .value
                .clone(),
            "journaled",
        ),
        (
            "db2i.transactions.survives_rollback",
            t(Db2i).transactions.survives_rollback.value.join(","),
            "identity,sequence,no_commit",
        ),
        (
            "db2i.transactions.routine_mode.function",
            t(Db2i).routine_mode("function").into(),
            "commit_level",
        ),
        (
            "db2i.transactions.routine_mode.trigger",
            t(Db2i).routine_mode("trigger").into(),
            "commit_level",
        ),
        (
            "db2i.transactions.routine_mode.anonymous",
            t(Db2i).routine_mode("anonymous").into(),
            "commit_level",
        ),
        (
            "db2i.transactions.routine_mode.script",
            t(Db2i).routine_mode("script").into(),
            "commit_level",
        ),
        (
            "db2i.card.select-into",
            card(Db2i, "select_into"),
            "error/not_found_warning",
        ),
        (
            "db2i.card.scalar-subquery",
            card(Db2i, "scalar_subquery"),
            "error/null",
        ),
        (
            "db2i.card.correlated-update",
            card(Db2i, "update_from"),
            "error/null",
        ),
        (
            "db2i.card.merge-many-sources",
            card(Db2i, "merge"),
            "error/unchanged",
        ),
        (
            "db2i.card.dml-no-rows",
            card(Db2i, "dml"),
            "ok/not_found_warning",
        ),
    ]
}

#[test]
fn descriptive_rows() {
    for (id, got, want) in value_rows() {
        assert_eq!(got, want, "{id}");
    }
}

#[test]
fn every_row_has_a_test() {
    let mut tested: BTreeSet<String> = CASES
        .iter()
        .flat_map(|c| c.rows.iter().map(|r| r.to_string()))
        .collect();
    tested.extend(value_rows().into_iter().map(|(id, _, _)| id.to_string()));
    let mut missing = Vec::new();
    for d in Dialect::ALL {
        for id in Profile::row_ids(d) {
            if !tested.contains(&id) {
                missing.push(id);
            }
        }
    }
    assert!(
        missing.is_empty(),
        "profile rows without a test: {missing:#?}"
    );
}

#[test]
fn undeclared_db2_commit_level_caps_durability() {
    let c = case(
        &[],
        Db2i,
        "CREATE PROCEDURE p () LANGUAGE SQL BEGIN INSERT INTO a VALUES (1); END;",
        NoFault,
        "",
    );
    let (_, _, o) = run(&c);
    assert!(o.durability_unknown.is_some());
    assert!(o.assumptions.contains("ASM-DB2-COMMIT-LEVEL"));
}

#[test]
fn error_kind_codes_map_back() {
    let pg = Profile::of(Pg);
    assert_eq!(pg.kind_of_code("23505"), Some(Constraint));
    assert_eq!(pg.kind_of_code("40P01"), Some(Lock));
    assert_eq!(pg.condition_kinds("unique_violation"), vec![Constraint]);
    assert!(!pg.condition_kinds("OTHERS").contains(&Cancel));
    let db2 = Profile::of(Db2i);
    assert!(db2.is_warning(NotFound));
    assert_eq!(db2.condition_kinds("'23505'"), vec![Constraint]);
    assert!(!db2.condition_kinds("SQLEXCEPTION").contains(&NotFound));
    let ts = Profile::of(Tsql);
    assert_eq!(ts.kind_of_code("1205"), Some(Lock));
    assert_eq!(ts.kind_of_code("50010"), Some(ErrorKind::UserSignal));
    assert!(!ts.condition_kinds("CATCH").contains(&NameResolution));
}

/// The raise action each statement of a routine lowers to, in node order.
fn raises(d: Dialect, sql: &str) -> Vec<Action> {
    let art = Artifact::new("case.sql", d, sql);
    let a = Analyzed::new(&art, Side::A, 3);
    let cfg = ir::lower(&a);
    (0..cfg.nodes.len())
        .filter_map(|n| cfg.node(n).stmt().map(|s| s.action.clone()))
        .filter(|a| matches!(a, Action::Raise { .. } | Action::None))
        .collect()
}

#[test]
fn raise_forms_lower_to_their_kind() {
    use sqldiff_core::ir::cfg::Action::Raise;
    // RAISERROR's severity is the second argument, not the message id.
    let ts = raises(
        Tsql,
        "CREATE PROCEDURE p AS\nBEGIN\n  RAISERROR(50001, 10, 1);\nEND",
    );
    assert_eq!(ts, vec![Action::None], "informational message");
    let ts = raises(
        Tsql,
        "CREATE PROCEDURE p AS\nBEGIN\n  RAISERROR('x', 16, 1);\nEND",
    );
    assert_eq!(
        ts,
        vec![Raise {
            kind: ErrorKind::UserSignal,
            form: "raiserror"
        }]
    );
    let ts = raises(
        Tsql,
        "CREATE PROCEDURE p AS\nBEGIN\n  RAISERROR('x', 20, 1) WITH LOG;\nEND",
    );
    assert_eq!(
        ts,
        vec![Raise {
            kind: Cancel,
            form: "raiserror"
        }]
    );
    // RAISE SQLSTATE raises that code, so WHEN unique_violation catches it.
    let pg = raises(
        Pg,
        "CREATE PROCEDURE p() LANGUAGE plpgsql AS $$\nBEGIN\n  RAISE SQLSTATE '23505';\nEND;\n$$;",
    );
    assert_eq!(
        pg,
        vec![Raise {
            kind: Constraint,
            form: "raise"
        }]
    );
    let pg = raises(
        Pg,
        "CREATE PROCEDURE p() LANGUAGE plpgsql AS $$\nBEGIN\n  RAISE EXCEPTION 'x' USING ERRCODE = 'unique_violation';\nEND;\n$$;",
    );
    assert_eq!(
        pg,
        vec![Raise {
            kind: Constraint,
            form: "raise"
        }]
    );
}

#[test]
fn raise_with_a_level_word_keeps_its_condition() {
    use sqldiff_core::ir::cfg::Action::Raise;
    for stmt in [
        "RAISE EXCEPTION SQLSTATE '23505';",
        "RAISE EXCEPTION unique_violation;",
        "RAISE unique_violation USING MESSAGE = 'dup';",
    ] {
        let sql =
            format!("CREATE PROCEDURE p() LANGUAGE plpgsql AS $$\nBEGIN\n  {stmt}\nEND;\n$$;");
        assert_eq!(
            raises(Pg, &sql),
            vec![Raise {
                kind: Constraint,
                form: "raise"
            }],
            "{stmt}"
        );
    }
}

#[test]
fn raiserror_severity_20_ends_the_routine() {
    // Severity 20+ drops the connection: the INSERT after it never runs.
    let sql = "CREATE PROCEDURE p AS\nBEGIN\n  RAISERROR('x', 20, 1) WITH LOG;\n  INSERT INTO t (c) VALUES (1);\nEND";
    let art = Artifact::new("case.sql", Tsql, sql);
    let a = Analyzed::new(&art, Side::A, 3);
    let outs = ir::explore(&ir::lower(&a), &Scenario::default());
    assert_eq!(outs.len(), 1);
    let o = &outs[0];
    assert!(o.effects.is_empty(), "{:?}", o.effects);
    assert!(matches!(o.exit, Exit::Raised(_)), "{:?}", o.exit);
    assert!(o.rows.contains("tsql.unhandled.cancel"), "{:?}", o.rows);
    // Severity 16 is an ordinary error: the batch goes on.
    let sql = sql.replace("20, 1) WITH LOG", "16, 1)");
    let art = Artifact::new("case.sql", Tsql, sql.as_str());
    let a = Analyzed::new(&art, Side::A, 3);
    let outs = ir::explore(&ir::lower(&a), &Scenario::default());
    assert!(
        outs[0].effects.contains_key("Insert:T#1"),
        "{:?}",
        outs[0].effects
    );
}

#[test]
fn tsql_cancel_is_a_severity_not_an_error_number() {
    let ts = Profile::of(Tsql);
    assert_ne!(ts.kind_of_code("8152"), Some(Cancel));
    assert_eq!(ts.kind_of_code("attention"), Some(Cancel));
    assert_eq!(ts.kind_of_severity(20), Some(Cancel));
    assert_eq!(ts.kind_of_severity(16), None);
}
