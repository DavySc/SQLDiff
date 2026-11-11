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
//! (statements with no counterpart); `rename-local` and
//! `swap-and-operands` are equal in truth and reported `Unknown`.

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
            pinned: Unknown,
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
            pinned: Unknown,
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
