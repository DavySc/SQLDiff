//! IF / ELSE IF / ELSE and WHILE through a conversion: the check pairs
//! statements through the units' trace, and the IF and loop structure still
//! decides which statements may be compared (`same_branches`).

use sqldiff_convert::{convert, Conversion, Status, Unit};
use sqldiff_core::equiv::Anchor;
use sqldiff_core::{diff, Artifact, Dialect, Options, Verdict};

const PROC: &str = "CREATE PROCEDURE dbo.p
    @Mode int
AS
BEGIN
    DECLARE @x int;
    DECLARE @i int;
    SET @i = 0;
    IF @Mode = 1
        SET @x = 5;
    ELSE IF @Mode = 2
        SET @x = 6;
    ELSE
    BEGIN
        SET @x = 7;
    END
    WHILE @i < 3
    BEGIN
        SET @i = 2;
        IF @x = 6
            BREAK;
    END
END
";

/// The unit written from the input statement that starts with `needle`.
fn unit<'a>(c: &'a Conversion, needle: &str) -> &'a Unit {
    let at = PROC.find(needle).unwrap();
    c.units
        .iter()
        .find(|u| u.src.start == at)
        .unwrap_or_else(|| panic!("{needle}"))
}

#[test]
fn statements_in_branches_and_loops_stay_proven() {
    for to in [Dialect::Postgres, Dialect::Db2i] {
        let c = convert(PROC, Dialect::Tsql, to).unwrap();
        // `ELSE IF` becomes ELSIF / ELSEIF: its markers sit in the trace unit
        // of its condition on both sides, so the chain still pairs.
        for s in [
            "IF @Mode = 1",
            "SET @x = 5",
            "IF @Mode = 2",
            "SET @x = 6",
            "SET @x = 7",
            "WHILE @i < 3",
            "SET @i = 2",
            "IF @x = 6",
            "BREAK",
        ] {
            let u = unit(&c, s);
            assert!(
                u.status <= Status::Assumptions,
                "{to:?} {s}: {:?}\n{:#?}\n{}",
                u.status,
                c.report.unknown_reasons,
                c.output
            );
        }
    }
}

/// The conversion's anchors, as the check builds them.
fn anchors(c: &Conversion) -> Vec<Anchor> {
    c.units
        .iter()
        .map(|u| Anchor {
            a: (u.src.start, u.src.end),
            b: (u.out.start, u.out.end),
        })
        .collect()
}

fn check(output: &str, anchors: Vec<Anchor>) -> sqldiff_core::Report {
    let a = Artifact::new("input.sql", Dialect::Tsql, PROC);
    let b = Artifact::new("converted.sql", Dialect::Postgres, output);
    diff(
        &a,
        &b,
        &Options {
            anchors,
            ..Options::default()
        },
    )
}

fn equal(r: &sqldiff_core::Report) -> bool {
    matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions)
}

/// The reasons that name input line `line`.
fn reasons(r: &sqldiff_core::Report, line: usize) -> Vec<&String> {
    let at = format!("side A (T-SQL) line {line}:");
    r.unknown_reasons
        .iter()
        .filter(|x| x.starts_with(&at))
        .collect()
}

fn line_of(needle: &str) -> usize {
    PROC[..PROC.find(needle).unwrap()].matches('\n').count() + 1
}

#[test]
fn a_trace_into_the_other_branch_does_not_pair() {
    let c = convert(PROC, Dialect::Tsql, Dialect::Postgres).unwrap();
    // As converted: equal under assumptions.
    let r = check(&c.output, anchors(&c));
    assert!(equal(&r), "{:#?}", r.unknown_reasons);
    // A trace that sends the THEN statement to the ELSE branch's statement
    // and back: neither is compared with a statement of another branch.
    let mut swapped = anchors(&c);
    let (five, seven) = (unit(&c, "SET @x = 5").out, unit(&c, "SET @x = 7").out);
    for x in &mut swapped {
        if x.b == (five.start, five.end) {
            x.b = (seven.start, seven.end);
        } else if x.b == (seven.start, seven.end) {
            x.b = (five.start, five.end);
        }
    }
    let r = check(&c.output, swapped);
    assert!(!equal(&r), "{:?}", r.verdict());
    for s in ["SET @x = 5", "SET @x = 7"] {
        let why = reasons(&r, line_of(s));
        assert!(
            why.iter().any(|x| x.contains("is not paired")),
            "{s}: {:#?}",
            r.unknown_reasons
        );
    }
}

#[test]
fn a_statement_in_a_loop_does_not_pair_with_one_in_an_if() {
    let c = convert(PROC, Dialect::Tsql, Dialect::Postgres).unwrap();
    // The loop rewritten as an IF, byte for byte in place, so every anchor
    // still holds its statement.
    let out = c
        .output
        .replacen("WHILE v_i < 3 LOOP", "IF    v_i < 3 THEN", 1)
        .replacen("END LOOP;", "END IF;  ", 1)
        .replacen("EXIT;", "NULL;", 1);
    assert_eq!(out.len(), c.output.len());
    assert_ne!(out, c.output);
    let r = check(&out, anchors(&c));
    assert!(!equal(&r), "{:?}", r.verdict());
    let why = reasons(&r, line_of("SET @i = 2"));
    assert!(
        why.iter()
            .any(|x| x.contains("sits in another IF or loop branch")),
        "{:#?}",
        r.unknown_reasons
    );
}
