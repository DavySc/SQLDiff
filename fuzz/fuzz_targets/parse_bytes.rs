//! Arbitrary text: the lexer and parser never panic and never lose bytes.
#![no_main]

use libfuzzer_sys::fuzz_target;
use sqldiff_syntax::{parse, Dialect};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    for d in Dialect::ALL {
        let p = parse(text, d);
        assert_eq!(p.syntax().text().to_string(), text, "{d} lost bytes");
    }
});
