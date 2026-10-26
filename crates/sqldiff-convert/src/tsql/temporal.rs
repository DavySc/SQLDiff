//! System-versioned (temporal) tables, T-SQL → PostgreSQL.
//!
//! PostgreSQL has no system versioning, so the table gets a BEFORE row
//! trigger that does what SQL Server does (checked on SQL Server 2022 and
//! PostgreSQL 18, 2026-10-26):
//! - the period columns get the transaction's start time in UTC on INSERT and
//!   UPDATE, and the current row's end is the type's maximum;
//! - UPDATE and DELETE copy the old row into the history table, ending at
//!   that time, also when nothing changed and when the row was written earlier
//!   in the same transaction (a history row with start = end);
//! - Msg 13535: a row whose period starts after the transaction began (a
//!   later transaction changed it) cannot be changed;
//! - Msg 13536: no explicit value on INSERT (DEFAULT is allowed). The columns
//!   default to `-infinity`, which no T-SQL value converts to, so the trigger
//!   tells an omitted column from a given one;
//! - Msg 13537: no period column in UPDATE … SET, even to its own value
//!   (`BEFORE UPDATE OF` fires on the SET list, not on changed values);
//! - Msg 13545: no TRUNCATE.
//!
//! Db2 for i has native system-period temporal tables, but which clock its
//! row-begin values read, their precision and how ADD VERSIONING matches the
//! history table's columns are not verified on IBM i yet, so Db2 refuses.

use super::*;
use sqldiff_core::catalog::{table_position, Ident};

/// What `PERIOD FOR SYSTEM_TIME` and `SYSTEM_VERSIONING = ON (…)` say.
pub(super) struct Temporal {
    /// Period start and end columns, T-SQL names unquoted.
    start: String,
    end: String,
    /// Fractional digits PostgreSQL keeps (DATETIME2(7) keeps 6).
    digits: u32,
    /// The history table name as written.
    history: Vec<T>,
}

const DB2_REASON: &str = "system-versioned (temporal) table: Db2 for i has system-period temporal tables (ROW BEGIN/ROW END, PERIOD SYSTEM_TIME, ALTER TABLE … ADD VERSIONING), but which clock its row-begin values read (transaction or statement, local or UTC), their precision and how it matches the history table's columns are not verified on IBM i yet; no rule until a probe run settles them";

