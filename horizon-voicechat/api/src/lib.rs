//! Simple Voice Chat for Horizon: talk with players on Java servers that run the Simple Voice Chat
//! plugin or mod (compat version 20). It is a clean-room implementation of the protocol, written
//! from captures of a real server's traffic; none of Simple Voice Chat's code is used.
//!
//! The mod is a "trait mod" (docs/native-mods.md): the launcher links this crate into the game and
//! calls `hooks()`.
//!
//! How it works, in the order things happen:
//! 1. `client_player_network_logging_in`: the player joined a server. We announce the plugin
//!    channels (`minecraft:register`) and ask for a secret (`voicechat:request_secret`).
//! 2. `client_packet` (API 408) hears the server's plugin messages; the `voicechat:*` ones are
//!    queued. A server without the plugin never answers and nothing else happens.
//! 3. `client_tick_pre`: the queue is handled. `voicechat:secret` gives the secret and the UDP port,
//!    and the UDP session starts (`net::session::UdpClient`). Once connected the audio devices open.
//! 4. While the push-to-talk key is held, microphone frames are Opus-encoded and sent; other
//!    players' voice frames are decoded and played.

mod audio;
// The network core keeps parts the mod does not use yet (groups, the state accessor).
#[allow(dead_code)]
mod net;
mod util;

use std::collections::HashMap;
use std::net::ToSocketAddrs;
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use horizon_client::mod_api::{
    ClientDisconnectingEvent, ClientPacketEvent, ClientPlayerNetworkLoggingInEvent,
    ClientStoppingEvent, ClientTickPreEvent, ClientboundPlayPacket, Hooks, Host, KeyEvent,
    ServerboundPlayPacket,
};
use net::session::{Event, UdpClient};
use net::tcp::{self, channel, PlayerState, Secret};
use net::udp::Packet;

/// Hold to talk (vanilla key names, as `Hooks::key` gets them).
const PUSH_TO_TALK: &str = "key.keyboard.v";
/// Press to mute or unmute the microphone.
const MUTE: &str = "key.keyboard.m";

pub fn hooks() -> Box<dyn Hooks> {
    Box::new(VoiceChat { inner: Mutex::new(Inner::default()) })
}

fn log(message: &str) {
    eprintln!("[voicechat] {message}");
}

/// Sends a plugin message to the server (API 409). A mod cannot name the packet's type, so the
/// packet is decoded from its body with the packet enum's own `decode`.
fn send_payload(host: &mut dyn Host, channel: &str, data: &[u8]) -> Result<(), String> {
    let body = util::custom_payload_body(channel, data);
    let packet = ServerboundPlayPacket::decode(util::CUSTOM_PAYLOAD_ID, &body)
        .map_err(|e| format!("could not build the packet: {e:?}"))?;
    if !matches!(packet, ServerboundPlayPacket::CustomPayload(_)) {
        return Err(format!(
            "packet id {} is not a custom payload in this Horizon version",
            util::CUSTOM_PAYLOAD_ID
        ));
    }
    host.send_packet(packet)
}

struct VoiceChat {
    inner: Mutex<Inner>,
}

