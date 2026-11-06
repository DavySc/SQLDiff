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
//! `--temporal-historyless` (`RuleOptions::temporal_historyless`) is the
//! opt-in way out for tables whose period columns are both HIDDEN: a plain
//! table, no history (`ASM-TEMPORAL-HISTORYLESS`); history support for them
//! is future work.
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
    /// `--temporal-historyless`: the table became a plain table without the
    /// period columns, the history table and the triggers
    /// (`ASM-TEMPORAL-HISTORYLESS`); `versioning` is not called for it.
    pub(super) historyless: bool,
}

const DB2_REASON: &str = "system-versioned (temporal) table: Db2 for i has system-period temporal tables (ROW BEGIN/ROW END, PERIOD SYSTEM_TIME, ALTER TABLE … ADD VERSIONING), but which clock its row-begin values read (transaction or statement, local or UTC), their precision and how it matches the history table's columns are not verified on IBM i yet; no rule until a probe run settles them";

impl Cx<'_> {
    /// Reads the versioning clauses of a system-versioned CREATE TABLE and
    /// returns the table without them: period columns lose `GENERATED ALWAYS
    /// AS ROW START|END` and become `NOT NULL DEFAULT '-infinity'`, the PERIOD
    /// item and the SYSTEM_VERSIONING option go.
    ///
    /// With `--temporal-historyless` (`RuleOptions`) and both period columns
    /// HIDDEN, the period columns go as well and the table is the plain table
    /// the caller converts like any other (`Temporal::historyless`, no
    /// history table, no triggers), under `ASM-TEMPORAL-HISTORYLESS`. Any
    /// other item that names a period column refuses the table. Otherwise a
    /// HIDDEN period column keeps its place as an ordinary column under
    /// `ASM-HIDDEN-PERIOD-COLUMN`.
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
        // The table, and the trigger functions and triggers after it.
        self.step_ts("tsql.temporal.table", ts);

        // The table options: SYSTEM_VERSIONING out, the rest kept.
        let mut history = None;
        let mut retention_period = None;
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
                    let (h, retention) = versioning_option(trimmed(opt), true)?;
                    history = Some(h);
                    retention_period = retention;
                    self.step_ts("tsql.temporal.versioning-option", opt);
                } else {
                    kept.push(opt);
                }
            }
            if !kept.is_empty() {
                tail.extend(ts[i..=p].iter().cloned());
                for (n, opt) in kept.iter().enumerate() {
                    if n > 0 {
                        let at = opt.first().map_or(0, |t| t.pos.0);
                        tail.push(T::made(COMMA, ",", at, "tsql.temporal.table"));
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
        let mut gen: Vec<(String, bool, u32, bool)> = Vec::new();
        let mut period_items: Vec<usize> = Vec::new();
        let mut any_hidden = false;
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
                        self.step_ts("tsql.temporal.period", item);
                    }
                    _ => return no("PERIOD FOR SYSTEM_TIME: unexpected form"),
                }
                continue;
            }
            match row_generated(item)? {
                Some((at, len, is_start, hidden)) => {
                    any_hidden |= hidden;
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
                    it.push(T::made(
                        NUMBER,
                        format!(
                            "{}DEFAULT '-infinity'",
                            if not_null { "" } else { "NOT NULL " }
                        ),
                        item[at].pos.0,
                        "tsql.temporal.period-column",
                    ));
                    it.extend(item[at + len..].iter().cloned());
                    gen.push((col, is_start, digits, hidden));
                    period_items.push(items.len());
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
                .find(|(c, s, _, _)| c.eq_ignore_ascii_case(name) && *s == want_start)
                .map(|g| g.2)
        };
        let (Some(d1), Some(d2)) = (find(&start, true), find(&end, false)) else {
            return no("PERIOD FOR SYSTEM_TIME names columns that are not GENERATED ALWAYS AS ROW START and ROW END");
        };
        if gen.len() != 2 || d1 != d2 {
            return no("period columns of different types: no rule yet");
        }

        let historyless = self.options.temporal_historyless && gen.iter().all(|g| g.3);
        if historyless {
            // The period columns go; nothing else of the table may name them,
            // or the table would not be valid without them.
            for (n, it) in items.iter().enumerate() {
                if period_items.contains(&n) {
                    continue;
                }
                if let Some(t) = it.iter().find(|t| {
                    matches!(t.k, IDENT | QUOTED_IDENT) && {
                        let w = tsql_unquote(&t.s);
                        w.eq_ignore_ascii_case(&start) || w.eq_ignore_ascii_case(&end)
                    }
                }) {
                    return no(format!(
                        "historyless system-versioned table: {} also names the period column {}, which the table loses",
                        if sig(it).first().is_some_and(|&f| constraint_item(&it[f])) {
                            "a constraint"
                        } else {
                            "a column"
                        },
                        t.s
                    ));
                }
            }
            for &n in &period_items {
                self.step_ts("tsql.temporal.historyless", &items[n]);
            }
            let history_name = self.expr(&history).trim().to_string();
            self.note(
                Status::Assumptions,
                "ASM-TEMPORAL-HISTORYLESS",
                &format!("History support is future work: this table is converted without history. The HIDDEN period columns {start} and {end}, the PERIOD FOR SYSTEM_TIME item, the history table {history_name} and the versioning triggers are left out, so the PostgreSQL table keeps no history. SELECT * and an INSERT without a column list see the same columns as in T-SQL. Code that reads the history table, names {start} or {end} or uses FOR SYSTEM_TIME does not bind or is refused on the converted tables; nothing may rely on the period values."),
            );
            let mut out: Vec<T> = ts[..=open].to_vec();
            let mut first = true;
            for (n, it) in items.into_iter().enumerate() {
                if period_items.contains(&n) {
                    continue;
                }
                if !first {
                    let at = it.first().map_or(0, |t| t.pos.0);
                    out.push(T::made(COMMA, ",", at, "tsql.temporal.table"));
                }
                first = false;
                out.extend(it);
            }
            out.push(ts[close].clone());
            out.extend(tail);
            return Ok((
                Temporal {
                    start,
                    end,
                    digits: d1,
                    history,
                    historyless: true,
                },
                out,
            ));
        }
        if let Some(r) = retention_period {
            self.note(
                Status::Assumptions,
                "ASM-HISTORY-RETENTION",
                &format!("SQL Server hides history rows older than {r} from FOR SYSTEM_TIME queries at once and deletes them in the background; the converted table keeps every history row visible until a scheduled job (pg_cron or the like) deletes those whose period end is older than {r} (UTC), or the history grows without bound."),
            );
        }
        if any_hidden {
            self.note(
                Status::Assumptions,
                "ASM-HIDDEN-PERIOD-COLUMN",
                &format!("The period columns {start} and {end} are HIDDEN in T-SQL: SELECT *, t.* and an INSERT without a column list leave them out. PostgreSQL has no hidden columns, so the converted table has them as ordinary columns: SELECT *, t.*, INSERT … SELECT * and a view defined with * return them, and an INSERT without a column list fills the columns by position, so its values shift into the period columns unless those come last. Code that reads or writes the table that way must name its columns."),
            );
        }
        let mut out: Vec<T> = ts[..=open].to_vec();
        for (n, it) in items.into_iter().enumerate() {
            if n > 0 {
                let at = it.first().map_or(0, |t| t.pos.0);
                out.push(T::made(COMMA, ",", at, "tsql.temporal.table"));
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
                historyless: false,
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
        let named = |suffix: &str| trigger_name(table, suffix);
        let func = named("_versioning")?;
        let guard = named("_period_update")?;
        let truncate = named("_versioning_truncate")?;
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
            "System versioning becomes the trigger function {func}: INSERT and UPDATE set {start} to the transaction's start time in UTC (transaction_timestamp(), as SQL Server takes the transaction's begin time) and {end} to {max}; UPDATE and DELETE first copy the old row into {history}. As in T-SQL, TRUNCATE, explicit period values and updates of the period columns fail, and so does changing a row that a later transaction already changed (Msg 13535). Not the same: PostgreSQL does not stop direct writes to {history} (SQL Server does, Msg 13559/13560) or ALTER TABLE on either table (it would not reach the trigger's column list); an INSERT without a column list may leave the period columns out; a DATETIME2(7) end-of-time literal ('9999-12-31 23:59:59.9999999') rounds to 10000-01-01 in PostgreSQL, so code comparing with it is marked as differing; the user who changes the table needs INSERT on {history}. To load rows with their old period values, disable the triggers first, as SQL Server needs SYSTEM_VERSIONING = OFF."
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

CREATE TRIGGER {} BEFORE TRUNCATE ON {table}
    FOR EACH STATEMENT EXECUTE FUNCTION {func}()",
            names.join(", "),
            old.join(", "),
            trig(&guard),
            trig(&func),
            trig(&truncate),
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
            self.step_ts("tsql.temporal-history-created", &t.history);
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
            // The type, not its spelling (`NVARCHAR (MAX)`, `nvarchar(max)`).
            let ty = |c: &sqldiff_core::catalog::Column| {
                (
                    c.data_type.base.clone(),
                    c.data_type.args.clone(),
                    c.data_type.max,
                )
            };
            if ty(a) != ty(b) || (!a.data_type.is_known() && a.data_type.text != b.data_type.text) {
                return no(format!("history table {history}: column {} has another type than in the table; SQL Server refuses it", b.name));
            }
        }
        Ok(String::new())
    }
}

