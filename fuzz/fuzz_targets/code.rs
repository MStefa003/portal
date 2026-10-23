//! Codes typed by people.
#![no_main]

use libfuzzer_sys::fuzz_target;
use portal_proto::Code;

fuzz_target!(|text: &str| {
    if let Ok(code) = Code::parse(text) {
        assert_eq!(Code::parse(&code.to_string()), Ok(code));
    }
});