impl VoiceChat {
    fn state(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[derive(Default)]
struct Inner {
    /// `voicechat:*` messages heard but not handled yet: channel and data.
    inbox: Vec<(String, Vec<u8>)>,
    /// The host the player connected to, without its port.
    server_host: Option<String>,
    udp: Option<UdpClient>,
    /// A secret whose voice host is still being resolved on a helper thread.
    resolving: Option<Resolving>,
    audio: Option<audio::Audio>,
    mic: Option<audio::Mic>,
    /// Every player's voice state the server told us about, by UUID.
    players: HashMap<[u8; 16], PlayerState>,
    push_to_talk: bool,
    muted: bool,
    /// We are in the middle of a transmission, so the end marker is still owed.
    talking: bool,
    /// The "voice port is closed" notice was already logged for this session.
    refused_logged: bool,
}

/// A DNS lookup running off the game thread, so a slow resolver never stalls a tick.
struct Resolving {
    secret: Secret,
    target: String,
    result: std::sync::mpsc::Receiver<Option<std::net::SocketAddr>>,
}

impl Inner {
    /// Forgets the session: drops the sockets and closes the audio devices.
    fn reset(&mut self) {
        let muted = self.muted;
        *self = Inner::default();
        self.muted = muted;
    }

    fn handle_message(&mut self, channel_name: &str, data: &[u8]) {
        match channel_name {
            channel::SECRET => match Secret::parse(data) {
                Ok(secret) => self.connect(&secret),
                Err(e) => log(&format!("bad secret packet: {e}")),
            },
            channel::STATES => match tcp::parse_states(data) {
                Ok(states) => {
                    for state in states {
                        self.players.insert(state.player.0, state);
                    }
                }
                Err(e) => log(&format!("bad states packet: {e}")),
            },
            channel::STATE => match PlayerState::parse(data) {
                Ok(state) => {
                    self.players.insert(state.player.0, state);
                }
                Err(e) => log(&format!("bad state packet: {e}")),
            },
            // Groups, categories and removals are heard but not used yet: there is no group menu.
            _ => {}
        }
    }

    fn connect(&mut self, secret: &Secret) {
        let Some(host) = self.server_host.clone() else {
            log("got a secret but the server address is unknown");
            return;
        };
        let target = secret.udp_target(&host);
        let (tx, rx) = std::sync::mpsc::channel();
        let lookup = target.clone();
        let spawned = std::thread::Builder::new().name("voicechat-dns".into()).spawn(move || {
            let _ = tx.send(lookup.to_socket_addrs().ok().and_then(|mut a| a.next()));
        });
        match spawned {
            Ok(_) => {
                self.resolving = Some(Resolving { secret: secret.clone(), target, result: rx });
            }
            Err(e) => log(&format!("could not start the DNS lookup: {e}")),
        }
    }

    /// Finishes `connect` once the helper thread has resolved the voice host.
    fn poll_resolving(&mut self) {
        let Some(r) = self.resolving.as_ref() else { return };
        let address = match r.result.try_recv() {
            Ok(address) => address,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
        };
        let Some(r) = self.resolving.take() else { return };
        let Some(address) = address else {
            log(&format!("could not resolve the voice server {}", r.target));
            return;
        };
        match UdpClient::new(address, &r.secret, Instant::now()) {
            Ok(client) => {
                log(&format!("voice chat: connecting to {address}"));
                self.udp = Some(client);
            }
            Err(e) => log(&format!("could not open the voice socket: {e}")),
        }
    }

    fn start_audio(&mut self) {
        if self.audio.is_some() {
            return;
        }
        match audio::Audio::new() {
            Ok(audio) => {
                if !audio.has_input() {
                    log("no microphone: you can listen but not talk");
                }
                self.audio = Some(audio);
            }
            Err(e) => log(&format!("audio unavailable: {e}")),
        }
        match audio::Mic::new() {
            Ok(mic) => self.mic = Some(mic),
            Err(e) => log(&e),
        }
    }

    fn tick(&mut self) {
        let messages = std::mem::take(&mut self.inbox);
        for (channel_name, data) in messages {
            self.handle_message(&channel_name, &data);
        }
        self.poll_resolving();
        self.poll_udp();
        self.transmit();
        if let Some(audio) = &self.audio {
            audio.collect_finished();
        }
    }

    fn poll_udp(&mut self) {
        let Some(udp) = self.udp.as_mut() else { return };
        let events = match udp.poll(Instant::now()) {
            Ok(events) => events,
            // Nothing listens on the voice port (yet): the session keeps authenticating every second.
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                if !self.refused_logged {
                    self.refused_logged = true;
                    log("the voice server refused the connection (is its UDP port open?); retrying");
                }
                return;
            }
            Err(e) => {
                log(&format!("voice socket error, giving up: {e}"));
                self.udp = None;
                return;
            }
        };
        for event in events {
            match event {
                Event::Connected => {
                    log("voice chat: connected");
                    self.start_audio();
                }
                Event::TimedOut => log("voice chat: the server stopped answering"),
                Event::Received(Packet::PlayerSound { first_id, opus, sequence, .. })
                | Event::Received(Packet::GroupSound { first_id, opus, sequence, .. }) => {
                    if let Some(audio) = &self.audio {
                        audio.play(first_id.0, &opus, sequence);
                    }
                }
                Event::Received(_) => {}
            }
        }
    }

    /// Sends the microphone while push-to-talk is held, and the end marker when it is let go.
    fn transmit(&mut self) {
        let (Some(udp), Some(audio)) = (self.udp.as_mut(), self.audio.as_ref()) else { return };
        if !udp.is_connected() || !audio.has_input() {
            return;
        }
        let transmitting = self.push_to_talk && !self.muted;
        if transmitting {
            while let Some(frame) = audio.take_frame() {
                let Some(mic) = self.mic.as_mut() else { break };
                match mic.encode(&frame) {
                    Ok(opus) => {
                        if let Err(e) = udp.send_mic(&opus, 0) {
                            log(&format!("could not send voice: {e}"));
                        }
                        self.talking = true;
                    }
                    Err(e) => log(&e),
                }
            }
        } else {
            audio.clear_capture();
            if self.talking {
                self.talking = false;
                if let Err(e) = udp.send_mic_end() {
                    log(&format!("could not send voice: {e}"));
                }
            }
        }
    }
}

impl Hooks for VoiceChat {
    /// The player joined a server: announce the plugin channels and ask for a secret. Anything
    /// left from an earlier server is dropped first.
    fn client_player_network_logging_in(
        &self,
        host: &mut dyn Host,
        _: &mut ClientPlayerNetworkLoggingInEvent,
    ) {
        let mut state = self.state();
        state.reset();
        state.server_host = host
            .client_state()
            .and_then(|s| s.server_address)
            .map(|address| util::host_of(&address));
        if let Err(e) = send_payload(host, "minecraft:register", &tcp::register_payload()) {
            log(&format!("could not announce the channels: {e}"));
        }
        if let Err(e) = send_payload(host, channel::REQUEST_SECRET, &tcp::request_secret()) {
            log(&format!("could not ask for a secret: {e}"));
        }
    }