impl Cx<'_> {
    /// Whether `--temporal-historyless` converts the catalog table `t`
    /// without history: it is system-versioned and both period columns are
    /// HIDDEN (see [`Temporal::historyless`]).
    fn historyless(&self, t: &sqldiff_core::catalog::Table) -> bool {
        self.options.temporal_historyless
            && t.versioning.as_ref().is_some_and(|v| {
                [&v.start, &v.end].into_iter().all(|p| {
                    t.columns
                        .iter()
                        .any(|c| c.hidden && c.name.eq_ignore_ascii_case(p))
                })
            })
    }

    /// With `--temporal-historyless`: the table of the tree whose history
    /// table is the CREATE TABLE `name_t`, when that table is converted
    /// without history. The history table is then not converted: nothing
    /// writes it, so a plain table of that name would look like a history
    /// that reads as empty, while SQL Server's holds the old rows.
    pub(super) fn history_of_historyless(&self, name_t: &[T]) -> Option<String> {
        if !self.options.temporal_historyless {
            return None;
        }
        let cat = self.catalog?;
        let strip = |mut p: Vec<String>| {
            if p.len() == 2 && p[0].eq_ignore_ascii_case("dbo") {
                p.remove(0);
            }
            p.into_iter()
                .map(|x| x.to_ascii_lowercase())
                .collect::<Vec<_>>()
        };
        let name = strip(name_parts(name_t)?);
        cat.tables
            .iter()
            .filter(|t| self.historyless(t))
            .find(|t| {
                t.versioning
                    .as_ref()
                    .and_then(|v| v.history.clone())
                    .is_some_and(|h| strip(h) == name)
            })
            .map(|t| t.display())
    }

    /// ALTER TABLE on a table the tree creates system-versioned, in the four
    /// forms a data load uses to write period values (WWI
    /// DeactivateTemporalTablesBeforeDataLoad and Reactivate…):
    /// - `SET (SYSTEM_VERSIONING = OFF)` → the versioning triggers disabled;
    /// - `DROP PERIOD FOR SYSTEM_TIME` → the period-update guard disabled;
    /// - `ADD PERIOD FOR SYSTEM_TIME (start, end)` → the guard enabled;
    /// - `SET (SYSTEM_VERSIONING = ON (HISTORY_TABLE = h …))` → the
    ///   versioning triggers enabled.
    ///
    /// The catalog must show the CREATE TABLE's period and history table, and
    /// the ALTER must name the same ones, so the triggers it switches are the
    /// ones the converted CREATE TABLE made and write the same history.
    pub(super) fn alter_versioning(&mut self, ts: &[T]) -> R<String> {
        if self.db2() {
            return no("ALTER TABLE … SYSTEM_VERSIONING / PERIOD FOR SYSTEM_TIME: Db2 for i system-period tables are not converted yet (their facts are not verified on IBM i); no rule yet");
        }
        const FORMS: &str = "ALTER TABLE … SYSTEM_VERSIONING / PERIOD FOR SYSTEM_TIME: only SET (SYSTEM_VERSIONING = OFF), DROP PERIOD FOR SYSTEM_TIME, ADD PERIOD FOR SYSTEM_TIME (start, end) and SET (SYSTEM_VERSIONING = ON (HISTORY_TABLE = …)) have a rule";
        let s = sig(ts);
        let w = |k: usize| s.get(k).map(|&j| &ts[j]);
        let is = |k: usize, word: &str| w(k).is_some_and(|t| t.is(word));
        if !(is(0, "ALTER") && is(1, "TABLE")) {
            return no(FORMS);
        }
        let Some(act) = (2..s.len()).find(|&k| is(k, "SET") || is(k, "DROP") || is(k, "ADD"))
        else {
            return no(FORMS);
        };
        let name_t: Vec<T> = ts[s[2]..s[act]].to_vec();
        let Some(parts) = name_parts(&name_t) else {
            return no(FORMS);
        };
        let words = &s[act..];
        let word = |k: usize| words.get(k).map(|&j| &ts[j]);
        let wis = |k: usize, x: &str| word(k).is_some_and(|t| t.is(x));
        let period = wis(1, "PERIOD") && wis(2, "FOR") && wis(3, "SYSTEM_TIME");
        enum Switch {
            Off,
            On(Vec<T>),
            Drop,
            Add(String, String),
        }
        let switch = if wis(0, "DROP") && period && words.len() == 4 {
            Switch::Drop
        } else if wis(0, "ADD") && period {
            let rest: Vec<&T> = words[4..].iter().map(|&j| &ts[j]).collect();
            match rest.as_slice() {
                [l, a, c, b, r] if l.k == L_PAREN && c.k == COMMA && r.k == R_PAREN => {
                    Switch::Add(tsql_unquote(&a.s), tsql_unquote(&b.s))
                }
                _ => return no(FORMS),
            }
        } else if wis(0, "SET") && word(1).is_some_and(|t| t.k == L_PAREN) {
            let p = words[1];
            let r = matching(ts, p).ok_or(No("unbalanced parentheses".into()))?;
            if words.last() != Some(&r) {
                return no(FORMS);
            }
            let opts = split_commas(&ts[p + 1..r]);
            let [opt] = opts.as_slice() else {
                return no(FORMS);
            };
            let opt = trimmed(opt);
            let os = sig(opt);
            if !os.first().is_some_and(|&j| opt[j].is("SYSTEM_VERSIONING")) {
                return no(FORMS);
            }
            if os.len() == 3 && opt[os[1]].s == "=" && opt[os[2]].is("OFF") {
                Switch::Off
            } else {
                Switch::On(versioning_option(opt, false)?.0)
            }
        } else {
            return no(FORMS);
        };

        // What the tree's CREATE TABLE declared.
        let strip = |mut p: Vec<String>| {
            if p.len() == 2 && p[0] == "dbo" {
                p.remove(0);
            }
            p
        };
        let parts = strip(parts);
        let found = self.catalog.and_then(|cat| {
            if parts.len() > 2 {
                return None;
            }
            let idents: Vec<Ident> = parts.iter().map(|p| Ident::plain(p.clone())).collect();
            let t = &cat.tables[table_position(Dialect::Tsql, cat, &idents)?];
            if self.historyless(t) {
                return Some(None);
            }
            Some(t.versioning.clone())
        });
        let table = self.expr(&name_t).trim().to_string();
        if found == Some(None) {
            return no(format!("ALTER TABLE {table} … SYSTEM_VERSIONING / PERIOD FOR SYSTEM_TIME: with --temporal-historyless the CREATE TABLE became a plain table without history triggers (ASM-TEMPORAL-HISTORYLESS), so there is nothing to switch"));
        }
        let found = found.flatten();
        let Some(v) = found else {
            return no(format!("ALTER TABLE {table} … SYSTEM_VERSIONING / PERIOD FOR SYSTEM_TIME: the tree has no CREATE TABLE {table} with PERIOD FOR SYSTEM_TIME and SYSTEM_VERSIONING = ON, so which history triggers the converted table has is not known; no rule"));
        };
        let func = trigger_name(&table, "_versioning")?;
        let truncate = trigger_name(&table, "_versioning_truncate")?;
        let guard = trigger_name(&table, "_period_update")?;
        let (how, out) = match switch {
            Switch::Off => (
                "versioning-off",
                format!(
                    "ALTER TABLE {table} DISABLE TRIGGER {}, DISABLE TRIGGER {}",
                    trig(&func),
                    trig(&truncate)
                ),
            ),
            Switch::Drop => (
                "drop-period",
                format!("ALTER TABLE {table} DISABLE TRIGGER {}", trig(&guard)),
            ),
            Switch::Add(a, b) => {
                if !(a.eq_ignore_ascii_case(&v.start) && b.eq_ignore_ascii_case(&v.end)) {
                    return no(format!("ADD PERIOD FOR SYSTEM_TIME ({a}, {b}): the CREATE TABLE's period is ({}, {}), which the converted triggers write; no rule", v.start, v.end));
                }
                (
                    "add-period",
                    format!("ALTER TABLE {table} ENABLE TRIGGER {}", trig(&guard)),
                )
            }
            Switch::On(history) => {
                let named = name_parts(&history).map(strip);
                let declared = v
                    .history
                    .clone()
                    .map(|h| strip(h.iter().map(|x| x.to_ascii_lowercase()).collect()));
                if named.is_none() || named != declared {
                    let h = self.expr(&history).trim().to_string();
                    return no(format!("SYSTEM_VERSIONING = ON (HISTORY_TABLE = {h}): the converted CREATE TABLE's trigger writes the history table its SYSTEM_VERSIONING named; another one has no rule"));
                }
                (
                    "versioning-on",
                    format!(
                        "ALTER TABLE {table} ENABLE TRIGGER {}, ENABLE TRIGGER {}",
                        trig(&func),
                        trig(&truncate)
                    ),
                )
            }
        };
        self.step_ts_with("tsql.temporal.switch", ts, how);
        self.note(Status::Assumptions, "ASM-TEMPORAL-SWITCH", &format!(
            "Turning versioning off and on and dropping and adding the period become disabling and enabling the triggers the converted CREATE TABLE made ({} and {} for versioning, {} for the period). The table and its rows stay the same. Not the same: SQL Server refuses DROP PERIOD while versioning is on, and checks on ADD PERIOD that every row's {} is before its {} and on SYSTEM_VERSIONING = ON that the history table matches (with DATA_CONSISTENCY_CHECK = ON, also its rows); PostgreSQL checks none of that. Between SYSTEM_VERSIONING = OFF and DROP PERIOD, and between ADD PERIOD and SYSTEM_VERSIONING = ON, SQL Server still sets the period columns and refuses explicit values on INSERT, PostgreSQL does not. After DROP PERIOD an INSERT that leaves out a period column fails in SQL Server (the column has no default) and stores '-infinity' in PostgreSQL. Enabling and disabling triggers needs the table's owner in PostgreSQL, ALTER on the table in SQL Server.",
            trig(&func), trig(&truncate), trig(&guard), v.start, v.end
        ));
        Ok(out)
    }
}

