//! T-SQL against T-SQL: refactoring pairs with a hand-argued truth, and the
//! verdict the engine gives today (design: `tsql-tsql/design.md`, PR 0).
//!
//! Two kinds of check:
//! - **Soundness (the gate).** A pair whose truth is "different" may never
//!   come out `Equal` or `EqualUnderAssumptions`, in either direction.
//! - **Pins.** Every pair's current verdict is pinned, so a later change to
//!   the same-dialect path shows exactly which verdicts it moved. A pin that
//!   is weaker than the truth (a true "equal" reported `Unknown`) is a known
//!   gap, not an endorsement.
//!
//! Every "equal" truth carries a one-line argument and every "different" a
//! concrete witness input. A truth that needs a premise names it. None of
//! these is a corpus fixture; the routines are written for this test.
//!
//! Known gaps visible in the pins: `total-guard-clause` and
//! `select-into-variable-to-set` are equal in truth and reported `Different`
//! (statements with no counterpart; path summaries, step 2). `rename-local`
//! and `swap-and-operands` were `Unknown` before step 1 (the second opinion
//! of the statement lowering, `equiv::second`) and are now `Equal` and
//! `EqualUnderAssumptions`.

use sqldiff_core::{diff, Artifact, Dialect, Options, Verdict};
use Verdict::{Different, Equal, EqualUnderAssumptions as Eua, Unknown};

const T: Dialect = Dialect::Tsql;

const DDL: &str = "
CREATE TABLE dbo.Item (ItemId int NOT NULL PRIMARY KEY, Price money NOT NULL);
CREATE TABLE dbo.Orders (CustomerId int NULL, Qty int NULL, Total money NULL, Note varchar(100) NULL);
CREATE TABLE dbo.Stock (ItemId int NOT NULL PRIMARY KEY, Qty int NOT NULL);
";

#[derive(Clone, Copy, PartialEq, Debug)]
enum Truth {
    Equal,
    Different,
}

struct Case {
    name: &'static str,
    truth: Truth,
    /// The one-line argument (equal) or the witness input (different).
    why: &'static str,
    a: String,
    b: String,
    pinned: Verdict,
}

fn verdict(a: &str, b: &str) -> Verdict {
    let opts = Options {
        catalog_a: vec![Artifact::new("catalog.sql", T, DDL)],
        catalog_b: vec![Artifact::new("catalog.sql", T, DDL)],
        ..Options::default()
    };
    sqldiff_core::with_engine_stack(|| {
        diff(
            &Artifact::new("a.sql", T, a),
            &Artifact::new("b.sql", T, b),
            &opts,
        )
        .verdict()
    })
}

/// The reference routine and its variants are built from small parts so each
/// variant differs from the reference in exactly one named place.
fn place_order(params: &str, decl: &str, guard_body: &str) -> String {
    format!(
        "CREATE PROCEDURE dbo.PlaceOrder {params}
AS
BEGIN
    SET NOCOUNT ON;
    DECLARE {decl};
    SELECT @Price = Price FROM dbo.Item WHERE ItemId = 1;
    {guard_body}
END"
    )
}

const PARAMS: &str = "@CustomerId int, @Qty int";
const DECL: &str = "@Price money";
const GUARD: &str = "IF @Qty > 0 AND @CustomerId IS NOT NULL
    BEGIN
        INSERT INTO dbo.Orders (CustomerId, Qty, Total) VALUES (@CustomerId, @Qty, @Qty * @Price);
    END
    ELSE
    BEGIN
        RAISERROR('bad', 16, 1);
    END";

fn reference() -> String {
    place_order(PARAMS, DECL, GUARD)
}

