//! User settings and shortcuts, read from `stickit.ini` in the data folder.
//!
//! Every setting is one row in `SETTINGS` (section, key, description, how to read and write
//! it), so parsing and writing the file come from the same table and adding a setting is a
//! single line. The file is grouped into sections: note look, dock behaviour/list/icons/tab,
//! virtual desktops, then shortcuts. Older files are migrated (with a `.bak` copy) keeping the
//! user's values. It is re-read only when its modification time changes: when the dock opens,
//! and continuously while it is open in an editor via "Settings…".

use crate::theme::Rgb;
use global_hotkey::hotkey::{HotKey, Modifiers};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use winit::event::KeyEvent;
use winit::keyboard::{ModifiersState, PhysicalKey};

/// Marks files written in the current layout; older files are migrated.
const FORMAT_MARKER: &str = "# stickit.ini format 2";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// Dock appearance and behaviour, in logical pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct DockStyle {
    pub side: Side,
    /// Vertical placement of the dock's centre, 0 = top of the screen, 1 = bottom.
    pub position: f32,
    pub open_delay_ms: u64,
    /// List views: card width, spacing and text.
    pub width: f32,
    pub card_gap: f32,
    pub row_padding: f32,
    pub font_size: f32,
    pub icon_size: f32,
    pub radius: f32,
    pub opacity: u8,
    /// How far the card under the pointer slides out (neighbours less).
    pub hover_push: f32,
    /// How each list is labelled when the dock shows more than one.
    pub list_headers: ListHeaders,
    pub edge_gap: f32,
    /// Icon view.
    pub tile_size: f32,
    /// Space around the tiles and between them.
    pub tile_padding: f32,
    pub tile_gap: f32,
    pub magnify: f32,
    /// The slim tab shown on the edge while tucked away; None = follow light/dark.
    pub tab_color: Option<Rgb>,
    pub tab_width: f32,
    pub tab_height: f32,
    pub tab_opacity: u8,
    /// The invisible strip on the screen edge that opens the dock when the pointer rests on it.
    pub trigger_width: f32,
    pub trigger_height: f32,
}

impl Default for DockStyle {
    fn default() -> Self {
        Self {
            side: Side::Right,
            position: 0.5,
            open_delay_ms: 150,
            width: 264.0,
            card_gap: 6.0,
            row_padding: 6.0,
            font_size: 13.5,
            icon_size: 16.0,
            radius: 16.0,
            opacity: 245,
            hover_push: 26.0,
            list_headers: ListHeaders::Pill,
            edge_gap: 10.0,
            tile_size: 34.0,
            tile_padding: 10.0,
            tile_gap: 8.0,
            magnify: 1.6,
            tab_color: None,
            tab_width: 4.0,
            tab_height: 56.0,
            tab_opacity: 190,
            trigger_width: 3.0,
            trigger_height: 140.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconPosition {
    /// Small, in the note's top bar (top-left corner).
    Bar,
    /// Large, on its own row above the text (Notion's page icon).
    Page,
}

/// Note appearance, in logical pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct NoteStyle {
    pub icon_position: IconPosition,
    pub icon_size: f32,
    /// Height of the top strip that holds the icon, reminder and hover controls.
    pub top_bar: f32,
    /// Space between the top strip (or page icon) and the text.
    pub gap: f32,
    pub padding: f32,
    pub bottom_padding: f32,
    pub radius: f32,
    pub font_size: f32,
    /// Line height as a multiple of the font size.
    pub line_height: f32,
    /// Shadow strength, 0 = none.
    pub shadow: f32,
    /// Strength of the colour cover fading in from the top, 0 = plain page.
    pub cover: f32,
    /// Size a note starts from as it appears (it grows to full size while fading in).
    pub appear_scale: f32,
    /// Colour of new notes.
    pub default_color: NewNoteColor,
    pub compact: CompactStyle,
    pub fade: FadeStyle,
    /// + buttons on a table's edges to add a row or a column (the row and column menus do it too).
    pub table_buttons: bool,
}

/// The colour a new note gets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NewNoteColor {
    /// The next colour of `theme::ROTATION` each time.
    Cycle,
    /// A random colour from `theme::ROTATION`, never the same as the last one.
    Random,
    /// Always this colour (a preset index or `theme::CUSTOM + hue`).
    Fixed(u16),
}

/// The compact view: a note shown as one line (icon and title).
#[derive(Clone, Debug, PartialEq)]
pub struct CompactStyle {
    pub height: f32,
    pub min_width: f32,
    /// The title is cut off with "…" beyond this.
    pub max_width: f32,
    pub font_size: f32,
    pub icon_size: f32,
    pub padding: f32,
    pub radius: f32,
    /// How much of the note's colour tints the pill, 0 = plain.
    pub tint: f32,
    /// Show "2/5" for notes with to-dos.
    pub show_progress: bool,
    /// An opened compact note folds back to one line when you click elsewhere.
    pub collapse_on_blur: bool,
}

/// Notes fading while you aren't using them, so they cover less of what is behind.
#[derive(Clone, Debug, PartialEq)]
pub struct FadeStyle {
    pub enabled: bool,
    /// Seconds without focus or the pointer before a note fades.
    pub after: f32,
    /// How visible a faded note stays (0..1).
    pub opacity: f32,
    pub which: FadeWhich,
    /// Compact (one-line) notes fade too.
    pub compact: bool,
}

/// How the dock labels each list when it shows more than one (a single list has no label).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListHeaders {
    /// A small floating label above the list.
    Pill,
    /// A card in the column, the size of a note, that slides out under the pointer like one.
    Card,
    /// Quiet text above the list.
    Caption,
    /// The list's name followed by a thin rule across the column.
    Line,
    /// No labels: the lists follow one another.
    None,
}

impl ListHeaders {
    pub const ALL: [(ListHeaders, &'static str); 5] = [
        (ListHeaders::Pill, "pill"),
        (ListHeaders::Card, "card"),
        (ListHeaders::Caption, "caption"),
        (ListHeaders::Line, "line"),
        (ListHeaders::None, "none"),
    ];

    pub fn name(self) -> &'static str {
        Self::ALL.iter().find(|(h, _)| *h == self).map_or("pill", |(_, n)| n)
    }

