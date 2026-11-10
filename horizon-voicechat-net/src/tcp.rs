//! The part of the protocol that rides on Minecraft's own connection, as
//! custom payloads on `voicechat:*` channels. This module only encodes and
//! decodes payload bytes; sending and receiving them is the host's job.
//!
//! Layout verified against a capture of a Paper server with compat
//! version 20. Other compat versions may differ: check [`Secret::parse`]'s
//! caller against [`COMPAT_VERSION`].

use super::{varint, Error, Uuid};

/// The compat version this crate was written against.
pub const COMPAT_VERSION: i32 = 20;

/// Channel ids used by the protocol.
pub mod channel {
    pub const REQUEST_SECRET: &str = "voicechat:request_secret";
    pub const SECRET: &str = "voicechat:secret";
    pub const STATE: &str = "voicechat:state";
    pub const STATES: &str = "voicechat:states";
    pub const UPDATE_STATE: &str = "voicechat:update_state";
    pub const ADD_GROUP: &str = "voicechat:add_group";
    pub const CREATE_GROUP: &str = "voicechat:create_group";
    pub const SET_GROUP: &str = "voicechat:set_group";
    pub const LEAVE_GROUP: &str = "voicechat:leave_group";
    pub const JOINED_GROUP: &str = "voicechat:joined_group";
    pub const REMOVE_GROUP: &str = "voicechat:remove_group";
    pub const ADD_CATEGORY: &str = "voicechat:add_category";
    pub const REMOVE_CATEGORY: &str = "voicechat:remove_category";
    pub const REMOVE_STATE: &str = "voicechat:remove_state";
}

/// Channels a client listens on, in the order a Fabric client announced them
/// in `minecraft:register`.
pub const CLIENT_CHANNELS: &[&str] = &[
    channel::ADD_GROUP,
    channel::STATE,
    channel::SECRET,
    channel::JOINED_GROUP,
    channel::REMOVE_CATEGORY,
    channel::REMOVE_GROUP,
    channel::ADD_CATEGORY,
    channel::REMOVE_STATE,
    channel::STATES,
];

/// Payload of `minecraft:register` announcing [`CLIENT_CHANNELS`]:
/// ids separated by NUL, no trailing NUL (as observed).
pub fn register_payload() -> Vec<u8> {
    CLIENT_CHANNELS.join("\0").into_bytes()
}

/// Payload of `voicechat:request_secret`: the compat version, big-endian i32.
pub fn request_secret() -> Vec<u8> {
    COMPAT_VERSION.to_be_bytes().to_vec()
}

/// Payload of `voicechat:secret`, the server's answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Secret {
    /// Key for the UDP packets (AES-128) and proof of identity.
    pub secret: Uuid,
    /// UDP port of the voice server.
    pub server_port: i32,
    /// The player's UUID as the server knows it. In offline mode it is not
    /// the UUID the client sent at login; use this one on UDP.
    pub player: Uuid,
    /// Audio codec selector. Only 0 has been observed.
    pub codec: u8,
    /// Probably the maximum datagram size (1275 observed).
    pub mtu_size: i32,
    /// Distance in blocks at which players can be heard (48.0 observed).
    pub voice_distance: f64,
    /// Keep-alive interval in milliseconds (1000 observed).
    pub keep_alive_ms: i32,
    pub groups_enabled: bool,
    /// Host to send UDP to. Empty (observed) means the Minecraft server's
    /// own host. Non-empty values have not been seen yet.
    pub voice_host: String,
    pub allow_recording: bool,
}

impl Secret {
    /// Parses the payload bytes that follow the channel id. Trailing bytes
    /// are ignored so that newer servers adding fields do not break this.
    pub fn parse(data: &[u8]) -> Result<Secret, Error> {
        let mut r = Reader { buf: data, pos: 0 };
        Ok(Secret {
            secret: r.uuid()?,
            server_port: r.i32()?,
            player: r.uuid()?,
            codec: r.u8()?,
            mtu_size: r.i32()?,
            voice_distance: r.f64()?,
            keep_alive_ms: r.i32()?,
            groups_enabled: r.bool()?,
            voice_host: r.string()?,
            allow_recording: r.bool()?,
        })
    }

    /// `host:port` to send UDP to, given the host the client used for the
    /// Minecraft connection. A `voice_host` that already has a port is used
    /// as is (unverified: not seen in captures yet).
    pub fn udp_target(&self, minecraft_host: &str) -> String {
        if self.voice_host.is_empty() {
            format!("{}:{}", minecraft_host, self.server_port)
        } else if self.voice_host.contains(':') {
            self.voice_host.clone()
        } else {
            format!("{}:{}", self.voice_host, self.server_port)
        }
    }
}

