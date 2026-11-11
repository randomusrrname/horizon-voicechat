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
//! 5. `G` opens a menu drawn by the mod itself (API 111): the server's groups (create), the
//!    players (click one to whisper) and a settings page that picks the microphone and its gain.
//!    `N` deafens locally and `H` picks a whisper target, both without sending anything: those
//!    payloads have not been captured.

mod audio;
// The protocol core; categories and removals are kept for when a capture confirms them.
#[allow(dead_code)]
mod net;
mod settings;
mod ui;
mod util;

use std::collections::HashMap;
use std::net::ToSocketAddrs;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use horizon_client::mod_api::{
    ClientDisconnectingEvent, ClientPacketEvent, ClientPlayerNetworkLoggingInEvent,
    ClientStoppingEvent, ClientTickPreEvent, ClientboundPlayPacket, Flow, GuiRenderPostEvent, Hooks,
    Host, KeyEvent, NativeScreenKind, ScreenCharTypedPreEvent, ScreenKeyPressedPreEvent,
    ScreenMouseButtonPressedPreEvent, ScreenRenderPostEvent, ServerboundPlayPacket,
};
use net::session::{Event, UdpClient};
use net::tcp::{self, channel, Group, JoinedGroup, PlayerState, Secret};
use net::udp::Packet;
use settings::Settings;
use ui::{DeviceRow, GroupRow, HudView, MenuTab, MenuView, MicState, PlayerRow, Status};

/// Hold to talk (vanilla key names, as `Hooks::key` gets them).
const PUSH_TO_TALK: &str = "key.keyboard.v";
/// Press to mute or unmute the microphone.
const MUTE: &str = "key.keyboard.m";
/// Press to open or close the group menu.
const GROUPS: &str = "key.keyboard.g";
/// Press to stop or resume hearing other players. Local only: nothing is sent.
const DEAFEN: &str = "key.keyboard.n";
/// Press to pick the next whisper target (or none). No whisper packet is sent yet.
const WHISPER: &str = "key.keyboard.h";
/// How many rows the menu draws before telling the player the rest are hidden.
const MAX_GROUP_ROWS: usize = ui::MAX_ROWS;
/// Longest group name the text field accepts.
const MAX_GROUP_NAME: usize = 32;
const ENTER: &str = "key.keyboard.enter";
const BACKSPACE: &str = "key.keyboard.backspace";
const TAB: &str = "key.keyboard.tab";

/// The bundle id: the mod screen id, the API 111 screen it opens, and the name of the mod's
/// data folder where its settings live. (`voicechat` itself is another mod's registry id.)
const MOD_ID: &str = "voicechat_client";

/// How long a speaker stays in the indicator after the last voice frame (an end marker normally
/// clears them at once; this is only for a lost one).
const SPEAKING_TIMEOUT: Duration = Duration::from_secs(2);

pub fn hooks() -> Box<dyn Hooks> {
    Box::new(VoiceChat { inner: Mutex::new(Inner::default()) })
}

fn log(message: &str) {
    eprintln!("[voicechat] {message}");
}