    pub fn from_name(v: &str) -> Option<ListHeaders> {
        let v = v.trim().to_ascii_lowercase();
        Self::ALL.iter().find(|(_, n)| *n == v).map(|(h, _)| *h)
    }
}

/// Which notes fade when idle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FadeWhich {
    /// Only "always on top" notes: the ones that sit over your work.
    OnTop,
    /// Every note.
    All,
}

impl Default for FadeStyle {
    fn default() -> Self {
        Self { enabled: false, after: 30.0, opacity: 0.35, which: FadeWhich::OnTop, compact: true }
    }
}

impl Default for CompactStyle {
    fn default() -> Self {
        Self {
            height: 36.0,
            min_width: 120.0,
            max_width: 240.0,
            font_size: 13.5,
            icon_size: 15.0,
            padding: 12.0,
            radius: 12.0,
            tint: 0.55,
            show_progress: true,
            collapse_on_blur: true,
        }
    }
}

impl NoteStyle {
    pub fn line(&self) -> f32 {
        self.font_size * self.line_height
    }

    /// Height of the page-icon row (zero when the icon lives in the top bar).
    pub fn icon_row(&self, has_icon: bool) -> f32 {
        if has_icon && self.icon_position == IconPosition::Page {
            self.icon_size + 16.0
        } else {
            0.0
        }
    }
}

impl Default for NoteStyle {
    fn default() -> Self {
        Self {
            icon_position: IconPosition::Bar,
            icon_size: 18.0,
            top_bar: 38.0,
            gap: 2.0,
            padding: 20.0,
            bottom_padding: 16.0,
            radius: 18.0,
            font_size: 14.5,
            line_height: 1.5,
            shadow: 1.0,
            cover: 1.0,
            appear_scale: 0.97,
            default_color: NewNoteColor::Cycle,
            compact: CompactStyle::default(),
            fade: FadeStyle::default(),
            table_buttons: false,
        }
    }
}

/// The reminder menu's quick choices.
#[derive(Clone, Debug, PartialEq)]
pub struct ReminderSettings {
    /// Times of day offered for the chosen day.
    pub times: Vec<chrono::NaiveTime>,
    /// "In 20m"-style choices, in seconds.
    pub soon: Vec<i64>,
    /// Minutes the reminder menu's fine-tune buttons move the time by.
    pub minute_step: u32,
    /// A short sound when a reminder rings.
    pub sound: bool,
}

impl Default for ReminderSettings {
    fn default() -> Self {
        let t = |h| chrono::NaiveTime::from_hms_opt(h, 0, 0).unwrap_or_default();
        Self {
            times: vec![t(9), t(12), t(15), t(18), t(21)],
            soon: vec![20 * 60, 3600, 3 * 3600],
            minute_step: 5,
            sound: true,
        }
    }
}

/// A comma-separated list where every item must parse; at most `max` items.
fn list<T>(value: &str, max: usize, parse: impl Fn(&str) -> Option<T>) -> Option<Vec<T>> {
    let items: Option<Vec<T>> = value.split(',').map(str::trim).filter(|v| !v.is_empty()).map(parse).collect();
    items.filter(|v| !v.is_empty() && v.len() <= max)
}

/// App-wide behaviour.
#[derive(Clone, Debug, PartialEq)]
pub struct GeneralSettings {
    /// An icon in the notification area (menu bar on macOS) with a quick menu.
    pub tray_icon: bool,
    /// "Delete note" in a note's menu needs a second click.
    pub confirm_delete: bool,
    /// Days a deleted note stays in the trash.
    pub trash_days: f32,
    /// Seconds the "Note deleted · Undo" toast stays up.
    pub undo_seconds: f32,
    /// Light or dark mode, or following the system.
    pub theme: crate::theme::Appearance,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            tray_icon: true,
            confirm_delete: true,
            trash_days: 30.0,
            undo_seconds: 6.0,
            theme: crate::theme::Appearance::Auto,
        }
    }
}

/// How notes and the dock relate to virtual desktops (Windows) and Spaces (macOS).
#[derive(Clone, Debug, PartialEq)]
pub struct DesktopSettings {
    /// Each note stays on the virtual desktop it was made on; off, notes show on all of them.
    /// Windows only does this for windows with a taskbar entry, so notes then share one grouped
    /// taskbar button.
    pub per_desktop: bool,
    /// The dock also lists notes from other desktops (in their own group).
    pub dock_shows_others: bool,
    /// Each desktop remembers its own active profile.
    pub profile_per_desktop: bool,
}

impl Default for DesktopSettings {
    fn default() -> Self {
        Self { per_desktop: true, dock_shows_others: false, profile_per_desktop: true }
    }
}

macro_rules! actions {
    ($name:ident { $($variant:ident = $key:literal, $default:literal, $doc:literal;)* }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $name { $($variant,)* }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant,)*];
            pub fn key(self) -> &'static str { match self { $($name::$variant => $key,)* } }
            pub fn default_binding(self) -> &'static str { match self { $($name::$variant => $default,)* } }
            pub fn doc(self) -> &'static str { match self { $($name::$variant => $doc,)* } }
        }
    };
}

actions!(GlobalAction {
    NewNote = "new_note", "CmdOrCtrl+Alt+N", "Create a note from anywhere";
    PasteAsNote = "paste_as_note", "CmdOrCtrl+Alt+V", "Turn the clipboard into a new note";
    ToggleNotes = "toggle_notes", "CmdOrCtrl+Alt+H", "Hide or show every note";
    OpenDock = "open_dock", "CmdOrCtrl+Alt+D", "Slide the dock out (or tuck it away)";
    NextProfile = "next_profile", "CmdOrCtrl+Alt+P", "Switch to the next profile";
    OpenSettings = "open_settings", "CmdOrCtrl+Alt+Comma", "Open this settings file";
});

