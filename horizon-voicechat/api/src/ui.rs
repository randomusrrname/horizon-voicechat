//! The mod's own look: the voice HUD in the corner of the world and the group menu.
//!
//! The native API only gives mods solid rectangles (`Host::fill`) and the game's font
//! (`Host::text`/`text_width`), so the look is built from those: dark translucent panels with a
//! thin violet accent, a header, tab pills and rows that react to the pointer. Both the HUD and
//! the menu share the colours here, so they stay one design.
//!
//! Colours for `fill` are ARGB (the alpha matters); colours for `text` are RGB.

use horizon_client::mod_api::{Host, HudBatch};

pub const MAX_ROWS: usize = 10;
/// The most speakers the HUD lists before collapsing the rest into a count.
const MAX_SPEAKERS: usize = 4;

// Fills (ARGB).
const PANEL: u32 = 0xE6131722;
const PANEL_ALT: u32 = 0xF01B2130;
const INNER: u32 = 0xFF0C0F16;
const BORDER: u32 = 0xFF2B3345;
const ACCENT: u32 = 0xFF8B5CF6;
const ACCENT_DIM: u32 = 0xFF5B3FA8;
const ACCENT_WASH: u32 = 0x338B5CF6;
const HOVER: u32 = 0x1FFFFFFF;
const SHADOW: u32 = 0x55000000;
const DIM_BACKDROP: u32 = 0x66000000;
const DOT_OFF: u32 = 0xFF46516A;
const HUD_PANEL: u32 = 0xC80F131C;

// Text (RGB).
const TEXT: u32 = 0xE9EEF7;
const TEXT_MUTED: u32 = 0x9AA5BA;
const TEXT_DIM: u32 = 0x69748A;
const GREEN: u32 = 0x3DDC84;
const AMBER: u32 = 0xFFC857;
const RED: u32 = 0xFF6B6B;
const AQUA: u32 = 0x6FE3E1;

const PAD: f32 = 10.0;
const HEADER: f32 = 34.0;
const TABS: f32 = 20.0;
const ROW: f32 = 16.0;
const FIELD: f32 = 20.0;
const LINE: f32 = 11.0;
const CREATE_W: f32 = 86.0;
const GAP: f32 = 6.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MenuTab {
    #[default]
    Groups,
    Players,
}

impl MenuTab {
    pub fn label(self) -> &'static str {
        match self {
            MenuTab::Groups => "Groups",
            MenuTab::Players => "Players",
        }
    }

    pub fn next(self) -> MenuTab {
        match self {
            MenuTab::Groups => MenuTab::Players,
            MenuTab::Players => MenuTab::Groups,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Offline,
    Connecting,
    Connected,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Offline => "Offline",
            Status::Connecting => "Connecting",
            Status::Connected => "Connected",
        }
    }

    fn color(self) -> u32 {
        match self {
            Status::Offline => TEXT_DIM,
            Status::Connecting => AMBER,
            Status::Connected => GREEN,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MicState {
    Absent,
    Idle,
    Talking,
    Muted,
}

impl MicState {
    fn label(self) -> &'static str {
        match self {
            MicState::Absent => "no input",
            MicState::Idle => "ready",
            MicState::Talking => "talking",
            MicState::Muted => "muted",
        }
    }

    fn color(self) -> u32 {
        match self {
            MicState::Absent | MicState::Idle => TEXT_DIM,
            MicState::Talking => GREEN,
            MicState::Muted => RED,
        }
    }
}

/// What the HUD shows, already resolved by `Inner`: the drawing never reads the session.
pub struct HudView<'a> {
    pub status: Status,
    pub mic: MicState,
    /// Peak microphone level of the last frames, 0.0 to 1.0.
    pub level: f32,
    pub group: Option<&'a str>,
    pub deafened: bool,
    pub whisper: Option<&'a str>,
    pub speaking: Vec<&'a str>,
}

pub struct GroupRow<'a> {
    pub name: &'a str,
    pub password: bool,
    pub own: bool,
}

pub struct PlayerRow<'a> {
    pub name: &'a str,
    pub speaking: bool,
    pub own: bool,
    pub whisper: bool,
}

/// What the menu shows, already resolved by `Inner` (sorted groups and players).
pub struct MenuView<'a> {
    pub tab: MenuTab,
    pub status: Status,
    pub group_count: usize,
    pub player_count: usize,
    pub groups: Vec<GroupRow<'a>>,
    pub players: Vec<PlayerRow<'a>>,
    pub group_name: &'a str,
    /// Blink phase for the text caret.
    pub caret: bool,
    pub note: Option<&'a str>,
}

