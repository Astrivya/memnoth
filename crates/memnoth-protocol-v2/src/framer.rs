//! Binary framing primitives: read and write length-prefixed frames over any
//! [`AsyncRead`] / [`AsyncWrite`] stream.
//!
//! See the crate-level docs for the wire format.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Protocol version byte for v2 frames.
pub const VERSION: u8 = 0x02;

/// Size of the little-endian payload-length field, in bytes.
pub const LENGTH_PREFIX_SIZE: usize = 4;

/// Size of the full frame header: 1 (version) + 4 (length) = 5 bytes.
pub const HEADER_SIZE: usize = 1 + LENGTH_PREFIX_SIZE;

/// Maximum payload size for a single frame: 16 MiB.
///
/// The 4-byte length prefix can address up to ~4 GiB; this cap guards against
/// accidental allocation of oversized buffers from a malformed or malicious peer.
pub const MAX_PAYLOAD_SIZE: usize = 16 * 1024 * 1024;

/// A complete binary frame, ready to be sent or received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The opaque payload bytes.
    pub payload: Vec<u8>,
}

impl Frame {
    /// Create a new frame from raw payload bytes.
    pub fn new(payload: Vec<u8>) -> Self {
        Self { payload }
    }

    /// Create a frame from a borrowed slice (copies into an owned `Vec`).
    pub fn from_slice(payload: &[u8]) -> Self {
        Self {
            payload: payload.to_vec(),
        }
    }

    /// The number of bytes the serialized frame will occupy on the wire.
    pub fn size(&self) -> usize {
        HEADER_SIZE + self.payload.len()
    }
}

impl From<Vec<u8>> for Frame {
    fn from(payload: Vec<u8>) -> Self {
        Self { payload }
    }
}

impl From<&[u8]> for Frame {
    fn from(payload: &[u8]) -> Self {
        Self::from_slice(payload)
    }
}

/// Errors that can arise while framing or deframing a byte stream.
#[derive(Debug, thiserror::Error)]
pub enum FramingError {
    /// Underlying I/O error from the stream.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// The version byte in a received header did not match [`VERSION`].
    #[error("incompatible protocol version: expected {expected:#x}, got {got:#x}")]
    BadVersion { expected: u8, got: u8 },

    /// The declared payload length exceeds [`MAX_PAYLOAD_SIZE`].
    #[error("payload length {length} exceeds maximum of {max}")]
    PayloadTooLarge { length: usize, max: usize },

    /// The stream ended before a complete frame could be read.
    #[error("connection closed mid-frame")]
    UnexpectedEof,
}

/// Read a single frame from `reader`.
///
/// Returns `Ok(None)` when the stream reaches a clean end-of-file *before*
/// any header bytes are consumed — i.e. the peer closed gracefully and there
/// is nothing left to read.
///
/// If the version byte or length field arrives but the payload is truncated,
/// [`FramingError::UnexpectedEof`] is returned, signalling a broken stream.
pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Option<Frame>, FramingError> {
    let mut header = [0u8; HEADER_SIZE];

    // A graceful close before any header byte is read means "no more frames."
    let n = reader.read(&mut header).await?;
    if n == 0 {
        return Ok(None);
    }
    if n != HEADER_SIZE {
        return Err(FramingError::UnexpectedEof);
    }

    if header[0] != VERSION {
        return Err(FramingError::BadVersion {
            expected: VERSION,
            got: header[0],
        });
    }

    let length = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;

    if length > MAX_PAYLOAD_SIZE {
        return Err(FramingError::PayloadTooLarge {
            length,
            max: MAX_PAYLOAD_SIZE,
        });
    }

    let mut payload = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut payload).await.map_err(|e| {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                FramingError::UnexpectedEof
            } else {
                FramingError::Io(e)
            }
        })?;
    }

    Ok(Some(Frame { payload }))
}

/// Write a single frame to `writer`, flushing afterwards.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &Frame,
) -> Result<(), FramingError> {
    if frame.payload.len() > MAX_PAYLOAD_SIZE {
        return Err(FramingError::PayloadTooLarge {
            length: frame.payload.len(),
            max: MAX_PAYLOAD_SIZE,
        });
    }

    let length = frame.payload.len() as u32;

    writer.write_all(&[VERSION]).await?;
    writer.write_all(&length.to_le_bytes()).await?;
    writer.write_all(&frame.payload).await?;
    writer.flush().await?;

    Ok(())
}

/// A convenience wrapper that turns any bidirectional stream into a
/// frame-oriented channel.
#[derive(Debug)]
pub struct Framer<S> {
    stream: S,
}