fn cases() -> Vec<Case> {
    let r = reference;
    let with_guard = |g: &str| place_order(PARAMS, DECL, g);
    vec![
        // ---- equal truths ----
        Case {
            name: "reformat",
            truth: Truth::Equal,
            why: "whitespace, comments and keyword case only",
            a: r(),
            b: r()
                .replace("    ", "  ")
                .replace("SELECT @Price", "select @Price -- the price\n      ")
                .replace("ELSE", "else"),
            pinned: Equal,
        },
        Case {
            name: "quoting",
            truth: Truth::Equal,
            why: "[dbo].[Item] names the same table as dbo.Item",
            a: r(),
            b: r().replace("dbo.Item", "[dbo].[Item]"),
            pinned: Equal,
        },
        Case {
            name: "rename-local",
            truth: Truth::Equal,
            why: "@Price is local; every use is renamed, no caller can see it",
            a: r(),
            b: r().replace("@Price", "@UnitPrice"),
            pinned: Equal,
        },
        Case {
            name: "swap-and-operands",
            truth: Truth::Equal,
            why: "neither comparison can raise, so AND commutes (three-valued AND is commutative)",
            a: r(),
            b: r().replace(
                "@Qty > 0 AND @CustomerId IS NOT NULL",
                "@CustomerId IS NOT NULL AND @Qty > 0",
            ),
            pinned: Eua,
        },
        Case {
            name: "total-guard-clause",
            truth: Truth::Equal,
            why: "the guard is true exactly when Qty is NULL or <= 0 or CustomerId is NULL, \
                  never UNKNOWN (IS NULL is total, Qty <= 0 is known once Qty is not NULL); \
                  its complement is the original's true branch, and UNKNOWN never reaches it",
            a: r(),
            b: with_guard(
                "IF @Qty IS NULL OR @Qty <= 0 OR @CustomerId IS NULL
    BEGIN
        RAISERROR('bad', 16, 1);
        RETURN;
    END
    INSERT INTO dbo.Orders (CustomerId, Qty, Total) VALUES (@CustomerId, @Qty, @Qty * @Price);",
            ),
            pinned: Different,
        },
        Case {
            name: "select-into-variable-to-set",
            truth: Truth::Equal,
            why: "ItemId is the primary key, so the subquery returns at most one row; no row \
                  leaves @Price NULL, as it was after DECLARE",
            a: r(),
            b: r().replace(
                "SELECT @Price = Price FROM dbo.Item WHERE ItemId = 1;",
                "SET @Price = (SELECT Price FROM dbo.Item WHERE ItemId = 1);",
            ),
            pinned: Different,
        },
        // ---- different truths ----
        Case {
            name: "gt-to-ge",
            truth: Truth::Different,
            why: "@Qty = 0, @CustomerId = 1: A raises, B inserts",
            a: r(),
            b: r().replace("@Qty > 0", "@Qty >= 0"),
            pinned: Different,
        },
        Case {
            name: "guard-clause-not-and",
            truth: Truth::Different,
            why: "@Qty = NULL, @CustomerId = 5: A's condition is UNKNOWN so it raises; \
                  B's NOT (...) is UNKNOWN too, skips the guard and inserts NULLs",
            a: r(),
            b: with_guard(
                "IF NOT (@CustomerId IS NOT NULL AND @Qty > 0)
    BEGIN
        RAISERROR('bad', 16, 1);
        RETURN;
    END
    INSERT INTO dbo.Orders (CustomerId, Qty, Total) VALUES (@CustomerId, @Qty, @Qty * @Price);",
            ),
            pinned: Different,
        },
        Case {
            name: "negate-if-and-swap-branches",
            truth: Truth::Different,
            why: "@Qty = NULL: A takes ELSE (raises), B takes the branch holding the INSERT",
            a: with_guard(
                "IF @Qty > 0 BEGIN INSERT INTO dbo.Orders (CustomerId, Qty) VALUES (@CustomerId, @Qty); END \
                 ELSE BEGIN RAISERROR('bad', 16, 1); END",
            ),
            b: with_guard(
                "IF NOT (@Qty > 0) BEGIN RAISERROR('bad', 16, 1); END \
                 ELSE BEGIN INSERT INTO dbo.Orders (CustomerId, Qty) VALUES (@CustomerId, @Qty); END",
            ),
            pinned: Unknown,
        },
        Case {
            name: "values-to-insert-select",
            truth: Truth::Different,
            why: "@Qty = 1, @CustomerId = 1, no Item row 1: A inserts one row (Total NULL), B inserts none",
            a: r(),
            b: with_guard(
                "IF @Qty > 0 AND @CustomerId IS NOT NULL
    BEGIN
        INSERT INTO dbo.Orders (CustomerId, Qty, Total)
        SELECT @CustomerId, @Qty, @Qty * i.Price FROM dbo.Item i WHERE i.ItemId = 1;
    END
    ELSE
    BEGIN
        RAISERROR('bad', 16, 1);
    END",
            )
            .replace("    DECLARE @Price money;\n    SELECT @Price = Price FROM dbo.Item WHERE ItemId = 1;\n", ""),
            pinned: Different,
        },
        Case {
            name: "wrap-insert-in-empty-try-catch",
            truth: Truth::Different,
            why: "Item 1 has Price 922337203685477, @Qty = 2, @CustomerId = 1: @Qty * @Price overflows money; A returns error 8115 to the caller, B swallows it",
            a: r(),
            b: r().replace(
                "INSERT INTO dbo.Orders (CustomerId, Qty, Total) VALUES (@CustomerId, @Qty, @Qty * @Price);",
                "BEGIN TRY INSERT INTO dbo.Orders (CustomerId, Qty, Total) VALUES (@CustomerId, @Qty, @Qty * @Price); END TRY \
                 BEGIN CATCH END CATCH",
            ),
            pinned: Different,
        },
        Case {
            name: "rename-parameter",
            truth: Truth::Different,
            why: "EXEC dbo.PlaceOrder @CustomerId = 1, @Qty = 2 fails against B (no such parameter)",
            a: r(),
            b: r().replace("@CustomerId", "@CustId"),
            pinned: Unknown,
        },
        Case {
            name: "remove-parameter-default",
            truth: Truth::Different,
            why: "EXEC dbo.PlaceOrder @CustomerId = 1 works against A (@Qty defaults to 1), fails against B",
            a: place_order("@CustomerId int, @Qty int = 1", DECL, GUARD),
            b: r(),
            pinned: Unknown,
        },
        Case {
            name: "narrow-parameter",
            truth: Truth::Different,
            why: "EXEC with a 50-character @Note: A stores it, B truncates it silently",
            a: "CREATE PROCEDURE dbo.AddNote @Note varchar(100) AS BEGIN INSERT INTO dbo.Orders (Note) VALUES (@Note); END".into(),
            b: "CREATE PROCEDURE dbo.AddNote @Note varchar(10) AS BEGIN INSERT INTO dbo.Orders (Note) VALUES (@Note); END".into(),
            pinned: Different,
        },
        Case {
            name: "reorder-read-and-write",
            truth: Truth::Different,
            why: "Stock row 1 holds Qty 5: A returns 5, B returns 6 (the read moved after the write)",
            a: "CREATE PROCEDURE dbo.Bump AS BEGIN SELECT Qty FROM dbo.Stock WHERE ItemId = 1; UPDATE dbo.Stock SET Qty = Qty + 1 WHERE ItemId = 1; END".into(),
            b: "CREATE PROCEDURE dbo.Bump AS BEGIN UPDATE dbo.Stock SET Qty = Qty + 1 WHERE ItemId = 1; SELECT Qty FROM dbo.Stock WHERE ItemId = 1; END".into(),
            pinned: Unknown,
        },
        Case {
            name: "swap-two-updates",
            truth: Truth::Different,
            why: "Qty = 5: A ends at (5 * 2) + 1 = 11, B at (5 + 1) * 2 = 12",
            a: "CREATE PROCEDURE dbo.Adj AS BEGIN UPDATE dbo.Stock SET Qty = Qty * 2; UPDATE dbo.Stock SET Qty = Qty + 1; END".into(),
            b: "CREATE PROCEDURE dbo.Adj AS BEGIN UPDATE dbo.Stock SET Qty = Qty + 1; UPDATE dbo.Stock SET Qty = Qty * 2; END".into(),
            pinned: Unknown,
        },
        Case {
            name: "drop-a-write",
            truth: Truth::Different,
            why: "Stock holds (1, 5): A ends at 11, B at 10",
            a: "CREATE PROCEDURE dbo.Adj AS BEGIN UPDATE dbo.Stock SET Qty = Qty * 2; UPDATE dbo.Stock SET Qty = Qty + 1; END".into(),
            b: "CREATE PROCEDURE dbo.Adj AS BEGIN UPDATE dbo.Stock SET Qty = Qty * 2; END".into(),
            pinned: Different,
        },
        Case {
            name: "error-message-text",
            truth: Truth::Different,
            why: "@Qty = 0: the caller receives 'bad' from A and 'bad input' from B",
            a: r(),
            b: r().replace("'bad'", "'bad input'"),
            pinned: Different,
        },
        Case {
            name: "multiply-to-add",
            truth: Truth::Different,
            why: "Item 1 has Price 10, @Qty = 2, @CustomerId = 1: A stores Total 20, B stores 12",
            a: r(),
            b: r().replace("@Qty * @Price", "@Qty + @Price"),
            pinned: Unknown,
        },
        Case {
            name: "declared-type-precision",
            truth: Truth::Different,
            why: "datetime2(7) keeps SYSDATETIME()'s seventh fraction digit, datetime2(6) rounds it away: SELECT @at returns different values",
            a: "CREATE PROCEDURE dbo.Stamp AS BEGIN DECLARE @at datetime2(7) = SYSDATETIME(); SELECT @at; END".into(),
            b: "CREATE PROCEDURE dbo.Stamp AS BEGIN DECLARE @at datetime2(6) = SYSDATETIME(); SELECT @at; END".into(),
            pinned: Unknown,
        },
        Case {
            name: "swap-declared-types",
            truth: Truth::Different,
            why: "SELECT @a, @b returns an int then a money in A and a money then an int in B (set a = 1)",
            a: "CREATE PROCEDURE dbo.Types AS BEGIN DECLARE @a int = 1, @b money = 2; SELECT @a, @b; END".into(),
            b: "CREATE PROCEDURE dbo.Types AS BEGIN DECLARE @a money = 1, @b int = 2; SELECT @a, @b; END".into(),
            pinned: Unknown,
        },
        Case {
            name: "swap-subtract-operands",
            truth: Truth::Different,
            why: "Qty = 5, Price = 2: A stores 3, B stores -3",
            a: "CREATE PROCEDURE dbo.Sub @Qty int, @Price int AS BEGIN INSERT INTO dbo.Orders (Qty) VALUES (@Qty - @Price); END".into(),
            b: "CREATE PROCEDURE dbo.Sub @Qty int, @Price int AS BEGIN INSERT INTO dbo.Orders (Qty) VALUES (@Price - @Qty); END".into(),
            pinned: Different,
        },
        Case {
            name: "narrow-local-type",
            truth: Truth::Different,
            why: "Item 1 has Price 1000.00: money converts into decimal(5,2) with an overflow error in B; or Price 1.2345 stores Total 1.23 * Qty in B",
            a: r(),
            b: r().replace("@Price money", "@Price decimal(5,2)"),
            pinned: Unknown,
        },
    ]
}

