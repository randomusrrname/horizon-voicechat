# Simple Voice Chat for Horizon (work in progress)

Voice chat for Horizon on servers that run the Simple Voice Chat plugin or mod (compat version 20).
It is a clean-room implementation of the network protocol, written from captured traffic; none of
Simple Voice Chat's code is used. See `../horizon-voicechat-net` for the protocol notes.

## Status: written, never run in Horizon

Nothing here has been built against the real Horizon. What was checked:

- The network core (`api/src/net`, copied from `horizon-voicechat-net`): 30 tests on real captured
  packets pass (with a stand-in for `ring`).
- The mod glue and audio code compile and their tests pass against stand-ins for `horizon-client`,
  `cpal` and `rusty-opus` written from the repo's tests, `docs/native-mods.md` and docs.rs. That
  catches mistakes of my own, not differences with the real APIs.

Likely first problems when you build it, in the order I would check them:

1. Names and signatures of the hook API in `api/src/lib.rs`, guessed from `crates/horizon-client/tests/mod_api_hooks.rs`:
   `ClientPlayerNetworkLoggingInEvent`, `ClientDisconnectingEvent`, `ClientStoppingEvent`,
   `ClientTickPreEvent`, `Host::client_state`, `Host::playing`, `Host::send_packet`, and whether the
   two `&self` hooks take `&dyn Host`.
2. `rusty-opus` constructor argument types (`OpusEncoder::new(rate, channels, Application)`,
   `OpusDecoder::new(rate, channels)`) and the public `bitrate_bps` field.
3. `cpal` 0.16 details in `api/src/audio.rs` (stream configs and sample formats).
4. `CUSTOM_PAYLOAD_ID = 22` in `api/src/util.rs` (serverbound play `CustomPayload` at Horizon
   2ebf00a). `send_payload` checks the decoded packet really is a `CustomPayload`, so a wrong id
   prints an error instead of sending garbage.

## Build and try

```
horizon-launcher native-build <this folder> --instance <instance> --trust-native-code
```

Join a server with Simple Voice Chat (the plugin on Paper, or the mod). Watch the game's standard
error for lines starting with `[voicechat]`: they say when the secret arrives, when the UDP session
connects and why anything failed. Hold `V` to talk, press `M` to mute.

On the server side, the UDP port (24454 by default) must be reachable from the client.

## What it does

- Joins a server: announces the plugin channels and asks for a secret (API 409, `native_send_packet`).
- Hears the server's plugin messages (API 408, `native_client_packet`), connects the UDP session,
  keeps it alive, and tracks the players' voice states.
- Push-to-talk and mute. Encodes the microphone with Opus and sends it.
- Resolves the voice host on a helper thread (`voicechat-dns`), so a slow DNS lookup never stalls a tick (written, not compiled).
- Decodes and mixes other players' voice (proximity and group), with a small jitter buffer and
  concealment of up to 3 lost frames.

## Not done yet

- Positional audio: voice plays the same in both ears at the same volume. Needs the speaker's
  position (the voice packets carry none; it would come from the entity list) and a stereo mix.
- A group menu: `create_group`, joining with a password and `leave_group` are not wired (the
  create payload is decoded and tested; the join payload is still unknown, see the protocol notes).
- Whispers and deafening: the formats are not known yet (no capture has them).
- A status indicator on screen (it only logs), configurable keys and volume.
- Sending the microphone from the game tick means frames leave in small bursts every 50 ms; a worker
  thread that owns the socket would fix that.

## Layout

- `horizon-mixin.toml`: the bundle. `api/`: the trait mod crate.
- `api/src/lib.rs`: hooks and session logic. `api/src/audio.rs`: cpal, Opus, mixing.
  `api/src/util.rs`: small pure helpers. `api/src/net/`: protocol core.
- `tools/sync-net.sh`: copies `../horizon-voicechat-net/src` into `api/src/net` (a mod cannot use a path
  dependency). Edit the core in the crate, then run it.

## Dependencies

A trait mod may only use crates and versions already in Horizon's `Cargo.lock`: `ring` (AES-128-GCM),
`rusty-opus` (listed in `crates/horizon-mod-deps`, added for voice chat in issue #7759) and `cpal`.

## Before sharing it

Horizon's `CONTRIBUTING.md` and `CLAUDE.md` ask contributors to check open work first. Issue #7759
was opened by someone building the same thing as a native mod, so coordinate with them before opening
a PR. A license has to be chosen (Horizon is MIT), and Simple Voice Chat's author should be asked
for the protocol spec and whether he wants credit.
