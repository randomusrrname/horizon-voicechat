//! Network core of a Simple Voice Chat compatible client, written from the
//! wire behaviour observed in packet captures (compat version 20), without
//! translating any of the original mod's code.
//!
//! The crate does no audio and knows nothing about Horizon. The host (the
//! native mod) is responsible for:
//!
//! * announcing [`tcp::CLIENT_CHANNELS`] to the server,
//! * sending [`tcp::request_secret`] on `voicechat:request_secret`,
//! * feeding the payload of `voicechat:secret` to [`tcp::Secret::parse`],
//! * creating a [`session::UdpClient`] and calling `poll` every tick,
//! * microphone capture, Opus coding and playback.

pub mod session;
pub mod tcp;
pub mod udp;
pub mod varint;

use std::fmt;

/// A UUID as it travels on the wire: 16 bytes, big-endian, no dashes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Uuid(pub [u8; 16]);

impl Uuid {
    /// Parses 32 hex digits, with or without dashes.
    pub fn from_hex(s: &str) -> Option<Uuid> {
        let digits: Vec<u8> = s.bytes().filter(|b| *b != b'-').collect();
        if digits.len() != 32 {
            return None;
        }
        let mut out = [0u8; 16];
        for (i, pair) in digits.chunks(2).enumerate() {
            let hi = (pair[0] as char).to_digit(16)?;
            let lo = (pair[1] as char).to_digit(16)?;
            out[i] = (hi * 16 + lo) as u8;
        }
        Some(Uuid(out))
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = &self.0;
        write!(
            f,
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
        )
    }
}

impl fmt::Debug for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Uuid({})", self)
    }
}

/// Everything that can go wrong decoding or decrypting a packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The buffer ended before the packet did.
    Truncated,
    /// A UDP datagram did not start with 0xFF.
    BadMagic(u8),
    /// The length written in a datagram does not match its size.
    LengthMismatch,
    /// Authentication failed: wrong secret or a corrupted datagram.
    Decrypt,
    /// A string field was not valid UTF-8.
    InvalidUtf8,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated => write!(f, "packet is truncated"),
            Error::BadMagic(b) => write!(f, "bad magic byte 0x{b:02x}"),
            Error::LengthMismatch => write!(f, "datagram length mismatch"),
            Error::Decrypt => write!(f, "decryption failed"),
            Error::InvalidUtf8 => write!(f, "invalid UTF-8 in string field"),
        }
    }
}

impl std::error::Error for Error {}
