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
use std::time::{Duration, Instant};

use horizon_client::mod_api::{
    ClientDisconnectingEvent, ClientPacketEvent, ClientPlayerNetworkLoggingInEvent,
    ClientStoppingEvent, ClientTickPreEvent, ClientboundPlayPacket, Flow, GuiRenderPostEvent, Hooks,
    Host, KeyEvent, ScreenCharTypedPreEvent, ScreenKeyPressedPreEvent,
    ScreenMouseButtonPressedPreEvent, ScreenRenderPostEvent, ServerboundPlayPacket,
};
use net::session::{Event, UdpClient};
use net::tcp::{self, channel, Group, JoinedGroup, PlayerState, Secret};
use net::udp::Packet;

/// Hold to talk (vanilla key names, as `Hooks::key` gets them).
const PUSH_TO_TALK: &str = "key.keyboard.v";
/// Press to mute or unmute the microphone.
const MUTE: &str = "key.keyboard.m";
/// Press to open or close the group menu.
const GROUPS: &str = "key.keyboard.g";
const ENTER: &str = "key.keyboard.enter";
const BACKSPACE: &str = "key.keyboard.backspace";

/// How many group rows the menu draws before telling the player the rest are hidden.
const MAX_GROUP_ROWS: usize = 10;
/// Longest group name the text field accepts.
const MAX_GROUP_NAME: usize = 32;

/// The mod screen id (API 111) the group menu opens.
const MENU_SCREEN: &str = "voicechat";

/// How long a speaker stays in the indicator after the last voice frame (an end marker normally
/// clears them at once; this is only for a lost one).
const SPEAKING_TIMEOUT: Duration = Duration::from_secs(2);

const COLOR_GREEN: u32 = 0x55FF55;
const COLOR_YELLOW: u32 = 0xFFFF55;
const COLOR_RED: u32 = 0xFF5555;
const COLOR_AQUA: u32 = 0x55FFFF;
const COLOR_GRAY: u32 = 0xAAAAAA;

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
    /// Every group the server announced, by UUID.
    groups: HashMap<[u8; 16], Group>,
    /// The group we are in, as far as the server has told us.
    own_group: Option<[u8; 16]>,
    /// Our own UUID, from the server's secret: how `state` messages name us.
    own_id: Option<[u8; 16]>,
    /// The group name being typed in the menu.
    group_name: String,
    /// The last thing the menu said, e.g. why a join did nothing.
    menu_note: Option<String>,
    /// A group name the menu asked to create, sent on the next tick.
    pending_create: Option<String>,
    /// An unknown group-related payload was already reported in the log.
    unverified_logged: bool,
    /// When each speaker's voice was last heard, for the on-screen indicator.
    speaking: HashMap<[u8; 16], Instant>,
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

/// Where the group menu draws its parts, in GUI pixels.
struct MenuLayout {
    panel: [f32; 4],
    name: [f32; 4],
    create: [f32; 4],
    rows: Vec<[f32; 4]>,
}

/// Whether `point` is inside `rect` (`[x, y, width, height]`).
fn inside(rect: [f32; 4], point: [f32; 2]) -> bool {
    let [x, y, w, h] = rect;
    point[0] >= x && point[0] <= x + w && point[1] >= y && point[1] <= y + h
}

impl Inner {
    /// Forgets the session: drops the sockets and closes the audio devices.
    fn reset(&mut self) {
        let muted = self.muted;
        *self = Inner::default();
        self.muted = muted;
    }

    /// The lines of the on-screen voice indicator, with their text colour. Empty when the
    /// server has no voice chat (or before it answers).
    fn indicator_lines(&self) -> Vec<(String, u32)> {
        if self.udp.is_none() && self.resolving.is_none() {
            return Vec::new();
        }
        let mut lines = Vec::new();
        match &self.udp {
            Some(udp) if udp.is_connected() => {
                lines.push(("Voice chat: connected".to_owned(), COLOR_GREEN));
            }
            _ => lines.push(("Voice chat: connecting".to_owned(), COLOR_YELLOW)),
        }
        if let Some(group) = self.own_group.and_then(|id| self.groups.get(&id)) {
            lines.push((format!("Group: {}", group.name), COLOR_AQUA));
        }
        if self.muted {
            lines.push(("Microphone: muted (M)".to_owned(), COLOR_RED));
        } else if self.push_to_talk {
            lines.push(("Microphone: talking (V)".to_owned(), COLOR_AQUA));
        } else {
            lines.push(("Hold V to talk, M to mute, G for groups".to_owned(), COLOR_GRAY));
        }
        let mut names: Vec<&str> = self
            .speaking
            .keys()
            .filter_map(|id| self.players.get(id))
            .map(|p| p.name.as_str())
            .collect();
        names.sort_unstable();
        if !names.is_empty() {
            lines.push((format!("Speaking: {}", names.join(", ")), COLOR_GREEN));
        }
        lines
    }