/// One player's voice state: payload of `voicechat:state`, and the entries of
/// `voicechat:states`. Layout seen in capture 5 (two players, one group).
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerState {
    /// First flag. Always false in the capture; the client's own
    /// `update_state` sends a single bool that is probably this one
    /// (inferred, never toggled).
    pub disabled: bool,
    /// Second flag. True while the player's voice client has not finished
    /// connecting over UDP, false afterwards (seen flipping 1.0 s after
    /// the client authenticated).
    pub disconnected: bool,
    pub player: Uuid,
    pub name: String,
    /// The group the player is in, if any.
    pub group: Option<Uuid>,
}

impl PlayerState {
    /// Parses a `voicechat:state` payload.
    pub fn parse(data: &[u8]) -> Result<PlayerState, Error> {
        Reader { buf: data, pos: 0 }.player_state()
    }
}

/// Parses a `voicechat:states` payload: an i32 count and that many
/// [`PlayerState`] entries.
pub fn parse_states(data: &[u8]) -> Result<Vec<PlayerState>, Error> {
    let mut r = Reader { buf: data, pos: 0 };
    let count = r.i32()?;
    let count = usize::try_from(count).map_err(|_| Error::Truncated)?;
    // Each entry takes at least 21 bytes: a hostile count cannot make us
    // reserve more than the payload could hold.
    let mut out = Vec::with_capacity(count.min(data.len() / 21 + 1));
    for _ in 0..count {
        out.push(r.player_state()?);
    }
    Ok(out)
}

/// Payload of `voicechat:add_group` (server to client).
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub id: Uuid,
    pub name: String,
    /// The server announces that a password exists, never the password.
    pub has_password: bool,
    /// Four bytes after `has_password`, all zero in the capture (probably
    /// flags and a type, but that is a guess). Kept raw on purpose.
    pub trailing: [u8; 4],
}

impl Group {
    pub fn parse(data: &[u8]) -> Result<Group, Error> {
        let mut r = Reader { buf: data, pos: 0 };
        let id = r.uuid()?;
        let name = r.string()?;
        let has_password = r.bool()?;
        let t = r.take(4)?;
        Ok(Group { id, name, has_password, trailing: [t[0], t[1], t[2], t[3]] })
    }
}

/// Payload of `voicechat:joined_group` (server to client), the answer to a
/// create or join request.
#[derive(Debug, Clone, PartialEq)]
pub struct JoinedGroup {
    /// True when the request worked (seen: true after creating a group).
    pub in_group: bool,
    pub group: Uuid,
    /// Seen only as false.
    pub wrong_password: bool,
}

impl JoinedGroup {
    pub fn parse(data: &[u8]) -> Result<JoinedGroup, Error> {
        let mut r = Reader { buf: data, pos: 0 };
        Ok(JoinedGroup { in_group: r.bool()?, group: r.uuid()?, wrong_password: r.bool()? })
    }
}

/// Payload of `voicechat:update_state` (client to server): one bool, the
/// `disabled` flag. Only `false` was seen.
pub fn update_state(disabled: bool) -> Vec<u8> {
    vec![disabled as u8]
}

/// Payload of `voicechat:create_group` (client to server): name, a bool
/// followed by the password when there is one, and a 16-bit type that was 0.
/// The no-password form (bool false, no string) was not captured: it is the
/// obvious reading, not an observation.
pub fn create_group(name: &str, password: Option<&str>) -> Vec<u8> {
    let mut out = Vec::new();
    write_string(name, &mut out);
    match password {
        Some(p) => {
            out.push(1);
            write_string(p, &mut out);
        }
        None => out.push(0),
    }
    out.extend_from_slice(&0i16.to_be_bytes());
    out
}

fn write_string(s: &str, out: &mut Vec<u8>) {
    varint::write(s.len() as u32, out);
    out.extend_from_slice(s.as_bytes());
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.pos.checked_add(n).ok_or(Error::Truncated)?;
        let slice = self.buf.get(self.pos..end).ok_or(Error::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    fn bool(&mut self) -> Result<bool, Error> {
        Ok(self.u8()? != 0)
    }

    fn i32(&mut self) -> Result<i32, Error> {
        let b = self.take(4)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn f64(&mut self) -> Result<f64, Error> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(f64::from_be_bytes(a))
    }

    fn uuid(&mut self) -> Result<Uuid, Error> {
        let b = self.take(16)?;
        let mut a = [0u8; 16];
        a.copy_from_slice(b);
        Ok(Uuid(a))
    }

    fn player_state(&mut self) -> Result<PlayerState, Error> {
        let disabled = self.bool()?;
        let disconnected = self.bool()?;
        let player = self.uuid()?;
        let name = self.string()?;
        let group = if self.bool()? { Some(self.uuid()?) } else { None };
        Ok(PlayerState { disabled, disconnected, player, name, group })
    }

    fn string(&mut self) -> Result<String, Error> {
        let rest = self.buf.get(self.pos..).ok_or(Error::Truncated)?;
        let (len, used) = varint::read(rest).ok_or(Error::Truncated)?;
        self.pos += used;
        let bytes = self.take(len as usize)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| Error::InvalidUtf8)
    }
}