/// Where every part of the menu goes, in GUI pixels. Drawing and clicks both use it, so they
/// cannot drift apart.
pub struct MenuLayout {
    pub panel: [f32; 4],
    pub header: [f32; 4],
    pub tabs: [[f32; 4]; 2],
    pub rows: Vec<[f32; 4]>,
    /// The group-name field and the create button (empty rectangles on the players tab).
    pub name: [f32; 4],
    pub create: [f32; 4],
    pub note: [f32; 2],
    pub footer: [f32; 2],
}

/// Whether `point` is inside `rect` (`[x, y, width, height]`).
pub fn inside(rect: [f32; 4], point: [f32; 2]) -> bool {
    let [x, y, w, h] = rect;
    point[0] >= x && point[0] <= x + w && point[1] >= y && point[1] <= y + h
}

pub fn menu_layout(
    tab: MenuTab,
    group_count: usize,
    player_count: usize,
    width: f32,
    height: f32,
) -> MenuLayout {
    let visible = match tab {
        MenuTab::Groups => group_count.min(MAX_ROWS),
        MenuTab::Players => player_count.min(MAX_ROWS),
    };
    let w = 360.0_f32.min((width - 12.0).max(240.0));
    let inner_w = w - PAD * 2.0;
    let list_h = visible as f32 * ROW;
    let form_h = match tab {
        MenuTab::Groups => 8.0 + FIELD + 8.0,
        MenuTab::Players => 6.0,
    };
    // Two lines under the list: the note and the key hints.
    let h = PAD * 2.0
        + HEADER
        + GAP
        + TABS
        + 8.0
        + list_h
        + form_h
        + 10.0
        + LINE
        + 12.0;
    let x = ((width - w) * 0.5).max(4.0);
    let y = ((height - h) * 0.5).max(6.0);
    let inner_x = x + PAD;
    let tab_y = y + HEADER + GAP;
    let tab_w = (inner_w - GAP) * 0.5;
    let tabs = [
        [inner_x, tab_y, tab_w, TABS],
        [inner_x + tab_w + GAP, tab_y, tab_w, TABS],
    ];
    let mut cursor = tab_y + TABS + 8.0;
    let mut rows = Vec::with_capacity(visible);
    for _ in 0..visible {
        rows.push([inner_x, cursor, inner_w, ROW]);
        cursor += ROW;
    }
    let (name, create, form_end) = if tab == MenuTab::Groups {
        let field_y = cursor + 8.0;
        let name_w = inner_w - CREATE_W - GAP;
        (
            [inner_x, field_y, name_w, FIELD],
            [inner_x + name_w + GAP, field_y, CREATE_W, FIELD],
            field_y + FIELD,
        )
    } else {
        ([0.0; 4], [0.0; 4], cursor + 6.0)
    };
    let note = [inner_x, form_end + 10.0];
    let footer = [inner_x, note[1] + LINE];
    MenuLayout {
        panel: [x, y, w, h],
        header: [x, y, w, HEADER],
        tabs,
        rows,
        name,
        create,
        note,
        footer,
    }
}

fn fill(host: &dyn Host, batch: &mut HudBatch, rect: [f32; 4], color: u32) {
    if rect[2] > 0.0 && rect[3] > 0.0 {
        host.fill(batch, rect, color);
    }
}

fn border(host: &dyn Host, batch: &mut HudBatch, rect: [f32; 4], color: u32) {
    let [x, y, w, h] = rect;
    fill(host, batch, [x, y, w, 1.0], color);
    fill(host, batch, [x, y + h - 1.0, w, 1.0], color);
    fill(host, batch, [x, y, 1.0, h], color);
    fill(host, batch, [x + w - 1.0, y, 1.0, h], color);
}

fn label(host: &dyn Host, batch: &mut HudBatch, text: &str, at: [f32; 2], rgb: u32) {
    host.text(batch, text, at, rgb, true);
}

fn right_label(
    host: &dyn Host,
    batch: &mut HudBatch,
    text: &str,
    right: f32,
    y: f32,
    rgb: u32,
) {
    host.text(batch, text, [right - host.text_width(text), y], rgb, true);
}

fn dot(host: &dyn Host, batch: &mut HudBatch, at: [f32; 2], size: f32, color: u32) {
    fill(host, batch, [at[0], at[1], size, size], color);
}

/// Shortens `text` with two dots until it fits `max` pixels.
fn clip(host: &dyn Host, text: &str, max: f32) -> String {
    if host.text_width(text) <= max {
        return text.to_owned();
    }
    let mut out = String::new();
    for c in text.chars() {
        let mut probe = out.clone();
        probe.push(c);
        probe.push_str("..");
        if host.text_width(&probe) > max {
            break;
        }
        out.push(c);
    }
    out.push_str("..");
    out
}

