//! Protocol version negotiation.

use crate::{MIN_PROTOCOL_VERSION, PROTOCOL_VERSION};

/// The peers have no protocol version in common.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VersionError {
    /// The peer only speaks versions older than we support.
    #[error(
        "the other side runs an older Portal (protocol {peer}); this one needs at least {MIN_PROTOCOL_VERSION}. Please update Portal on the other side."
    )]
    PeerTooOld {
        /// The peer's newest version.
        peer: u16,
    },
    /// The peer requires versions newer than we support.
    #[error(
        "the other side runs a newer Portal (protocol {peer_min} or later); this one supports up to {PROTOCOL_VERSION}. Please update Portal here."
    )]
    PeerTooNew {
        /// The oldest version the peer accepts.
        peer_min: u16,
    },
}

/// Picks the protocol version to use, given the peer's `Hello`.
///
/// Both sides compute the same answer: the highest version both support.
pub fn negotiate(peer_version: u16, peer_min: u16) -> Result<u16, VersionError> {
    if peer_version < MIN_PROTOCOL_VERSION {
        return Err(VersionError::PeerTooOld { peer: peer_version });
    }
    if peer_min > PROTOCOL_VERSION {
        return Err(VersionError::PeerTooNew { peer_min });
    }
    Ok(peer_version.min(PROTOCOL_VERSION))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_version_agrees() {
        assert_eq!(
            negotiate(PROTOCOL_VERSION, MIN_PROTOCOL_VERSION),
            Ok(PROTOCOL_VERSION)
        );
    }

    #[test]
    fn newer_compatible_peer_uses_our_version() {
        assert_eq!(
            negotiate(PROTOCOL_VERSION + 3, MIN_PROTOCOL_VERSION),
            Ok(PROTOCOL_VERSION)
        );
    }

    #[test]
    fn incompatible_peers_fail_clearly() {
        assert_eq!(negotiate(0, 0), Err(VersionError::PeerTooOld { peer: 0 }));
        let err = negotiate(99, 99).unwrap_err();
        assert_eq!(err, VersionError::PeerTooNew { peer_min: 99 });
        assert!(err.to_string().contains("update Portal here"));
    }
}