impl<S> Framer<S> {
    /// Wrap a stream.
    pub fn new(stream: S) -> Self {
        Self { stream }
    }

    /// Consume the wrapper and return the inner stream.
    pub fn into_inner(self) -> S {
        self.stream
    }

    /// Borrow the inner stream.
    pub fn inner(&self) -> &S {
        &self.stream
    }

    /// Mutably borrow the inner stream.
    pub fn inner_mut(&mut self) -> &mut S {
        &mut self.stream
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> Framer<S> {
    /// Read the next frame from the stream.
    pub async fn read_frame(&mut self) -> Result<Option<Frame>, FramingError> {
        read_frame(&mut self.stream).await
    }

    /// Write a frame to the stream and flush.
    pub async fn write_frame(&mut self, frame: &Frame) -> Result<(), FramingError> {
        write_frame(&mut self.stream, frame).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trips_a_frame_through_a_duplex() {
        let (left, right) = tokio::io::duplex(8192);

        let mut reader = Framer::new(left);
        let mut writer = Framer::new(right);

        let msg = b"hello, binary world!";
        writer.write_frame(&Frame::from_slice(msg)).await.unwrap();

        let frame = reader.read_frame().await.unwrap().expect("frame");
        assert_eq!(frame.payload, msg);
    }

    #[tokio::test]
    async fn returns_none_on_graceful_close() {
        let (left, right) = tokio::io::duplex(8192);
        drop(right);

        let mut reader = Framer::new(left);
        assert_eq!(reader.read_frame().await.unwrap(), None);
    }

    #[tokio::test]
    async fn rejects_mismatched_version() {
        let (mut left, mut right) = tokio::io::duplex(8192);

        tokio::spawn(async move {
            // Write version byte 0xFF then garbage.
            right.write_all(&[0xFF, 0, 0, 0, 0, 1]).await.unwrap();
        });

        let frame = read_frame(&mut left).await;
        match frame {
            Err(FramingError::BadVersion { expected, got }) => {
                assert_eq!(expected, VERSION);
                assert_eq!(got, 0xFF);
            }
            other => panic!("expected BadVersion, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_oversized_payload() {
        let (left, right) = tokio::io::duplex(8192);
        let mut writer = Framer::new(right);

        let big = vec![0u8; MAX_PAYLOAD_SIZE + 1];
        let result = writer.write_frame(&Frame::new(big)).await;
        assert!(matches!(result, Err(FramingError::PayloadTooLarge { .. })));
        drop(left);
    }

    #[tokio::test]
    async fn reads_multiple_sequential_frames() {
        let (left, right) = tokio::io::duplex(8192);

        let mut writer = Framer::new(right);
        let mut reader = Framer::new(left);

        for i in 0..5u8 {
            writer.write_frame(&Frame::from_slice(&[i])).await.unwrap();
        }

        for i in 0..5u8 {
            let frame = reader.read_frame().await.unwrap().expect("frame");
            assert_eq!(frame.payload, vec![i]);
        }
    }

    #[tokio::test]
    async fn handles_zero_length_payload() {
        let (left, right) = tokio::io::duplex(8192);

        let mut writer = Framer::new(right);
        let mut reader = Framer::new(left);

        writer.write_frame(&Frame::from_slice(b"")).await.unwrap();

        let frame = reader.read_frame().await.unwrap().expect("frame");
        assert!(frame.payload.is_empty());
    }

    #[tokio::test]
    async fn detects_truncated_header() {
        let (mut left, mut right) = tokio::io::duplex(8192);

        tokio::spawn(async move {
            // Write only 2 of 4 length bytes then close.
            right.write_all(&[0x05, 0x00]).await.unwrap();
        });

        let result = read_frame(&mut left).await;
        assert!(
            matches!(result, Err(FramingError::UnexpectedEof)),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn detects_truncated_payload() {
        let (mut left, mut right) = tokio::io::duplex(8192);

        tokio::spawn(async move {
            // A *valid* header that claims 10 bytes, then only 3 of them: the
            // version byte has to be there, or the framer rejects the header as
            // a bad version and this stops testing a truncated payload at all.
            right.write_all(&[VERSION]).await.unwrap();
            right.write_all(&10u32.to_le_bytes()).await.unwrap();
            right.write_all(b"abc").await.unwrap();
        });

        let result = read_frame(&mut left).await;
        assert!(
            matches!(result, Err(FramingError::UnexpectedEof)),
            "{result:?}"
        );
    }

    #[test]
    fn frame_size_is_correct() {
        let frame = Frame::from_slice(b"payload");
        assert_eq!(frame.size(), HEADER_SIZE + 7);
    }
}
