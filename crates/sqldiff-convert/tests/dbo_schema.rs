//! `[dbo].x` is the same name as `dbo.x`: the schema is dropped either way
//! (ASM-DBO-SCHEMA), so no `dbo.` residue is left in the output.

use sqldiff_convert::{convert, Status};
use sqldiff_core::Dialect;

const SRC: &str = "ALTER PROCEDURE [dbo].[P] @a int AS BEGIN SET NOCOUNT ON; SELECT @a = @a + 1; END\nGO\nSELECT * FROM [dbo].[T];\nGO\nSELECT * FROM \"dbo\".T;\nGO\n";

#[test]
fn bracketed_dbo_is_dropped_like_plain_dbo() {
    for to in [Dialect::Postgres, Dialect::Db2i] {
        let c = convert(SRC, Dialect::Tsql, to).unwrap();
        let o = &c.output;
        assert!(!o.to_ascii_lowercase().contains("dbo."), "{to:?}: {o}");
        assert!(o.contains("CREATE OR REPLACE PROCEDURE P"), "{to:?}: {o}");
        assert_eq!(o.matches("SELECT * FROM T;").count(), 2, "{to:?}: {o}");
        for u in &c.units {
            assert!(
                u.notes.iter().all(|n| n.rule != "residue.tsql.dbo-schema"),
                "{to:?}: {:?}",
                u.notes
            );
            assert_ne!(u.status, Status::Differs, "{to:?}: {:?}", u.notes);
        }
        assert!(c
            .units
            .iter()
            .any(|u| u.notes.iter().any(|n| n.rule == "ASM-DBO-SCHEMA")));
    }
}