#[test]
fn a_different_pair_is_never_equal_in_either_direction() {
    for c in cases() {
        for (x, y, dir) in [(&c.a, &c.b, "A to B"), (&c.b, &c.a, "B to A")] {
            if c.truth == Truth::Different {
                let v = verdict(x, y);
                assert!(
                    !matches!(v, Equal | Eua),
                    "{} ({dir}): reported {v:?}, witness: {}",
                    c.name,
                    c.why
                );
            }
        }
    }
}

#[test]
fn current_verdicts_are_pinned() {
    let mut moved = Vec::new();
    for c in cases() {
        let v = verdict(&c.a, &c.b);
        if v != c.pinned {
            moved.push(format!("{}: pinned {:?}, now {v:?}", c.name, c.pinned));
        }
    }
    assert!(moved.is_empty(), "verdicts moved:\n{}", moved.join("\n"));
}

#[test]
fn the_same_text_is_equal_to_itself() {
    for c in cases() {
        assert_eq!(verdict(&c.a, &c.a), Equal, "{}: A", c.name);
        assert_eq!(verdict(&c.b, &c.b), Equal, "{}: B", c.name);
    }
}

/// One semantic mutation of the refactored side (B) of an equal pair, with
/// the input that tells the mutant from A.
struct Mutant {
    of: &'static str,
    name: &'static str,
    why: &'static str,
    from: &'static str,
    to: &'static str,
}

