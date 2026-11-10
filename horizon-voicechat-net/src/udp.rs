//! The UDP side: datagram framing, encryption and packet bodies.
//!
//! Datagrams:
//!
//! * client to server: `0xFF`, player UUID (16 bytes), VarInt length, payload
//! * server to client: `0xFF`, VarInt length, payload
//!
//! The payload is AES-128-GCM with the secret as key: a 12-byte nonce, then
//! the ciphertext, then the 16-byte tag, no associated data. The plaintext
//! starts with a type byte (see [`kind`]). The server pads its plaintexts
//! with zeros up to a multiple of 256 bytes (256 for everything seen except
//! one 269-byte voice packet that became 512); the client does not pad.

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_128_GCM};
use ring::rand::{SecureRandom, SystemRandom};

use super::{varint, Error, Uuid};

/// First byte of every datagram.
pub const MAGIC: u8 = 0xFF;
/// Length of the nonce that prefixes every payload.
pub const NONCE_LEN: usize = 12;

/// Packet type bytes seen in captures.
pub mod kind {
    /// Microphone audio, client to server.
    pub const MIC: u8 = 1;
    /// Another player's voice, server to client, heard by proximity.
    pub const PLAYER_SOUND: u8 = 2;
    /// Another player's voice, server to client, heard because you share a group.
    pub const GROUP_SOUND: u8 = 3;
    /// First packet: proves the client knows the secret.
    pub const AUTHENTICATE: u8 = 5;
    /// Server's reply to the authentication.
    pub const AUTHENTICATE_ACK: u8 = 6;
    pub const KEEP_ALIVE: u8 = 8;
    /// Sent once by the client, about one keep-alive interval after the
    /// authentication was acknowledged. (The name is inferred.)
    pub const CONNECTION_CHECK: u8 = 9;
    /// Server's reply to the connection check. (The name is inferred.)
    pub const CONNECTION_CHECK_ACK: u8 = 10;
}

/// A decrypted packet.
#[derive(Debug, Clone, PartialEq)]
pub enum Packet {
    Authenticate { player: Uuid, secret: Uuid },
    AuthenticateAck,
    ConnectionCheck,
    ConnectionCheckAck,
    KeepAlive,
    /// A microphone frame. `sequence` counted 0, 1, 2... without gaps for the
    /// whole session (it is not reset between transmissions). An **empty**
    /// `opus` marks the end of a transmission and also takes a sequence
    /// number. `flags` was 0 in every packet seen (2090 of them, none while
    /// whispering), so its meaning is unknown.
    Mic { opus: Vec<u8>, sequence: i64, flags: u8 },
    /// Someone else's voice, heard by proximity (server to client).
    ///
    /// `first_id` and `second_id` were both equal to the speaker's UUID in
    /// every packet seen (2515), so which one is which is not known; use
    /// `first_id` to pick the speaker. `sequence` is per speaker and keeps
    /// counting when the speaker switches between this and [`Packet::GroupSound`].
    /// An empty `opus` ends the speaker's transmission. `distance` was always
    /// 48.0, the `voice_distance` of the secret packet.
    PlayerSound { first_id: Uuid, second_id: Uuid, opus: Vec<u8>, sequence: i64, distance: f32 },
    /// Someone else's voice, heard because you share a group (server to
    /// client). Same as [`Packet::PlayerSound`] without the distance.
    GroupSound { first_id: Uuid, second_id: Uuid, opus: Vec<u8>, sequence: i64 },
    /// A type this crate does not know yet (for example the packets that
    /// carry other players' voice from the server).
    Unknown { kind: u8, body: Vec<u8> },
}

