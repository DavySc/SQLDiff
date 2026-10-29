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
    u.notes[0].text.clone()
}

#[test]
fn sqlcmd_is_not_sql() {
    assert!(reason(":r .\\pds100-ins-app-people.sql\n").contains("includes another file"));
    let r = reason("SELECT * FROM [$(Db)].dbo.t;\n");
    assert!(r.contains("SQLCMD variable") && r.contains("template"), "{r}");
    // A string that only looks like one is SQL.
    let c = pg("SELECT '$(x)' AS v;\n");
    assert_ne!(c.units[0].status, Status::NotConverted, "{}", c.output);
}

#[test]
fn a_filegroup_is_storage() {
    let c = pg("ALTER DATABASE [$(DatabaseName)]\n    ADD FILEGROUP [WWI_MemoryOptimized_Data] CONTAINS MEMORY_OPTIMIZED_DATA;\n");
    assert!(c.output.contains("storage only"), "{}", c.output);
    assert_eq!(c.units[0].status, Status::Converted, "{:#?}", c.units[0].notes);
    let r = reason("ALTER DATABASE CURRENT SET MEMORY_OPTIMIZED_ELEVATE_TO_SNAPSHOT=ON;\n");
    assert!(r.contains("server configuration"), "{r}");
}

#[test]
fn goto_names_the_rewrite() {
    let src = "CREATE PROCEDURE dbo.p @x int\nAS\nBEGIN\n  IF @x = 1\n  BEGIN\n    GOTO Logging\n  END\n  SET @x = 2;\n  Logging:\n  RETURN @x\nEND\n";
    let c = pg(src);
    let u = c.units.iter().find(|u| u.src.line == 6).unwrap();
    assert_eq!(u.status, Status::NotConverted);
    assert!(u.notes[0].text.contains("<<logging>>"), "{}", u.notes[0].text);
}
