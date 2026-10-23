//! Dialect semantic tables as data (design §4, §7): error kinds, what
//! happens to unhandled and caught errors, transaction and atomicity rules,
//! temporary state and cardinality contracts. The lowering and the
//! transaction interpreter read these tables; they are the single source for
//! the tests and the formal model.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use sqldiff_syntax::Dialect;

const TSQL: &str = include_str!("../../profiles/semantics/tsql.toml");
const POSTGRES: &str = include_str!("../../profiles/semantics/postgres.toml");
const DB2I: &str = include_str!("../../profiles/semantics/db2i.toml");

/// The abstract error kinds of design §3 Layer 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Constraint,
    Conversion,
    Arithmetic,
    Cardinality,
    NotFound,
    Lock,
    UserSignal,
    NameResolution,
    /// Query cancel, attention, assertion failure, fatal severities: errors
    /// that catch-all handlers (`WHEN OTHERS`, TRY/CATCH) do not catch.
    Cancel,
    Other,
}

impl ErrorKind {
    pub const ALL: [ErrorKind; 10] = [
        ErrorKind::Constraint,
        ErrorKind::Conversion,
        ErrorKind::Arithmetic,
        ErrorKind::Cardinality,
        ErrorKind::NotFound,
        ErrorKind::Lock,
        ErrorKind::UserSignal,
        ErrorKind::NameResolution,
        ErrorKind::Cancel,
        ErrorKind::Other,
    ];