impl Packet {
    /// The plaintext to encrypt. For the types only a server sends, this is
    /// just the type byte (the real server pads it).
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Packet::Authenticate { player, secret } => {
                let mut out = Vec::with_capacity(33);
                out.push(kind::AUTHENTICATE);
                out.extend_from_slice(&player.0);
                out.extend_from_slice(&secret.0);
                out
            }
            Packet::AuthenticateAck => vec![kind::AUTHENTICATE_ACK],
            Packet::ConnectionCheck => vec![kind::CONNECTION_CHECK],
            Packet::ConnectionCheckAck => vec![kind::CONNECTION_CHECK_ACK],
            Packet::KeepAlive => vec![kind::KEEP_ALIVE],
            Packet::Mic { opus, sequence, flags } => {
                let mut out = Vec::with_capacity(opus.len() + 16);
                out.push(kind::MIC);
                varint::write(opus.len() as u32, &mut out);
                out.extend_from_slice(opus);
                out.extend_from_slice(&sequence.to_be_bytes());
                out.push(*flags);
                out
            }
            Packet::PlayerSound { first_id, second_id, opus, sequence, distance } => {
                let mut out = sound_head(kind::PLAYER_SOUND, first_id, second_id, opus, *sequence);
                out.extend_from_slice(&distance.to_be_bytes());
                out
            }
            Packet::GroupSound { first_id, second_id, opus, sequence } => {
                sound_head(kind::GROUP_SOUND, first_id, second_id, opus, *sequence)
            }
            Packet::Unknown { kind, body } => {
                let mut out = Vec::with_capacity(body.len() + 1);
                out.push(*kind);
                out.extend_from_slice(body);
                out
            }
        }
    }

    /// Decodes a plaintext. Trailing padding is ignored.
    pub fn decode(plain: &[u8]) -> Result<Packet, Error> {
        let (&first, body) = plain.split_first().ok_or(Error::Truncated)?;
        match first {
            kind::AUTHENTICATE => {
                let ids = body.get(..32).ok_or(Error::Truncated)?;
                let mut player = [0u8; 16];
                let mut secret = [0u8; 16];
                player.copy_from_slice(&ids[..16]);
                secret.copy_from_slice(&ids[16..]);
                Ok(Packet::Authenticate { player: Uuid(player), secret: Uuid(secret) })
            }
            kind::AUTHENTICATE_ACK => Ok(Packet::AuthenticateAck),
            kind::CONNECTION_CHECK => Ok(Packet::ConnectionCheck),
            kind::CONNECTION_CHECK_ACK => Ok(Packet::ConnectionCheckAck),
            kind::KEEP_ALIVE => Ok(Packet::KeepAlive),
            kind::MIC => {
                let (len, used) = varint::read(body).ok_or(Error::Truncated)?;
                let start = used;
                let end = start.checked_add(len as usize).ok_or(Error::Truncated)?;
                let opus = body.get(start..end).ok_or(Error::Truncated)?;
                let tail = body.get(end..end + 9).ok_or(Error::Truncated)?;
                let mut seq = [0u8; 8];
                seq.copy_from_slice(&tail[..8]);
                Ok(Packet::Mic {
                    opus: opus.to_vec(),
                    sequence: i64::from_be_bytes(seq),
                    flags: tail[8],
                })
            }
            kind::PLAYER_SOUND => {
                let (first_id, second_id, opus, sequence, used) = sound_head_decode(body)?;
                let d = body.get(used..used + 4).ok_or(Error::Truncated)?;
                let distance = f32::from_be_bytes([d[0], d[1], d[2], d[3]]);
                Ok(Packet::PlayerSound { first_id, second_id, opus, sequence, distance })
            }
            kind::GROUP_SOUND => {
                let (first_id, second_id, opus, sequence, _) = sound_head_decode(body)?;
                Ok(Packet::GroupSound { first_id, second_id, opus, sequence })
            }
            other => Ok(Packet::Unknown { kind: other, body: body.to_vec() }),
        }
    }
}

/// `type, id, id, VarInt length, opus, sequence`, shared by both sound packets.
fn sound_head(kind: u8, first: &Uuid, second: &Uuid, opus: &[u8], sequence: i64) -> Vec<u8> {
    let mut out = Vec::with_capacity(opus.len() + 48);
    out.push(kind);
    out.extend_from_slice(&first.0);
    out.extend_from_slice(&second.0);
    varint::write(opus.len() as u32, &mut out);
    out.extend_from_slice(opus);
    out.extend_from_slice(&sequence.to_be_bytes());
    out
}

/// Reads what [`sound_head`] wrote from `body` (the plaintext without the
/// type byte). Returns the ids, the opus frame, the sequence and how many
/// bytes of `body` were used.
fn sound_head_decode(body: &[u8]) -> Result<(Uuid, Uuid, Vec<u8>, i64, usize), Error> {
    let ids = body.get(..32).ok_or(Error::Truncated)?;
    let mut first = [0u8; 16];
    let mut second = [0u8; 16];
    first.copy_from_slice(&ids[..16]);
    second.copy_from_slice(&ids[16..]);
    let (len, used) = varint::read(&body[32..]).ok_or(Error::Truncated)?;
    let start = 32 + used;
    let end = start.checked_add(len as usize).ok_or(Error::Truncated)?;
    let opus = body.get(start..end).ok_or(Error::Truncated)?.to_vec();
    let seq = body.get(end..end + 8).ok_or(Error::Truncated)?;
    let mut sb = [0u8; 8];
    sb.copy_from_slice(seq);
    Ok((Uuid(first), Uuid(second), opus, i64::from_be_bytes(sb), end + 8))
}

/// AES-128-GCM keyed with the secret (`ring`, which Horizon's lock file has).
pub struct Cipher {
    key: LessSafeKey,
    rng: SystemRandom,
}

impl Cipher {
    pub fn new(secret: &Uuid) -> Cipher {
        let key = UnboundKey::new(&AES_128_GCM, &secret.0).expect("a UUID is 16 bytes");
        Cipher { key: LessSafeKey::new(key), rng: SystemRandom::new() }
    }