impl Cx<'_> {
    /// Reads the versioning clauses of a system-versioned CREATE TABLE and
    /// returns the table without them: period columns lose `GENERATED ALWAYS
    /// AS ROW START|END` and become `NOT NULL DEFAULT '-infinity'`, the PERIOD
    /// item and the SYSTEM_VERSIONING option go.
    pub(super) fn temporal(&mut self, ts: &[T]) -> R<(Temporal, Vec<T>)> {
        if self.db2() {
            return no(DB2_REASON);
        }
        let open = ts
            .iter()
            .position(|t| t.k == L_PAREN)
            .ok_or(No("CREATE TABLE without columns".into()))?;
        let close = matching(ts, open).ok_or(No("unbalanced parentheses".into()))?;
        if ts[..open].iter().any(|t| t.s.starts_with('#')) {
            return no("system-versioned temporary table: SQL Server refuses it");
        }

        // The table options: SYSTEM_VERSIONING out, the rest kept.
        let mut history = None;
        let mut tail: Vec<T> = Vec::new();
        let mut i = close + 1;
        while i < ts.len() {
            let with = ts[i].is("WITH") && prev_sig(ts, i).is_some_and(|p| p == close);
            let paren = next_sig(ts, i).filter(|&p| ts[p].k == L_PAREN);
            let (true, Some(p)) = (with, paren) else {
                tail.push(ts[i].clone());
                i += 1;
                continue;
            };
            let r = matching(ts, p).ok_or(No("unbalanced WITH".into()))?;
            let mut kept: Vec<&[T]> = Vec::new();
            for opt in split_commas(&ts[p + 1..r]) {
                if trimmed(opt)
                    .first()
                    .is_some_and(|t| t.is("SYSTEM_VERSIONING"))
                {
                    history = Some(versioning_option(trimmed(opt))?);
                } else {
                    kept.push(opt);
                }
            }
            if !kept.is_empty() {
                tail.extend(ts[i..=p].iter().cloned());
                for (n, opt) in kept.iter().enumerate() {
                    if n > 0 {
                        tail.push(T {
                            k: COMMA,
                            s: ",".into(),
                        });
                    }
                    tail.extend(opt.iter().cloned());
                }
                tail.push(ts[r].clone());
            }
            i = r + 1;
        }
        let Some(history) = history else {
            return no("PERIOD FOR SYSTEM_TIME without SYSTEM_VERSIONING = ON: a period without history; no rule yet");
        };

        // The column list: PERIOD out, period columns rewritten.
        let mut period = None;
        let mut gen: Vec<(String, bool, u32)> = Vec::new();
        let mut items: Vec<Vec<T>> = Vec::new();
        for item in split_commas(&ts[open + 1..close]) {
            let s = sig(item);
            if s.first().is_some_and(|&j| item[j].is("PERIOD")) {
                let w: Vec<&T> = s.iter().map(|&j| &item[j]).collect();
                match w.as_slice() {
                    [_, f, st, l, a, c, b, r]
                        if f.is("FOR")
                            && st.is("SYSTEM_TIME")
                            && l.k == L_PAREN
                            && c.k == COMMA
                            && r.k == R_PAREN =>
                    {
                        period = Some((tsql_unquote(&a.s), tsql_unquote(&b.s)));
                    }
                    _ => return no("PERIOD FOR SYSTEM_TIME: unexpected form"),
                }
                continue;
            }
            match row_generated(item)? {
                Some((at, len, is_start)) => {
                    let col = tsql_unquote(&item[s[0]].s);
                    let digits = period_digits(item, &s)?;
                    let words: Vec<&T> = s.iter().map(|&j| &item[j]).collect();
                    if words.iter().any(|t| t.is("DEFAULT")) {
                        return no(format!(
                            "period column {col} with a DEFAULT: SQL Server refuses it"
                        ));
                    }
                    let not_null = words.windows(2).any(|w| w[0].is("NOT") && w[1].is("NULL"));
                    if !not_null && words.iter().any(|t| t.is("NULL")) {
                        return no(format!(
                            "nullable period column {col}: SQL Server refuses it (Msg 13587)"
                        ));
                    }
                    let mut it: Vec<T> = item[..at].to_vec();
                    // Already converted: a token the expression walk copies as is.
                    it.push(T {
                        k: NUMBER,
                        s: format!(
                            "{}DEFAULT '-infinity'",
                            if not_null { "" } else { "NOT NULL " }
                        ),
                    });
                    it.extend(item[at + len..].iter().cloned());
                    gen.push((col, is_start, digits));
                    items.push(it);
                }
                None => items.push(item.to_vec()),
            }
        }
        let Some((start, end)) = period else {
            return no("SYSTEM_VERSIONING without PERIOD FOR SYSTEM_TIME: SQL Server refuses it");
        };
        let find = |name: &str, want_start: bool| {
            gen.iter()
                .find(|(c, s, _)| c.eq_ignore_ascii_case(name) && *s == want_start)
                .map(|g| g.2)
        };
        let (Some(d1), Some(d2)) = (find(&start, true), find(&end, false)) else {
            return no("PERIOD FOR SYSTEM_TIME names columns that are not GENERATED ALWAYS AS ROW START and ROW END");
        };
        if gen.len() != 2 || d1 != d2 {
            return no("period columns of different types: no rule yet");
        }

        let mut out: Vec<T> = ts[..=open].to_vec();
        for (n, it) in items.into_iter().enumerate() {
            if n > 0 {
                out.push(T {
                    k: COMMA,
                    s: ",".into(),
                });
            }
            out.extend(it);
        }
        out.push(ts[close].clone());
        out.extend(tail);
        Ok((
            Temporal {
                start,
                end,
                digits: d1,
                history,
            },
            out,
        ))
    }

    /// The trigger function and triggers that keep the history, after the
    /// converted CREATE TABLE. `table`: the converted name; `name_t`: the
    /// T-SQL table name; `cols`: the columns in order, T-SQL name unquoted
    /// and converted name.
    pub(super) fn versioning(
        &mut self,
        t: &Temporal,
        table: &str,
        name_t: &[T],
        cols: &[(String, String)],
    ) -> R<String> {
        let history = self.expr(&t.history).trim().to_string();
        let col = |n: &str| {
            cols.iter()
                .find(|c| c.0.eq_ignore_ascii_case(n))
                .map(|c| c.1.clone())
                .ok_or(No(format!(
                    "period column {n} is not a column of the table"
                )))
        };
        let start = col(&t.start)?;
        let end = col(&t.end)?;
        let create_history = self.history_check(t, name_t)?;
        let named = |suffix: &str| -> R<String> {
            let (schema, last) = match table.rfind('.') {
                Some(d) if !table[d..].contains('"') || table[d..].starts_with(".\"") => {
                    (&table[..=d], &table[d + 1..])
                }
                _ => ("", table),
            };
            let inner = last.trim_matches('"');
            if inner.len() + suffix.len() > 63 {
                return no(format!(
                    "system-versioned table {table}: the trigger name {inner}{suffix} is longer than PostgreSQL's 63 bytes"
                ));
            }
            Ok(if last.starts_with('"') {
                format!("{schema}\"{inner}{suffix}\"")
            } else {
                format!("{schema}{inner}{suffix}")
            })
        };
        let func = named("_versioning")?;
        let guard = named("_period_update")?;
        let trig = |n: &str| n.rsplit('.').next().unwrap_or(n).to_string();
        let ty = format!("timestamp({})", t.digits);
        let max = if t.digits == 0 {
            "9999-12-31 23:59:59".to_string()
        } else {
            format!("9999-12-31 23:59:59.{}", "9".repeat(t.digits as usize))
        };
        let names: Vec<&str> = cols.iter().map(|c| c.1.as_str()).collect();
        let old: Vec<String> = names
            .iter()
            .map(|&c| {
                if c == end {
                    "now_utc".to_string()
                } else {
                    format!("OLD.{c}")
                }
            })
            .collect();
        let text = format!(
            "System versioning becomes the trigger function {func}: INSERT and UPDATE set {start} to the transaction's start time in UTC (transaction_timestamp(), as SQL Server takes the transaction's begin time) and {end} to {max}; UPDATE and DELETE first copy the old row into {history}. As in T-SQL, TRUNCATE, explicit period values and updates of the period columns fail, and so does changing a row that a later transaction already changed (Msg 13535). Not the same: PostgreSQL does not stop direct writes to {history} (SQL Server does, Msg 13559/13560) or ALTER TABLE on either table (it would not reach the trigger's column list); an INSERT without a column list may leave the period columns out; the user who changes the table needs INSERT on {history}. To load rows with their old period values, disable the triggers first, as SQL Server needs SYSTEM_VERSIONING = OFF."
        );
        self.note(Status::Assumptions, "ASM-TEMPORAL-TRIGGER", &text);
        Ok(format!(
            "{create_history}CREATE FUNCTION {func}() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    now_utc {ty} := CAST(transaction_timestamp() AT TIME ZONE 'UTC' AS {ty});
BEGIN
    IF TG_OP = 'TRUNCATE' THEN
        RAISE EXCEPTION 'Truncate failed on table % because it is not a supported operation on system-versioned tables.', TG_TABLE_NAME;
    END IF;
    IF TG_OP = 'INSERT' THEN
        IF NEW.{start} IS DISTINCT FROM '-infinity' OR NEW.{end} IS DISTINCT FROM '-infinity' THEN
            RAISE EXCEPTION 'Cannot insert an explicit value into a GENERATED ALWAYS column in table %.', TG_TABLE_NAME;
        END IF;
    ELSE
        IF OLD.{start} > now_utc THEN
            RAISE EXCEPTION 'Data modification failed on system-versioned table % because transaction time was earlier than period start time for affected records.', TG_TABLE_NAME;
        END IF;
        INSERT INTO {history} ({})
        VALUES ({});
        IF TG_OP = 'DELETE' THEN
            RETURN OLD;
        END IF;
    END IF;
    NEW.{start} := now_utc;
    NEW.{end} := '{max}';
    RETURN NEW;
END
$$;

CREATE FUNCTION {guard}() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'Cannot update GENERATED ALWAYS columns in table %.', TG_TABLE_NAME;
END
$$;

CREATE TRIGGER {} BEFORE UPDATE OF {start}, {end} ON {table}
    FOR EACH ROW EXECUTE FUNCTION {guard}();

CREATE TRIGGER {} BEFORE INSERT OR UPDATE OR DELETE ON {table}
    FOR EACH ROW EXECUTE FUNCTION {func}();

CREATE TRIGGER {}_truncate BEFORE TRUNCATE ON {table}
    FOR EACH STATEMENT EXECUTE FUNCTION {func}()",
            names.join(", "),
            old.join(", "),
            trig(&guard),
            trig(&func),
            trig(&func),
        ))
    }

    /// With the tree's catalog: the history table must have the table's
    /// columns, by name and type in the same order, as SQL Server checks when
    /// versioning starts (Msg 13523/13524); when the tree has no such table,
    /// SQL Server creates one, so the output does too. Without a catalog this
    /// is an assumption.
    fn history_check(&mut self, t: &Temporal, name_t: &[T]) -> R<String> {
        let history = self.expr(&t.history).trim().to_string();
        let lookup = |ts: &[T]| -> Option<Vec<Ident>> {
            let mut p = name_parts(ts)?;
            if p.len() > 2 {
                return None;
            }
            if p.len() == 2 && p[0] == "dbo" {
                p.remove(0);
            }
            Some(p.into_iter().map(Ident::plain).collect())
        };
        let found = self.catalog.and_then(|cat| {
            let main = &cat.tables[table_position(Dialect::Tsql, cat, &lookup(name_t)?)?];
            let hist = lookup(&t.history)
                .and_then(|p| table_position(Dialect::Tsql, cat, &p))
                .map(|i| &cat.tables[i]);
            Some((main, hist))
        });
        let Some((main, hist)) = found else {
            self.note(Status::Assumptions, "ASM-TEMPORAL-HISTORY", &format!("Assumes {history} exists with the table's columns, by name and type in the same order, as SQL Server checks when versioning starts (it creates the table when there is none)."));
            return Ok(String::new());
        };
        let Some(hist) = hist else {
            self.note(Status::Converted, "tsql.temporal-history-created", &format!("The tree has no table {history}, so SQL Server creates it with the table's columns; so does the output (CREATE TABLE … (LIKE …): columns, types and NOT NULL, no defaults, keys or identity)."));
            let table = self.expr(name_t).trim().to_string();
            return Ok(format!("CREATE TABLE {history} (LIKE {table});\n\n"));
        };
        if !main.columns_known || !hist.columns_known {
            return no(format!(
                "history table {history}: the columns of one of the tables could not be read"
            ));
        }
        if main.columns.len() != hist.columns.len() {
            return no(format!(
                "history table {history} has {} columns and the table has {}: SQL Server refuses it (Msg 13523)",
                hist.columns.len(),
                main.columns.len()
            ));
        }
        for (n, (a, b)) in main.columns.iter().zip(&hist.columns).enumerate() {
            if !a.name.eq_ignore_ascii_case(&b.name) {
                return no(format!("history table {history}: column {} at ordinal {} is {} in the table; SQL Server refuses it (Msg 13524)", b.name, n + 1, a.name));
            }
            if a.data_type != b.data_type {
                return no(format!("history table {history}: column {} has another type than in the table; SQL Server refuses it", b.name));
            }
        }
        Ok(String::new())
    }
}