const MUTANTS: &[Mutant] = &[
    Mutant {
        of: "total-guard-clause",
        name: "le-to-lt",
        why: "@Qty = 0, @CustomerId = 1: A raises, B inserts",
        from: "@Qty <= 0",
        to: "@Qty < 0",
    },
    Mutant {
        of: "total-guard-clause",
        name: "drop-null-test",
        why: "@Qty = NULL, @CustomerId = 1: A raises, B's guard is UNKNOWN so it inserts",
        from: "@Qty IS NULL OR ",
        to: "",
    },
    Mutant {
        of: "total-guard-clause",
        name: "drop-return",
        why: "@Qty = 0, @CustomerId = 1: B raises and then inserts as well",
        from: "RETURN;",
        to: "",
    },
    Mutant {
        of: "total-guard-clause",
        name: "message-text",
        why: "@Qty = 0: the caller receives 'bad' from A and 'bad!' from B",
        from: "'bad'",
        to: "'bad!'",
    },
    Mutant {
        of: "select-into-variable-to-set",
        name: "drop-where",
        why:
            "two Item rows: the subquery returns two and raises error 512; A keeps one row's Price",
        from: " WHERE ItemId = 1)",
        to: ")",
    },
    Mutant {
        of: "swap-and-operands",
        name: "and-to-or",
        why: "@Qty = 0, @CustomerId = 1: A raises, B inserts",
        from: "@CustomerId IS NOT NULL AND @Qty > 0",
        to: "@CustomerId IS NOT NULL OR @Qty > 0",
    },
    Mutant {
        of: "rename-local",
        name: "other-item",
        why: "Item rows (1, 10) and (2, 20): A prices with 10, B with 20",
        from: "ItemId = 1",
        to: "ItemId = 2",
    },
    Mutant {
        of: "quoting",
        name: "other-item",
        why: "Item rows (1, 10) and (2, 20): A prices with 10, B with 20",
        from: "ItemId = 1",
        to: "ItemId = 2",
    },
];

#[test]
fn a_mutant_of_a_refactored_side_is_never_equal() {
    let all = cases();
    for m in MUTANTS {
        let c = all
            .iter()
            .find(|c| c.name == m.of)
            .unwrap_or_else(|| panic!("no case {}", m.of));
        assert_eq!(c.truth, Truth::Equal, "{} mutates an equal pair", m.of);
        assert!(
            c.b.contains(m.from),
            "{}/{}: `{}` not in B",
            m.of,
            m.name,
            m.from
        );
        let mutant = c.b.replacen(m.from, m.to, 1);
        for (x, y, dir) in [(&c.a, &mutant, "A to B"), (&mutant, &c.a, "B to A")] {
            let v = verdict(x, y);
            assert!(
                !matches!(v, Equal | Eua),
                "{}/{} ({dir}): reported {v:?}, witness: {}",
                m.of,
                m.name,
                m.why
            );
        }
    }
}

/// Step 1 clears structural findings only for a pair whose routine headers
/// are token-identical (strict same-dialect signature): the same body change
/// that is cleared above must not be, once the signature moves too. Every
/// pair here is a caller-visible change and may never come out equal.
const SIGNATURE_WITNESSES: &[(&str, &str, &str)] = &[
    (
        "rename-parameter",
        "@CustomerId int, @Qty int",
        "@CustId int, @Qty int",
    ),
    (
        "add-default",
        "@CustomerId int, @Qty int",
        "@CustomerId int, @Qty int = 1",
    ),
    (
        "widen-type",
        "@CustomerId int, @Qty int",
        "@CustomerId bigint, @Qty int",
    ),
    (
        "add-output",
        "@CustomerId int, @Qty int",
        "@CustomerId int, @Qty int OUTPUT",
    ),
    (
        "reorder",
        "@CustomerId int, @Qty int",
        "@Qty int, @CustomerId int",
    ),
];

#[test]
fn a_signature_change_next_to_a_cleared_rename_is_never_equal() {
    let renamed = |params: &str| {
        place_order(params, "@UnitPrice money", GUARD).replace("@Price", "@UnitPrice")
    };
    for (name, from, to) in SIGNATURE_WITNESSES {
        let a = place_order(from, DECL, GUARD);
        let b = renamed(to);
        for (x, y, dir) in [(&a, &b, "A to B"), (&b, &a, "B to A")] {
            let v = verdict(x, y);
            assert!(!matches!(v, Equal | Eua), "{name} ({dir}): {v:?}");
        }
    }
}