    /// Queues the server's `voicechat:*` messages for the next tick.
    fn client_packet(&self, _host: &mut dyn Host, event: &mut ClientPacketEvent<'_>) {
        if let ClientboundPlayPacket::CustomPayload(p) = event.packet {
            if p.channel.starts_with("voicechat:") {
                self.state().inbox.push((p.channel.clone(), p.data.clone()));
            }
        }
    }

    fn client_tick_pre(&self, _host: &mut dyn Host, _: &mut ClientTickPreEvent) {
        self.state().tick();
    }

    fn key(&self, host: &mut dyn Host, event: &mut KeyEvent<'_>) {
        let mut state = self.state();
        match event.key {
            // Letting go always counts, even when a screen opened meanwhile.
            PUSH_TO_TALK => state.push_to_talk = event.pressed && host.playing(),
            MUTE if event.pressed && host.playing() => {
                state.muted = !state.muted;
                log(if state.muted { "microphone muted" } else { "microphone on" });
            }
            _ => {}
        }
    }

    fn client_disconnecting(&self, _host: &dyn Host, _: &mut ClientDisconnectingEvent) {
        self.state().reset();
    }

    fn client_stopping(&self, _host: &dyn Host, _: &mut ClientStoppingEvent) {
        self.state().reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Payloads from capture 5 (a Paper server with Simple Voice Chat).
    const SECRET: &str = "0c741dce19b10e134b7cc93facf5282500005f86936a93914e253635bbc48eae6b883b2900000004fb4048000000000000000003e8010001";
    const STATES: &str = "0000000200001efedefe0a5b374d92051eb799151a8105356875746d000001936a93914e253635bbc48eae6b883b2905356875745f00";

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn a_secret_starts_the_udp_session_and_states_fill_the_player_list() {
        let mut inner = Inner { server_host: Some("127.0.0.1".into()), ..Inner::default() };
        inner.inbox.push((channel::SECRET.into(), unhex(SECRET)));
        inner.inbox.push((channel::STATES.into(), unhex(STATES)));
        inner.tick();
        for _ in 0..200 {
            if inner.udp.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            inner.poll_resolving();
        }
        assert!(inner.udp.is_some(), "the secret should open the voice socket");
        assert_eq!(inner.players.len(), 2);
        assert!(inner.inbox.is_empty());
        let mut names: Vec<_> = inner.players.values().map(|p| p.name.clone()).collect();
        names.sort();
        assert_eq!(names, ["5hut_", "5hutm"]);
    }

    #[test]
    fn a_secret_without_a_known_server_address_is_ignored() {
        let mut inner = Inner::default();
        inner.handle_message(channel::SECRET, &unhex(SECRET));
        assert!(inner.udp.is_none());
    }

    #[test]
    fn garbage_messages_are_logged_not_fatal() {
        let mut inner = Inner { server_host: Some("127.0.0.1".into()), ..Inner::default() };
        inner.handle_message(channel::SECRET, &[1, 2, 3]);
        inner.handle_message(channel::STATES, &[0xff]);
        inner.handle_message("voicechat:unknown_channel", &[]);
        assert!(inner.udp.is_none() && inner.players.is_empty());
    }

    #[test]
    fn reset_drops_the_session_but_keeps_the_mute_choice() {
        let mut inner = Inner { server_host: Some("127.0.0.1".into()), muted: true, ..Inner::default() };
        inner.handle_message(channel::SECRET, &unhex(SECRET));
        for _ in 0..200 {
            if inner.udp.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            inner.poll_resolving();
        }
        assert!(inner.udp.is_some());
        inner.reset();
        assert!(inner.udp.is_none() && inner.server_host.is_none() && inner.muted);
    }
}
