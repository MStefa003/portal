//! The code handshake: SPAKE2 over the rendezvous relay, then one sealed
//! message in each direction.
//!
//! ```text
//! sender                                   receiver
//!   │── Pake { version, spake2 msg } ──────►│  (both at once)
//!   │◄────── Pake { version, spake2 msg } ──│
//!   │  K = SPAKE2 key; one subkey per direction
//!   │── Sealed(sender's endpoint address) ─►│
//!   │◄───────── Sealed(receiver's node ID) ─│
//! ```
//!
//! Both sides use the whole code as the SPAKE2 password. Someone without the
//! code words (including the rendezvous server) gets one guess: a wrong guess
//! yields a different key, the sealed message fails to open, and both sides
//! give up. Each direction has its own key, so a message cannot be reflected
//! back to its author, and each key seals exactly one message, so a fixed
//! nonce is safe.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use serde::{Deserialize, Serialize};
use spake2::{Ed25519Group, Identity, Password, Spake2};

use crate::code::Code;
use crate::{MIN_PROTOCOL_VERSION, PROTOCOL_VERSION, VersionError, negotiate};

/// SPAKE2 identity: binds the key to this protocol.
const PAKE_IDENTITY: &[u8] = b"portal/1 code handshake";

/// blake3 key-derivation contexts for the two directions.
const SENDER_CONTEXT: &str = "portal 2026-10-21 sealed message from sender";
const RECEIVER_CONTEXT: &str = "portal 2026-10-21 sealed message from receiver";

/// A message relayed between the two peers through the rendezvous server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerMsg {
    /// First message: protocol versions and the SPAKE2 message.
    Pake {
        /// Newest peer protocol version.
        version: u16,
        /// Oldest peer protocol version.
        min_version: u16,
        /// SPAKE2 message (33 bytes).
        msg: Vec<u8>,
    },
    /// Second message: an encrypted, authenticated payload.
    Sealed(Vec<u8>),
}

/// Which side of the transfer a party is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Has the files and the code; waits for the receiver.
    Sender,
    /// Typed the code; connects to the sender.
    Receiver,
}

/// Why the handshake failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PakeError {
    /// The peer used a different code (or someone tried to guess it).
    #[error("the code did not match")]
    WrongCode,
    /// The peer's message was not a valid handshake message.
    #[error("the other side sent an invalid handshake message")]
    Malformed,
    /// The peer speaks an incompatible protocol version.
    #[error(transparent)]
    Version(#[from] VersionError),
}

/// A handshake in progress: our SPAKE2 message has been sent.
pub struct Handshake(Spake2<Ed25519Group>);

impl std::fmt::Debug for Handshake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Handshake(..)")
    }
}

/// Starts the handshake for `code`; send the returned message to the peer.
pub fn start(code: &Code) -> (Handshake, PeerMsg) {
    let (state, msg) = Spake2::<Ed25519Group>::start_symmetric(
        &Password::new(code.password()),
        &Identity::new(PAKE_IDENTITY),
    );
    let msg = PeerMsg::Pake {
        version: PROTOCOL_VERSION,
        min_version: MIN_PROTOCOL_VERSION,
        msg,
    };
    (Handshake(state), msg)
}

impl Handshake {
    /// Completes SPAKE2 with the peer's first message.
    ///
    /// Succeeds even with a wrong code: that only shows when a sealed message
    /// fails to open.
    pub fn finish(self, peer: PeerMsg, me: Role) -> Result<(Sealer, Opener), PakeError> {
        let PeerMsg::Pake {
            version,
            min_version,
            msg,
        } = peer
        else {
            return Err(PakeError::Malformed);
        };
        negotiate(version, min_version)?;
        let key = self.0.finish(&msg).map_err(|_| PakeError::Malformed)?;
        let sender = Key::from(blake3::derive_key(SENDER_CONTEXT, &key));
        let receiver = Key::from(blake3::derive_key(RECEIVER_CONTEXT, &key));
        let (mine, theirs) = match me {
            Role::Sender => (sender, receiver),
            Role::Receiver => (receiver, sender),
        };
        Ok((Sealer(mine), Opener(theirs)))
    }
}

/// Seals our one message to the peer.
pub struct Sealer(Key);

/// Opens the peer's one message.
pub struct Opener(Key);

impl std::fmt::Debug for Sealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sealer(..)")
    }
}

impl std::fmt::Debug for Opener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Opener(..)")
    }
}