#[test]
fn a_rename_beside_a_header_naming_execute_as_is_not_cleared() {
    // Both headers are identical, so the signature holds; the ownership-chain
    // premise is only vacuous for two callers' contexts, so a routine that
    // names EXECUTE keeps the structural findings.
    for opt in ["WITH EXECUTE AS CALLER", "WITH EXECUTE AS OWNER"] {
        let with = |p: String| p.replacen("\nAS\n", &format!("\n{opt}\nAS\n"), 1);
        let a = with(reference());
        let b = with(reference().replace("@Price", "@UnitPrice"));
        assert_ne!(a, reference());
        let v = verdict(&a, &b);
        assert!(!matches!(v, Equal), "{opt}: {v:?}");
        assert_eq!(v, Unknown, "{opt}");
    }
}

#[test]
fn the_cleared_rename_names_no_assumption_it_does_not_need() {
    let opts = Options {
        catalog_a: vec![Artifact::new("catalog.sql", T, DDL)],
        catalog_b: vec![Artifact::new("catalog.sql", T, DDL)],
        ..Options::default()
    };
    let r = sqldiff_core::with_engine_stack(|| {
        diff(
            &Artifact::new("a.sql", T, reference()),
            &Artifact::new("b.sql", T, reference().replace("@Price", "@UnitPrice")),
            &opts,
        )
    });
    let ids: Vec<&str> = r.assumptions.iter().map(|x| x.id.as_str()).collect();
    assert!(r.findings.is_empty(), "{:?}", r.findings);
    assert!(
        !ids.contains(&"ASM-UNTYPED-STORE") && !ids.contains(&"ASM-TSQL-OWNERSHIP-CHAIN"),
        "{ids:?}"
    );
}

#[test]
fn a_fresh_and_a_repeated_comparison_render_the_same_bytes() {
    let opts = Options {
        catalog_a: vec![Artifact::new("catalog.sql", T, DDL)],
        catalog_b: vec![Artifact::new("catalog.sql", T, DDL)],
        ..Options::default()
    };
    let run = || {
        sqldiff_core::with_engine_stack(|| {
            let r = diff(
                &Artifact::new("a.sql", T, reference()),
                &Artifact::new(
                    "b.sql",
                    T,
                    reference().replace(
                        "@Qty > 0 AND @CustomerId IS NOT NULL",
                        "@CustomerId IS NOT NULL AND @Qty > 0",
                    ),
                ),
                &opts,
            );
            serde_json::to_string(&r).unwrap()
        })
    };
    assert_eq!(run(), run());
}

/// Pairs with a witness that the second opinion once cleared (statements the
/// lowering only notes, and operand orders that can raise) or that stay
/// different: none may be Equal or EqualUnderAssumptions, in either direction.
fn proc_body(body: &str) -> String {
    format!("CREATE PROCEDURE dbo.P AS BEGIN {body} END")
}