    /// Encrypts with a fresh random nonce. Output: nonce, ciphertext, tag.
    pub fn seal(&self, plain: &[u8]) -> Vec<u8> {
        let mut nonce = [0u8; NONCE_LEN];
        self.rng.fill(&mut nonce).expect("the system random source works");
        let mut body = plain.to_vec();
        self.key
            .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut body)
            .expect("AES-GCM encryption does not fail for in-memory buffers");
        let mut out = Vec::with_capacity(NONCE_LEN + body.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&body);
        out
    }

    /// Decrypts and authenticates a payload made of nonce, ciphertext, tag.
    pub fn open(&self, payload: &[u8]) -> Result<Vec<u8>, Error> {
        if payload.len() < NONCE_LEN {
            return Err(Error::Truncated);
        }
        let (nonce, rest) = payload.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| Error::Truncated)?;
        let mut body = rest.to_vec();
        let plain = self.key.open_in_place(nonce, Aad::empty(), &mut body).map_err(|_| Error::Decrypt)?;
        Ok(plain.to_vec())
    }
}

/// Builds a client-to-server datagram around an encrypted payload.
pub fn frame_to_server(player: &Uuid, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 22);
    out.push(MAGIC);
    out.extend_from_slice(&player.0);
    varint::write(payload.len() as u32, &mut out);
    out.extend_from_slice(payload);
    out
}

/// Builds a server-to-client datagram (used by tests and fake servers).
pub fn frame_from_server(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 6);
    out.push(MAGIC);
    varint::write(payload.len() as u32, &mut out);
    out.extend_from_slice(payload);
    out
}

/// Splits a server-to-client datagram into its encrypted payload.
pub fn unframe_from_server(datagram: &[u8]) -> Result<&[u8], Error> {
    let (&magic, rest) = datagram.split_first().ok_or(Error::Truncated)?;
    if magic != MAGIC {
        return Err(Error::BadMagic(magic));
    }
    take_payload(rest)
}

/// Splits a client-to-server datagram into the player UUID and the payload.
pub fn unframe_to_server(datagram: &[u8]) -> Result<(Uuid, &[u8]), Error> {
    let (&magic, rest) = datagram.split_first().ok_or(Error::Truncated)?;
    if magic != MAGIC {
        return Err(Error::BadMagic(magic));
    }
    let id = rest.get(..16).ok_or(Error::Truncated)?;
    let mut player = [0u8; 16];
    player.copy_from_slice(id);
    Ok((Uuid(player), take_payload(&rest[16..])?))
}

fn take_payload(buf: &[u8]) -> Result<&[u8], Error> {
    let (len, used) = varint::read(buf).ok_or(Error::Truncated)?;
    let payload = &buf[used..];
    if payload.len() != len as usize {
        return Err(Error::LengthMismatch);
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> Uuid {
        Uuid::from_hex("5b4154cf-59d8-6db3-d0ce-064bb1363cc3").unwrap()
    }

    #[test]
    fn seal_open_round_trip() {
        let cipher = Cipher::new(&key());
        let sealed = cipher.seal(b"hello voice");
        assert_eq!(sealed.len(), NONCE_LEN + 11 + 16);
        assert_eq!(cipher.open(&sealed).unwrap(), b"hello voice");
    }

    #[test]
    fn tampering_is_detected() {
        let cipher = Cipher::new(&key());
        let mut sealed = cipher.seal(b"hello voice");
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        assert_eq!(cipher.open(&sealed), Err(Error::Decrypt));
        assert_eq!(cipher.open(&[0u8; 4]), Err(Error::Truncated));
    }

    #[test]
    fn mic_packet_round_trip() {
        let packet = Packet::Mic { opus: vec![7; 153], sequence: 42, flags: 0 };
        assert_eq!(Packet::decode(&packet.encode()).unwrap(), packet);
    }

    #[test]
    fn sound_packets_round_trip() {
        let id = Uuid([5; 16]);
        let p = Packet::PlayerSound { first_id: id, second_id: id, opus: vec![9; 40], sequence: 7, distance: 48.0 };
        assert_eq!(Packet::decode(&p.encode()).unwrap(), p);
        let g = Packet::GroupSound { first_id: id, second_id: id, opus: vec![], sequence: 8 };
        assert_eq!(Packet::decode(&g.encode()).unwrap(), g);
        // truncated bodies are errors, not panics
        let full = p.encode();
        for cut in 1..full.len() {
            assert!(Packet::decode(&full[..cut]).is_err(), "cut at {cut}");
        }
    }

    #[test]
    fn framing_round_trip() {
        let player = Uuid([9; 16]);
        let payload = vec![1, 2, 3, 4];
        let up = frame_to_server(&player, &payload);
        assert_eq!(unframe_to_server(&up).unwrap(), (player, &payload[..]));
        let down = frame_from_server(&payload);
        assert_eq!(unframe_from_server(&down).unwrap(), &payload[..]);
    }

    #[test]
    fn bad_datagrams_are_rejected() {
        assert_eq!(unframe_from_server(&[]), Err(Error::Truncated));
        assert_eq!(unframe_from_server(&[0x00, 0x00]), Err(Error::BadMagic(0)));
        assert_eq!(unframe_from_server(&[0xff, 0x05, 1, 2]), Err(Error::LengthMismatch));
    }
}
