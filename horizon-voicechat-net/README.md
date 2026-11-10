# horizon-voicechat-net

Network core for a Simple Voice Chat compatible client, meant to sit inside a
Horizon native mod. It handles the secret exchange payloads, the encrypted UDP
channel and microphone packets. It does no audio and has no Horizon dependency.

Written from the behaviour observed in packet captures of a Paper server
(compat version 20, Minecraft protocol 777). No code of the original mod was
translated. Its author, Max Henkel, was asked and said he is fine with an
independent implementation. Keep that conversation.

## Status

Tested with `cargo test`: 30 tests pass, including every packet of two real
captures. Caveat: the sandbox where it was run could not reach crates.io, so
the test run used a local stand-in for `ring` (a real AES-128-GCM, checked against Python's
`cryptography` and the captured packets). Run `cargo test` once with the real
crate to close that gap; the API used is the standard `ring` 0.17 one
(`aead::LessSafeKey`, `rand::SystemRandom`). `ring` replaced `aes-gcm` and
`rand` because Horizon's lock file has it and mods may only use what it has.

`tests/vectors.rs` (capture 4) and `tests/capture5.rs` (capture 5) decode real
packets. All 5265 decrypted datagrams of capture 5 also decode with no unknown
type and re-encode to the original bytes (that sweep is not in the repo, it
needs the raw capture).

## What is implemented

| Part | Where | Verified against capture |
| --- | --- | --- |
| `request_secret` payload | `tcp::request_secret` | yes |
| `secret` payload parsing | `tcp::Secret::parse` | yes (both captures) |
| `minecraft:register` payload | `tcp::register_payload` | yes |
| `state` / `states` | `tcp::PlayerState`, `tcp::parse_states` | yes, flags partly inferred |
| `add_group`, `joined_group` | `tcp::Group`, `tcp::JoinedGroup` | yes, 4 trailing bytes unknown |
| `create_group`, `update_state` (client) | `tcp::create_group`, `tcp::update_state` | yes, byte-identical |
| Datagram framing, AES-128-GCM | `udp::frame_*`, `udp::Cipher` | yes, 5846 datagrams |
| Authenticate, ack, connection check, keep-alive | `udp::Packet` | yes |
| Microphone packets and end marker | `udp::Packet::Mic`, `UdpClient::send_mic_end` | yes, 2090 packets |
| Other players' voice (proximity, group) | `udp::Packet::PlayerSound`, `GroupSound` | yes, 2973 packets |
| Handshake state machine and timers | `session::UdpClient` | fake server in tests, timings from the captures |

## Wire format (summary)

TCP, as custom payloads: the client sends `voicechat:request_secret` with the
compat version (i32). The server answers `voicechat:secret`: secret UUID, UDP
port (i32), player UUID, codec (u8), MTU (i32), distance (f64), keep-alive ms
(i32), groups enabled (bool), voice host (string), allow recording (bool).

Player state (`voicechat:state`, and `states` = i32 count + entries): bool,
bool (second one is true until the voice client has connected), UUID, name
(string), bool + UUID of the group. Groups: `add_group` = UUID, name, bool
has-password, 4 bytes (zero); `joined_group` = bool, group UUID, bool.
Client: `create_group` = name, bool + password, i16 (0); `update_state` = bool.

UDP: `0xFF`, then (client only) the player UUID, then a VarInt length and the
payload. The payload is a 12-byte nonce, the ciphertext and a 16-byte tag
(AES-128-GCM, the secret as key, no AAD). Plaintext starts with a type byte:
1 mic, 2 voice by proximity, 3 voice by group, 5 authenticate, 6 authenticate
ack, 8 keep-alive, 9 connection check, 10 connection check ack.

- Mic (client): `01`, VarInt length, Opus frame, sequence (i64), flags (u8).
  The sequence runs 0, 1, 2... for the whole session, across transmissions. An
  empty frame (length 0) ends a transmission and takes a sequence number.
- Voice (server): `02`, id, id, VarInt length, Opus frame, sequence (i64),
  distance (f32, 48.0). Type `03` is the same without the distance. Both ids
  were always the speaker's UUID. An empty frame ends the transmission. The
  server pads plaintexts with zeros up to a multiple of 256 bytes.
- When a player joins your group the server switches their voice from type 2
  to type 3 without resetting the sequence.

## Not implemented yet

- Whispers. Capture 5 has none: the mic `flags` byte was 0 in all 2090 packets
  and the voice packets carry nothing else. Needs a capture where someone
  whispers (the capture also never muted or deafened anyone).
- `leave_group`, `set_group` (join with password), `remove_group`,
  categories and `remove_state`: channel ids only, no payloads seen.
- Voice from a player who is not heard by proximity or group (e.g. API sources).
- Non-empty `voice_host` (never seen). Other compat versions than 20.
- The second player's own traffic is not in capture 5: Wireshark was only on
  loopback, so the phone's LAN traffic never showed up.

## How the Horizon mod would use it

1. On connecting, announce `tcp::CLIENT_CHANNELS` (needs the custom payload
   hook proposed for `native_hooks.rs`).
2. After joining, send `tcp::request_secret()` on `voicechat:request_secret`.
3. On `voicechat:secret`, call `Secret::parse`, build the target with
   `secret.udp_target(host)`, then `UdpClient::new`.
4. Each tick, `client.poll(Instant::now())`; when `Event::Connected` arrives,
   start sending Opus frames with `send_mic`, and `send_mic_end` when the
   microphone goes quiet. `Event::Received` carries `PlayerSound` / `GroupSound`
   frames to decode and play (an empty frame means the speaker stopped).
5. Microphone (`cpal`), Opus (`audiopus`) and playback live in the mod, not here.

## Licence

Not chosen yet. Horizon itself is MIT; pick the one you want before publishing.