/// The world is being played, with no screen over it (not even chat): where a bare key is a
/// command rather than typing.
fn in_world(host: &dyn Host) -> bool {
    host.client_state().is_some_and(|state| state.screen == NativeScreenKind::Playing)
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
    /// Which page the menu shows.
    menu_tab: MenuTab,
    /// The last thing the menu said, e.g. why a join did nothing.
    menu_note: Option<String>,
    /// A group name the menu asked to create, sent on the next tick.
    pending_create: Option<String>,
    /// Ticks since the menu opened, for the text caret's blink.
    menu_ticks: u32,
    /// The microphone and gain the player picked, and whether the file was read this session.
    settings: Settings,
    settings_loaded: bool,
    /// The settings changed and must reach the audio thread and the file on the next tick.
    settings_dirty: bool,
    /// The microphones cpal reported when the menu opened, for the settings page.
    input_devices: Vec<String>,
    /// An unknown group-related payload was already reported in the log.
    unverified_logged: bool,
    /// When each speaker's voice was last heard, for the on-screen indicator.
    speaking: HashMap<[u8; 16], Instant>,
    push_to_talk: bool,
    muted: bool,
    /// Peak microphone level of the last frames, 0.0 to 1.0, for the HUD meter.
    mic_level: f32,
    /// Deafened: other players' voice is decoded but not played. Local only, never sent.
    deafened: bool,
    /// The player a whisper is meant for, if any. Only shown: the whisper packet is unknown.
    whisper: Option<[u8; 16]>,
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

/// The peak of one 20 ms microphone frame as a 0.0 to 1.0 bar value. A voip frame peaks around
/// -20 dBFS, so amplify it into something the eye can follow.
fn frame_level(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt();
    (rms * 4.0).clamp(0.0, 1.0)
}

impl Inner {
    /// Forgets the session: drops the sockets and closes the audio devices. The player's
    /// microphone choice, its gain and mute/deafen stay.
    fn reset(&mut self) {
        let (muted, deafened) = (self.muted, self.deafened);
        let (settings, settings_loaded) = (self.settings.clone(), self.settings_loaded);
        *self = Inner::default();
        self.muted = muted;
        self.deafened = deafened;
        self.settings = settings;
        self.settings_loaded = settings_loaded;
    }

    /// Deafens or undeafens. Local: the microphone and the network are untouched.
    fn toggle_deafen(&mut self) -> bool {
        self.deafened = !self.deafened;
        self.deafened
    }

    /// Picks the next player as the whisper target, and nobody after the last one.
    fn cycle_whisper(&mut self) -> Option<String> {
        let mut ids: Vec<[u8; 16]> = self.players.keys().copied().collect();
        // Whispering at yourself is not a thing.
        ids.retain(|id| Some(*id) != self.own_id);
        ids.sort_by(|a, b| {
            let name = |id: &[u8; 16]| {
                self.players.get(id).map(|p| p.name.to_lowercase()).unwrap_or_default()
            };
            name(a).cmp(&name(b))
        });
        if ids.is_empty() {
            self.whisper = None;
            return None;
        }
        self.whisper = match self.whisper {
            Some(current) => match ids.iter().position(|id| *id == current) {
                Some(i) => ids.get(i + 1).copied(),
                None => ids.first().copied(),
            },
            None => ids.first().copied(),
        };
        self.whisper
            .and_then(|id| self.players.get(&id))
            .map(|player| player.name.clone())
    }

    /// The name of the whisper target, if the server still lists them.
    fn whisper_name(&self) -> Option<&str> {
        self.whisper.and_then(|id| self.players.get(&id)).map(|p| p.name.as_str())
    }

    /// The connection state the HUD and the menu show.
    fn status(&self) -> Status {
        match &self.udp {
            Some(udp) if udp.is_connected() => Status::Connected,
            Some(_) => Status::Connecting,
            None if self.resolving.is_some() => Status::Connecting,
            None => Status::Offline,
        }
    }

    /// The microphone state: muted wins over talking, and no input device is its own state.
    fn mic_state(&self) -> MicState {
        if self.muted {
            MicState::Muted
        } else if self.audio.as_ref().is_some_and(|audio| !audio.has_input()) {
            MicState::Absent
        } else if self.push_to_talk {
            MicState::Talking
        } else {
            MicState::Idle
        }
    }

    /// Everything the HUD draws. `None` when the server has no voice chat (or before it answers).
    fn hud_view(&self) -> Option<HudView<'_>> {
        if self.udp.is_none() && self.resolving.is_none() {
            return None;
        }
        let mut speaking: Vec<&str> = self
            .speaking
            .keys()
            .filter(|id| Some(**id) != self.own_id)
            .filter_map(|id| self.players.get(id))
            .map(|p| p.name.as_str())
            .collect();
        speaking.sort_unstable();
        Some(HudView {
            status: self.status(),
            mic: self.mic_state(),
            level: self.mic_level,
            group: self.own_group.and_then(|id| self.groups.get(&id)).map(|g| g.name.as_str()),
            deafened: self.deafened,
            whisper: self.whisper_name(),
            speaking,
        })
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

    /// The players the menu lists, by name, with ourselves first so "you" is easy to find.
    fn menu_players(&self) -> Vec<&PlayerState> {
        let mut players: Vec<&PlayerState> = self.players.values().collect();
        players.sort_by(|a, b| {
            let own = |p: &PlayerState| Some(p.player.0) != self.own_id;
            own(a).cmp(&own(b)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        players
    }

    /// Where the menu draws each part for a window of `width` x `height` GUI pixels. The
    /// drawing and the click handling both go through this, so they cannot drift apart.
    fn menu_layout(&self, width: f32, height: f32) -> ui::MenuLayout {
        // The settings page always has its "System default" row.
        let devices = 1 + self.input_devices.iter().take(ui::MAX_DEVICES - 1).count();
        ui::menu_layout(
            self.menu_tab,
            self.groups.len(),
            self.players.len(),
            devices,
            width,
            height,
        )
    }

    /// Asks cpal for the microphones, for the settings page. Done when the menu opens, not every
    /// frame: enumerating devices is not free.
    fn refresh_devices(&mut self) {
        self.input_devices = audio::input_devices();
        if self.input_devices.is_empty() {
            self.menu_note = Some("No microphone devices found.".to_owned());
        }
    }

    /// Reads the settings file once, from the mod's own data folder.
    fn ensure_settings(&mut self, host: &dyn Host) {
        if self.settings_loaded {
            return;
        }
        self.settings_loaded = true;
        self.settings = Settings::load(&host.data_dir(MOD_ID));
    }

    /// Pushes the chosen microphone and gain to the audio thread. Called when they change and
    /// right after the devices open.
    fn apply_audio_settings(&self) {
        let Some(audio) = &self.audio else { return };
        audio.set_gain(self.settings.gain);
        audio.set_input(self.settings.input.as_deref());
    }

    fn save_settings(&self, host: &dyn Host) {
        if let Err(e) = self.settings.save(&host.data_dir(MOD_ID)) {
            log(&format!("could not save the settings: {e}"));
        }
    }

    /// Reads the settings the first time and, when they changed, sends them to the audio thread
    /// and writes the file. Only ticks have a `Host`, and only they need one.
    fn sync_settings(&mut self, host: &dyn Host) {
        self.ensure_settings(host);
        if self.settings_dirty {
            self.settings_dirty = false;
            self.apply_audio_settings();
            self.save_settings(host);
        }
    }

    /// Switches pages, clearing the last note so it does not look like it belongs to the new one.
    fn menu_tab_next(&mut self) {
        self.menu_tab = self.menu_tab.next();
        self.menu_note = None;
        if self.menu_tab == MenuTab::Settings {
            self.refresh_devices();
        }
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

    /// A left click on the menu: tabs switch pages, a player becomes the whisper target, a
    /// group row can only explain why joining or leaving does nothing yet, and the settings
    /// page picks a microphone or moves its gain.
    fn menu_click(&mut self, layout: &ui::MenuLayout, mouse: [f32; 2]) {
        for (i, rect) in layout.tabs.iter().enumerate() {
            if ui::inside(*rect, mouse) {
                let Some(tab) = MenuTab::ALL.get(i).copied() else { return };
                if self.menu_tab != tab {
                    self.menu_tab = tab;
                    self.menu_note = None;
                    if tab == MenuTab::Settings {
                        self.refresh_devices();
                    }
                }
                return;
            }
        }
        match self.menu_tab {
            MenuTab::Groups => {
                if ui::inside(layout.create, mouse) {
                    self.menu_create();
                    return;
                }
                if ui::inside(layout.name, mouse) {
                    return;
                }
                let groups: Vec<[u8; 16]> = self.menu_groups().iter().map(|g| g.id.0).collect();
                for (i, row) in layout.rows.iter().enumerate() {
                    if !ui::inside(*row, mouse) {
                        continue;
                    }
                    let Some(id) = groups.get(i) else { return };
                    self.menu_note = Some(if Some(*id) == self.own_group {
                        "Leave needs leave_group: not captured yet.".to_owned()
                    } else {
                        "Join needs set_group: not captured yet.".to_owned()
                    });
                    return;
                }
            }
            MenuTab::Players => {
                let players: Vec<([u8; 16], String, bool)> = self
                    .menu_players()
                    .iter()
                    .map(|p| (p.player.0, p.name.clone(), Some(p.player.0) == self.own_id))
                    .collect();
                for (i, row) in layout.rows.iter().enumerate() {
                    if !ui::inside(*row, mouse) {
                        continue;
                    }
                    let Some((id, name, own)) = players.get(i) else { return };
                    if *own {
                        self.menu_note = Some("That is you.".to_owned());
                    } else {
                        self.whisper = Some(*id);
                        self.menu_note =
                            Some(format!("Whisper: {name} (local only, not sent yet)."));
                    }
                    return;
                }
            }
            MenuTab::Settings => {
                if ui::inside(layout.minus, mouse) {
                    self.settings.nudge_gain(-settings::GAIN_STEP);
                    self.settings_dirty = true;
                    self.menu_note =
                        Some(format!("Microphone gain: {}%", self.settings.gain_percent()));
                    return;
                }
                if ui::inside(layout.plus, mouse) {
                    self.settings.nudge_gain(settings::GAIN_STEP);
                    self.settings_dirty = true;
                    self.menu_note =
                        Some(format!("Microphone gain: {}%", self.settings.gain_percent()));
                    return;
                }
                for (i, row) in layout.rows.iter().enumerate() {
                    if !ui::inside(*row, mouse) {
                        continue;
                    }
                    // Row 0 is the system default; the rest follow the enumerated devices.
                    let chosen = if i == 0 {
                        None
                    } else {
                        match self.input_devices.get(i - 1) {
                            Some(name) => Some(name.clone()),
                            None => return,
                        }
                    };
                    self.settings.input = chosen;
                    self.settings_dirty = true;
                    self.menu_note = Some(match &self.settings.input {
                        Some(name) => format!("Microphone: {name}"),
                        None => "Microphone: the system default".to_owned(),
                    });
                    return;
                }
            }
        }
    }

    /// Draws the menu. `mouse` is the pointer in GUI pixels, for the hover highlight.
    fn draw_menu(
        &self,
        host: &dyn Host,
        batch: &mut horizon_client::mod_api::HudBatch,
        width: f32,
        height: f32,
        mouse: [f32; 2],
    ) {
        let layout = self.menu_layout(width, height);
        let groups: Vec<GroupRow<'_>> = self
            .menu_groups()
            .iter()
            .take(MAX_GROUP_ROWS)
            .map(|g| GroupRow {
                name: &g.name,
                password: g.has_password,
                own: Some(g.id.0) == self.own_group,
            })
            .collect();
        let players: Vec<PlayerRow<'_>> = self
            .menu_players()
            .iter()
            .take(MAX_GROUP_ROWS)
            .map(|p| PlayerRow {
                name: &p.name,
                speaking: self.speaking.contains_key(&p.player.0),
                own: Some(p.player.0) == self.own_id,
                whisper: self.whisper == Some(p.player.0),
            })
            .collect();
        let mut devices: Vec<DeviceRow<'_>> = Vec::with_capacity(ui::MAX_DEVICES);
        devices.push(DeviceRow {
            name: "System default",
            selected: self.settings.input.is_none(),
            default: true,
        });
        for name in self.input_devices.iter().take(ui::MAX_DEVICES - 1) {
            devices.push(DeviceRow {
                name,
                selected: self.settings.input.as_deref() == Some(name.as_str()),
                default: false,
            });
        }
        let view = MenuView {
            tab: self.menu_tab,
            status: self.status(),
            group_count: self.groups.len(),
            player_count: self.players.len(),
            groups,
            players,
            devices,
            gain: self.settings.gain,
            level: self.mic_level,
            group_name: &self.group_name,
            caret: (self.menu_ticks / 8) % 2 == 0,
            note: self.menu_note.as_deref(),
        };
        ui::draw_menu(host, batch, &view, &layout, mouse);
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
        match audio::Audio::new(self.settings.input.as_deref(), self.settings.gain) {
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
        self.menu_ticks = self.menu_ticks.wrapping_add(1);
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
                    // Deafened skips the speakers, not the packets: the session stays alive.
                    if !self.deafened {
                        if let Some(audio) = &self.audio {
                            audio.play(first_id.0, &opus, sequence);
                        }
                    }
                }
                Event::Received(_) => {}
            }
        }
    }

    /// Sends the microphone while push-to-talk is held, and the end marker when it is let go.
    fn transmit(&mut self) {
        // The meter follows the loudest recent frame and falls back between them.
        self.mic_level = (self.mic_level * 0.7).clamp(0.0, 1.0);
        let (Some(udp), Some(audio)) = (self.udp.as_mut(), self.audio.as_ref()) else { return };
        if !udp.is_connected() || !audio.has_input() {
            return;
        }
        let transmitting = self.push_to_talk && !self.muted;
        if transmitting {
            while let Some(frame) = audio.take_frame() {
                self.mic_level = self.mic_level.max(frame_level(&frame));
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
        state.sync_settings(host);
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
            GROUPS if event.pressed && in_world(host) && host.mod_screen().is_none() => {
                if host.mod_screen_open(MOD_ID) {
                    state.menu_note = None;
                    state.menu_tab = MenuTab::Groups;
                    state.group_name.clear();
                    state.menu_ticks = 0;
                    state.ensure_settings(host);
                    state.refresh_devices();
                }
            }
            DEAFEN if event.pressed && in_world(host) => {
                let deafened = state.toggle_deafen();
                log(if deafened {
                    "deafened: you hear nobody (local only, nothing is sent)"
                } else {
                    "no longer deafened"
                });
            }
            // Picks who a whisper would go to. The packet is still unknown, so nothing is sent
            // and the indicator says so.
            WHISPER if event.pressed && in_world(host) => match state.cycle_whisper() {
                Some(name) => log(&format!(
                    "whisper target: {name} (no whisper packet is sent until a capture confirms it)"
                )),
                None => log("whisper target cleared"),
            },
            _ => {}
        }
    }

    /// Draws the group menu over the mod screen, and takes its clicks and keys (API 78 to 111).
    fn screen_render_post(&self, host: &dyn Host, event: &mut ScreenRenderPostEvent<'_>) {
        if event.screen.id != "mod" || host.mod_screen() != Some(MOD_ID) {
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
        if event.screen.id != "mod" || host.mod_screen() != Some(MOD_ID) {
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
        if event.screen.id != "mod" || host.mod_screen() != Some(MOD_ID) {
            return Flow::Continue;
        }
        match event.key {
            // Escape and the gamepad close the screen; the game does that itself.
            "key.keyboard.escape" => Flow::Continue,
            TAB if !event.repeat => {
                self.state().menu_tab_next();
                Flow::Cancel
            }
            ENTER if !event.repeat => {
                let mut state = self.state();
                if state.menu_tab == MenuTab::Groups {
                    state.menu_create();
                }
                Flow::Cancel
            }
            BACKSPACE => {
                let mut state = self.state();
                if state.menu_tab == MenuTab::Groups {
                    state.menu_backspace();
                }
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
        if event.screen.id != "mod" || host.mod_screen() != Some(MOD_ID) {
            return Flow::Continue;
        }
        let mut state = self.state();
        if state.menu_tab == MenuTab::Groups {
            state.menu_type(event.c);
        }
        Flow::Cancel
    }

    /// The voice panel in the lower left: connection, a live microphone meter, the group, the
    /// whisper target and who is speaking. `gui_render_post` keeps it above screens and menus
    /// (API 410); it follows F1.
    fn gui_render_post(&self, host: &dyn Host, event: &mut GuiRenderPostEvent<'_>) {
        if event.gui_hidden || !host.playing() {
            return;
        }
        let state = self.state();
        let Some(view) = state.hud_view() else { return };
        ui::draw_hud(host, event.batch, &view, event.batch.height);
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
        let view = inner.hud_view().expect("a session starts the HUD");
        assert_eq!(view.status, Status::Connecting);
        assert_eq!(view.mic, MicState::Muted);
        assert_eq!(view.speaking, ["Ana"]);
    }

    #[test]
    fn the_hud_never_lists_our_own_voice_as_someone_speaking() {
        let (_tx, rx) = std::sync::mpsc::channel();
        let mut inner = Inner {
            resolving: Some(Resolving {
                secret: fake_secret(),
                target: "127.0.0.1:24454".into(),
                result: rx,
            }),
            own_id: Some([1; 16]),
            ..Inner::default()
        };
        with_player(&mut inner, [1; 16], "Me");
        with_player(&mut inner, [2; 16], "Ana");
        inner.speaking.insert([1; 16], Instant::now());
        inner.speaking.insert([2; 16], Instant::now());
        let view = inner.hud_view().unwrap();
        assert_eq!(view.speaking, ["Ana"]);
    }

    #[test]
    fn the_microphone_meter_follows_the_frame_peak() {
        assert_eq!(frame_level(&[]), 0.0);
        assert_eq!(frame_level(&[0.0; 960]), 0.0);
        let quiet = frame_level(&[0.05; 960]);
        let loud = frame_level(&[0.5; 960]);
        assert!(quiet > 0.0 && quiet < loud);
        assert_eq!(loud, 1.0, "a frame at half scale fills the bar");
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

        assert!(inner.hud_view().is_none(), "no UDP session yet: the HUD stays hidden");

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

    fn with_player(inner: &mut Inner, id: [u8; 16], name: &str) {
        let uuid = net::Uuid(id);
        inner.players.insert(
            id,
            PlayerState {
                disabled: false,
                disconnected: false,
                player: uuid,
                name: name.into(),
                group: None,
            },
        );
    }

    #[test]
    fn deafening_is_local_and_survives_a_reconnect() {
        let mut inner = Inner::default();
        assert!(!inner.deafened);
        assert!(inner.toggle_deafen());
        inner.deafened = true;
        inner.reset();
        assert!(inner.deafened, "the deafen choice is the player's, like mute");
        assert!(!inner.muted);
    }

    #[test]
    fn whisper_cycles_through_players_in_name_order_and_back_to_none() {
        let mut inner = Inner::default();
        let ana = net::Uuid([0xAA; 16]);
        let bob = net::Uuid([0xBB; 16]);
        with_player(&mut inner, ana.0, "Ana");
        with_player(&mut inner, bob.0, "Bob");

        assert_eq!(inner.cycle_whisper().as_deref(), Some("Ana"));
        assert_eq!(inner.whisper_name(), Some("Ana"));
        assert_eq!(inner.cycle_whisper().as_deref(), Some("Bob"));
        assert_eq!(inner.cycle_whisper(), None);
        assert_eq!(inner.whisper, None);

        // A target who leaves is shown as unknown, and the next press starts over.
        assert_eq!(inner.cycle_whisper().as_deref(), Some("Ana"));
        inner.players.remove(&ana.0);
        assert_eq!(inner.whisper_name(), None);
        assert_eq!(inner.cycle_whisper().as_deref(), Some("Bob"));

        // Our own entry in the player list is never a whisper target.
        inner.whisper = None;
        inner.own_id = Some(bob.0);
        assert_eq!(inner.cycle_whisper(), None, "only our own player is left");
    }

    #[test]
    fn the_hud_says_when_deafened_or_whispering() {
        let (_tx, rx) = std::sync::mpsc::channel();
        let mut inner = Inner {
            resolving: Some(Resolving {
                secret: fake_secret(),
                target: "127.0.0.1:24454".into(),
                result: rx,
            }),
            ..Inner::default()
        };
        with_player(&mut inner, [0xAA; 16], "Ana");
        inner.deafened = true;
        inner.whisper = Some([0xAA; 16]);
        let view = inner.hud_view().unwrap();
        assert!(view.deafened);
        assert_eq!(view.whisper, Some("Ana"));
    }

    #[test]
    fn clicking_the_tabs_switches_pages_and_a_player_becomes_the_whisper_target() {
        let mut inner = Inner::default();
        with_player(&mut inner, [0xAA; 16], "Ana");
        let layout = inner.menu_layout(1280.0, 720.0);
        assert!(layout.name[2] > 0.0, "the groups page has the create form");

        let players_tab = layout.tabs[1];
        inner.menu_click(&layout, [players_tab[0] + 2.0, players_tab[1] + 2.0]);
        assert_eq!(inner.menu_tab, MenuTab::Players);
        assert!(inner.menu_note.is_none());

        let layout = inner.menu_layout(1280.0, 720.0);
        assert_eq!(layout.name, [0.0; 4], "the players page has no create form");
        let row = layout.rows[0];
        inner.menu_click(&layout, [row[0] + 2.0, row[1] + 2.0]);
        assert_eq!(inner.whisper, Some([0xAA; 16]));
        assert!(inner.menu_note.as_deref().unwrap_or("").contains("Ana"));

        let groups_tab = layout.tabs[0];
        inner.menu_click(&layout, [groups_tab[0] + 2.0, groups_tab[1] + 2.0]);
        assert_eq!(inner.menu_tab, MenuTab::Groups);
    }

    #[test]
    fn the_players_page_lists_ourselves_first() {
        let mut inner = Inner { own_id: Some([0xAA; 16]), ..Inner::default() };
        with_player(&mut inner, [0xBB; 16], "Ana");
        with_player(&mut inner, [0xAA; 16], "Me");
        let names: Vec<&str> = inner.menu_players().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Me", "Ana"]);
    }

    #[test]
    fn the_settings_page_picks_a_microphone_and_changes_the_gain() {
        let mut inner =
            Inner { input_devices: vec!["USB Mic".to_owned()], ..Inner::default() };
        inner.menu_tab = MenuTab::Settings;
        let layout = inner.menu_layout(1280.0, 720.0);
        assert_eq!(layout.rows.len(), 2, "the system default row and USB Mic");
        assert!(layout.gain_bar[2] > 0.0);

        let plus = [layout.plus[0] + 2.0, layout.plus[1] + 2.0];
        inner.menu_click(&layout, plus);
        inner.menu_click(&layout, plus);
        assert_eq!(inner.settings.gain, 1.5);
        assert!(inner.settings_dirty, "the change has to reach the audio thread");
        inner.menu_click(&layout, [layout.minus[0] + 2.0, layout.minus[1] + 2.0]);
        assert_eq!(inner.settings.gain, 1.25);

        let mic = layout.rows[1];
        inner.menu_click(&layout, [mic[0] + 2.0, mic[1] + 2.0]);
        assert_eq!(inner.settings.input.as_deref(), Some("USB Mic"));

        let default = layout.rows[0];
        inner.menu_click(&layout, [default[0] + 2.0, default[1] + 2.0]);
        assert!(inner.settings.input.is_none(), "row 0 follows the system default");
    }

    #[test]
    fn gain_clicks_stop_at_the_allowed_range() {
        let mut inner = Inner { menu_tab: MenuTab::Settings, ..Inner::default() };
        let layout = inner.menu_layout(1280.0, 720.0);
        for _ in 0..40 {
            inner.menu_click(&layout, [layout.plus[0] + 2.0, layout.plus[1] + 2.0]);
        }
        assert_eq!(inner.settings.gain, settings::MAX_GAIN);
        for _ in 0..40 {
            inner.menu_click(&layout, [layout.minus[0] + 2.0, layout.minus[1] + 2.0]);
        }
        assert_eq!(inner.settings.gain, settings::MIN_GAIN);
    }

    #[test]
    fn tab_cycles_through_all_three_pages() {
        assert_eq!(MenuTab::Groups.next(), MenuTab::Players);
        assert_eq!(MenuTab::Players.next(), MenuTab::Settings);
        assert_eq!(MenuTab::Settings.next(), MenuTab::Groups);
    }

    #[test]
    fn the_microphone_choice_survives_a_reconnect() {
        let mut inner = Inner::default();
        inner.settings.input = Some("USB Mic".to_owned());
        inner.settings.gain = 2.0;
        inner.settings_loaded = true;
        inner.reset();
        assert_eq!(inner.settings.input.as_deref(), Some("USB Mic"));
        assert_eq!(inner.settings.gain, 2.0);
        assert!(inner.settings_loaded, "a reconnect must not reread the file");
    }
}
