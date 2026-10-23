//! Frames on the control stream, from either peer.
#![no_main]

mod common;

use libfuzzer_sys::fuzz_target;
use portal_proto::frame::read_msg;
use portal_proto::{Control, MAX_CONTROL_FRAME, sanitize};

fuzz_target!(|data: &[u8]| {
    let mut input = data;
    while let Ok(Some(msg)) = common::now(read_msg::<_, Control>(&mut input, MAX_CONTROL_FRAME)) {
        if let Control::Error { message, .. } = &msg {
            // Peer text is printed after sanitizing: no terminal escapes survive.
            assert!(!sanitize(message).chars().any(char::is_control));
        }
        let bytes = postcard::to_stdvec(&msg).expect("a decoded message encodes");
        assert_eq!(postcard::from_bytes::<Control>(&bytes).expect("and decodes again"), msg);
    }
});
