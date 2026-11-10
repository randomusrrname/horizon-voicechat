//! Small pure helpers, kept free of the game's types so they can be unit-tested on their own.

/// The id of the serverbound play `CustomPayload` packet
/// (`horizon_protocol::ids::play::serverbound::CUSTOM_PAYLOAD` at Horizon 2ebf00a, protocol 777).
/// `lib.rs` checks that decoding with it really gives a `CustomPayload`, so a wrong id fails loudly.
pub const CUSTOM_PAYLOAD_ID: i32 = 22;

/// The body of a `CustomPayload` packet: the channel as a VarInt-prefixed string, then the data.
/// A mod cannot name the packet's type, so `lib.rs` hands this body to the packet enum's `decode`.
pub fn custom_payload_body(channel: &str, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(channel.len() + data.len() + 5);
    let mut n = channel.len() as u32;
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            body.push(byte);
            break;
        }
        body.push(byte | 0x80);
    }
    body.extend_from_slice(channel.as_bytes());
    body.extend_from_slice(data);
    body
}

/// The host part of the address the player connected to (`play.example.com:25565` gives
/// `play.example.com`), ready to be joined with a port again: an IPv6 address keeps its brackets.
pub fn host_of(address: &str) -> String {
    let address = address.trim();
    if let Some(rest) = address.strip_prefix('[') {
        // `[::1]:25565` or `[::1]`
        return match rest.find(']') {
            Some(end) => format!("[{}]", &rest[..end]),
            None => address.to_owned(),
        };
    }
    match address.matches(':').count() {
        0 => address.to_owned(),
        1 => address.split(':').next().unwrap_or(address).to_owned(),
        // A bare IPv6 address: more than one colon and no brackets.
        _ => format!("[{address}]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_body_is_a_string_then_the_data() {
        let body = custom_payload_body("voicechat:request_secret", &[0, 0, 0, 20]);
        assert_eq!(body[0] as usize, "voicechat:request_secret".len());
        assert_eq!(&body[1..25], b"voicechat:request_secret");
        assert_eq!(&body[25..], &[0, 0, 0, 20]);
    }

    #[test]
    fn long_channel_names_get_a_two_byte_length() {
        let channel = "a".repeat(200);
        let body = custom_payload_body(&channel, b"x");
        assert_eq!(&body[..2], &[0xc8, 0x01]);
        assert_eq!(body.len(), 2 + 200 + 1);
    }

    #[test]
    fn hosts_lose_their_port() {
        assert_eq!(host_of("play.example.com:25565"), "play.example.com");
        assert_eq!(host_of("play.example.com"), "play.example.com");
        assert_eq!(host_of("192.168.1.6:25565"), "192.168.1.6");
        assert_eq!(host_of("localhost"), "localhost");
        assert_eq!(host_of(" example.org:1 "), "example.org");
    }

    #[test]
    fn ipv6_hosts_keep_their_brackets() {
        assert_eq!(host_of("[::1]:25565"), "[::1]");
        assert_eq!(host_of("[2001:db8::1]"), "[2001:db8::1]");
        assert_eq!(host_of("::1"), "[::1]");
    }
}
