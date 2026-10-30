//! SQL Server geography constants for PostGIS.
//!
//! `INSERT t (…, Location) VALUES (…, 0xE6100000010C…)` stores SQL Server's
//! binary form of a geography, which PostGIS rejects ("Invalid endian flag
//! value encountered"). A point both keep as it is (SRID 4326, finite,
//! within ±180 / ±90; `sqldiff_core::equiv::geography`) becomes its EWKB,
//! `'0101000020E6100000…'::geography`, the same doubles with longitude
//! first; Z and M are kept. Anything else stays a bytea, which the check
//! does not show equal.

use super::bits::{opening, operand_start, BitEdits};
use super::*;
use sqldiff_core::equiv::geography as geo;

impl Cx<'_> {
    /// Point constants stored in geography columns by `VALUES` rows.
    pub(super) fn geography_edits(&mut self, ts: &[T], e: &mut BitEdits) {
        if self.to != Dialect::Postgres {
            return;
        }
        for i in sig(ts) {
            if ts[i].k == IDENT && ts[i].is("VALUES") {
                self.geography_values(ts, i, e);
            }
        }
    }

    fn geography_values(&mut self, ts: &[T], i: usize, e: &mut BitEdits) {
        let Some(close) = prev_sig(ts, i).filter(|&c| ts[c].k == R_PAREN) else {
            return;
        };
        let Some(open) = opening(ts, close) else {
            return;
        };
        let Some(name_end) = prev_sig(ts, open) else {
            return;
        };
        let Some(name_start) = operand_start(ts, name_end) else {
            return;
        };
        let entry = ScopeEntry {
            name: ts[name_start..=name_end]
                .iter()
                .filter(|t| matches!(t.k, IDENT | QUOTED_IDENT))
                .map(|t| tsql_unquote(&t.s).to_ascii_lowercase())
                .collect(),
            alias: None,
            named: true,
        };
        let geog: Vec<bool> = {
            use sqldiff_core::catalog::Ident;
            let Some(table) = self.entry_table(&entry) else {
                return;
            };
            split_commas(&ts[open + 1..close])
                .iter()
                .map(|c| {
                    let c: String = trimmed(c).iter().map(|t| tsql_unquote(&t.s)).collect();
                    table
                        .column(Dialect::Tsql, &Ident::plain(c))
                        .is_some_and(|c| is_geography(&c.data_type.base))
                })
                .collect()
        };
        if !geog.contains(&true) {
            return;
        }
        let mut j = i;
        while let Some(row) = next_sig(ts, j).filter(|&p| ts[p].k == L_PAREN) {
            let Some(rend) = matching(ts, row) else {
                return;
            };
            let mut at = row + 1;
            for (k, v) in split_commas(&ts[row + 1..rend]).into_iter().enumerate() {
                if let ([f], Some(true)) = (sig(v).as_slice(), geog.get(k)) {
                    let t = &ts[at + f];
                    let point = (t.k == NUMBER)
                        .then(|| t.s.get(..2).filter(|p| p.eq_ignore_ascii_case("0x")))
                        .flatten()
                        .and_then(|_| geo::unhex(&t.s[2..]))
                        .and_then(|b| geo::from_sqlserver(&b));
                    if let Some(p) = point {
                        e.rep
                            .insert(at + f, format!("'{}'::geography", geo::ewkb_hex(&p)));
                        e.steps
                            .push(("tsql.geography.point-constant", at + f, at + f));
                    }
                }
                at += v.len() + 1;
            }
            match next_sig(ts, rend).filter(|&c| ts[c].k == COMMA) {
                Some(c) => j = c,
                None => break,
            }
        }
    }
}

/// A catalog type that is SQL Server's geography (`[sys].[geography]` too).
fn is_geography(base: &str) -> bool {
    let b = base.replace(['[', ']'], "").replace(" . ", ".");
    b == "GEOGRAPHY" || b == "SYS.GEOGRAPHY"
}