actions!(NoteAction {
    NewNote = "new_note", "CmdOrCtrl+N", "New note";
    CloseNote = "close_note", "CmdOrCtrl+W", "Hide this note (reopen it from the dock)";
    DeleteNote = "delete_note", "CmdOrCtrl+D", "Move this note to the trash";
    Quit = "quit", "CmdOrCtrl+Q", "Quit Stick-itt";
    CycleMode = "cycle_mode", "CmdOrCtrl+T", "Window: normal -> always on top -> on desktop (behind windows)";
    PickColor = "pick_color", "CmdOrCtrl+Shift+K", "Choose a colour";
    NextColor = "next_color", "CmdOrCtrl+Shift+C", "Next colour";
    PickIcon = "pick_icon", "CmdOrCtrl+Shift+I", "Choose an icon";
    MoveToList = "move_to_list", "CmdOrCtrl+Shift+L", "Open the note menu (list, view, delete)";
    ToggleCompact = "toggle_compact", "CmdOrCtrl+Shift+M", "Compact view: one line with the icon and title";
    ToggleTodo = "toggle_todo", "CmdOrCtrl+Enter", "Tick the to-do on this line (or make one)";
    Bold = "bold", "CmdOrCtrl+B", "Bold";
    Italic = "italic", "CmdOrCtrl+I", "Italic";
    InlineCode = "inline_code", "CmdOrCtrl+E", "Inline code";
    Strikethrough = "strikethrough", "CmdOrCtrl+Shift+X", "Strikethrough";
    Highlight = "highlight", "CmdOrCtrl+Shift+H", "Highlight (==text==, or ==red:text== for a colour)";
    AddReminder = "add_reminder", "CmdOrCtrl+R", "Set a reminder (day, time, repeat)";
    ClearReminder = "clear_reminder", "CmdOrCtrl+Shift+R", "Clear the reminder";
    ZoomIn = "zoom_in", "CmdOrCtrl+Equal", "Larger text";
    ZoomOut = "zoom_out", "CmdOrCtrl+Minus", "Smaller text";
    ZoomReset = "zoom_reset", "CmdOrCtrl+0", "Reset text size";
    OpenSettings = "open_settings", "CmdOrCtrl+Comma", "Open this settings file";
});

/// A key chord matched against physical keys (layout-independent, like the global hotkeys).
#[derive(Clone, Debug, PartialEq)]
pub struct Chord {
    mods: Modifiers,
    /// W3C key code name, e.g. "KeyN" or "Equal".
    code: String,
}

impl Chord {
    pub fn parse(s: &str) -> Option<Chord> {
        let hk: HotKey = s.trim().parse().ok()?;
        Some(Chord { mods: hk.mods, code: hk.key.to_string() })
    }

    pub fn matches(&self, ev: &KeyEvent, mods: ModifiersState) -> bool {
        let PhysicalKey::Code(code) = ev.physical_key else { return false };
        let held = [
            (Modifiers::CONTROL, mods.control_key()),
            (Modifiers::SHIFT, mods.shift_key()),
            (Modifiers::ALT, mods.alt_key()),
            (Modifiers::META, mods.super_key()),
        ];
        held.iter().all(|&(m, down)| self.mods.contains(m) == down) && format!("{code:?}") == self.code
    }
}

#[derive(Clone)]
pub struct Config {
    pub general: GeneralSettings,
    pub reminders: ReminderSettings,
    pub notes: NoteStyle,
    pub dock: DockStyle,
    pub desktops: DesktopSettings,
    /// Global hotkeys as written (parsed by the hotkey manager); empty = disabled.
    pub global: Vec<(GlobalAction, String)>,
    /// In-note shortcuts as written, and compiled.
    note_bindings: Vec<(NoteAction, String)>,
    pub note: Vec<(NoteAction, Chord)>,
    path: Option<PathBuf>,
    modified: Option<SystemTime>,
}

// ---- the settings table -----------------------------------------------------------------

struct Setting {
    section: &'static str,
    key: &'static str,
    doc: &'static str,
    get: fn(&Config) -> String,
    set: fn(&mut Config, &str),
}

/// Sections in file order, with the comment shown above each.
const SECTIONS: &[(&str, &str)] = &[
    ("general", "The app itself."),
    ("notes", "How notes look. Sizes are in pixels at 100% scaling."),
    (
        "notes.compact",
        "Compact view (note menu or Ctrl+Shift+M): the note as one line, icon and title. Click it to open it.",
    ),
    (
        "notes.fade",
        "Fading idle notes: after a while without focus or the pointer, a note turns see-through. Point at it to bring it back.",
    ),
    ("dock", "The dock on the screen edge: where it sits and when it opens."),
    ("dock.list", "The list views (compact and detailed): the cards."),
    ("dock.icons", "The icon view (Dock-style tiles)."),
    ("dock.tab", "The tab that peeks out while the dock is tucked away, and the edge strip that opens it."),
    (
        "reminders",
        "The reminder menu (clock button or Ctrl+R in a note). You can also type @3pm, @fri 9am, @daily 8am in a note.",
    ),
    ("desktops", "Virtual desktops (Windows) and Spaces (macOS)."),
];

fn num(value: &str, lo: f32, hi: f32) -> Option<f32> {
    value.trim().parse::<f32>().ok().map(|v| v.clamp(lo, hi))
}