const NEVER_EQUAL: &[(&str, &str, &str, &str)] = &[
    (
        "nocount-moved-after-update",
        "A sends no count for the UPDATE of the one Stock row, B sends '(1 row affected)'",
        "SET NOCOUNT ON; UPDATE dbo.Stock SET Qty = 1; DELETE FROM dbo.Orders;",
        "UPDATE dbo.Stock SET Qty = 1; SET NOCOUNT ON; DELETE FROM dbo.Orders;",
    ),
    (
        "nocount-moved-with-renamed-local",
        "as above, beside a renamed local",
        "DECLARE @a int = 1; SET NOCOUNT ON; UPDATE dbo.Stock SET Qty = @a; DELETE FROM dbo.Orders;",
        "DECLARE @b int = 1; UPDATE dbo.Stock SET Qty = @b; SET NOCOUNT ON; DELETE FROM dbo.Orders;",
    ),
    (
        "nocount-on-off-swapped",
        "Stock has one row, Orders is empty: A reports 0 rows then 1, B reports 1 then 0",
        "SET NOCOUNT ON; UPDATE dbo.Stock SET Qty = 1; SET NOCOUNT OFF; DELETE FROM dbo.Orders;",
        "SET NOCOUNT OFF; UPDATE dbo.Stock SET Qty = 1; SET NOCOUNT ON; DELETE FROM dbo.Orders;",
    ),
    (
        "nocount-off-moved-between-inserts",
        "A sends one count, B sends two",
        "SET NOCOUNT ON; INSERT INTO dbo.Orders (Qty) VALUES (1); SELECT 5; SET NOCOUNT OFF; INSERT INTO dbo.Orders (Qty) VALUES (2);",
        "SET NOCOUNT OFF; INSERT INTO dbo.Orders (Qty) VALUES (1); SELECT 5; SET NOCOUNT ON; INSERT INTO dbo.Orders (Qty) VALUES (2);",
    ),
    (
        "nested-ifs-swapped-with-a-raising-condition",
        "@b = 0: A divides before its guard and raises error 8134, B tests @b <> 0 first and skips",
        "CREATE PROCEDURE dbo.P @a int, @b int AS BEGIN IF @a / @b > 0 BEGIN IF @b <> 0 INSERT INTO dbo.Orders (Qty) VALUES (1); END END",
        "CREATE PROCEDURE dbo.P @a int, @b int AS BEGIN IF @b <> 0 BEGIN IF @a / @b > 0 INSERT INTO dbo.Orders (Qty) VALUES (1); END END",
    ),
    (
        "nested-ifs-swapped-with-inner-else",
        "@a = 0, @b = 1: A takes the outer ELSE-less IF false and does nothing, B reaches its inner ELSE and inserts 2",
        "CREATE PROCEDURE dbo.P @a int, @b int AS BEGIN IF @a > 0 BEGIN IF @b > 0 INSERT INTO dbo.Orders (Qty) VALUES (1); ELSE INSERT INTO dbo.Orders (Qty) VALUES (2); END END",
        "CREATE PROCEDURE dbo.P @a int, @b int AS BEGIN IF @b > 0 BEGIN IF @a > 0 INSERT INTO dbo.Orders (Qty) VALUES (1); ELSE INSERT INTO dbo.Orders (Qty) VALUES (2); END END",
    ),
    (
        "not-with-null-in-front-of-a-nested-if",
        "@a NULL, @b = 1: NOT UNKNOWN is UNKNOWN so A skips; B's `@a IS NULL` is true and inserts",
        "CREATE PROCEDURE dbo.P @a int, @b int AS BEGIN IF NOT (@a > 0) BEGIN IF @b > 0 INSERT INTO dbo.Orders (Qty) VALUES (1); END END",
        "CREATE PROCEDURE dbo.P @a int, @b int AS BEGIN IF @a <= 0 OR @a IS NULL BEGIN IF @b > 0 INSERT INTO dbo.Orders (Qty) VALUES (1); END END",
    ),
    (
        "condition-changed-above-a-raising-inner-else",
        "@a = 0, @b = 1: A skips the outer branch, B enters it and inserts 1",
        "CREATE PROCEDURE dbo.P @a int, @b int AS BEGIN IF @a > 0 BEGIN IF @b > 0 INSERT INTO dbo.Orders (Qty) VALUES (1); ELSE RAISERROR('x', 16, 1); END END",
        "CREATE PROCEDURE dbo.P @a int, @b int AS BEGIN IF @a >= 0 BEGIN IF @b > 0 INSERT INTO dbo.Orders (Qty) VALUES (1); ELSE RAISERROR('x', 16, 1); END END",
    ),
    (
        "divide-after-its-guard",
        "@y = 0: A is false at the guard and never divides, B raises error 8134",
        "CREATE PROCEDURE dbo.P @x int, @y int AS BEGIN IF @y <> 0 AND @x / @y > 1 SELECT 1; END",
        "CREATE PROCEDURE dbo.P @x int, @y int AS BEGIN IF @x / @y > 1 AND @y <> 0 SELECT 1; END",
    ),
    (
        "cast-after-isnumeric",
        "@s = 'abc': A's ISNUMERIC is 0 and the CAST never runs, B raises error 245",
        "CREATE PROCEDURE dbo.P @s varchar(10) AS BEGIN IF ISNUMERIC(@s) = 1 AND CAST(@s AS int) > 0 SELECT 1; END",
        "CREATE PROCEDURE dbo.P @s varchar(10) AS BEGIN IF CAST(@s AS int) > 0 AND ISNUMERIC(@s) = 1 SELECT 1; END",
    ),
    (
        "divide-after-or-guard",
        "@y = 0: A is true at the guard and never divides, B raises error 8134",
        "CREATE PROCEDURE dbo.P @x int, @y int AS BEGIN IF @y = 0 OR @x / @y > 1 SELECT 1; END",
        "CREATE PROCEDURE dbo.P @x int, @y int AS BEGIN IF @x / @y > 1 OR @y = 0 SELECT 1; END",
    ),
    (
        "divide-after-guard-in-while",
        "@y = 0: A leaves the loop, B raises error 8134",
        "CREATE PROCEDURE dbo.P @x int, @y int AS BEGIN WHILE @y <> 0 AND @x / @y > 1 BEGIN SET @x = @x - 1; END END",
        "CREATE PROCEDURE dbo.P @x int, @y int AS BEGIN WHILE @x / @y > 1 AND @y <> 0 BEGIN SET @x = @x - 1; END END",
    ),
];

#[test]
fn noted_statements_and_raising_operand_orders_are_never_equal() {
    for (name, why, a, b) in NEVER_EQUAL {
        let (a, b) = if a.starts_with("CREATE") {
            (a.to_string(), b.to_string())
        } else {
            (proc_body(a), proc_body(b))
        };
        for (x, y, dir) in [(&a, &b, "A to B"), (&b, &a, "B to A")] {
            let v = verdict(x, y);
            assert!(
                !matches!(v, Equal | Eua),
                "{name} ({dir}): {v:?}; witness: {why}"
            );
        }
    }
}

#[test]
fn moved_session_settings_in_a_script_are_never_equal() {
    for (name, why, a, b) in [
        (
            "nocount-in-script",
            "A sends no count for the UPDATE, B sends one",
            "SET NOCOUNT ON;\nUPDATE dbo.Stock SET Qty = 1;\nDELETE FROM dbo.Orders;",
            "UPDATE dbo.Stock SET Qty = 1;\nSET NOCOUNT ON;\nDELETE FROM dbo.Orders;",
        ),
        (
            "quoted-identifier-in-script",
            "under QUOTED_IDENTIFIER OFF A returns the string 'a', B the column",
            "SET QUOTED_IDENTIFIER ON;\nSELECT \"Qty\" FROM dbo.Stock;",
            "SELECT \"Qty\" FROM dbo.Stock;\nSET QUOTED_IDENTIFIER ON;",
        ),
        (
            "add-filegroup-in-script",
            "the filegroup exists for the CREATE TABLE ... ON in A and not in B",
            "ALTER DATABASE db ADD FILEGROUP fg1;\nCREATE TABLE dbo.X (a int) ON fg1;",
            "CREATE TABLE dbo.X (a int) ON fg1;\nALTER DATABASE db ADD FILEGROUP fg1;",
        ),
    ] {
        for (x, y, dir) in [(a, b, "A to B"), (b, a, "B to A")] {
            let v = verdict(x, y);
            assert!(
                !matches!(v, Equal | Eua),
                "{name} ({dir}): {v:?}; witness: {why}"
            );
        }
    }
}

