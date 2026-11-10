//! Checks the crate against real packets captured from a Paper server
//! running Simple Voice Chat (compat version 20, offline mode, no TCP
//! compression). If these pass, the wire formats in this crate match what
//! the original client and server actually exchange.

mod common;

use common::*;
use horizon_voicechat_net::tcp::{self, Secret};
use horizon_voicechat_net::udp::{self, Cipher, Packet};
use horizon_voicechat_net::Uuid;

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn secret() -> Secret {
    Secret::parse(&unhex(SECRET_PAYLOAD)).unwrap()
}

#[test]
fn request_secret_is_the_compat_version_as_big_endian_i32() {
    assert_eq!(tcp::request_secret(), unhex("00000014"));
}

#[test]
fn register_payload_is_nul_separated_without_trailing_nul() {
    let payload = tcp::register_payload();
    assert!(payload.starts_with(b"voicechat:add_group\0voicechat:state\0"));
    assert!(payload.ends_with(b"voicechat:states"));
    assert_eq!(payload.iter().filter(|b| **b == 0).count(), tcp::CLIENT_CHANNELS.len() - 1);
}

#[test]
fn secret_packet_fields() {
    let s = secret();
    assert_eq!(s.secret, Uuid::from_hex("5b4154cf-59d8-6db3-d0ce-064bb1363cc3").unwrap());
    assert_eq!(s.server_port, 24454);
    assert_eq!(s.player, Uuid::from_hex("936a9391-4e25-3635-bbc4-8eae6b883b29").unwrap());
    assert_eq!(s.codec, 0);
    assert_eq!(s.mtu_size, 1275);
    assert_eq!(s.voice_distance, 48.0);
    assert_eq!(s.keep_alive_ms, 1000);
    assert!(s.groups_enabled);
    assert_eq!(s.voice_host, "");
    assert!(s.allow_recording);
    assert_eq!(s.udp_target("127.0.0.1"), "127.0.0.1:24454");
}

#[test]
fn secret_packet_truncated_is_an_error_not_a_panic() {
    let bytes = unhex(SECRET_PAYLOAD);
    for cut in 0..bytes.len() {
        assert!(Secret::parse(&bytes[..cut]).is_err(), "cut at {cut}");
    }
}

/// Decrypts a client-to-server datagram and returns the plaintext.
fn open_up(cipher: &Cipher, hex: &str, expected_plain: &str) -> Packet {
    let datagram = unhex(hex);
    let (player, payload) = udp::unframe_to_server(&datagram).unwrap();
    assert_eq!(player, secret().player, "header carries the server-assigned UUID");
    let plain = cipher.open(payload).unwrap();
    assert_eq!(plain, unhex(expected_plain));
    Packet::decode(&plain).unwrap()
}

/// Decrypts a server-to-client datagram and returns the plaintext.
fn open_down(cipher: &Cipher, hex: &str, expected_plain: &str) -> Packet {
    let datagram = unhex(hex);
    let payload = udp::unframe_from_server(&datagram).unwrap();
    let plain = cipher.open(payload).unwrap();
    assert_eq!(plain, unhex(expected_plain));
    assert_eq!(plain.len(), 256, "the server pads to 256 bytes");
    Packet::decode(&plain).unwrap()
}

#[test]
fn handshake_packets() {
    let s = secret();
    let cipher = Cipher::new(&s.secret);

    let auth = open_up(&cipher, AUTH_DATAGRAM, AUTH_PLAIN);
    assert_eq!(auth, Packet::Authenticate { player: s.player, secret: s.secret });

    assert_eq!(open_down(&cipher, AUTH_ACK_DATAGRAM, AUTH_ACK_PLAIN), Packet::AuthenticateAck);
    assert_eq!(open_up(&cipher, CHECK_DATAGRAM, CHECK_PLAIN), Packet::ConnectionCheck);
    assert_eq!(open_down(&cipher, CHECK_ACK_DATAGRAM, CHECK_ACK_PLAIN), Packet::ConnectionCheckAck);
    assert_eq!(open_up(&cipher, KEEPALIVE_DATAGRAM, KEEPALIVE_PLAIN), Packet::KeepAlive);
}

#[test]
fn our_authenticate_packet_decrypts_to_the_same_plaintext_as_the_original() {
    let s = secret();
    let ours = Packet::Authenticate { player: s.player, secret: s.secret }.encode();
    assert_eq!(ours, unhex(AUTH_PLAIN));
}

#[test]
fn voice_packets_including_two_byte_lengths() {
    let s = secret();
    let cipher = Cipher::new(&s.secret);

    match open_up(&cipher, MIC_SMALL_DATAGRAM, MIC_SMALL_PLAIN) {
        Packet::Mic { opus, sequence, flags } => {
            assert_eq!(opus.len(), 34);
            assert_eq!(sequence, 0);
            assert_eq!(flags, 0);
        }
        other => panic!("expected Mic, got {other:?}"),
    }

    match open_up(&cipher, MIC_BIG_DATAGRAM, MIC_BIG_PLAIN) {
        Packet::Mic { opus, sequence, flags } => {
            assert_eq!(opus.len(), 153, "a length above 127 takes a two-byte VarInt");
            assert_eq!(sequence, 42);
            assert_eq!(flags, 0);
        }
        other => panic!("expected Mic, got {other:?}"),
    }
}

#[test]
fn encoding_a_decoded_voice_packet_gives_back_the_original_plaintext() {
    for plain in [MIC_SMALL_PLAIN, MIC_BIG_PLAIN] {
        let plain = unhex(plain);
        assert_eq!(Packet::decode(&plain).unwrap().encode(), plain);
    }
}
