//! SQL Server geography constants for PostGIS.
//!
//! `INSERT t (…, Location) VALUES (…, 0xE6100000010C…)` stores SQL Server's
//! binary form of a geography, which PostGIS rejects ("Invalid endian flag
//! value encountered"). A point both keep as it is (SRID 4326, finite,
//! within ±180 / ±90; `sqldiff_equiv::equiv::geography`) becomes its EWKB,
//! `'0101000020E6100000…'::geography`, the same doubles with longitude
//! first; Z and M are kept. Anything else stays a bytea, which the check
//! does not show equal.

use super::bits::{opening, operand_start, BitEdits};
use super::*;
use sqldiff_equiv::equiv::geography as geo;

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
            use sqldiff_equiv::catalog::Ident;
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

impl Cx<'_> {
    /// T-SQL `geography::Method(args)` at `ts[i]`, a static method of a CLR
    /// type (also `geometry::` and `hierarchyid::`): the text it becomes and
    /// the index after its closing parenthesis. Two methods have a rule for
    /// PostgreSQL, both only with SRID 4326:
    ///
    /// - `geography::STGeomFromWKB(wkb, 4326)` becomes
    ///   `ST_GeomFromWKB(wkb, 4326)::geography`;
    /// - `geography::Point(lat, lon, 4326)` becomes
    ///   `ST_SetSRID(ST_MakePoint(lon, lat), 4326)::geography` (PostGIS takes
    ///   the longitude first).
    ///
    /// The conversion is exact only for a constant both engines read alike
    /// (`sqldiff_equiv::equiv::geography`): SQL Server raises on a latitude
    /// beyond ±90, NaN or trailing bytes where PostGIS moves the value into
    /// range or accepts it, so the engine shows these equal only for such
    /// constants and every other call stays unproven. Every other static
    /// method, and every method on Db2 for i, is refused with the method
    /// named instead of leaving `type::Method` in the output.
    pub(super) fn geography_static(&mut self, ts: &[T], i: usize) -> Option<(String, usize)> {
        if !(ts[i].k == IDENT
            && ["GEOGRAPHY", "GEOMETRY", "HIERARCHYID"]
                .iter()
                .any(|t| ts[i].is(t)))
        {
            return None;
        }
        let colons = next_sig(ts, i).filter(|&c| ts[c].k == OP && ts[c].s == "::")?;
        let method = next_sig(ts, colons).filter(|&m| ts[m].k == IDENT)?;
        let open = next_sig(ts, method).filter(|&o| ts[o].k == L_PAREN)?;
        let close = matching(ts, open)?;
        let ty = ts[i].s.to_ascii_lowercase();
        let name = ts[method].s.to_ascii_lowercase();
        let args = split_commas(&ts[open + 1..close]);
        let is_srid =
            |a: &[T]| matches!(sig(a).as_slice(), [k] if a[*k].k == NUMBER && a[*k].s == "4326");
        let text = match (self.to, ty.as_str(), name.as_str(), args.len()) {
            (Dialect::Postgres, "geography", "stgeomfromwkb", 2) if is_srid(args[1]) => {
                let wkb = self.expr(args[0]).trim().to_string();
                self.step_ts("tsql.geography.from-wkb", &ts[i..=close]);
                format!("ST_GeomFromWKB({wkb}, 4326)::geography")
            }
            (Dialect::Postgres, "geography", "point", 3) if is_srid(args[2]) => {
                let lat = self.expr(args[0]).trim().to_string();
                let lon = self.expr(args[1]).trim().to_string();
                self.step_ts("tsql.geography.point", &ts[i..=close]);
                format!("ST_SetSRID(ST_MakePoint({lon}, {lat}), 4326)::geography")
            }
            _ => {
                let why = if self.db2() {
                    format!("{ty}::{} is a SQL Server CLR type method; Db2 for i has no spatial type in this conversion.", ts[method].s)
                } else if matches!(name.as_str(), "stgeomfromwkb" | "point") && ty == "geography" {
                    format!("geography::{} is converted only with the SRID 4326 written as a number: PostGIS reads other SRIDs through its own table, which was not checked.", ts[method].s)
                } else {
                    format!("{ty}::{} is a SQL Server CLR type method; no rule yet (PostGIS functions differ in validation and argument order, so each method needs its own checked rule).", ts[method].s)
                };
                self.note(Status::NotConverted, "tsql.geography.static-method", &why);
                return Some((self.source(&ts[i..=close]), close + 1));
            }
        };
        if !self.db2() {
            self.note(
                Status::Assumptions,
                "ASM-POSTGIS-GEOGRAPHY",
                "GEOGRAPHY becomes PostGIS geography: the database has the postgis extension and its types are on the search path.",
            );
        }
        Some((text, close + 1))
    }
}

/// A catalog type that is SQL Server's geography (`[sys].[geography]` too).
fn is_geography(base: &str) -> bool {
    let b = base.replace(['[', ']'], "").replace(" . ", ".");
    b == "GEOGRAPHY" || b == "SYS.GEOGRAPHY"
}
