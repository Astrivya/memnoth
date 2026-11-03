//! Protocol handshake: negotiate the wire version before exchanging frames.
//!
//! The handshake is a minimal one-byte exchange performed over a fresh
//! connection. The server sends [`VERSION`] first; the client reads it and
//! echoes it back. Both sides validate the byte and abort with
//! [`HandshakeError::VersionMismatch`] on disagreement.
//!
//! ```text
//!  Server → Client: VERSION (1 byte)
//!  Client → Server: VERSION (1 byte)
//! ```
//!
//! After a successful handshake the parties proceed with framed messages as
//! described in the crate-level documentation.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Protocol version byte negotiated during the handshake.
pub const VERSION: u8 = 0x02;

/// Errors that can arise during the initial version handshake.
#[derive(Debug, thiserror::Error)]
pub enum HandshakeError {
    /// Version byte from the peer did not match [`VERSION`].
    #[error("protocol version mismatch: expected {expected:#x}, got {got:#x}")]
    VersionMismatch { expected: u8, got: u8 },

    /// Underlying I/O error during the handshake exchange.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Perform the client side of the handshake.
///
/// Reads the server's version byte, verifies it, then echoes [`VERSION`]
/// back. Returns [`Ok(())`](std::result::Result) on success or
/// [`HandshakeError`] on failure.
pub async fn client_handshake<S>(stream: &mut S) -> Result<(), HandshakeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut buf = [0u8; 1];
    stream.read_exact(&mut buf).await?;
    if buf[0] != VERSION {
        return Err(HandshakeError::VersionMismatch {
            expected: VERSION,
            got: buf[0],
        });
    }
    stream.write_all(&[VERSION]).await?;
    stream.flush().await?;
    Ok(())
}

/// Perform the server side of the handshake.
///
/// Sends [`VERSION`] first, then reads the client's version byte and verifies
/// it. Returns [`Ok(())`](std::result::Result) on success or
/// [`HandshakeError`] on failure.
pub async fn server_handshake<S>(stream: &mut S) -> Result<(), HandshakeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    stream.write_all(&[VERSION]).await?;
    stream.flush().await?;
    let mut buf = [0u8; 1];
    stream.read_exact(&mut buf).await?;
    if buf[0] != VERSION {
        return Err(HandshakeError::VersionMismatch {
            expected: VERSION,
            got: buf[0],
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn handshake_round_trips_through_duplex() {
        let (a, b) = duplex(64);
        let mut server = a;
        let mut client = b;

        let server_fut = server_handshake(&mut server);
        let client_fut = client_handshake(&mut client);
        let (s, c) = tokio::join!(server_fut, client_fut);
        s.unwrap();
        c.unwrap();
    }

    #[tokio::test]
    async fn client_rejects_bad_server_version() {
        let (mut a, mut b) = duplex(64);

        tokio::spawn(async move {
            // Server sends wrong version.
            b.write_all(&[0xFF]).await.unwrap();
        });

        let result = client_handshake(&mut a).await;
        assert!(matches!(
            result,
            Err(HandshakeError::VersionMismatch { got: 0xFF, .. })
        ));
    }

    #[tokio::test]
    async fn server_rejects_bad_client_version() {
        let (a, b) = duplex(64);
        let mut server = a;
        let mut client_fake = b;

        // Client reads the server's version byte and sends a wrong one back.
        let handle = tokio::spawn(async move {
            let mut buf = [0u8; 1];
            client_fake.read_exact(&mut buf).await.unwrap();
            assert_eq!(buf[0], VERSION);
            client_fake.write_all(&[0x00]).await.unwrap();
        });

        let result = server_handshake(&mut server).await;
        assert!(matches!(
            result,
            Err(HandshakeError::VersionMismatch { got: 0x00, .. })
        ));
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn handshake_detects_eof() {
        let (mut a, b) = duplex(64);
        drop(b);

        let result = client_handshake(&mut a).await;
        assert!(result.is_err());
    }
}