#[test]
fn a_noted_statement_that_stays_put_does_not_block_a_rename() {
    // SET NOCOUNT ON at the top of both, after the same tokens: the rename is cleared.
    let a = proc_body("SET NOCOUNT ON; DECLARE @a int = 1; UPDATE dbo.Stock SET Qty = @a;");
    let b = proc_body("SET NOCOUNT ON; DECLARE @b int = 1; UPDATE dbo.Stock SET Qty = @b;");
    assert_eq!(verdict(&a, &b), Equal);
}

#[test]
fn a_moved_set_nocount_is_not_excused_by_any_observer_profile() {
    use sqldiff_core::observer::{Observer, RowCounts};
    let a = proc_body("SET NOCOUNT ON; UPDATE dbo.Stock SET Qty = 1; DELETE FROM dbo.Orders;");
    let b = proc_body("UPDATE dbo.Stock SET Qty = 1; SET NOCOUNT ON; DELETE FROM dbo.Orders;");
    let run = |rc: RowCounts| {
        let opts = Options {
            catalog_a: vec![Artifact::new("catalog.sql", T, DDL)],
            catalog_b: vec![Artifact::new("catalog.sql", T, DDL)],
            observer: Observer { row_counts: rc },
            ..Options::default()
        };
        sqldiff_core::with_engine_stack(|| {
            diff(
                &Artifact::new("a.sql", T, &a),
                &Artifact::new("b.sql", T, &b),
                &opts,
            )
        })
    };
    let seen = run(RowCounts::SameDialect);
    assert!(
        !matches!(seen.verdict(), Equal | Eua),
        "{:?}",
        seen.verdict()
    );
    // Even a profile that does not observe counts leaves a moved SET NOCOUNT
    // to the structural diff (`stmt.moved` is not among the findings it drops).
    let never = run(RowCounts::Never);
    assert!(
        !matches!(never.verdict(), Equal | Eua),
        "{:?}",
        never.verdict()
    );
}

#[test]
fn untyped_store_is_not_recorded_for_equal_terms_and_does_not_hide_a_type_change() {
    // Without a catalog (the destination type is unknown): reordering two
    // assignments of one statement stores the same terms, so it is equal and
    // names no assumption.
    let run = |a: &str, b: &str| {
        sqldiff_core::with_engine_stack(|| {
            diff(
                &Artifact::new("a.sql", T, a),
                &Artifact::new("b.sql", T, b),
                &Options::default(),
            )
        })
    };
    let a = "CREATE PROCEDURE dbo.P @p int, @q int AS BEGIN UPDATE dbo.U SET c = @p, d = @q; END";
    let b = "CREATE PROCEDURE dbo.P @p int, @q int AS BEGIN UPDATE dbo.U SET d = @q, c = @p; END";
    let r = run(a, b);
    assert_eq!(r.verdict(), Equal, "{:?}", r.findings);
    assert!(r.assumptions.iter().all(|x| x.id != "ASM-UNTYPED-STORE"));
    // The stored values differ in type: never equal, with or without the assumption.
    let a = "CREATE PROCEDURE dbo.P @p int AS BEGIN UPDATE dbo.U SET c = CAST(@p AS int); END";
    let b = "CREATE PROCEDURE dbo.P @p int AS BEGIN UPDATE dbo.U SET c = CAST(@p AS money); END";
    let r = run(a, b);
    assert!(!matches!(r.verdict(), Equal | Eua), "{:?}", r.verdict());
}

const SCALAR_GUARDS: &[(&str, &str, &str, &str)] = &[
    (
        "set-case-guard",
        "@y = 0: A's CASE tests the guard first and never divides, B raises error 8134",
        "DECLARE @r int; SET @r = CASE WHEN @y <> 0 AND @x / @y > 1 THEN 1 ELSE 0 END; SELECT @r;",
        "DECLARE @r int; SET @r = CASE WHEN @x / @y > 1 AND @y <> 0 THEN 1 ELSE 0 END; SELECT @r;",
    ),
    (
        "select-case-guard",
        "@y = 0: B raises error 8134",
        "SELECT CASE WHEN @y <> 0 AND @x / @y > 1 THEN 1 ELSE 0 END;",
        "SELECT CASE WHEN @x / @y > 1 AND @y <> 0 THEN 1 ELSE 0 END;",
    ),
    (
        "iif-guard",
        "@y = 0: B raises error 8134",
        "DECLARE @r int; SET @r = IIF(@y <> 0 AND @x / @y > 1, 1, 0); SELECT @r;",
        "DECLARE @r int; SET @r = IIF(@x / @y > 1 AND @y <> 0, 1, 0); SELECT @r;",
    ),
    (
        "insert-values-case-guard",
        "@y = 0: B raises error 8134",
        "INSERT INTO dbo.Orders (Qty) VALUES (CASE WHEN @y <> 0 AND @x / @y > 1 THEN 1 ELSE 0 END);",
        "INSERT INTO dbo.Orders (Qty) VALUES (CASE WHEN @x / @y > 1 AND @y <> 0 THEN 1 ELSE 0 END);",
    ),
    (
        "return-case-guard",
        "@y = 0: B raises error 8134",
        "RETURN CASE WHEN @y <> 0 AND @x / @y > 1 THEN 1 ELSE 0 END;",
        "RETURN CASE WHEN @x / @y > 1 AND @y <> 0 THEN 1 ELSE 0 END;",
    ),
];

