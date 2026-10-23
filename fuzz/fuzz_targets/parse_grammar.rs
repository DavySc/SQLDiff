//! Generated well-formed programs: round trip and no error nodes.
#![no_main]

use libfuzzer_sys::fuzz_target;
use sqldiff_corpus::fuzz::{measure, program, Bytes};
use sqldiff_syntax::Dialect;

fuzz_target!(|data: &[u8]| {
    let Some((&first, rest)) = data.split_first() else {
        return;
    };
    let d = Dialect::ALL[first as usize % Dialect::ALL.len()];
    let text = program(&mut Bytes::new(rest), d);
    let s = measure(&text, d);
    assert!(s.round_trips, "{d} lost bytes:\n{text}");
    assert_eq!(s.errors, 0, "{d} error nodes in well-formed input:\n{text}");
});
