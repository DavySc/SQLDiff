//! A unit's id, from its file and the text it stands for.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `text` with every CRLF as LF (a lone CR is kept).
pub fn lf(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains("\r\n") {
        text.replace("\r\n", "\n").into()
    } else {
        text.into()
    }
}

/// `review::unit_ids` from each unit's input and output text, with CRLF hashed as
/// LF (see `review::file_hash`).
pub fn ids_of<'a>(path: &str, units: impl Iterator<Item = (&'a str, &'a str)>) -> Vec<String> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    units
        .map(|(src, out)| {
            let mut h = Sha256::new();
            h.update(path.as_bytes());
            h.update([0u8]);
            h.update(lf(src).as_bytes());
            h.update([0u8]);
            h.update(lf(out).as_bytes());
            let base = format!("u{}", hex(&h.finalize()[..8]));
            let n = seen.entry(base.clone()).or_insert(0);
            *n += 1;
            if *n == 1 {
                base
            } else {
                format!("{base}~{n}")
            }
        })
        .collect()
}