/// A panel: shadow, translucent fill, thin border and the violet strip on the left.
fn panel(host: &dyn Host, batch: &mut HudBatch, rect: [f32; 4], fill_color: u32) {
    let [x, y, w, h] = rect;
    fill(host, batch, [x + 3.0, y + 4.0, w, h], SHADOW);
    fill(host, batch, rect, fill_color);
    border(host, batch, rect, BORDER);
    fill(host, batch, [x, y, 2.0, h], ACCENT);
}

fn tab(host: &dyn Host, batch: &mut HudBatch, rect: [f32; 4], text: &str, active: bool, hover: bool) {
    if active {
        fill(host, batch, rect, ACCENT_DIM);
        border(host, batch, rect, ACCENT);
        fill(host, batch, [rect[0] + 1.0, rect[1] + rect[3] - 2.0, rect[2] - 2.0, 2.0], ACCENT);
    } else {
        if hover {
            fill(host, batch, rect, HOVER);
        }
        border(host, batch, rect, BORDER);
    }
    let color = if active { TEXT } else { TEXT_MUTED };
    label(
        host,
        batch,
        text,
        [rect[0] + (rect[2] - host.text_width(text)) * 0.5, rect[1] + 6.0],
        color,
    );
}

/// Draws the group/player menu over a dimmed world.
pub fn draw_menu(
    host: &dyn Host,
    batch: &mut HudBatch,
    view: &MenuView<'_>,
    layout: &MenuLayout,
    mouse: [f32; 2],
) {
    let [x, y, w, h] = layout.panel;
    fill(host, batch, [0.0, 0.0, w + x * 2.0, h + y * 2.0], DIM_BACKDROP);
    panel(host, batch, layout.panel, PANEL);

    // Header: title, status and the Esc hint.
    fill(host, batch, [x + 1.0, y + 1.0, w - 2.0, HEADER - 1.0], PANEL_ALT);
    fill(host, batch, [x, y, w, 2.0], ACCENT);
    label(host, batch, "Voice Chat", [x + PAD, y + 10.0], TEXT);
    label(host, batch, "Simple Voice Chat", [x + PAD, y + 21.0], TEXT_DIM);
    right_label(host, batch, "Esc", x + w - PAD, y + 10.0, TEXT_DIM);
    let status = clip(host, view.status.label(), 90.0);
    let status_x = x + w - PAD - host.text_width(&status);
    dot(host, batch, [status_x - 9.0, y + 12.0], 5.0, view.status.color());
    label(host, batch, &status, [status_x, y + 10.0], view.status.color());

    // Tabs, with their counts so an empty page is obvious before clicking it.
    let tabs = [
        format!("{} ({})", MenuTab::Groups.label(), view.group_count),
        format!("{} ({})", MenuTab::Players.label(), view.player_count),
    ];
    for (i, tab_rect) in layout.tabs.iter().enumerate() {
        let active = view.tab as usize == i;
        tab(host, batch, *tab_rect, &tabs[i], active, inside(*tab_rect, mouse));
    }

    // Rows.
    match view.tab {
        MenuTab::Groups => draw_groups(host, batch, view, layout, mouse),
        MenuTab::Players => draw_players(host, batch, view, layout, mouse),
    }

    // The create form and the note/hints.
    if view.tab == MenuTab::Groups {
        let field = layout.name;
        fill(host, batch, field, INNER);
        border(host, batch, field, ACCENT_DIM);
        if view.group_name.is_empty() {
            label(host, batch, "New group name", [field[0] + 6.0, field[1] + 6.0], TEXT_DIM);
        } else {
            let text = clip(host, view.group_name, field[2] - 12.0);
            label(host, batch, &text, [field[0] + 6.0, field[1] + 6.0], TEXT);
        }
        if view.caret {
            let typed = clip(host, view.group_name, field[2] - 14.0);
            let caret_x = field[0] + 6.0 + host.text_width(&typed).min(field[2] - 14.0);
            fill(host, batch, [caret_x + 1.0, field[1] + 5.0, 1.0, 10.0], ACCENT);
        }
        let enabled = !view.group_name.trim().is_empty();
        let create = layout.create;
        fill(host, batch, create, if enabled { ACCENT_DIM } else { INNER });
        border(host, batch, create, if enabled { ACCENT } else { BORDER });
        let create_text = "Create group";
        let color = if enabled { TEXT } else { TEXT_DIM };
        label(
            host,
            batch,
            create_text,
            [create[0] + (create[2] - host.text_width(create_text)) * 0.5, create[1] + 6.0],
            color,
        );
    }
    if let Some(note) = view.note {
        let note = clip(host, note, w - PAD * 2.0);
        label(host, batch, &note, layout.note, AMBER);
    }
    let hint = match view.tab {
        MenuTab::Groups => "Enter create   Esc close   Tab switches",
        MenuTab::Players => "Click a player to whisper   Esc close   Tab switches",
    };
    let hint = clip(host, hint, w - PAD * 2.0);
    label(host, batch, &hint, layout.footer, TEXT_DIM);
}