fn flag(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

/// Short decimal form: 14.5, 38, 0.745.
fn fmt(v: f32) -> String {
    let s = format!("{:.3}", v);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn fraction(v: u8) -> String {
    fmt(v as f32 / 255.0)
}

fn to_byte(v: f32) -> u8 {
    (v * 255.0).round() as u8
}

/// A numeric setting stored as f32: `num!(section, key, lo, hi, path.to.field, doc)`.
macro_rules! num_setting {
    ($section:literal, $key:literal, $lo:expr, $hi:expr, $($field:ident).+, $doc:literal) => {
        Setting {
            section: $section,
            key: $key,
            doc: $doc,
            get: |c| fmt(c.$($field).+),
            set: |c, v| {
                if let Some(v) = num(v, $lo, $hi) {
                    c.$($field).+ = v;
                }
            },
        }
    };
}

const SETTINGS: &[Setting] = &[
    // [notes]
    Setting {
        section: "notes",
        key: "icon_position",
        doc: "bar = small, top-left in the top strip; page = large, above the text",
        get: |c| if c.notes.icon_position == IconPosition::Page { "page" } else { "bar" }.into(),
        set: |c, v| {
            c.notes.icon_position = if v.eq_ignore_ascii_case("page") { IconPosition::Page } else { IconPosition::Bar }
        },
    },
    num_setting!("notes", "icon_size", 10.0, 64.0, notes.icon_size, ""),
    num_setting!(
        "notes",
        "top_bar",
        28.0,
        80.0,
        notes.top_bar,
        "height of the top strip (icon, reminder, hover controls)"
    ),
    num_setting!("notes", "gap", 0.0, 60.0, notes.gap, "space between the top strip (or page icon) and the text"),
    num_setting!("notes", "padding", 4.0, 60.0, notes.padding, "space left and right of the text"),
    num_setting!("notes", "bottom_padding", 0.0, 60.0, notes.bottom_padding, ""),
    num_setting!("notes", "radius", 0.0, 40.0, notes.radius, "corner radius"),
    num_setting!("notes", "font_size", 9.0, 32.0, notes.font_size, ""),
    num_setting!("notes", "line_height", 1.0, 2.5, notes.line_height, "multiple of the font size"),
    num_setting!("notes", "shadow", 0.0, 2.0, notes.shadow, "0 = none, 2 = strong"),
    num_setting!("notes", "cover", 0.0, 1.0, notes.cover, "colour fading in from the top: 0 = plain, 1 = full"),
    Setting {
        section: "notes",
        key: "default_color",
        doc: "colour of new notes: cycle (a different one each time), random, a name (lavender, cobalt...) or a hue 0-359",
        get: |c| match c.notes.default_color {
            NewNoteColor::Cycle => "cycle".into(),
            NewNoteColor::Random => "random".into(),
            NewNoteColor::Fixed(color) => match crate::theme::custom_hue(color) {
                Some(hue) => format!("{hue}"),
                None => crate::theme::color_name(color).to_lowercase(),
            },
        },
        set: |c, v| {
            let v = v.trim();
            if v.eq_ignore_ascii_case("cycle") {
                c.notes.default_color = NewNoteColor::Cycle;
            } else if v.eq_ignore_ascii_case("random") {
                c.notes.default_color = NewNoteColor::Random;
            } else if let Some(p) = crate::theme::preset_named(v) {
                c.notes.default_color = NewNoteColor::Fixed(p);
            } else if let Ok(hue) = v.parse::<u16>() {
                c.notes.default_color = NewNoteColor::Fixed(crate::theme::CUSTOM + hue % 360);
            }
        },
    },
    num_setting!(
        "notes",
        "appear_scale",
        0.5,
        1.0,
        notes.appear_scale,
        "size a note grows from as it appears; 1 = fade only"
    ),
    Setting {
        section: "notes",
        key: "table_buttons",
        doc: "+ buttons on a table's edges to add a row or a column (its menus can too)",
        get: |c| c.notes.table_buttons.to_string(),
        set: |c, v| c.notes.table_buttons = flag(v).unwrap_or(c.notes.table_buttons),
    },
    // [notes.compact]
    num_setting!("notes.compact", "height", 24.0, 80.0, notes.compact.height, "height of the one-line note"),
    num_setting!("notes.compact", "min_width", 60.0, 1000.0, notes.compact.min_width, "narrowest it gets"),
    num_setting!(
        "notes.compact",
        "max_width",
        80.0,
        1600.0,
        notes.compact.max_width,
        "widest it gets; longer titles end in ..."
    ),
    num_setting!("notes.compact", "font_size", 9.0, 32.0, notes.compact.font_size, ""),
    num_setting!("notes.compact", "icon_size", 8.0, 40.0, notes.compact.icon_size, ""),
    num_setting!("notes.compact", "padding", 4.0, 40.0, notes.compact.padding, "space at either end"),
    num_setting!("notes.compact", "radius", 0.0, 40.0, notes.compact.radius, "corner radius (capped at a full pill)"),
    num_setting!(
        "notes.compact",
        "tint",
        0.0,
        1.0,
        notes.compact.tint,
        "how much of the note's colour shows, 0 = plain"
    ),
    Setting {
        section: "notes.compact",
        key: "show_progress",
        doc: "show 2/5 for notes with to-dos",
        get: |c| c.notes.compact.show_progress.to_string(),
        set: |c, v| c.notes.compact.show_progress = flag(v).unwrap_or(c.notes.compact.show_progress),
    },
    Setting {
        section: "notes.compact",
        key: "collapse_on_blur",
        doc: "an opened compact note folds back to one line when you click elsewhere",
        get: |c| c.notes.compact.collapse_on_blur.to_string(),
        set: |c, v| c.notes.compact.collapse_on_blur = flag(v).unwrap_or(c.notes.compact.collapse_on_blur),
    },
    // [notes.fade]
    Setting {
        section: "notes.fade",
        key: "enabled",
        doc: "fade notes you haven't used for a while",
        get: |c| c.notes.fade.enabled.to_string(),
        set: |c, v| c.notes.fade.enabled = flag(v).unwrap_or(c.notes.fade.enabled),
    },
    num_setting!(
        "notes.fade",
        "after",
        1.0,
        3600.0,
        notes.fade.after,
        "seconds without focus or the pointer before a note fades"
    ),
    num_setting!("notes.fade", "opacity", 0.05, 1.0, notes.fade.opacity, "how visible a faded note stays, 0.05 - 1"),
    Setting {
        section: "notes.fade",
        key: "which",
        doc: "on_top = only always-on-top notes, all = every note",
        get: |c| match c.notes.fade.which {
            FadeWhich::OnTop => "on_top".into(),
            FadeWhich::All => "all".into(),
        },
        set: |c, v| match v.trim().to_ascii_lowercase().replace(['-', ' '], "_").as_str() {
            "on_top" | "top" | "ontop" => c.notes.fade.which = FadeWhich::OnTop,
            "all" | "every" => c.notes.fade.which = FadeWhich::All,
            _ => {}
        },
    },
    Setting {
        section: "notes.fade",
        key: "compact",
        doc: "compact (one-line) notes fade too",
        get: |c| c.notes.fade.compact.to_string(),
        set: |c, v| c.notes.fade.compact = flag(v).unwrap_or(c.notes.fade.compact),
    },
    // [dock]
    Setting {
        section: "dock",
        key: "side",
        doc: "left or right edge (dragging the dock also sets this)",
        get: |c| if c.dock.side == Side::Left { "left" } else { "right" }.into(),
        set: |c, v| c.dock.side = if v.eq_ignore_ascii_case("left") { Side::Left } else { Side::Right },
    },
    num_setting!("dock", "position", 0.0, 1.0, dock.position, "0 = top, 0.5 = middle, 1 = bottom"),
    Setting {
        section: "dock",
        key: "open_delay_ms",
        doc: "resting time on the edge before the dock opens",
        get: |c| c.dock.open_delay_ms.to_string(),
        set: |c, v| {
            if let Some(v) = num(v, 0.0, 3000.0) {
                c.dock.open_delay_ms = v as u64;
            }
        },
    },
    num_setting!("dock", "edge_gap", 0.0, 60.0, dock.edge_gap, "distance from the screen edge"),
    // [dock.list]
    num_setting!("dock.list", "width", 140.0, 520.0, dock.width, "card width (also Ctrl+mouse wheel over the dock)"),
    num_setting!("dock.list", "card_gap", 0.0, 40.0, dock.card_gap, "space between cards"),
    num_setting!("dock.list", "row_padding", 0.0, 20.0, dock.row_padding, "space above and below the text in a card"),
    num_setting!("dock.list", "font_size", 10.0, 22.0, dock.font_size, ""),
    num_setting!("dock.list", "icon_size", 10.0, 32.0, dock.icon_size, ""),
    num_setting!("dock.list", "radius", 0.0, 40.0, dock.radius, "card corner radius"),
    Setting {
        section: "dock.list",
        key: "opacity",
        doc: "0.5 - 1",
        get: |c| fraction(c.dock.opacity),
        set: |c, v| {
            if let Some(v) = num(v, 0.5, 1.0) {
                c.dock.opacity = to_byte(v);
            }
        },
    },
    num_setting!("dock.list", "hover_push", 0.0, 80.0, dock.hover_push, "how far a hovered card slides out (0 = off)"),
    Setting {
        section: "dock.list",
        key: "list_headers",
        doc: "how lists are labelled when the dock shows more than one:\n\
              #     pill      a small floating label above each list (the default)\n\
              #     card      a card in the column, the size of a note, that slides out like one\n\
              #     caption   quiet text above each list\n\
              #     line      the list's name followed by a thin rule across the column\n\
              #     none      no labels; the lists simply follow one another",
        get: |c| c.dock.list_headers.name().into(),
        set: |c, v| c.dock.list_headers = ListHeaders::from_name(v).unwrap_or(c.dock.list_headers),
    },
    // [dock.icons]
    num_setting!("dock.icons", "tile_size", 16.0, 96.0, dock.tile_size, "also Ctrl+mouse wheel over the dock"),
    num_setting!("dock.icons", "tile_padding", 2.0, 30.0, dock.tile_padding, "space around the tiles"),
    num_setting!("dock.icons", "tile_gap", 0.0, 30.0, dock.tile_gap, "space between tiles"),
    num_setting!("dock.icons", "magnify", 1.0, 2.5, dock.magnify, "magnification under the pointer (1 = off)"),
    // [dock.tab]
    Setting {
        section: "dock.tab",
        key: "tab_color",
        doc: "auto (follows light/dark) or a colour like #4F8EF7",
        get: |c| c.dock.tab_color.map_or("auto".into(), |Rgb(r, g, b)| format!("#{r:02X}{g:02X}{b:02X}")),
        set: |c, v| c.dock.tab_color = parse_hex(v),
    },
    num_setting!("dock.tab", "tab_width", 0.0, 24.0, dock.tab_width, ""),
    num_setting!("dock.tab", "tab_height", 0.0, 400.0, dock.tab_height, ""),
    Setting {
        section: "dock.tab",
        key: "tab_opacity",
        doc: "0 = invisible tab",
        get: |c| fraction(c.dock.tab_opacity),
        set: |c, v| {
            if let Some(v) = num(v, 0.0, 1.0) {
                c.dock.tab_opacity = to_byte(v);
            }
        },
    },
    num_setting!(
        "dock.tab",
        "trigger_width",
        1.0,
        40.0,
        dock.trigger_width,
        "how close to the edge the pointer must be"
    ),
    num_setting!(
        "dock.tab",
        "trigger_height",
        20.0,
        2000.0,
        dock.trigger_height,
        "length of the edge strip that opens the dock"
    ),
    // [general]
    Setting {
        section: "general",
        key: "theme",
        doc: "auto (follows the system), light or dark",
        get: |c| {
            use crate::theme::Appearance;
            match c.general.theme {
                Appearance::Auto => "auto",
                Appearance::Light => "light",
                Appearance::Dark => "dark",
            }
            .into()
        },
        set: |c, v| {
            use crate::theme::Appearance;
            match v.to_ascii_lowercase().as_str() {
                "auto" | "system" => c.general.theme = Appearance::Auto,
                "light" => c.general.theme = Appearance::Light,
                "dark" => c.general.theme = Appearance::Dark,
                _ => {}
            }
        },
    },
    Setting {
        section: "general",
        key: "tray_icon",
        doc: "show an icon in the notification area with a quick menu",
        get: |c| c.general.tray_icon.to_string(),
        set: |c, v| c.general.tray_icon = flag(v).unwrap_or(c.general.tray_icon),
    },
    Setting {
        section: "general",
        key: "confirm_delete",
        doc: "\"Delete note\" in a note's menu asks for a second click",
        get: |c| c.general.confirm_delete.to_string(),
        set: |c, v| c.general.confirm_delete = flag(v).unwrap_or(c.general.confirm_delete),
    },
    num_setting!("general", "trash_days", 1.0, 3650.0, general.trash_days, "days a deleted note stays in the trash"),
    num_setting!(
        "general",
        "undo_seconds",
        2.0,
        60.0,
        general.undo_seconds,
        "how long the \"Note deleted - Undo\" toast stays"
    ),
    // [reminders]
    Setting {
        section: "reminders",
        key: "times",
        doc: "times offered for the chosen day (up to 6)",
        get: |c| c.reminders.times.iter().map(|t| crate::reminder::clock_setting(*t)).collect::<Vec<_>>().join(", "),
        set: |c, v| {
            if let Some(times) = list(v, 6, crate::reminder::clock) {
                c.reminders.times = times;
            }
        },
    },
    Setting {
        section: "reminders",
        key: "soon",
        doc: "\"in ...\" choices, e.g. 10m, 1h, 1h30m (up to 4)",
        get: |c| c.reminders.soon.iter().map(|s| crate::reminder::duration_label(*s)).collect::<Vec<_>>().join(", "),
        set: |c, v| {
            if let Some(soon) = list(v, 4, crate::reminder::span) {
                c.reminders.soon = soon;
            }
        },
    },
    Setting {
        section: "reminders",
        key: "minute_step",
        doc: "minutes the -/+ buttons move the time by (1 - 60)",
        get: |c| c.reminders.minute_step.to_string(),
        set: |c, v| {
            if let Some(n) = num(v, 1.0, 60.0) {
                c.reminders.minute_step = n.round() as u32;
            }
        },
    },
    Setting {
        section: "reminders",
        key: "sound",
        doc: "play a short sound when a reminder rings",
        get: |c| c.reminders.sound.to_string(),
        set: |c, v| c.reminders.sound = flag(v).unwrap_or(c.reminders.sound),
    },
    // [desktops]
    Setting {
        section: "desktops",
        key: "per_desktop",
        doc: "notes stay on their own desktop; off, they show on all (Windows: one taskbar button)",
        get: |c| c.desktops.per_desktop.to_string(),
        set: |c, v| c.desktops.per_desktop = flag(v).unwrap_or(c.desktops.per_desktop),
    },
    Setting {
        section: "desktops",
        key: "dock_shows_other_desktops",
        doc: "the dock also lists notes from other desktops",
        get: |c| c.desktops.dock_shows_others.to_string(),
        set: |c, v| c.desktops.dock_shows_others = flag(v).unwrap_or(c.desktops.dock_shows_others),
    },
    Setting {
        section: "desktops",
        key: "profile_per_desktop",
        doc: "each desktop remembers its own profile",
        get: |c| c.desktops.profile_per_desktop.to_string(),
        set: |c, v| c.desktops.profile_per_desktop = flag(v).unwrap_or(c.desktops.profile_per_desktop),
    },
];

/// The file is missing a section or a key that this version knows about.
fn lacks_settings(text: &str) -> bool {
    let keys: std::collections::HashSet<&str> =
        text.lines().filter_map(|l| l.split_once('=').map(|(k, _)| k.trim())).collect();
    SECTIONS.iter().any(|(name, _)| !text.contains(&format!("[{name}]")))
        || SETTINGS.iter().any(|st| !keys.contains(st.key))
        || GlobalAction::ALL.iter().any(|a| !keys.contains(a.key()))
        || NoteAction::ALL.iter().any(|a| !keys.contains(a.key()))
}

/// Older section names still read correctly.
fn canonical_section(name: &str) -> &str {
    match name {
        "note_style" => "notes",
        "global" => "shortcuts.global",
        "note" => "shortcuts.note",
        other => other,
    }
}

impl Config {
    fn defaults() -> Config {
        Config {
            general: GeneralSettings::default(),
            reminders: ReminderSettings::default(),
            notes: NoteStyle::default(),
            dock: DockStyle::default(),
            desktops: DesktopSettings::default(),
            global: GlobalAction::ALL.iter().map(|a| (*a, a.default_binding().to_string())).collect(),
            note_bindings: NoteAction::ALL.iter().map(|a| (*a, a.default_binding().to_string())).collect(),
            note: Vec::new(),
            path: None,
            modified: None,
        }
    }

    /// Loads `path`. A missing file is created; one in an older layout is rewritten in the
    /// current layout with the same values (the original is kept as `stickit.ini.bak`).
    pub fn load(path: Option<PathBuf>) -> Config {
        let text = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok());
        let mut cfg = Config::parse(text.as_deref().unwrap_or(""));
        if let Some(p) = &path {
            match &text {
                None => {
                    let _ = std::fs::write(p, cfg.render());
                }
                // An older layout, or settings added since the file was written: rewrite it with
                // the same values so every option is listed.
                Some(t) if !t.contains(FORMAT_MARKER) || lacks_settings(t) => {
                    let _ = std::fs::copy(p, p.with_extension("ini.bak"));
                    let _ = std::fs::write(p, cfg.render());
                }
                _ => {}
            }
        }
        cfg.modified = path.as_deref().and_then(modified);
        cfg.path = path;
        cfg
    }

    /// Writes the settings back to the file (after a change made in the app, e.g. resizing the
    /// dock with Ctrl+wheel). Values the user wrote are kept; comments are regenerated.
    pub fn save(&mut self) {
        let Some(path) = self.path.clone() else { return };
        if std::fs::write(&path, self.render()).is_ok() {
            self.modified = modified(&path);
        }
    }

    /// Re-reads the file if it changed on disk. Returns true when settings changed.
    pub fn reload_if_changed(&mut self) -> bool {
        let Some(path) = self.path.clone() else { return false };
        if modified(&path) == self.modified {
            return false;
        }
        let fresh = Config::load(Some(path));
        let changed = fresh.render() != self.render();
        *self = fresh;
        changed
    }

    /// Opens the settings file in the user's default editor.
    pub fn open_in_editor(&self) {
        let Some(path) = &self.path else { return };
        if !path.exists() {
            let _ = std::fs::write(path, self.render());
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::UI::Shell::ShellExecuteW;
            use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
            let wide = |s: &std::ffi::OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
            let (verb, file) = (wide("open".as_ref()), wide(path.as_os_str()));
            unsafe {
                ShellExecuteW(0, verb.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL);
            }
        }
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open").arg("-t").arg(path).spawn();
        #[cfg(all(unix, not(target_os = "macos")))]
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }

    pub fn parse(text: &str) -> Config {
        let mut cfg = Config::defaults();
        let mut section = String::new();
        for line in text.lines() {
            let line = strip_comment(line).trim();
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = canonical_section(&name.trim().to_lowercase()).to_string();
                continue;
            }
            let Some((key, value)) = line.split_once('=') else { continue };
            let (key, value) = (key.trim().to_lowercase(), value.trim());
            match section.as_str() {
                "shortcuts.global" => {
                    if let Some(slot) = cfg.global.iter_mut().find(|(a, _)| a.key() == key) {
                        slot.1 = value.to_string();
                    }
                }
                "shortcuts.note" => {
                    if let Some(slot) = cfg.note_bindings.iter_mut().find(|(a, _)| a.key() == key) {
                        slot.1 = value.to_string();
                    }
                }
                // A key is looked up in its section's family ("dock" covers dock.list etc.), so
                // files written before the dock was split into sub-sections still work.
                s => {
                    let family = s.split('.').next().unwrap_or(s);
                    // The section itself wins: [notes.compact] padding is not [notes] padding.
                    let found = SETTINGS.iter().find(|st| st.key == key && st.section == s).or_else(|| {
                        let legacy = s == family;
                        SETTINGS
                            .iter()
                            .find(|st| legacy && st.key == key && st.section.split('.').next() == Some(family))
                    });
                    if let Some(st) = found {
                        (st.set)(&mut cfg, value);
                    }
                }
            }
        }
        cfg.note = cfg.note_bindings.iter().filter_map(|(a, s)| Some((*a, Chord::parse(s)?))).collect();
        cfg
    }

    /// The whole file, in the current layout, with these values.
    pub fn render(&self) -> String {
        let mut out = format!(
            "{FORMAT_MARKER}\n\
             # Stick-itt settings. Saved changes apply within a second while this file is open from\n\
             # \"Settings...\" (and otherwise the next time the dock opens).\n\n"
        );
        let line = |key: &str, value: &str, doc: &str| {
            if doc.is_empty() {
                format!("{key:<26}= {value}\n")
            } else {
                format!("{key:<26}= {value:<10} # {doc}\n")
            }
        };
        for (section, about) in SECTIONS {
            out.push_str(&format!("[{section}]\n# {about}\n"));
            for st in SETTINGS.iter().filter(|st| st.section == *section) {
                out.push_str(&line(st.key, &(st.get)(self), st.doc));
            }
            out.push('\n');
        }
        out.push_str(
            "# ---- Shortcuts ------------------------------------------------------------------\n\
             # Modifiers: Ctrl, Shift, Alt, Cmd/Super, CmdOrCtrl. Keys: A-Z, 0-9, F1-F24, Enter, Tab,\n\
             # Space, Escape, Backspace, Delete, ArrowUp/Down/Left/Right, Equal, Minus, Comma, Period,\n\
             # Slash, Backquote, BracketLeft/Right. Leave a shortcut empty to turn it off.\n\n\
             [shortcuts.global]\n# Work in any app.\n",
        );
        for (a, binding) in &self.global {
            out.push_str(&line(a.key(), binding, a.doc()));
        }
        out.push_str("\n[shortcuts.note]\n# Inside a note.\n");
        for (a, binding) in &self.note_bindings {
            out.push_str(&line(a.key(), binding, a.doc()));
        }
        out
    }

    pub fn note_action(&self, ev: &KeyEvent, mods: ModifiersState) -> Option<NoteAction> {
        self.note.iter().find(|(_, c)| c.matches(ev, mods)).map(|(a, _)| *a)
    }
}