/// `SYSTEM_VERSIONING = ON (HISTORY_TABLE = x [, DATA_CONSISTENCY_CHECK = …]
/// [, HISTORY_RETENTION_PERIOD = INFINITE])` → the history table name.
fn versioning_option(opt: &[T]) -> R<Vec<T>> {
    let s = sig(opt);
    let on = s.get(2).is_some_and(|&j| opt[j].is("ON"));
    if !s.get(1).is_some_and(|&j| opt[j].s == "=") || !on {
        return no("SYSTEM_VERSIONING = OFF: a period without history; no rule yet");
    }
    let Some(&p) = s.get(3).filter(|&&j| opt[j].k == L_PAREN) else {
        return no("SYSTEM_VERSIONING = ON without HISTORY_TABLE: SQL Server creates a history table with a generated name (MSSQL_TemporalHistoryFor_…); name one to convert");
    };
    let r = matching(opt, p).ok_or(No("unbalanced SYSTEM_VERSIONING".into()))?;
    let mut history = None;
    for o in split_commas(&opt[p + 1..r]) {
        let o = trimmed(o);
        let os = sig(o);
        let (Some(&k), Some(&eq)) = (os.first(), os.get(1)) else {
            continue;
        };
        if o[eq].s != "=" {
            return no("SYSTEM_VERSIONING option without a value");
        }
        let value = trimmed(&o[eq + 1..]);
        match o[k].up().as_str() {
            "HISTORY_TABLE" => history = Some(value.to_vec()),
            // Checks rows already in the history table when versioning
            // starts; a new table has none.
            "DATA_CONSISTENCY_CHECK" => {}
            "HISTORY_RETENTION_PERIOD" if value.len() == 1 && value[0].is("INFINITE") => {}
            "HISTORY_RETENTION_PERIOD" => {
                return no("HISTORY_RETENTION_PERIOD: SQL Server deletes old history rows in the background; PostgreSQL needs a scheduled job for that; no rule yet")
            }
            w => return no(format!("SYSTEM_VERSIONING option {w}: no rule yet")),
        }
    }
    history.ok_or(No("SYSTEM_VERSIONING = ON without HISTORY_TABLE: SQL Server creates a history table with a generated name (MSSQL_TemporalHistoryFor_…); name one to convert".into()))
}