fn draw_groups(
    host: &dyn Host,
    batch: &mut HudBatch,
    view: &MenuView<'_>,
    layout: &MenuLayout,
    mouse: [f32; 2],
) {
    if view.groups.is_empty() {
        let empty = "No groups yet: create one below.";
        label(
            host,
            batch,
            empty,
            [layout.panel[0] + (layout.panel[2] - host.text_width(empty)) * 0.5, layout.rows.first().map_or(layout.panel[1] + HEADER + TABS, |r| r[1]) + 4.0],
            TEXT_DIM,
        );
        return;
    }
    for (i, row) in layout.rows.iter().enumerate() {
        let Some(group) = view.groups.get(i) else { break };
        let hover = inside(*row, mouse);
        if group.own {
            fill(host, batch, *row, ACCENT_WASH);
            fill(host, batch, [row[0], row[1], 2.0, row[3]], ACCENT);
        } else if hover {
            fill(host, batch, *row, HOVER);
        }
        dot(
            host,
            batch,
            [row[0] + 6.0, row[1] + 5.0],
            6.0,
            if group.own { ACCENT } else { DOT_OFF },
        );
        let name = clip(host, group.name, row[2] - 70.0);
        label(host, batch, &name, [row[0] + 17.0, row[1] + 4.0], if group.own { TEXT } else { TEXT_MUTED });
        if group.password {
            right_label(host, batch, "PW", row[0] + row[2] - 6.0, row[1] + 4.0, AMBER);
        }
        if group.own {
            right_label(host, batch, "you", row[0] + row[2] - 28.0, row[1] + 4.0, AQUA);
        }
    }
}

fn draw_players(
    host: &dyn Host,
    batch: &mut HudBatch,
    view: &MenuView<'_>,
    layout: &MenuLayout,
    mouse: [f32; 2],
) {
    if view.players.is_empty() {
        let empty = "Nobody is listed yet.";
        label(
            host,
            batch,
            empty,
            [layout.panel[0] + (layout.panel[2] - host.text_width(empty)) * 0.5, layout.rows.first().map_or(layout.panel[1] + HEADER + TABS, |r| r[1]) + 4.0],
            TEXT_DIM,
        );
        return;
    }
    for (i, row) in layout.rows.iter().enumerate() {
        let Some(player) = view.players.get(i) else { break };
        let hover = inside(*row, mouse);
        if player.whisper {
            fill(host, batch, *row, ACCENT_WASH);
            fill(host, batch, [row[0], row[1], 2.0, row[3]], ACCENT);
        } else if hover {
            fill(host, batch, *row, HOVER);
        }
        dot(
            host,
            batch,
            [row[0] + 6.0, row[1] + 5.0],
            6.0,
            if player.speaking { GREEN } else { DOT_OFF },
        );
        let mut name = player.name.to_owned();
        if player.own {
            name.push_str(" (you)");
        }
        let name = clip(host, &name, row[2] - 34.0);
        label(host, batch, &name, [row[0] + 17.0, row[1] + 4.0], if player.own { TEXT_DIM } else { TEXT });
        if player.whisper {
            right_label(host, batch, "W", row[0] + row[2] - 8.0, row[1] + 4.0, ACCENT);
        }
    }
}

