//! Writing the `[format]` section into a config file (`sqldiff.toml`) and
//! nothing else: comments, key order, other sections and the file's line
//! endings stay as they are.

use serde_json::{Map, Value as Json};
use sqldiff_core::format::{FormatConfig, FormatKeys};
use toml_edit::{DocumentMut, Item, Table, Value};

use crate::config::ConfigLayer;
use crate::{Error, Result};

const BOM: char = '\u{feff}';

/// `text` with its `[format]`, `[format.tsql]`, `[format.postgres]` and
/// `[format.db2i]` keys set to exactly `format`: a key `format` leaves unset
/// is removed (it falls through to lower layers again). A key that already
/// has the wanted value is not touched, and one whose value changes keeps the
/// comment after it.
///
/// A file that uses CRLF stays CRLF, including the lines added here; an empty
/// one gets CRLF if `crlf_if_empty`. The result is parsed again as a config
/// file and must give back `format`, or this is an error and nothing should
/// be written.
pub fn set_format(text: &str, format: &FormatConfig, crlf_if_empty: bool) -> Result<String> {
    format.validate().map_err(Error::new)?;
    let (bom, body) = match text.strip_prefix(BOM) {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let mut doc: DocumentMut = body.parse().map_err(|e| Error::new(format!("{e}")))?;

    let sections: [(&str, &FormatKeys); 4] = [
        ("", &format.all),
        ("tsql", &format.tsql),
        ("postgres", &format.postgres),
        ("db2i", &format.db2i),
    ];
    let any = sections.iter().any(|(_, k)| **k != FormatKeys::default());
    if let Some(root) = table_in(doc.as_table_mut(), "format", any)? {
        for (name, keys) in sections {
            let table = match name {
                "" => Some(&mut *root),
                n => table_in(root, n, *keys != FormatKeys::default())?,
            };
            if let Some(t) = table {
                apply(t, keys);
                if *keys != FormatKeys::default() {
                    // A section with keys needs its header line.
                    t.set_implicit(false);
                }
            }
        }
    }

    // The document drops a carriage return when it reads, so the file's line
    // ending is taken from the text it was given.
    let crlf = body.matches("\r\n").count();
    let lf = body.matches('\n').count() - crlf;
    let mut out = doc.to_string();
    if (crlf > 0 && crlf >= lf) || (crlf + lf == 0 && crlf_if_empty) {
        out = out.replace("\r\n", "\n").replace('\n', "\r\n");
    }

    let back = ConfigLayer::parse(&out)?;
    if back.policy.format != *format {
        return Err(Error::new(
            "the [format] section did not read back as written; nothing was changed",
        ));
    }
    Ok(if bom { format!("{BOM}{out}") } else { out })
}

/// The table `name` in `parent`, made if absent and `create`; an inline
/// table becomes a regular one.
fn table_in<'a>(parent: &'a mut Table, name: &str, create: bool) -> Result<Option<&'a mut Table>> {
    if parent.get(name).is_none() {
        if !create {
            return Ok(None);
        }
        let mut t = Table::new();
        // Printed only if it holds keys of its own.
        t.set_implicit(true);
        parent.insert(name, Item::Table(t));
    }
    let item = parent.get_mut(name).expect("just checked");
    if let Item::Value(Value::InlineTable(t)) = item {
        *item = Item::Table(std::mem::take(t).into_table());
    }
    match item {
        Item::Table(t) => Ok(Some(t)),
        _ => Err(Error::new(format!("`{name}` is not a table"))),
    }
}

fn apply(table: &mut Table, keys: &FormatKeys) {
    let want: Map<String, Json> = match serde_json::to_value(keys) {
        Ok(Json::Object(m)) => m,
        _ => Map::new(),
    };
    for name in FormatKeys::from_options(&Default::default()).names() {
        let Some(v) = want.get(&name) else {
            table.remove(&name);
            continue;
        };
        let new = match v {
            Json::Bool(b) => Value::from(*b),
            Json::Number(n) => Value::from(n.as_i64().unwrap_or_default()),
            Json::String(s) => Value::from(s.as_str()),
            _ => continue,
        };
        match table.get_mut(&name) {
            Some(Item::Value(old)) if same(old, &new) => {}
            Some(Item::Value(old)) => {
                let mut new = new;
                *new.decor_mut() = old.decor().clone();
                *old = new;
            }
            _ => {
                table.insert(&name, Item::Value(new));
            }
        }
    }
}

fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Integer(a), Value::Integer(b)) => a.value() == b.value(),
        (Value::Boolean(a), Value::Boolean(b)) => a.value() == b.value(),
        (Value::String(a), Value::String(b)) => a.value() == b.value(),
        _ => false,
    }
}
