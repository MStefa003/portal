//! Length-prefixed framing: `u32` little-endian length, then that many bytes.
//!
//! The reader checks the length against a caller-supplied cap *before*
//! allocating, so a peer cannot make us reserve memory by lying about sizes.

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Why a frame could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The underlying stream failed.
    #[error("connection error: {0}")]
    Io(#[from] std::io::Error),
    /// The peer announced a frame larger than allowed.
    #[error("the other side sent a {len}-byte message, larger than the {max}-byte limit")]
    TooLarge {
        /// Announced length.
        len: usize,
        /// Allowed maximum.
        max: usize,
    },
    /// The stream ended in the middle of a frame.
    #[error("the connection ended in the middle of a message")]
    Truncated,
    /// The frame's content is not a valid message.
    #[error("the other side sent a malformed message: {0}")]
    Malformed(#[from] postcard::Error),
    /// The frame had bytes left over after the message.
    #[error("the other side sent a malformed message (trailing bytes)")]
    TrailingBytes,
}

/// Writes `msg` as one frame.
pub async fn write_msg<W, T>(w: &mut W, msg: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let bytes = postcard::to_allocvec(msg)?;
    write_raw(w, &bytes).await
}

/// Writes `bytes` as one frame.
pub async fn write_raw<W: AsyncWrite + Unpin>(w: &mut W, bytes: &[u8]) -> Result<(), FrameError> {
    let len = u32::try_from(bytes.len()).map_err(|_| FrameError::TooLarge {
        len: bytes.len(),
        max: u32::MAX as usize,
    })?;
    w.write_all(&len.to_le_bytes()).await?;
    w.write_all(bytes).await?;
    Ok(())
}

/// Reads one frame of at most `max` bytes and decodes it.
///
/// Returns `Ok(None)` if the stream ended cleanly before a new frame began.
///
/// Not cancel-safe: if the future is dropped mid-frame, the stream is left
/// mid-frame. Read from a dedicated task when you need `select!`.
pub async fn read_msg<R, T>(r: &mut R, max: usize) -> Result<Option<T>, FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let Some(bytes) = read_raw(r, max).await? else {
        return Ok(None);
    };
    let (msg, rest) = postcard::take_from_bytes(&bytes)?;
    if !rest.is_empty() {
        return Err(FrameError::TrailingBytes);
    }
    Ok(Some(msg))
}

/// Reads one raw frame of at most `max` bytes. `Ok(None)` on clean end of stream.
pub async fn read_raw<R: AsyncRead + Unpin>(
    r: &mut R,
    max: usize,
) -> Result<Option<Vec<u8>>, FrameError> {
    let mut len = [0u8; 4];
    let mut filled = 0;
    while filled < len.len() {
        let n = r.read(&mut len[filled..]).await?;
        if n == 0 {
            return if filled == 0 {
                Ok(None)
            } else {
                Err(FrameError::Truncated)
            };
        }
        filled += n;
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > max {
        return Err(FrameError::TooLarge { len, max });
    }
    let mut buf = vec![0u8; len];
    read_exact_or_truncated(r, &mut buf).await?;
    Ok(Some(buf))
}

/// `read_exact`, mapping a premature end of stream to [`FrameError::Truncated`].
pub(crate) async fn read_exact_or_truncated<R: AsyncRead + Unpin>(
    r: &mut R,
    buf: &mut [u8],
) -> Result<(), FrameError> {
    match r.read_exact(buf).await {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Err(FrameError::Truncated),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Control;

    #[tokio::test]
    async fn roundtrip_and_clean_eof() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        write_msg(&mut a, &Control::Accept).await.unwrap();
        write_msg(&mut a, &Control::Done).await.unwrap();
        drop(a);
        assert_eq!(
            read_msg::<_, Control>(&mut b, 100).await.unwrap(),
            Some(Control::Accept)
        );
        assert_eq!(
            read_msg::<_, Control>(&mut b, 100).await.unwrap(),
            Some(Control::Done)
        );
        assert_eq!(read_msg::<_, Control>(&mut b, 100).await.unwrap(), None);
    }

    #[tokio::test]
    async fn rejects_oversized_frames_before_reading_them() {
        let (mut a, mut b) = tokio::io::duplex(64);
        // Announce 4 GiB but send nothing else.
        a.write_all(&u32::MAX.to_le_bytes()).await.unwrap();
        let err = read_msg::<_, Control>(&mut b, 1024).await.unwrap_err();
        assert!(matches!(err, FrameError::TooLarge { .. }));
    }

    #[tokio::test]
    async fn detects_truncation() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&[10, 0, 0, 0, 1, 2]).await.unwrap();
        drop(a);
        let err = read_msg::<_, Control>(&mut b, 1024).await.unwrap_err();
        assert!(matches!(err, FrameError::Truncated));

        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&[10, 0]).await.unwrap();
        drop(a);
        let err = read_msg::<_, Control>(&mut b, 1024).await.unwrap_err();
        assert!(matches!(err, FrameError::Truncated));
    }

    #[tokio::test]
    async fn rejects_garbage_and_trailing_bytes() {
        let (mut a, mut b) = tokio::io::duplex(64);
        write_raw(&mut a, &[200, 200, 200]).await.unwrap();
        let err = read_msg::<_, Control>(&mut b, 1024).await.unwrap_err();
        assert!(matches!(err, FrameError::Malformed(_)));

        let (mut a, mut b) = tokio::io::duplex(64);
        write_raw(&mut a, &[3, 0xff]).await.unwrap(); // `Accept` + junk
        let err = read_msg::<_, Control>(&mut b, 1024).await.unwrap_err();
        assert!(matches!(err, FrameError::TrailingBytes));
    }
}