    /// Drops speakers whose end marker never arrived.
    fn prune_speaking(&mut self, now: Instant) {
        self.speaking.retain(|_, heard| now.duration_since(*heard) < SPEAKING_TIMEOUT);
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
                        self.track_state(state);
                    }
                }
                Err(e) => log(&format!("bad states packet: {e}")),
            },
            channel::STATE => match PlayerState::parse(data) {
                Ok(state) => self.track_state(state),
                Err(e) => log(&format!("bad state packet: {e}")),
            },
            channel::ADD_GROUP => match Group::parse(data) {
                Ok(group) => {
                    self.groups.insert(group.id.0, group);
                }
                Err(e) => log(&format!("bad add_group packet: {e}")),
            },
            channel::JOINED_GROUP => match JoinedGroup::parse(data) {
                Ok(joined) => self.joined(joined),
                Err(e) => log(&format!("bad joined_group packet: {e}")),
            },
            // Removal and category payloads have never been captured, so their layout is only a
            // guess: say so once and leave the group list alone.
            channel::REMOVE_GROUP
            | channel::ADD_CATEGORY
            | channel::REMOVE_CATEGORY
            | channel::REMOVE_STATE => {
                if !self.unverified_logged {
                    self.unverified_logged = true;
                    log(&format!(
                        "ignoring {channel_name}: its layout has not been captured (groups may look stale)"
                    ));
                }
            }
            _ => {}
        }
    }

    fn track_state(&mut self, state: PlayerState) {
        if Some(state.player.0) == self.own_id {
            self.own_group = state.group.map(|group| group.0);
        }
        self.players.insert(state.player.0, state);
    }

    /// The server answered a create or join request.
    fn joined(&mut self, joined: JoinedGroup) {
        if joined.in_group {
            self.own_group = Some(joined.group.0);
            self.menu_note = None;
            let name = self
                .groups
                .get(&joined.group.0)
                .map(|group| group.name.clone())
                .unwrap_or_else(|| "the server's group".to_owned());
            log(&format!("joined the voice group {name}"));
        } else if joined.wrong_password {
            self.menu_note = Some("The group's password was wrong.".to_owned());
        } else {
            self.menu_note = Some("The server refused the group.".to_owned());
        }
    }

    /// The groups the menu lists, by name.
    fn menu_groups(&self) -> Vec<&Group> {
        let mut groups: Vec<&Group> = self.groups.values().collect();
        groups.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        groups
    }

    /// Where the group menu draws each part for a window of `width` x `height` GUI pixels. The
    /// drawing and the click handling both go through this, so they cannot drift apart.
    fn menu_layout(&self, width: f32, height: f32) -> MenuLayout {
        const W: f32 = 260.0;
        const PAD: f32 = 8.0;
        const LINE: f32 = 12.0;
        const ROW: f32 = 13.0;
        const FIELD: f32 = 14.0;
        const GAP: f32 = 8.0;
        let rows = self.menu_groups().len().min(MAX_GROUP_ROWS);
        let h = PAD * 2.0
            + LINE * 2.0
            + rows as f32 * ROW
            + 4.0
            + FIELD
            + GAP
            + FIELD
            + GAP
            + LINE * 2.0;
        let x = ((width - W) * 0.5).max(4.0);
        let y = ((height - h) * 0.5).max(4.0);
        let inner_x = x + PAD;
        let inner_w = W - PAD * 2.0;
        let mut cursor = y + PAD + LINE * 2.0;
        let mut row_rects = Vec::with_capacity(rows);
        for _ in 0..rows {
            row_rects.push([inner_x, cursor, inner_w, ROW]);
            cursor += ROW;
        }
        cursor += 4.0;
        let name = [inner_x, cursor, inner_w, FIELD];
        cursor += FIELD + GAP;
        let create = [inner_x, cursor, inner_w, FIELD];
        MenuLayout { panel: [x, y, W, h], name, create, rows: row_rects }
    }

    /// Queues the group name for `create_group`, sent on the next tick where the host is mutable.
    fn menu_create(&mut self) {
        let name = self.group_name.trim().to_owned();
        if name.is_empty() {
            self.menu_note = Some("Type a name for the new group.".to_owned());
            return;
        }
        self.group_name.clear();
        self.pending_create = Some(name);
    }

    fn menu_type(&mut self, c: char) {
        if c.is_control() || self.group_name.chars().count() >= MAX_GROUP_NAME {
            return;
        }
        self.group_name.push(c);
    }

    fn menu_backspace(&mut self) {
        self.group_name.pop();
    }

    /// A left click on the group menu: rows can only explain why they do nothing yet.
    fn menu_click(&mut self, layout: &MenuLayout, mouse: [f32; 2]) {
        if inside(layout.create, mouse) {
            self.menu_create();
            return;
        }
        if inside(layout.name, mouse) {
            return;
        }
        let groups = self.menu_groups();
        for (i, row) in layout.rows.iter().enumerate() {
            if !inside(*row, mouse) {
                continue;
            }
            let Some(group) = groups.get(i) else { return };
            self.menu_note = Some(if Some(group.id.0) == self.own_group {
                "Leaving needs the leave_group packet, which has not been captured yet.".to_owned()
            } else {
                "Joining needs the set_group packet, which has not been captured yet.".to_owned()
            });
            return;
        }
    }

    /// Draws the group menu. `mouse` is the pointer in GUI pixels, for the hover highlight.
    fn draw_menu(
        &self,
        host: &dyn Host,
        batch: &mut horizon_client::mod_api::HudBatch,
        width: f32,
        height: f32,
        mouse: [f32; 2],
    ) {
        let layout = self.menu_layout(width, height);
        let [x, y, w, h] = layout.panel;
        host.fill(batch, [x, y, w, h], 0xE0181818);
        host.fill(batch, [x, y, w, 1.0], 0xFF55FFFF);
        host.fill(batch, [x, y + h - 1.0, w, 1.0], 0xFF55FFFF);
        host.text(batch, "Voice chat groups", [x + 8.0, y + 8.0], COLOR_AQUA, true);
        let status = match self.own_group.and_then(|id| self.groups.get(&id)) {
            Some(group) => format!("Your group: {}", group.name),
            None => "You are not in a group".to_owned(),
        };
        host.text(batch, &status, [x + 8.0, y + 20.0], COLOR_GRAY, true);
        let groups = self.menu_groups();
        for (i, row) in layout.rows.iter().enumerate() {
            let Some(group) = groups.get(i) else { break };
            let hovered = inside(*row, mouse);
            if hovered {
                host.fill(batch, *row, 0x40FFFFFF);
            }
            let mut label = group.name.clone();
            if group.has_password {
                label.push_str("  (password)");
            }
            let color = if Some(group.id.0) == self.own_group { COLOR_AQUA } else { COLOR_GRAY };
            host.text(batch, &label, [row[0] + 4.0, row[1] + 2.0], color, true);
        }
        host.fill(batch, layout.name, 0x60000000);
        let typed = format!("New group: {}_", self.group_name);
        host.text(batch, &typed, [layout.name[0] + 4.0, layout.name[1] + 3.0], COLOR_GRAY, true);
        host.fill(batch, layout.create, 0x8055FF55);
        host.text(
            batch,
            "Create group",
            [layout.create[0] + 4.0, layout.create[1] + 3.0],
            COLOR_GREEN,
            true,
        );
        let note_y = layout.create[1] + layout.create[3] + 6.0;
        if let Some(note) = &self.menu_note {
            host.text(batch, note, [x + 8.0, note_y], COLOR_YELLOW, true);
        }
        let hidden = groups.len().saturating_sub(MAX_GROUP_ROWS);
        let footer = if hidden > 0 {
            format!("{hidden} more groups not shown. Enter creates, Esc closes.")
        } else {
            "Enter creates, Esc closes. Join and leave need a capture: disabled.".to_owned()
        };
        host.text(batch, &footer, [x + 8.0, note_y + 12.0], COLOR_GRAY, true);
    }

    fn connect(&mut self, secret: &Secret) {
        let Some(host) = self.server_host.clone() else {
            log("got a secret but the server address is unknown");
            return;
        };
        self.own_id = Some(secret.player.0);
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
        self.prune_speaking(Instant::now());
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
                    if opus.is_empty() {
                        self.speaking.remove(&first_id.0);
                    } else {
                        self.speaking.insert(first_id.0, Instant::now());
                    }
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

    fn client_tick_pre(&self, host: &mut dyn Host, _: &mut ClientTickPreEvent) {
        let mut state = self.state();
        state.tick();
        // The screen hooks only have `&dyn Host`, so a create request waits here for a mutable one.
        if let Some(name) = state.pending_create.take() {
            match send_payload(host, channel::CREATE_GROUP, &tcp::create_group(&name, None)) {
                Ok(()) => {
                    state.menu_note = Some(format!("Asking the server for \"{name}\"..."));
                    log(&format!("creating the voice group {name}"));
                }
                Err(e) => {
                    state.menu_note = Some(format!("Could not create the group: {e}"));
                    log(&format!("could not create the group: {e}"));
                }
            }
        }
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
            // Only from the world: with our screen open the key belongs to its text field, and
            // Escape (or the gamepad) closes that.
            GROUPS if event.pressed && host.playing() && host.mod_screen().is_none() => {
                if host.mod_screen_open(MENU_SCREEN) {
                    state.menu_note = None;
                }
            }
            _ => {}
        }
    }

    /// Draws the group menu over the mod screen, and takes its clicks and keys (API 78 to 111).
    fn screen_render_post(&self, host: &dyn Host, event: &mut ScreenRenderPostEvent<'_>) {
        if event.screen.id != "mod" || host.mod_screen() != Some(MENU_SCREEN) {
            return;
        }
        let state = self.state();
        state.draw_menu(
            host,
            event.batch,
            event.screen.rect[2],
            event.screen.rect[3],
            event.screen.mouse,
        );
    }

    fn screen_mouse_button_pressed_pre(
        &self,
        host: &dyn Host,
        event: &mut ScreenMouseButtonPressedPreEvent<'_>,
    ) -> Flow {
        if event.screen.id != "mod" || host.mod_screen() != Some(MENU_SCREEN) {
            return Flow::Continue;
        }
        if event.button == 0 {
            let mut state = self.state();
            let layout = state.menu_layout(event.screen.rect[2], event.screen.rect[3]);
            state.menu_click(&layout, event.screen.mouse);
        }
        // Nothing under the panel should react: the menu is modal.
        Flow::Cancel
    }

    fn screen_key_pressed_pre(
        &self,
        host: &dyn Host,
        event: &mut ScreenKeyPressedPreEvent<'_>,
    ) -> Flow {
        if event.screen.id != "mod" || host.mod_screen() != Some(MENU_SCREEN) {
            return Flow::Continue;
        }
        match event.key {
            // Escape and the gamepad close the screen; the game does that itself.
            "key.keyboard.escape" => Flow::Continue,
            ENTER if !event.repeat => {
                self.state().menu_create();
                Flow::Cancel
            }
            BACKSPACE => {
                self.state().menu_backspace();
                Flow::Cancel
            }
            _ => Flow::Continue,
        }
    }

    fn screen_char_typed_pre(
        &self,
        host: &dyn Host,
        event: &mut ScreenCharTypedPreEvent<'_>,
    ) -> Flow {
        if event.screen.id != "mod" || host.mod_screen() != Some(MENU_SCREEN) {
            return Flow::Continue;
        }
        self.state().menu_type(event.c);
        Flow::Cancel
    }

    /// A small panel in the lower left with the voice connection, the microphone and who is
    /// speaking. `gui_render_post` keeps it above screens and menus (API 410); it follows F1.
    fn gui_render_post(&self, host: &dyn Host, event: &mut GuiRenderPostEvent<'_>) {
        if event.gui_hidden || !host.playing() {
            return;
        }
        let state = self.state();
        let lines = state.indicator_lines();
        if lines.is_empty() {
            return;
        }
        const PAD: f32 = 4.0;
        const LINE: f32 = 10.0;
        let width =
            lines.iter().map(|(text, _)| host.text_width(text)).fold(0.0_f32, f32::max) + PAD * 2.0;
        let height = lines.len() as f32 * LINE + PAD * 2.0;
        let x = 6.0;
        let y = (event.batch.height - height - 6.0).max(4.0);
        host.fill(event.batch, [x, y, width, height], 0x80000000);
        for (i, (text, color)) in lines.iter().enumerate() {
            host.text(event.batch, text, [x + PAD, y + PAD + i as f32 * LINE], *color, true);
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

    fn fake_secret() -> Secret {
        Secret {
            secret: net::Uuid([0x11; 16]),
            server_port: 24454,
            player: net::Uuid([0x22; 16]),
            codec: 0,
            mtu_size: 1275,
            voice_distance: 48.0,
            keep_alive_ms: 1000,
            groups_enabled: true,
            voice_host: String::new(),
            allow_recording: true,
        }
    }

    #[test]
    fn the_indicator_shows_the_microphone_and_who_is_speaking() {
        let (_tx, rx) = std::sync::mpsc::channel();
        let mut inner = Inner {
            resolving: Some(Resolving {
                secret: fake_secret(),
                target: "127.0.0.1:24454".into(),
                result: rx,
            }),
            muted: true,
            ..Inner::default()
        };
        let ana = net::Uuid([1; 16]);
        inner.players.insert(
            ana.0,
            PlayerState {
                disabled: false,
                disconnected: false,
                player: ana,
                name: "Ana".into(),
                group: None,
            },
        );
        inner.speaking.insert(ana.0, Instant::now());
        let lines: Vec<String> = inner.indicator_lines().into_iter().map(|(t, _)| t).collect();
        assert_eq!(lines[0], "Voice chat: connecting");
        assert!(lines.contains(&"Microphone: muted (M)".to_owned()));
        assert!(lines.iter().any(|line| line == "Speaking: Ana"));
    }

    #[test]
    fn a_lost_end_marker_stops_showing_a_speaker() {
        let mut inner = Inner::default();
        inner.speaking.insert([1; 16], Instant::now() - Duration::from_secs(5));
        inner.prune_speaking(Instant::now());
        assert!(inner.speaking.is_empty());
    }

    // Group payloads from capture 5 (a Paper server with Simple Voice Chat).
    const ADD_GROUP: &str = "40180f619bd44f4c86a42e4b7ff394a9033131310100000000";
    const JOINED_GROUP: &str = "0140180f619bd44f4c86a42e4b7ff394a900";
    const STATE_IN_GROUP: &str =
        "0000936a93914e253635bbc48eae6b883b2905356875745f0140180f619bd44f4c86a42e4b7ff394a9";

    fn me() -> [u8; 16] {
        let bytes = unhex("936a93914e253635bbc48eae6b883b29");
        let mut id = [0u8; 16];
        id.copy_from_slice(&bytes);
        id
    }

    fn group_id() -> [u8; 16] {
        let bytes = unhex("40180f619bd44f4c86a42e4b7ff394a9");
        let mut id = [0u8; 16];
        id.copy_from_slice(&bytes);
        id
    }

    #[test]
    fn the_group_list_and_our_group_come_from_the_server() {
        let mut inner = Inner { own_id: Some(me()), ..Inner::default() };
        inner.handle_message(channel::ADD_GROUP, &unhex(ADD_GROUP));
        assert_eq!(inner.groups.len(), 1);
        assert_eq!(inner.menu_groups()[0].name, "111");
        assert!(inner.menu_groups()[0].has_password);

        inner.handle_message(channel::STATE, &unhex(STATE_IN_GROUP));
        assert_eq!(inner.own_group, Some(group_id()));

        let lines: Vec<String> = inner.indicator_lines().into_iter().map(|(t, _)| t).collect();
        assert!(lines.is_empty(), "no UDP session yet: the indicator stays hidden");

        inner.handle_message(channel::JOINED_GROUP, &unhex(JOINED_GROUP));
        assert_eq!(inner.own_group, Some(group_id()));
    }

    #[test]
    fn a_state_for_another_player_does_not_change_our_group() {
        let mut inner = Inner { own_id: Some(me()), ..Inner::default() };
        inner.own_group = None;
        inner.handle_message(channel::STATE, &unhex(STATE_IN_GROUP));
        assert_eq!(inner.own_group, Some(group_id()));
        inner.own_id = Some([0xFF; 16]);
        inner.own_group = None;
        inner.handle_message(channel::STATE, &unhex(STATE_IN_GROUP));
        assert_eq!(inner.own_group, None, "another player's group is not ours");
    }

    #[test]
    fn an_uncaptured_removal_leaves_the_list_alone() {
        let mut inner = Inner::default();
        inner.handle_message(channel::ADD_GROUP, &unhex(ADD_GROUP));
        inner.handle_message(channel::REMOVE_GROUP, &[0xff, 0xff]);
        inner.handle_message(channel::REMOVE_GROUP, &[0xff, 0xff]);
        assert_eq!(inner.groups.len(), 1);
        assert!(inner.unverified_logged);
    }

    #[test]
    fn the_menu_puts_every_part_inside_its_panel() {
        let mut inner = Inner::default();
        inner.handle_message(channel::ADD_GROUP, &unhex(ADD_GROUP));
        let layout = inner.menu_layout(1280.0, 720.0);
        let [x, y, w, h] = layout.panel;
        for rect in std::iter::once(layout.name)
            .chain(std::iter::once(layout.create))
            .chain(layout.rows.iter().copied())
        {
            assert!(rect[0] >= x && rect[1] >= y, "part starts before the panel");
            assert!(rect[0] + rect[2] <= x + w, "part is wider than the panel");
            assert!(rect[1] + rect[3] <= y + h, "part is below the panel");
        }
    }

    #[test]
    fn the_menu_caps_the_rows_it_draws() {
        let mut inner = Inner::default();
        for i in 0..MAX_GROUP_ROWS + 3 {
            let mut payload = unhex(ADD_GROUP);
            payload[0] = i as u8;
            inner.handle_message(channel::ADD_GROUP, &payload);
        }
        assert_eq!(inner.groups.len(), MAX_GROUP_ROWS + 3);
        assert_eq!(inner.menu_layout(320.0, 240.0).rows.len(), MAX_GROUP_ROWS);
    }

    #[test]
    fn typing_a_name_queues_a_create_and_clears_the_field() {
        let mut inner = Inner::default();
        for c in "Ray".chars() {
            inner.menu_type(c);
        }
        inner.menu_backspace();
        inner.menu_type('y');
        assert_eq!(inner.group_name, "Ray");
        inner.menu_create();
        assert_eq!(inner.pending_create.as_deref(), Some("Ray"));
        assert!(inner.group_name.is_empty());

        let sent = inner.pending_create.take(); // what the tick does
        assert_eq!(sent.as_deref(), Some("Ray"));
        inner.menu_create();
        assert!(inner.pending_create.is_none(), "an empty name sends nothing");
        assert!(inner.menu_note.is_some());

        for _ in 0..MAX_GROUP_NAME + 5 {
            inner.menu_type('x');
        }
        assert_eq!(inner.group_name.chars().count(), MAX_GROUP_NAME);
    }

    #[test]
    fn clicking_a_group_explains_that_joins_are_disabled() {
        let mut inner = Inner::default();
        inner.handle_message(channel::ADD_GROUP, &unhex(ADD_GROUP));
        let layout = inner.menu_layout(320.0, 240.0);
        let row = layout.rows[0];
        inner.menu_click(&layout, [row[0] + 2.0, row[1] + 2.0]);
        let note = inner.menu_note.clone().unwrap();
        assert!(note.contains("set_group"), "got {note}");
        assert!(inner.pending_create.is_none());

        let create = layout.create;
        inner.group_name = "Ray".into();
        inner.menu_click(&layout, [create[0] + 2.0, create[1] + 2.0]);
        assert_eq!(inner.pending_create.as_deref(), Some("Ray"));
    }

    #[test]
    fn a_refused_join_is_reported_in_the_menu() {
        let mut inner = Inner::default();
        let mut payload = vec![0x00];
        payload.extend_from_slice(&group_id());
        payload.push(0x00);
        inner.handle_message(channel::JOINED_GROUP, &payload);
        assert!(inner.own_group.is_none());
        assert!(inner.menu_note.is_some());
    }
}
