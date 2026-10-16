//! Safe mode's read-only check on arbitrary text: it must never panic, whatever it's given.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(sql) = std::str::from_utf8(data) {
        table_minus_lib::fuzz_read_only(sql);
    }
});