/// Drops a trailing comment: `#` or `;` at the start of the line or after whitespace. The
/// first character of a value is never a comment, so `tab_color = #4F8EF7` survives.
fn strip_comment(line: &str) -> &str {
    let value_start = line.find('=').map(|i| {
        let rest = &line[i + 1..];
        i + 1 + (rest.len() - rest.trim_start().len())
    });
    let b = line.as_bytes();
    let cut = (0..b.len())
        .find(|&i| matches!(b[i], b'#' | b';') && (i == 0 || b[i - 1].is_ascii_whitespace()) && Some(i) != value_start);
    &line[..cut.unwrap_or(line.len())]
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// "#RRGGBB" (or "RRGGBB"); anything else, e.g. "auto", means None.
fn parse_hex(value: &str) -> Option<Rgb> {
    let hex = value.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some(Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_roundtrip_through_the_generated_file() {
        let cfg = Config::parse(&Config::defaults().render());
        assert_eq!(cfg.dock, DockStyle::default());
        assert_eq!(cfg.notes, NoteStyle::default());
        assert_eq!(cfg.desktops, DesktopSettings::default());
        assert_eq!(cfg.general, GeneralSettings::default());
        assert_eq!(cfg.reminders, ReminderSettings::default());
        assert_eq!(cfg.note.len(), NoteAction::ALL.len());
        assert_eq!(cfg.global.len(), GlobalAction::ALL.len());
    }

    #[test]
    fn new_settings_are_noticed() {
        assert!(!lacks_settings(&Config::defaults().render()));
        let without = Config::defaults().render().replace("toggle_compact", "x");
        assert!(lacks_settings(&without));
    }

    #[test]
    fn values_survive_a_rewrite() {
        let text = "[dock.list]\nwidth = 300\n[dock.tab]\ntab_color = #4F8EF7 # blue\n[desktops]\nper_desktop = off\n\
                    [shortcuts.note]\nbold = Ctrl+Shift+B\n";
        let cfg = Config::parse(text);
        let again = Config::parse(&cfg.render());
        assert_eq!(again.dock.width, 300.0);
        assert_eq!(again.dock.tab_color, Some(Rgb(0x4F, 0x8E, 0xF7)));
        assert!(!again.desktops.per_desktop);
        let bold = &again.note.iter().find(|(a, _)| *a == NoteAction::Bold).unwrap().1;
        assert_eq!(bold, &Chord::parse("Ctrl+Shift+KeyB").unwrap());
    }

    #[test]
    fn old_layout_still_reads() {
        // Format 1: flat [dock], [note_style], [global], [note].
        let cfg = Config::parse(
            "[dock]\nside = left\nwidth = 9999\nopacity = 0.8\ncard_gap = 12\n[note_style]\ngap = 9\n\
             [note]\nitalic =\n[global]\nnew_note = Ctrl+Alt+J\n",
        );
        assert_eq!(cfg.dock.side, Side::Left);
        assert_eq!(cfg.dock.width, 520.0, "clamped");
        assert_eq!(cfg.dock.opacity, 204);
        assert_eq!(cfg.dock.card_gap, 12.0);
        assert_eq!(cfg.dock.list_headers, ListHeaders::Pill);
        assert_eq!(cfg.notes.gap, 9.0);
        assert!(cfg.note.iter().all(|(a, _)| *a != NoteAction::Italic), "empty value disables");
        assert_eq!(cfg.global[0].1, "Ctrl+Alt+J");
    }

    #[test]
    fn every_setting_has_a_known_section() {
        for st in SETTINGS {
            assert!(SECTIONS.iter().any(|(s, _)| *s == st.section), "{}", st.key);
        }
    }

    #[test]
    fn reminder_lists_parse_and_reject_bad_input() {
        let cfg = Config::parse("[reminders]\ntimes = 8am, 1:30pm, 17:00\nsoon = 10m, 1h30m\n");
        assert_eq!(cfg.reminders.times.len(), 3);
        assert_eq!(cfg.reminders.soon, vec![600, 5400]);
        assert_eq!(Config::parse(&cfg.render()).reminders, cfg.reminders);
        // Anything unreadable keeps the defaults.
        let bad = Config::parse("[reminders]\ntimes = 8am, lunch\nsoon =\n");
        assert_eq!(bad.reminders, ReminderSettings::default());
    }

    #[test]
    fn list_header_styles() {
        for (style, name) in ListHeaders::ALL {
            let cfg = Config::parse(&format!("[dock.list]\nlist_headers = {name}\n"));
            assert_eq!(cfg.dock.list_headers, style);
            let again = Config::parse(&cfg.render());
            assert_eq!(again.dock.list_headers, style, "{name} survives a rewrite");
            // Every choice is listed in the file's comments.
            assert!(cfg.render().contains(&format!("#     {name} ")));
        }
        let cfg = Config::parse("[dock.list]\nlist_headers = sparkly\n");
        assert_eq!(cfg.dock.list_headers, ListHeaders::Pill, "unknown names keep the default");
    }

    #[test]
    fn fade_settings() {
        assert!(!Config::defaults().notes.fade.enabled, "fading is opt-in");
        let cfg = Config::parse("[notes.fade]\nenabled = yes\nafter = 12\nopacity = 0.5\nwhich = all\ncompact = off\n");
        let want = FadeStyle { enabled: true, after: 12.0, opacity: 0.5, which: FadeWhich::All, compact: false };
        assert_eq!(cfg.notes.fade, want);
        assert_eq!(Config::parse(&cfg.render()).notes.fade, want);
        // Out of range values are clamped; unknown words keep the default.
        let cfg = Config::parse("[notes.fade]\nafter = 0\nopacity = 3\nwhich = sometimes\n");
        assert_eq!((cfg.notes.fade.after, cfg.notes.fade.opacity, cfg.notes.fade.which), (1.0, 1.0, FadeWhich::OnTop));
        assert_eq!(Config::parse("[notes.fade]\nwhich = On-Top\n").notes.fade.which, FadeWhich::OnTop);
    }

    #[test]
    fn default_note_colour_by_name_or_hue() {
        let parse = |v: &str| Config::parse(&format!("[notes]\ndefault_color = {v}\n")).notes.default_color;
        assert_eq!(Config::defaults().notes.default_color, NewNoteColor::Cycle);
        assert_eq!(parse("Random"), NewNoteColor::Random);
        assert_eq!(parse("cobalt"), NewNoteColor::Fixed(18));
        assert_eq!(parse("200"), NewNoteColor::Fixed(crate::theme::CUSTOM + 200));
        // Unknown names keep the default; every form survives a rewrite.
        assert_eq!(parse("plaid"), NewNoteColor::Cycle);
        for v in ["cycle", "random", "lavender", "200"] {
            let cfg = Config::parse(&format!("[notes]\ndefault_color = {v}\n"));
            assert_eq!(Config::parse(&cfg.render()).notes.default_color, cfg.notes.default_color, "{v}");
        }
    }

    #[test]
    fn subsection_keys_stay_in_their_section() {
        let cfg = Config::parse("[notes]\npadding = 30\n[notes.compact]\npadding = 6\nmax_width = 320\n");
        assert_eq!(cfg.notes.padding, 30.0);
        assert_eq!(cfg.notes.compact.padding, 6.0);
        assert_eq!(cfg.notes.compact.max_width, 320.0);
    }

    #[test]
    fn numbers_are_clamped_and_flags_parsed() {
        let cfg = Config::parse("[notes.compact]\nheight = 5000\n[general]\nundo_seconds = 0\ntray_icon = off\n");
        assert_eq!(cfg.notes.compact.height, 80.0);
        assert_eq!(cfg.general.undo_seconds, 2.0);
        assert!(!cfg.general.tray_icon);
    }

    #[test]
    fn theme_is_auto_light_or_dark() {
        use crate::theme::Appearance;
        assert_eq!(Config::parse("").general.theme, Appearance::Auto);
        assert_eq!(Config::parse("[general]\ntheme = Light\n").general.theme, Appearance::Light);
        assert_eq!(Config::parse("[general]\ntheme = dark\n").general.theme, Appearance::Dark);
        assert_eq!(Config::parse("[general]\ntheme = purple\n").general.theme, Appearance::Auto);
    }
}