/// The name of a trigger or trigger function the converted CREATE TABLE
/// makes: the converted table name with `suffix` on its last part.
fn trigger_name(table: &str, suffix: &str) -> R<String> {
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
}

/// A trigger's own name, without the schema: `ALTER TABLE … ENABLE TRIGGER`
/// takes it unqualified.
fn trig(n: &str) -> &str {
    n.rsplit('.').next().unwrap_or(n)
}

/// `SYSTEM_VERSIONING = ON (HISTORY_TABLE = x [, DATA_CONSISTENCY_CHECK = …]
/// [, HISTORY_RETENTION_PERIOD = INFINITE | n unit])` → the history table name
/// and the finite retention period as written (`12 MONTHS`). A finite period
/// is read only with `allow_retention` (a CREATE TABLE, where the caller
/// records ASM-HISTORY-RETENTION for it); `ALTER TABLE … SET` refuses it, as
/// no note would carry the assumption there.
fn versioning_option(opt: &[T], allow_retention: bool) -> R<(Vec<T>, Option<String>)> {
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
    let mut retention = None;
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
                // `n DAYS|WEEKS|MONTHS|YEARS` (singular accepted): the only
                // forms SQL Server takes.
                let w: Vec<&T> = value.iter().filter(|t| t.sig()).collect();
                let unit_ok = |u: &str| {
                    matches!(
                        u,
                        "DAY" | "DAYS" | "WEEK" | "WEEKS" | "MONTH" | "MONTHS" | "YEAR" | "YEARS"
                    )
                };
                match w.as_slice() {
                    [n, u]
                        if allow_retention
                            && !n.s.is_empty()
                            && n.s.chars().all(|c| c.is_ascii_digit())
                            && unit_ok(&u.up()) =>
                    {
                        retention = Some(format!("{} {}", n.s, u.s));
                    }
                    _ => return no("HISTORY_RETENTION_PERIOD: only a number of days, weeks, months or years in CREATE TABLE has a rule; SQL Server deletes old history rows in the background and PostgreSQL needs a scheduled job for that"),
                }
            }
            w => return no(format!("SYSTEM_VERSIONING option {w}: no rule yet")),
        }
    }
    history.map(|h| (h, retention)).ok_or(No("SYSTEM_VERSIONING = ON without HISTORY_TABLE: SQL Server creates a history table with a generated name (MSSQL_TemporalHistoryFor_…); name one to convert".into()))
}

/// `GENERATED ALWAYS AS ROW START|END [HIDDEN]` in a column item: its token
/// span (HIDDEN included), whether it is the start and whether it is HIDDEN.
/// The caller records ASM-HIDDEN-PERIOD-COLUMN for a hidden column.
fn row_generated(item: &[T]) -> R<Option<(usize, usize, bool, bool)>> {
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
        let hidden = is(5, "HIDDEN");
        let last = s[n + if hidden { 5 } else { 4 }];
        return Ok(Some((j, last + 1 - j, start, hidden)));
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
