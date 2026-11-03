//! memnoth-protocol-v2 — compact binary framing layer.
//!
//! Replaces the line-delimited JSON (NDJSON) transport that previously framed
//! messages over Unix sockets with a tight length-prefixed binary envelope.
//! The version is negotiated once during a handshake ([`handshake`]); each
//! subsequent message is a self-delimiting frame consisting of a 4-byte
//! little-endian length followed by the payload, so the receiver can split a
//! byte stream into discrete messages without scanning for delimiters or
//! parsing payload content.
//!
//! # Wire format
//!
//! A connection proceeds in two phases:
//!
//! 1. **Handshake** — the server sends the protocol version byte (`0x02`),
//!    the client echoes it, and both parties validate the exchange (see
//!    [`handshake`](self::handshake)).
//!
//! 2. **Framed messages** — each frame is:
//!
//! ```text
//!  ┌──────────────┬──────────┐
//!  │ length (4B)  │ payload  │
//!  │ LE u32       │ N bytes  │
//!  └──────────────┴──────────┘
//! ```
//!
//! - **`length`** (4 bytes): payload size in bytes, little-endian.
//! - **`payload`**: opaque message body (e.g. a serialized struct).
//!
//! # Usage
//!
//! ```
//! use tokio::io::duplex;
//! use memnoth_protocol_v2::{Framer, Frame};
//! #
//! # tokio::runtime::Builder::new_current_thread()
//! #     .enable_all()
//! #     .build()
//! #     .unwrap()
//! #     .block_on(async {
//! let (a, b) = duplex(8196);
//! let mut writer = Framer::new(a);
//! let mut reader = Framer::new(b);
//! writer.write_frame(&Frame::from_slice(b"hello")).await.unwrap();
//! let frame = reader.read_frame().await.unwrap().unwrap();
//! assert_eq!(frame.payload, b"hello");
//! # });
//! ```

pub mod framer;
pub mod handshake;

pub use framer::{
    Frame, Framer, FramingError, LENGTH_PREFIX_SIZE, MAX_PAYLOAD_SIZE, read_frame, write_frame,
};
pub use handshake::{HandshakeError, VERSION, client_handshake, server_handshake};
