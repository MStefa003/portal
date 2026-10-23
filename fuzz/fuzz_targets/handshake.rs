//! Messages from peers and servers that have not proven anything yet: the
//! code handshake and the rendezvous protocol.
#![no_main]

use libfuzzer_sys::fuzz_target;
use portal_proto::{ClientMsg, Code, PeerMsg, Role, ServerMsg, pake};

fuzz_target!(|data: &[u8]| {
    let _ = postcard::from_bytes::<ClientMsg>(data);
    let _ = postcard::from_bytes::<ServerMsg>(data);

    let Ok(peer) = postcard::from_bytes::<PeerMsg>(data) else {
        return;
    };
    let code = Code::parse("7-cedar-river").expect("a valid code");
    let (handshake, _) = pake::start(&code);
    // Garbage may complete SPAKE2, but never opens a sealed message: that
    // needs the code.
    if let Ok((_, opener)) = handshake.finish(peer.clone(), Role::Receiver) {
        assert!(opener.open(peer).is_err());
    }
});
