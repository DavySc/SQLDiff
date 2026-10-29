//! ALTER TABLE forms that T-SQL writes and PostgreSQL spells differently.
//! Before, they were copied as they stood (`ADD CONSTRAINT … DEFAULT … FOR`,
//! `WITH CHECK ADD`, `CHECK CONSTRAINT`), which PostgreSQL rejects.

use sqldiff_convert::{convert, Conversion, Status, Unit};
use sqldiff_core::Dialect;

const SRC: &str = "CREATE TABLE actor (actor_id int NOT NULL PRIMARY KEY, n int NOT NULL)
GO
ALTER TABLE actor ADD CONSTRAINT [DF_actor_n] DEFAULT (0) FOR n
GO
CREATE TABLE film (film_id int NOT NULL PRIMARY KEY, actor_id int NULL)
GO
ALTER TABLE film WITH CHECK ADD CONSTRAINT [FK_film_actor] FOREIGN KEY([actor_id]) REFERENCES actor ([actor_id])
GO
ALTER TABLE film CHECK CONSTRAINT [FK_film_actor]
GO
ALTER TABLE film WITH CHECK CHECK CONSTRAINT FK_film_actor
GO
";

fn unit<'a>(c: &'a Conversion, needle: &str) -> &'a Unit {
    let at = SRC.find(needle).unwrap();
    c.units
        .iter()
        .filter(|u| u.src.start <= at && at < u.src.end)
        .min_by_key(|u| u.src.end - u.src.start)
        .unwrap()
}

fn rules(u: &Unit) -> Vec<&str> {
    u.notes.iter().map(|n| n.rule.as_str()).collect()
}

#[test]
fn postgres_spells_the_alter_table_forms() {
    let c = convert(SRC, Dialect::Tsql, Dialect::Postgres).unwrap();
    let out = &c.output;
    assert!(
        out.contains("ALTER TABLE actor ALTER COLUMN n SET DEFAULT 0;"),
        "{out}"
    );
    assert!(!out.contains("FOR n"), "{out}");
    assert!(!out.contains("WITH CHECK"), "{out}");
    assert!(
        out.contains("ALTER TABLE film VALIDATE CONSTRAINT FK_film_actor;"),
        "{out}"
    );
    let d = unit(&c, "ADD CONSTRAINT [DF_actor_n]");
    assert!(
        rules(d).contains(&"ASM-DEFAULT-CONSTRAINT-NAME"),
        "{:?}",
        d.notes
    );
    // Turning the constraint on without WITH CHECK leaves the rows already
    // there unchecked in T-SQL; VALIDATE checks them.
    let plain = unit(&c, "ALTER TABLE film CHECK CONSTRAINT");
    assert!(
        rules(plain).contains(&"ASM-CONSTRAINT-ROWS-VALID"),
        "{:?}",
        plain.notes
    );
    let checked = unit(&c, "WITH CHECK CHECK CONSTRAINT");
    assert!(
        !rules(checked).contains(&"ASM-CONSTRAINT-ROWS-VALID"),
        "{:?}",
        checked.notes
    );
    // The output parses as PostgreSQL.
    for needle in [
        "ADD CONSTRAINT [DF",
        "WITH CHECK ADD",
        "CHECK CONSTRAINT [FK",
    ] {
        let u = unit(&c, needle);
        assert!(
            !u.notes.iter().any(|n| n.rule.starts_with("parse")),
            "{needle}: {:?}",
            u.notes
        );
    }
}

#[test]
fn db2_refuses_what_it_has_no_rule_for() {
    let c = convert(SRC, Dialect::Tsql, Dialect::Db2i).unwrap();
    for needle in [
        "ADD CONSTRAINT [DF",
        "WITH CHECK ADD",
        "CHECK CONSTRAINT [FK",
        "WITH CHECK CHECK",
    ] {
        let u = unit(&c, needle);
        assert_eq!(u.status, Status::NotConverted, "{needle}: {:?}", u.notes);
    }
}

#[test]
fn turning_a_constraint_off_is_refused() {
    for src in [
        "ALTER TABLE film NOCHECK CONSTRAINT FK_film_actor\n",
        "ALTER TABLE film WITH NOCHECK ADD CONSTRAINT FK_film_actor FOREIGN KEY (actor_id) REFERENCES actor (actor_id)\n",
        "ALTER TABLE film CHECK CONSTRAINT ALL\n",
    ] {
        let c = convert(src, Dialect::Tsql, Dialect::Postgres).unwrap();
        assert!(
            c.units.iter().any(|u| u.status == Status::NotConverted),
            "{src}: {}",
            c.output
        );
    }
}

#[test]
fn a_second_default_is_refused() {
    let src = "CREATE TABLE t (id int NOT NULL PRIMARY KEY, v int NULL DEFAULT 1)\nGO\nALTER TABLE t ADD DEFAULT 2 FOR v\nGO\n";
    let c = convert(src, Dialect::Tsql, Dialect::Postgres).unwrap();
    let at = src.find("ALTER TABLE").unwrap();
    let u = c
        .units
        .iter()
        .filter(|u| u.src.start <= at && at < u.src.end)
        .min_by_key(|u| u.src.end - u.src.start)
        .unwrap();
    assert_eq!(u.status, Status::NotConverted, "{}", c.output);
}

#[test]
fn key_constraints_added_later_lose_clustered_and_sort_order() {
    let src = "CREATE TABLE s (k int NOT NULL, d int NOT NULL, c int NULL)\nGO\nALTER TABLE s ADD CONSTRAINT PK_s PRIMARY KEY NONCLUSTERED\n(\n    k ASC,\n    d DESC\n)\nGO\nALTER TABLE s ADD CONSTRAINT UQ_c UNIQUE CLUSTERED (c), CONSTRAINT CK_c CHECK (c > 0)\nGO\n";
    let c = convert(src, Dialect::Tsql, Dialect::Postgres).unwrap();
    let out = &c.output;
    assert!(!out.contains("CLUSTERED"), "{out}");
    assert!(!out.contains("ASC") && !out.contains("DESC"), "{out}");
    assert!(
        out.contains("ADD CONSTRAINT UQ_c UNIQUE (c), ADD CONSTRAINT CK_c CHECK (c > 0)"),
        "{out}"
    );
    // Index options (IGNORE_DUP_KEY changes what the key accepts) are refused.
    let c = convert(
        "ALTER TABLE s ADD CONSTRAINT PK_s PRIMARY KEY (k) WITH (IGNORE_DUP_KEY = ON)\n",
        Dialect::Tsql,
        Dialect::Postgres,
    )
    .unwrap();
    assert!(
        c.units.iter().any(|u| u.status == Status::NotConverted),
        "{}",
        c.output
    );
}

#[test]
fn drop_trigger_is_refused_and_truncate_restarts_identity() {
    let c = convert(
        "DROP TRIGGER IF EXISTS Application.TR_x\nGO\nTRUNCATE TABLE dbo.Orders\nGO\n",
        Dialect::Tsql,
        Dialect::Postgres,
    )
    .unwrap();
    let out = &c.output;
    assert!(!out.contains("\nDROP TRIGGER"), "{out}");
    assert!(
        out.contains("TRUNCATE TABLE dbo.Orders RESTART IDENTITY;")
            || out.contains("TRUNCATE TABLE Orders RESTART IDENTITY;"),
        "{out}"
    );
    assert_eq!(
        c.units
            .iter()
            .filter(|u| u.status == Status::NotConverted)
            .count(),
        1,
        "{out}"
    );
}
