# Horizon Voice Chat

A clean-room client for the [Simple Voice Chat](https://github.com/henkelmax/simple-voice-chat)
protocol, written from scratch in Rust as a native trait mod for
[Horizon](https://github.com/horizon-rust/horizon). It contains none of Simple Voice Chat's code.

- `horizon-voicechat/`: the mod (the `.hnmod` bundle), its README and packaging scripts.
- `horizon-voicechat-net/`: the protocol core and its tests on captured packets.

Hold **V** to talk, **M** mutes, **N** deafens locally, **H** picks a whisper target and **G** opens
the menu (groups, players and a **Settings** page for the microphone and its gain).

Not affiliated with or endorsed by Max Henkel or the Simple Voice Chat project. MIT licensed.

Status: it has **not** been tested against a real Simple Voice Chat server. Group join/leave,
whispers and deafen over the network are disabled until their payloads are captured; the menu says
so. See [horizon-voicechat/README.md](horizon-voicechat/README.md) for details.
