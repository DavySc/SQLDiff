//! What is not SQL, or not one statement on the target, is reported with
//! its own reason: SQLCMD directives (`:r`) and variables (`$(name)`), GOTO
//! (PL/pgSQL has none), database settings. `ALTER DATABASE … ADD FILEGROUP`
//! only places data, so it becomes a comment the engine passes over.

use sqldiff_convert::{check, rewrite_in, Conversion, Status};
use sqldiff_core::{Dialect, Options};

fn pg(src: &str) -> Conversion {
    check(
        src,
        rewrite_in(src, Dialect::Tsql, Dialect::Postgres, None),
        &Options::default(),
    )
}

fn reason(src: &str) -> String {
    let c = pg(src);
    let u = &c.units[0];
    assert_eq!(u.status, Status::NotConverted, "{src}\n{}", c.output);
    u.notes[0].text.to_string()
}

#[test]
fn sqlcmd_is_not_sql() {
    // `:r` is an include: dropped with a decision to run the included
    // file's conversion there. That decision is its outcome, not "not
    // compared": there is no SQL to compare.
    let c = pg(":r .\\pds100-ins-app-people.sql\n:r \"..\\x\\b.sql\"\nSELECT 1;\n");
    let r: Vec<_> = c.units.iter().filter(|u| u.src.line <= 2).collect();
    assert_eq!(r.len(), 2, "{:#?}", c.units);
    for u in r {
        assert_eq!(u.status, Status::Decision, "{}", c.output);
        assert!(
            u.notes.iter().all(|n| n.rule != "equiv.not-compared"),
            "{:#?}",
            u.notes
        );
        assert!(
            u.notes
                .iter()
                .any(|n| n.status == Status::Decision && n.text.contains("SQLCMD include")),
            "{:#?}",
            u.notes
        );
    }
    assert!(
        c.output.contains("\\ir './pds100-ins-app-people.sql'"),
        "{}",
        c.output
    );
    assert!(c.output.contains("\\ir '../x/b.sql'"), "{}", c.output);
    // An include named by a SQLCMD variable stays not converted.
    assert!(reason(":r $(Dir)\\a.sql\n").contains("includes another file"));
    let r = reason("SELECT * FROM [$(Db)].dbo.t;\n");
    assert!(
        r.contains("SQLCMD variable") && r.contains("template"),
        "{r}"
    );
    // A string that only looks like one is SQL.
    let c = pg("SELECT '$(x)' AS v;\n");
    assert_ne!(c.units[0].status, Status::NotConverted, "{}", c.output);
}

#[test]
fn a_filegroup_is_storage() {
    let c = pg("ALTER DATABASE [$(DatabaseName)]\n    ADD FILEGROUP [WWI_MemoryOptimized_Data] CONTAINS MEMORY_OPTIMIZED_DATA;\n");
    assert!(c.output.contains("storage only"), "{}", c.output);
    assert_eq!(
        c.units[0].status,
        Status::Converted,
        "{:#?}",
        c.units[0].notes
    );
    let r = reason("ALTER DATABASE CURRENT SET MEMORY_OPTIMIZED_ELEVATE_TO_SNAPSHOT=ON;\n");
    assert!(r.contains("server configuration"), "{r}");
}

#[test]
fn a_forward_goto_is_a_labelled_block_left_with_exit() {
    let src = "CREATE PROCEDURE dbo.p @x int\nAS\nBEGIN\n  IF @x = 1\n  BEGIN\n    GOTO Logging\n  END\n  SET @x = 2;\n  Logging:\n  RETURN @x\nEND\n";
    let c = pg(src);
    assert!(
        c.output.contains("<<goto_logging>>\n  BEGIN"),
        "{}",
        c.output
    );
    assert!(c.output.contains("EXIT goto_logging;"), "{}", c.output);
    let end = c.output.find("END;\n").unwrap();
    assert!(
        c.output.find("RETURN").unwrap() > end,
        "the statements after the label are outside the block\n{}",
        c.output
    );
    assert!(
        c.units.iter().all(|u| u.status != Status::NotConverted),
        "{:#?}",
        c.units
    );
}

#[test]
fn a_jump_that_is_not_forward_is_named_and_not_converted() {
    // Backward: the label comes first.
    let back = "CREATE PROCEDURE dbo.p @x int\nAS\nBEGIN\n  Again:\n  SET @x = @x + 1;\n  IF @x < 3 GOTO Again;\nEND\n";
    let c = pg(back);
    assert!(
        c.units.iter().any(|u| u.status == Status::NotConverted),
        "{}",
        c.output
    );
    assert!(c.output.contains("-- GOTO Again"), "{}", c.output);
    assert!(!c.output.contains("<<goto_"), "{}", c.output);
    // A label that is also the target of a jump from another list.
    let other = "CREATE PROCEDURE dbo.p @x int\nAS\nBEGIN\n  IF @x = 1\n  BEGIN\n    GOTO l\n    Inner:\n    SET @x = 5;\n  END\n  GOTO Inner;\n  l:\n  RETURN @x\nEND\n";
    let c = pg(other);
    assert!(
        c.units.iter().any(|u| u.status == Status::NotConverted),
        "{}",
        c.output
    );
    assert!(!c.output.contains("<<goto_inner>>"), "{}", c.output);
}

#[test]
fn crossing_jumps_are_not_converted() {
    let src = "CREATE PROCEDURE dbo.p @x int\nAS\nBEGIN\n  IF @x = 1 GOTO a;\n  IF @x = 2 GOTO b;\n  a:\n  SET @x = 3;\n  b:\n  RETURN @x\nEND\n";
    let c = pg(src);
    assert!(!c.output.contains("<<goto_"), "{}", c.output);
    assert!(
        c.units.iter().any(|u| u.status == Status::NotConverted),
        "{}",
        c.output
    );
}

#[test]
fn nested_jumps_are_nested_blocks() {
    let src = "CREATE PROCEDURE dbo.p @x int\nAS\nBEGIN\n  IF @x = 2 GOTO b;\n  IF @x = 1 GOTO a;\n  SET @x = 3;\n  a:\n  SET @x = 4;\n  b:\n  RETURN @x\nEND\n";
    let c = pg(src);
    let outer = c.output.find("<<goto_b>>").expect(&c.output);
    let inner = c.output.find("<<goto_a>>").expect(&c.output);
    assert!(outer < inner, "{}", c.output);
    assert!(
        c.output.contains("EXIT goto_a;") && c.output.contains("EXIT goto_b;"),
        "{}",
        c.output
    );
}
