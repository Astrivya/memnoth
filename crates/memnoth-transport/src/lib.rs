//! memnoth-transport — transport layer for memnoth IPC and harness API.
//!
//! Minimal viable implementation to unblock memnoth-mcp compilation.
//! Provides `Stream` and `Listener` types used by `AgentBridge` for IPC.

use anyhow::Result;
use std::net::SocketAddr;
use std::path::Path;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::net::{TcpListener, TcpStream};

type ReadHalf = tokio::net::tcp::OwnedReadHalf;
type WriteHalf = tokio::net::tcp::OwnedWriteHalf;

/// A duplex stream to a harness-API endpoint (TCP socket).
pub struct Stream {
    reader: BufReader<ReadHalf>,
    writer: WriteHalf,
}

impl Stream {
    /// Connect to a socket at `path` (interpreted as a TCP address on Windows).
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self> {
        let addr = resolve_addr(path.as_ref())?;
        let stream = TcpStream::connect(addr)
            .await
            .map_err(|e| anyhow::anyhow!("connect {}: {}", addr, e))?;
        let (reader, writer) = stream.into_split();
        Ok(Stream {
            reader: BufReader::new(reader),
            writer,
        })
    }

    /// Split the stream into reader and writer halves.
    pub fn into_split(self) -> (BufReader<ReadHalf>, WriteHalf) {
        (self.reader, self.writer)
    }

    /// Get a mutable reference to the reader half.
    pub fn reader_mut(&mut self) -> &mut BufReader<ReadHalf> {
        &mut self.reader
    }

    /// Get a mutable reference to the writer half.
    pub fn writer_mut(&mut self) -> &mut WriteHalf {
        &mut self.writer
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = unsafe { self.map_unchecked_mut(|s| &mut s.reader) };
        this.poll_read(cx, buf)
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let this = unsafe { self.map_unchecked_mut(|s| &mut s.writer) };
        this.poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = unsafe { self.map_unchecked_mut(|s| &mut s.writer) };
        this.poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = unsafe { self.map_unchecked_mut(|s| &mut s.writer) };
        this.poll_shutdown(cx)
    }
}

/// A listener on a TCP socket.
pub struct Listener {
    inner: TcpListener,
}

impl Listener {
    /// Bind to a socket path (interpreted as a TCP address).
    pub async fn bind(path: impl AsRef<Path>) -> Result<Self> {
        let addr = resolve_addr(path.as_ref())?;
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| anyhow::anyhow!("bind {}: {}", addr, e))?;
        Ok(Listener { inner: listener })
    }

    /// Accept an incoming connection.
    pub async fn accept(&self) -> Result<(Stream, SocketAddr)> {
        let (stream, peer) = self
            .inner
            .accept()
            .await
            .map_err(|e| anyhow::anyhow!("accept: {}", e))?;
        let (reader, writer) = stream.into_split();
        let stream = Stream {
            reader: BufReader::new(reader),
            writer,
        };
        Ok((stream, peer))
    }
}

/// Remove a stale socket file at `path` before binding (no-op on TCP).
pub fn remove_stale_socket(_path: &Path) -> Result<()> {
    Ok(())
}

/// Resolve a path to a SocketAddr.
///
/// Without Unix-domain sockets the path is the *name* of a loopback endpoint, so
/// the port must be derived from the whole path: a stable hash means a client
/// connecting to a path finds the server that bound it, and two different paths
/// (two workspaces, two concurrent tests) cannot land on the same endpoint.
/// Taking the leading digits of the path would do the opposite — every path in
/// one process shares a pid and therefore a port.
fn resolve_addr(path: &Path) -> Result<SocketAddr> {
    // FNV-1a: stable across processes and toolchains, which `DefaultHasher` is not.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let port = 20_000 + (hash % 40_000) as u16;
    let ip: std::net::IpAddr = "127.0.0.1".parse().expect("loopback literal");
    Ok(SocketAddr::new(ip, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bind_and_connect() {
        let path = std::env::temp_dir().join(format!(
            "memnoth-transport-test-{}-{}.sock",
            std::process::id(),
            memnoth_now_ms()
        ));
        let listener = Listener::bind(&path).await.expect("bind");
        let connect_path = path.clone();
        let client = tokio::spawn(async move { Stream::connect(&connect_path).await });
        let (_stream, _peer) = listener.accept().await.expect("accept");
        client
            .await
            .expect("join")
            .expect("a client must reach the endpoint its path names");
    }

    fn memnoth_now_ms() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    }
}