impl Sealer {
    /// Encrypts and authenticates `plaintext`. Consumes the key: one message
    /// per key is what makes the fixed nonce safe.
    pub fn seal(self, plaintext: &[u8]) -> PeerMsg {
        let cipher = ChaCha20Poly1305::new(&self.0);
        // Encryption only fails for inputs over ~256 GiB.
        let sealed = cipher
            .encrypt(&Nonce::default(), plaintext)
            .unwrap_or_default();
        PeerMsg::Sealed(sealed)
    }
}

impl Opener {
    /// Decrypts the peer's sealed message, proving it knew the code.
    pub fn open(self, peer: PeerMsg) -> Result<Vec<u8>, PakeError> {
        let PeerMsg::Sealed(sealed) = peer else {
            return Err(PakeError::Malformed);
        };
        ChaCha20Poly1305::new(&self.0)
            .decrypt(&Nonce::default(), sealed.as_slice())
            .map_err(|_| PakeError::WrongCode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handshake(a: &str, b: &str) -> ((Sealer, Opener), (Sealer, Opener)) {
        let (sa, ma) = start(&Code::parse(a).unwrap());
        let (sb, mb) = start(&Code::parse(b).unwrap());
        (
            sa.finish(mb, Role::Sender).unwrap(),
            sb.finish(ma, Role::Receiver).unwrap(),
        )
    }

    #[test]
    fn same_code_opens_both_ways() {
        let ((s_seal, s_open), (r_seal, r_open)) = handshake("7-cedar-river", "7-cedar-river");
        assert_eq!(
            r_open.open(s_seal.seal(b"sender addr")).unwrap(),
            b"sender addr"
        );
        assert_eq!(
            s_open.open(r_seal.seal(b"receiver id")).unwrap(),
            b"receiver id"
        );
    }

    #[test]
    fn wrong_words_fail() {
        let ((s_seal, s_open), (r_seal, r_open)) = handshake("7-cedar-river", "7-cedar-ocean");
        assert_eq!(r_open.open(s_seal.seal(b"x")), Err(PakeError::WrongCode));
        assert_eq!(s_open.open(r_seal.seal(b"x")), Err(PakeError::WrongCode));
    }

    #[test]
    fn wrong_nameplate_fails() {
        let ((s_seal, _), (_, r_open)) = handshake("7-cedar-river", "8-cedar-river");
        assert_eq!(r_open.open(s_seal.seal(b"x")), Err(PakeError::WrongCode));
    }

    #[test]
    fn reflected_message_does_not_open() {
        let code = Code::parse("7-cedar-river").unwrap();
        let (state, msg) = start(&code);
        // An attacker bounces the sender's own messages back to it.
        let (seal, open) = state.finish(msg, Role::Sender).unwrap();
        assert_eq!(open.open(seal.seal(b"x")), Err(PakeError::WrongCode));
    }

    #[test]
    fn tampering_is_detected() {
        let ((s_seal, _), (_, r_open)) = handshake("7-cedar-river", "7-cedar-river");
        let PeerMsg::Sealed(mut bytes) = s_seal.seal(b"sender addr") else {
            unreachable!()
        };
        bytes[0] ^= 1;
        assert_eq!(
            r_open.open(PeerMsg::Sealed(bytes)),
            Err(PakeError::WrongCode)
        );
    }

    #[test]
    fn garbage_is_malformed() {
        let (state, _) = start(&Code::parse("7-cedar-river").unwrap());
        let junk = PeerMsg::Pake {
            version: PROTOCOL_VERSION,
            min_version: MIN_PROTOCOL_VERSION,
            msg: vec![1, 2, 3],
        };
        assert_eq!(
            state.finish(junk, Role::Sender).unwrap_err(),
            PakeError::Malformed
        );
        let (state, _) = start(&Code::parse("7-cedar-river").unwrap());
        assert_eq!(
            state
                .finish(PeerMsg::Sealed(vec![]), Role::Sender)
                .unwrap_err(),
            PakeError::Malformed
        );
    }

    #[test]
    fn incompatible_version_is_reported() {
        let (state, _) = start(&Code::parse("7-cedar-river").unwrap());
        let (_, PeerMsg::Pake { msg, .. }) = start(&Code::parse("7-cedar-river").unwrap()) else {
            unreachable!()
        };
        let future = PeerMsg::Pake {
            version: PROTOCOL_VERSION + 5,
            min_version: PROTOCOL_VERSION + 5,
            msg,
        };
        assert!(matches!(
            state.finish(future, Role::Sender),
            Err(PakeError::Version(VersionError::PeerTooNew { .. }))
        ));
    }
}