/// `GENERATED ALWAYS AS ROW START|END` in a column item: its token span and
/// whether it is the start. HIDDEN is refused.
fn row_generated(item: &[T]) -> R<Option<(usize, usize, bool)>> {
    let s = sig(item);
    for (n, &j) in s.iter().enumerate() {
        let w = |k: usize| s.get(n + k).map(|&x| &item[x]);
        let is = |k: usize, word: &str| w(k).is_some_and(|t| t.is(word));
        if !(item[j].is("GENERATED") && is(1, "ALWAYS") && is(2, "AS") && is(3, "ROW")) {
            continue;
        }
        let start = is(4, "START");
        if !start && !is(4, "END") {
            return no("GENERATED ALWAYS AS ROW …: only ROW START and ROW END have a rule");
        }
        if is(5, "HIDDEN") {
            return no("HIDDEN period column: PostgreSQL has no hidden columns, so SELECT * would return it");
        }
        let last = s[n + 4];
        return Ok(Some((j, last + 1 - j, start)));
    }
    Ok(None)
}

/// Fractional digits of a period column's DATETIME2(n), capped at
/// PostgreSQL's 6 (the type mapping notes the rounding of a 7th).
fn period_digits(item: &[T], s: &[usize]) -> R<u32> {
    let ty = s.get(1).map(|&j| &item[j]);
    if !ty.is_some_and(|t| tsql_unquote(&t.s).eq_ignore_ascii_case("DATETIME2")) {
        return no("period column that is not DATETIME2: SQL Server refuses it");
    }
    let p = match (s.get(2).map(|&j| &item[j]), s.get(3).map(|&j| &item[j])) {
        (Some(l), Some(n)) if l.k == L_PAREN && n.k == NUMBER => {
            n.s.parse::<u32>()
                .map_err(|_| No("DATETIME2 precision".into()))?
        }
        _ => 7,
    };
    Ok(p.min(6))
}