    pub fn phrase(self) -> &'static str {
        match self {
            ErrorKind::Constraint => "a constraint violation",
            ErrorKind::Conversion => "a conversion error",
            ErrorKind::Arithmetic => "an arithmetic error",
            ErrorKind::Cardinality => "a cardinality error",
            ErrorKind::NotFound => "no rows (not found)",
            ErrorKind::Lock => "a deadlock or lock timeout",
            ErrorKind::UserSignal => "a user-raised error",
            ErrorKind::NameResolution => "a name-resolution error",
            ErrorKind::Cancel => "a cancel or uncatchable error",
            ErrorKind::Other => "an error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Confirmed,
    Corrected,
    Partial,
    Unverified,
}

/// One scalar row of a table: a value plus its provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row<T> {
    pub value: T,
    #[serde(default)]
    pub fact: Option<String>,
    pub status: Status,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KindRow {
    /// `error` or `warning`.
    pub class: String,
    pub codes: Vec<String>,
    /// T-SQL severity levels (`n+` = n and above) that map to the kind
    /// whatever the error number.
    #[serde(default)]
    pub severities: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StmtEffect {
    /// The failing statement's own changes are rolled back.
    Rollback,
    /// Rows changed before the error stay changed.
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TxnEffect {
    None,
    RollbackAll,
    Doom,
    /// Roll back to the savepoint of the scope that handles the error.
    RollbackScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Control {
    Next,
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Context {
    Routine,
    Script,
}

/// Conditions on the interpreter state under which a rule applies.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct When {
    pub xact_abort: Option<bool>,
    pub no_commit: Option<bool>,
    pub in_txn: Option<bool>,
    pub context: Option<Context>,
    /// The caller holds an open transaction.
    pub caller_txn: Option<bool>,
}

/// The state facts a `When` is matched against.
#[derive(Debug, Clone, Copy)]
pub struct Situation {
    pub xact_abort: bool,
    pub no_commit: bool,
    pub in_txn: bool,
    pub context: Context,
    pub caller_txn: bool,
}

impl When {
    pub fn matches(&self, s: &Situation) -> bool {
        fn ok<T: PartialEq>(want: Option<T>, have: T) -> bool {
            !matches!(want, Some(w) if w != have)
        }
        ok(self.xact_abort, s.xact_abort)
            && ok(self.no_commit, s.no_commit)
            && ok(self.in_txn, s.in_txn)
            && ok(self.context, s.context)
            && ok(self.caller_txn, s.caller_txn)
    }
}

/// What happens to an error: an `unhandled` or a `caught` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    /// For caught rows: `try_catch`, `exception_block`, `continue_handler`,
    /// `exit_handler`, `undo_handler`; absent means any construct.
    #[serde(default)]
    pub construct: Option<String>,
    #[serde(default)]
    pub when: When,
    /// Empty means every kind.
    #[serde(default)]
    pub kinds: Vec<ErrorKind>,
    pub statement: StmtEffect,
    pub transaction: TxnEffect,
    #[serde(default)]
    pub control: Option<Control>,
    #[serde(default)]
    pub fact: Option<String>,
    pub status: Status,
    #[serde(default)]
    pub note: Option<String>,
}

impl Rule {
    fn applies(&self, construct: Option<&str>, kind: ErrorKind, s: &Situation) -> bool {
        (self.kinds.is_empty() || self.kinds.contains(&kind))
            && self.when.matches(s)
            && match (&self.construct, construct) {
                (Some(c), Some(x)) => c == x,
                (Some(_), None) => false,
                (None, _) => true,
            }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transactions {
    pub nesting: Row<String>,
    pub rollback_scope: Row<String>,
    pub commit_without_txn: Row<String>,
    pub rollback_without_txn: Row<String>,
    pub savepoints: Row<String>,
    pub savepoints_need_commitment_control: Row<bool>,
    #[serde(default)]
    pub commit_in_function: Option<Row<String>>,
    pub commit_in_exception_block: Row<String>,
    pub commit_in_atomic: Row<String>,
    pub commit_in_caller_txn: Row<String>,
    pub doomed_commit: Row<String>,
    pub doomed_writes: Row<String>,
    pub no_commit_levels: Row<Vec<String>>,
    pub survives_rollback: Row<Vec<String>>,
    pub ddl: Row<String>,
    #[serde(default)]
    pub session_switch: Option<Row<String>>,
    #[serde(default)]
    pub deferred_constraints: Option<Row<String>>,
    #[serde(default)]
    pub commit_level_default: Option<Row<String>>,
    #[serde(default)]
    pub prerequisite: Option<Row<String>>,
    #[serde(default)]
    pub trancount_mismatch_at_exit: Option<Row<String>>,
    /// Per routine kind (`procedure`, `function`, `trigger`, `anonymous`,
    /// `script`): `autocommit`, `caller`, `call` or `commit_level`.
    pub routine_mode: BTreeMap<String, Row<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Atomicity {
    /// `always` or `commitment_control`.
    pub statement: Row<String>,
    pub routine_body: Row<String>,
    pub atomic_compound: Row<String>,
    #[serde(default)]
    pub atomic_under_no_commit: Option<Row<bool>>,
    pub merge: Row<String>,
    /// Db2 for i `MERGE ... NOT ATOMIC`.
    #[serde(default)]
    pub merge_not_atomic: Option<Row<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TempRow {
    pub id: String,
    /// `table_variable`, `temp_table` or `dgtt`.
    pub object: String,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub schema: Option<String>,
    /// `keep` (survives), `revert` (transactional) or `empty`.
    pub on_rollback: String,
    pub on_commit: String,
    pub lifetime: String,
    #[serde(default)]
    pub fact: Option<String>,
    pub status: Status,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CardinalityRow {
    pub id: String,
    pub form: String,
    pub many: String,
    pub none: String,
    #[serde(default)]
    pub fact: Option<String>,
    pub status: Status,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub dialect: String,
    pub error_kinds: BTreeMap<ErrorKind, KindRow>,
    /// Handler condition name (upper case) -> kinds it catches.
    pub conditions: BTreeMap<String, Vec<ErrorKind>>,
    /// Statement category -> kinds it can raise.
    pub raises: BTreeMap<String, Vec<ErrorKind>>,
    pub unhandled: Vec<Rule>,
    pub caught: Vec<Rule>,
    pub transactions: Transactions,
    pub atomicity: Atomicity,
    pub temp: Vec<TempRow>,
    pub cardinality: Vec<CardinalityRow>,
}

impl Profile {
    pub fn parse(text: &str) -> Result<Profile, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    /// The shipped profile of a dialect.
    pub fn of(d: Dialect) -> &'static Profile {
        static P: OnceLock<[Profile; 3]> = OnceLock::new();
        let all = P.get_or_init(|| {
            [TSQL, POSTGRES, DB2I].map(|t| Profile::parse(t).expect("shipped semantic profile"))
        });
        match d {
            Dialect::Tsql => &all[0],
            Dialect::Postgres => &all[1],
            Dialect::Db2i => &all[2],
        }
    }

    /// Raw TOML of a shipped profile.
    pub fn source(d: Dialect) -> &'static str {
        match d {
            Dialect::Tsql => TSQL,
            Dialect::Postgres => POSTGRES,
            Dialect::Db2i => DB2I,
        }
    }

    pub fn is_warning(&self, k: ErrorKind) -> bool {
        self.error_kinds
            .get(&k)
            .is_some_and(|r| r.class == "warning")
    }

    pub fn raises(&self, category: &str) -> &[ErrorKind] {
        self.raises.get(category).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Kinds a handler condition catches. Unknown names, SQLSTATE literals
    /// and native codes are mapped through the error-kind code table.
    pub fn condition_kinds(&self, name: &str) -> Vec<ErrorKind> {
        let up = name.trim().to_ascii_uppercase();
        if let Some(k) = self.conditions.get(&up) {
            return k.clone();
        }
        let code = up.trim_matches('\'');
        self.kind_of_code(code).into_iter().collect()
    }

    /// The kind a native error code or SQLSTATE maps to (`None` if only the
    /// catch-all `*` matches).
    pub fn kind_of_code(&self, code: &str) -> Option<ErrorKind> {
        for (k, row) in &self.error_kinds {
            for c in &row.codes {
                let hit = if let Some(p) = c.strip_suffix('*') {
                    !p.is_empty() && code.starts_with(p)
                } else if let Some(n) = c.strip_suffix('+') {
                    matches!((code.parse::<u64>(), n.parse::<u64>()), (Ok(x), Ok(m)) if x >= m)
                } else {
                    c == code
                };
                if hit {
                    return Some(*k);
                }
            }
        }
        None
    }

    /// Kind for a T-SQL severity level, when the table maps that level.
    pub fn kind_of_severity(&self, severity: u32) -> Option<ErrorKind> {
        self.error_kinds.iter().find_map(|(k, row)| {
            row.severities
                .iter()
                .any(|c| match c.strip_suffix('+') {
                    Some(n) => n.parse::<u32>().is_ok_and(|m| severity >= m),
                    None => c.parse::<u32>().is_ok_and(|m| severity == m),
                })
                .then_some(*k)
        })
    }

    pub fn unhandled_rule(&self, kind: ErrorKind, s: &Situation) -> Option<&Rule> {
        self.unhandled.iter().find(|r| r.applies(None, kind, s))
    }

    pub fn caught_rule(&self, construct: &str, kind: ErrorKind, s: &Situation) -> Option<&Rule> {
        self.caught
            .iter()
            .find(|r| r.applies(Some(construct), kind, s))
    }

    pub fn routine_mode(&self, routine: &str) -> &str {
        self.transactions
            .routine_mode
            .get(routine)
            .map(|r| r.value.as_str())
            .unwrap_or("autocommit")
    }

    pub fn is_no_commit_level(&self, level: &str) -> bool {
        self.transactions
            .no_commit_levels
            .value
            .iter()
            .any(|l| l.eq_ignore_ascii_case(level))
    }

    pub fn temp_row(&self, object: &str) -> Option<&TempRow> {
        self.temp.iter().find(|t| t.object == object)
    }

    pub fn cardinality(&self, form: &str) -> Option<&CardinalityRow> {
        self.cardinality.iter().find(|c| c.form == form)
    }

    /// Ids of every row in the profile, for coverage checks: list rows by
    /// their `id`, scalar rows by their dotted path.
    pub fn row_ids(d: Dialect) -> Vec<String> {
        let v: toml::Value = toml::from_str(Profile::source(d)).expect("profile toml");
        let mut out = Vec::new();
        collect_ids(&v, d.id(), &mut out);
        out
    }
}

fn collect_ids(v: &toml::Value, path: &str, out: &mut Vec<String>) {
    match v {
        toml::Value::Table(t) => {
            if t.contains_key("value") && t.contains_key("status") {
                out.push(path.to_string());
                return;
            }
            for (k, x) in t {
                if matches!(k.as_str(), "error_kinds" | "conditions" | "raises") {
                    continue;
                }
                collect_ids(x, &format!("{path}.{k}"), out);
            }
        }
        toml::Value::Array(a) => {
            for x in a {
                if let Some(id) = x.get("id").and_then(|i| i.as_str()) {
                    out.push(id.to_string());
                }
            }
        }
        _ => {}
    }
}