/// Draws the corner HUD: connection, microphone meter, group, whisper and who is speaking.
pub fn draw_hud(host: &dyn Host, batch: &mut HudBatch, view: &HudView<'_>, height: f32) {
    const PAD: f32 = 6.0;
    const LINE: f32 = 11.0;
    const HEADER_H: f32 = 16.0;
    const METER: f32 = 31.0;
    const FOOTER: f32 = 12.0;

    let speaking: Vec<&str> = view.speaking.iter().copied().take(MAX_SPEAKERS).collect();
    let hidden = view.speaking.len().saturating_sub(speaking.len());
    let mut rows = 1; // microphone
    if view.group.is_some() {
        rows += 1;
    }
    if view.deafened {
        rows += 1;
    }
    if view.whisper.is_some() {
        rows += 1;
    }
    let speak_rows = if speaking.is_empty() { 0 } else { speaking.len() + if hidden > 0 { 1 } else { 0 } + 1 };

    // Measure the widest row so the panel hugs the text (capped so long names never take over).
    let mic_state = view.mic.label();
    let mic_w = 40.0 + METER + 8.0 + host.text_width(mic_state);
    let mut content = mic_w.max(host.text_width("Voice Chat") + 12.0 + host.text_width(view.status.label()));
    if let Some(group) = view.group {
        content = content.max(34.0 + host.text_width(group));
    }
    if let Some(whisper) = view.whisper {
        content = content.max(52.0 + host.text_width(whisper) + 8.0 + host.text_width("(local)"));
    }
    for name in &speaking {
        content = content.max(14.0 + host.text_width(name));
    }
    content = content.max(host.text_width("V talk  M mute  H whisper  G groups"));
    let panel_w = (content + PAD * 2.0 + 2.0).min(280.0).max(132.0);
    let panel_h = PAD * 2.0 + HEADER_H + (rows as f32 + speak_rows as f32) * LINE + FOOTER + 2.0;
    let x = 6.0;
    let y = (height - panel_h - 6.0).max(4.0);
    panel(host, batch, [x, y, panel_w, panel_h], HUD_PANEL);

    let inner_x = x + 8.0;
    let right = x + panel_w - 6.0;
    let mut cursor = y + PAD;

    // Header.
    label(host, batch, "Voice Chat", [inner_x, cursor], TEXT);
    right_label(host, batch, view.status.label(), right, cursor, view.status.color());
    let status_w = host.text_width(view.status.label());
    dot(
        host,
        batch,
        [right - status_w - 8.0, cursor + 2.0],
        5.0,
        view.status.color(),
    );
    cursor += HEADER_H;

    // Microphone: label, an eight-segment peak meter and the state.
    label(host, batch, "Mic", [inner_x, cursor], TEXT_MUTED);
    let meter_x = inner_x + 30.0;
    let lit = if view.mic == MicState::Muted {
        0
    } else {
        (view.level.clamp(0.0, 1.0) * 8.0).ceil() as usize
    };
    for i in 0..8 {
        let color = if i < lit {
            if view.mic == MicState::Muted { RED } else { GREEN }
        } else {
            DOT_OFF
        };
        fill(host, batch, [meter_x + i as f32 * 4.0, cursor + 3.0, 3.0, 5.0], color);
    }
    let state = clip(host, mic_state, right - (meter_x + METER) - 6.0);
    label(host, batch, &state, [meter_x + METER + 6.0, cursor], view.mic.color());
    cursor += LINE;

    // Group, deafen and whisper, each only when it says something.
    if let Some(group) = view.group {
        label(host, batch, "Group", [inner_x, cursor], TEXT_MUTED);
        let name = clip(host, group, right - inner_x - 38.0);
        label(host, batch, &name, [inner_x + 38.0, cursor], AQUA);
        cursor += LINE;
    }
    if view.deafened {
        label(host, batch, "Deaf", [inner_x, cursor], TEXT_MUTED);
        label(host, batch, "you hear nobody", [inner_x + 38.0, cursor], RED);
        cursor += LINE;
    }
    if let Some(whisper) = view.whisper {
        label(host, batch, "Whisper", [inner_x, cursor], TEXT_MUTED);
        let name = clip(host, whisper, right - inner_x - 52.0 - host.text_width(" (local)"));
        label(host, batch, &name, [inner_x + 52.0, cursor], AMBER);
        let after = inner_x + 52.0 + host.text_width(&name);
        label(host, batch, " (local)", [after, cursor], TEXT_DIM);
        cursor += LINE;
    }

    // Who is speaking, with a green dot each.
    if !speaking.is_empty() {
        label(host, batch, "Speaking", [inner_x, cursor], TEXT_DIM);
        cursor += LINE;
        for name in &speaking {
            dot(host, batch, [inner_x + 2.0, cursor + 3.0], 5.0, GREEN);
            let name = clip(host, name, right - inner_x - 14.0);
            label(host, batch, &name, [inner_x + 12.0, cursor], TEXT);
            cursor += LINE;
        }
        if hidden > 0 {
            label(host, batch, &format!("+{hidden} more"), [inner_x + 12.0, cursor], TEXT_DIM);
            cursor += LINE;
        }
    }

    label(host, batch, "V talk  M mute  H whisper  G groups", [inner_x, cursor], TEXT_DIM);
}
