use portal_core::manifest::ManifestError;
use portal_core::{SinkError, SourceError};
use portal_proto::{
    ChunkError, DeclineReason, ErrorCode, FrameError, PakeError, RendezvousError, VersionError,
};

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Why a transfer failed.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    /// The local endpoint could not be created.
    #[error("could not start networking")]
    Bind(#[source] BoxError),
    /// Connecting to the sender failed.
    #[error("could not connect to the sender")]
    Connect(#[source] BoxError),
    /// The QUIC connection or a stream failed.
    #[error("the connection failed")]
    Connection(#[source] BoxError),
    /// The rendezvous server URL is not a `ws://` or `wss://` URL.
    #[error("the rendezvous server URL must start with ws:// or wss://")]
    RendezvousUrl,
    /// The rendezvous server could not be reached.
    #[error("could not reach the rendezvous server")]
    RendezvousConnect(#[source] BoxError),
    /// The rendezvous server refused a request.
    #[error(transparent)]
    Rendezvous(RendezvousError),
    /// The connection to the rendezvous server dropped.
    #[error("lost the connection to the rendezvous server")]
    RendezvousLost,
    /// Announcing or searching on the local network or the DHT failed.
    #[error("peer discovery failed")]
    Discovery(#[source] BoxError),
    /// Nobody announced the code's number on the local network or the DHT.
    #[error("no sender found for this code")]
    SenderNotFound,
    /// The other peer left the rendezvous before the handshake finished.
    #[error("the other side disconnected before the handshake finished")]
    PeerLeft,
    /// The code handshake failed.
    #[error(transparent)]
    Pake(#[from] PakeError),
    /// The peer that connected is not the one that proved it knows the code.
    #[error("the connection came from an unexpected peer")]
    WrongPeer,
    /// A step took too long.
    #[error("timed out: {0}")]
    Timeout(&'static str),
    /// The peers have no protocol version in common.
    #[error(transparent)]
    Version(#[from] VersionError),
    /// The peer sent something the protocol does not allow at this point.
    #[error("the other side broke the protocol: {0}")]
    Protocol(String),
    /// A control frame could not be read or written.
    #[error(transparent)]
    Frame(#[from] FrameError),
    /// A chunk frame could not be read or decoded.
    #[error(transparent)]
    Chunk(#[from] ChunkError),
    /// The peer reported an error (message already sanitized).
    #[error("the other side reported an error: {0}")]
    Peer(String),
    /// The receiver declined the transfer.
    #[error("the transfer was declined: {0}")]
    Declined(DeclineReason),
    /// The peer went away mid-transfer.
    #[error("the other side closed the connection before the transfer finished")]
    PeerClosed,
    /// Writing received data failed.
    #[error(transparent)]
    Sink(#[from] SinkError),
    /// Reading data to send failed.
    #[error(transparent)]
    Source(#[from] SourceError),
    /// The manifest is invalid.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// A background task failed unexpectedly.
    #[error("internal error: {0}")]
    Internal(String),
}

impl NetError {
    pub(crate) fn bind(e: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Bind(Box::new(e))
    }

    pub(crate) fn connect(e: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Connect(Box::new(e))
    }

    pub(crate) fn connection(e: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Connection(Box::new(e))
    }

    pub(crate) fn join(e: tokio::task::JoinError) -> Self {
        Self::Internal(e.to_string())
    }

    /// Whether the peer already knows about this error (it told us, or it is gone).
    pub(crate) fn came_from_peer(&self) -> bool {
        matches!(
            self,
            Self::Peer(_) | Self::Declined(_) | Self::PeerClosed | Self::Connection(_)
        )
    }

    /// What to tell the peer about this error. Deliberately generic: local
    /// paths and details stay local.
    pub(crate) fn for_peer(&self) -> (ErrorCode, &'static str) {
        match self {
            Self::Sink(SinkError::CorruptChunk(_) | SinkError::CorruptFile(_)) | Self::Chunk(_) => {
                (ErrorCode::Corrupt, "received data failed verification")
            }
            Self::Source(SourceError::Changed(_)) => (
                ErrorCode::SourceChanged,
                "a file changed on the sender while it was being sent",
            ),
            Self::Sink(SinkError::Io { .. }) | Self::Source(SourceError::Io { .. }) => {
                (ErrorCode::Io, "a local file could not be read or written")
            }
            Self::Protocol(_)
            | Self::Frame(_)
            | Self::Manifest(_)
            | Self::Source(SourceError::UnknownChunk(_))
            | Self::Sink(SinkError::UnexpectedChunk(_) | SinkError::ChunkTooLarge(_)) => {
                (ErrorCode::Protocol, "protocol violation")
            }
            _ => (ErrorCode::Internal, "the transfer failed"),
        }
    }
}