#[test]
fn a_guard_reordered_in_a_scalar_expression_is_never_equal() {
    for (name, why, a, b) in SCALAR_GUARDS {
        let wrap =
            |body: &str| format!("CREATE PROCEDURE dbo.P @x int, @y int AS BEGIN {body} END");
        let (a, b) = (wrap(a), wrap(b));
        for (x, y, dir) in [(&a, &b, "A to B"), (&b, &a, "B to A")] {
            let v = verdict(x, y);
            assert!(
                !matches!(v, Equal | Eua),
                "{name} ({dir}): {v:?}; witness: {why}"
            );
        }
    }
}

#[test]
fn raise_free_operands_of_a_scalar_expression_may_still_swap() {
    let wrap = |c: &str| {
        format!("CREATE PROCEDURE dbo.P @x int, @y int AS BEGIN SELECT CASE WHEN {c} THEN 1 ELSE 0 END; END")
    };
    let v = verdict(&wrap("@y <> 0 AND @x > 1"), &wrap("@x > 1 AND @y <> 0"));
    assert!(matches!(v, Equal | Eua), "{v:?}");
}

#[test]
fn where_predicates_keep_sorting_in_a_query() {
    // The optimizer orders WHERE terms (checked on SQL Server 2022: both forms run).
    let q = |w: &str| {
        format!("CREATE PROCEDURE dbo.P @x int, @y int AS BEGIN SELECT Qty FROM dbo.Stock WHERE {w}; END")
    };
    let v = verdict(
        &q("ItemId <> 0 AND Qty / ItemId > 1"),
        &q("Qty / ItemId > 1 AND ItemId <> 0"),
    );
    assert!(matches!(v, Equal | Eua), "{v:?}");
}

/// `equiv.edit` compares which branch of a changed IF runs and how surely
/// each branch is observable, not what the branches do. A branch with a
/// condition of its own can re-derive the other's effect, so such a pair
/// carries no witnessed `equiv.edit` difference (the pair stays different
/// only through the unpaired statements, which a later proof may remove).
#[test]
fn an_if_reshaped_around_a_nested_condition_has_no_branch_witness() {
    let pairs = [
        (
            "nested-if-to-and",
            "the branch runs the INSERT exactly when both tests are true on both sides",
            "IF @a > 0 BEGIN IF @b > 0 INSERT INTO dbo.Orders (Qty) VALUES (1); END",
            "IF @a > 0 AND @b > 0 INSERT INTO dbo.Orders (Qty) VALUES (1);",
        ),
        (
            "guard-clause-without-else",
            "both raise for a NULL or non-positive @a and do nothing else; both insert otherwise",
            "IF @a IS NULL OR @a <= 0 BEGIN RAISERROR('bad', 16, 1); RETURN; END INSERT INTO dbo.Orders (Qty) VALUES (@a);",
            "IF @a > 0 INSERT INTO dbo.Orders (Qty) VALUES (@a); ELSE RAISERROR('bad', 16, 1);",
        ),
        (
            "guard-clause-with-blocks",
            "as above (this pair already had no equiv.edit finding before the change)",
            "IF @a IS NULL OR @a <= 0 BEGIN RAISERROR('bad', 16, 1); RETURN; END; INSERT INTO dbo.Orders (Qty) VALUES (@a);",
            "IF @a > 0 BEGIN INSERT INTO dbo.Orders (Qty) VALUES (@a); END ELSE BEGIN RAISERROR('bad', 16, 1); RETURN; END",
        ),
        (
            "early-return-to-if-else",
            "@a > 0: both insert 1 and the second insert is skipped by RETURN or by the ELSE; otherwise both insert 2",
            "IF @a > 0 BEGIN INSERT INTO dbo.Orders (Qty) VALUES (1); RETURN; END INSERT INTO dbo.Orders (Qty) VALUES (2);",
            "IF @a > 0 INSERT INTO dbo.Orders (Qty) VALUES (1); ELSE INSERT INTO dbo.Orders (Qty) VALUES (2);",
        ),
    ];
    for (name, why, a, b) in pairs {
        let wrap =
            |body: &str| format!("CREATE PROCEDURE dbo.P @a int, @b int AS BEGIN {body} END");
        let opts = Options {
            catalog_a: vec![Artifact::new("catalog.sql", T, DDL)],
            catalog_b: vec![Artifact::new("catalog.sql", T, DDL)],
            ..Options::default()
        };
        for (x, y) in [(wrap(a), wrap(b)), (wrap(b), wrap(a))] {
            let r = sqldiff_core::with_engine_stack(|| {
                diff(
                    &Artifact::new("a.sql", T, &x),
                    &Artifact::new("b.sql", T, &y),
                    &opts,
                )
            });
            assert!(
                !r.findings
                    .iter()
                    .any(|f| f.rule == "equiv.edit" && f.verdict == Different),
                "{name}: a witnessed equiv.edit difference ({why}): {:#?}",
                r.findings
            );
        }
    }
}
