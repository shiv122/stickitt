//! A single sticky note: a borderless native window that lays out and paints its own text.

use crate::config::{Config, IconPosition, NoteAction, NoteStyle, ReminderSettings};
use crate::editor::{Doc, EditKind};
use crate::gfx::{self, Clip};
use crate::markdown::{self, is_table_separator, line_info, table_cells, table_pipes, Flags};
use crate::motion::{Feel, Motion};
use crate::popover::{self, SlashKind, ICON_TABS, SLASH};
use crate::reminder::{self, Reminder, Repeat};
use crate::store::{List, NoteRecord};
use crate::surface::{self, Surface};
use crate::theme::{self, palette, Palette, Rgb};
use chrono::Local;
use cosmic_text::{
    Attrs, AttrsList, Buffer, BufferLine, Cursor, Ellipsize, EllipsizeHeightLimit, Family, FontSystem, LineEnding,
    Metrics, Scroll, Shaping, Style, SwashCache, UnderlineStyle, Weight, Wrap,
};
use std::error::Error;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tiny_skia::PixmapMut;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, ResizeDirection, UserAttentionType, Window, WindowLevel};

// Layout, in logical pixels.
/// Transparent space around the card for its shadow (only where we draw our own shadow).
const MARGIN: (f32, f32, f32) = if surface::ALPHA { (22.0, 16.0, 30.0) } else { (0.0, 0.0, 0.0) }; // sides, top, bottom
const EDGE: f32 = 7.0;
const CARD_SIZE: (f64, f64) = (280.0, 250.0);
pub const DEFAULT_SIZE: (f64, f64) = (CARD_SIZE.0 + 2.0 * MARGIN.0 as f64, CARD_SIZE.1 + (MARGIN.1 + MARGIN.2) as f64);
const MIN_SIZE: (f64, f64) = (180.0 + 2.0 * MARGIN.0 as f64, 120.0 + (MARGIN.1 + MARGIN.2) as f64);

const BLINK: Duration = Duration::from_millis(530);
/// Stop blinking after this much inactivity so an idle note costs zero CPU.
const BLINK_IDLE: Duration = Duration::from_secs(20);
const NUDGE_EVERY: Duration = Duration::from_secs(45);
/// A ringing note also wiggles this long after it starts, before settling into `NUDGE_EVERY`.
const RING_NUDGES: [Duration; 2] = [Duration::from_secs(4), Duration::from_secs(12)];
/// One breath of a ringing note's glow, and how often it repaints.
const PULSE: f32 = 1.6;
const PULSE_FRAME: Duration = Duration::from_millis(33);
const FRAME: Duration = Duration::from_millis(16);
/// Space either side of a table cell's text, and the height the `|---|` row shrinks to.
const TABLE_PAD: f32 = 10.0;
const TABLE_RULE: f32 = 1.0;
/// Table rows are this much taller than a line of text, and columns at least this wide (Notion's
/// roomy, light grid).
const TABLE_ROW: f32 = 11.0;
/// The note tint behind each of `markdown::HIGHLIGHTS` (Notion's background colours).
const HIGHLIGHT_TINT: [u16; 9] = [0, 5, 8, 6, 3, 2, 4, 1, 7];
const TABLE_MIN_COL: f32 = 64.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Normal,
    OnTop,
    Desktop,
}

impl Mode {
    pub fn from_u8(v: u8) -> Mode {
        match v {
            1 => Mode::OnTop,
            2 => Mode::Desktop,
            _ => Mode::Normal,
        }
    }

    fn next(self) -> Mode {
        match self {
            Mode::Normal => Mode::OnTop,
            Mode::OnTop => Mode::Desktop,
            Mode::Desktop => Mode::Normal,
        }
    }

    fn level(self) -> WindowLevel {
        match self {
            Mode::Normal => WindowLevel::Normal,
            Mode::OnTop => WindowLevel::AlwaysOnTop,
            Mode::Desktop => WindowLevel::AlwaysOnBottom,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    CycleMode,
    CycleColor,
    Close,
    Snooze(i64),
    Dismiss,
    ClearReminder,
    /// Open the icon picker.
    Icon,
    /// Open the note menu (list, view, delete).
    More,
    /// Open the reminder menu.
    Remind,
    /// Open the window-mode menu.
    Window,
    /// Open the colour menu.
    Colors,
}

/// Requests from a note that only the app can fulfil.
pub enum Cmd {
    NewNote,
    Close,
    /// Closed by the system (taskbar "Close window(s)", Alt+F4) rather than with ×.
    SystemClose,
    Delete,
    Quit,
    OpenSettings,
    /// Move the note to this virtual desktop (an id from `Desktops::list`).
    MoveToDesktop(String),
    /// A click on the note's shadow, outside its card, at this screen point (physical): it is
    /// meant for the note under it (notes' shadows overlap when they sit close together).
    PassClick(i32, i32),
}

pub struct Shared {
    pub fonts: FontSystem,
    pub swash: SwashCache,
    pub clipboard: Option<arboard::Clipboard>,
    pub config: Config,
    /// All lists, for the "move to list" popover.
    pub lists: Vec<List>,
    /// Window icon (taskbar, Alt-Tab).
    pub icon: Option<winit::window::Icon>,
}

#[derive(Clone, Copy)]
enum Icon {
    Close,
    Mode(Mode),
    Swatch(Rgb),
    Clock,
    SmallX,
    Smiley,
    More,
}

enum Draw {
    Nothing,
    Icon(Icon),
    Label(Buffer),
}

struct BarItem {
    rect: (f32, f32, f32, f32),
    action: Option<Action>,
    draw: Draw,
    color: Rgb,
    alpha: u8,
    /// Rounded background behind the item.
    pill: Option<(Rgb, u8)>,
    /// Only shown while the pointer is over the note.
    fade: bool,
}

/// Graphics standing in for markdown syntax. Background kinds paint under the text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Deco {
    Checkbox(bool),
    Bullet,
    CodePill,
    CodeBlock,
    Quote,
    Rule,
    /// A `==highlight==`: a soft badge in one of `markdown::HIGHLIGHTS`.
    Badge(u8),
    /// A callout (`> 💡 text`): a soft box behind the line.
    Callout,
}

impl Deco {
    fn is_background(self) -> bool {
        !matches!(self, Deco::Checkbox(_) | Deco::Bullet)
    }
}

/// A table as painted: each row's (top, height), and the column widths.
type TableDraw = (Vec<(f32, f32)>, Vec<f32>);

/// A Markdown table shown as a grid. `decorate` finds it; `style_tables` sizes its columns
/// (or leaves it as plain text while the caret is in it or it is too wide for the note).
#[derive(Clone, Debug, Default)]
struct Table {
    /// Each row's byte range in the display text; `rows[1]` is the `|---|` separator.
    rows: Vec<std::ops::Range<usize>>,
    /// Column widths in pixels; empty while the table is shown as plain text.
    cols: Vec<f32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Popover {
    /// The formatting toolbar over selected text; `colors` shows the badge colours.
    Format {
        colors: bool,
    },
    /// A table row's menu (the row as in `table::Grid`), or a column's.
    TableRow(u16),
    TableCol(u16),
    Icons,
    /// The note menu: move to list, compact view, delete.
    More,
    /// Day, time and repeat for a reminder.
    Remind,
    /// Normal / always on top / on desktop.
    Window,
    Colors,
    /// `:shortcode` completion; `at` is the offset of the colon.
    Emoji {
        at: usize,
        sel: usize,
    },
    /// The `/` block menu; `at` is the offset of the slash, `sel` the highlighted match.
    Slash {
        at: usize,
        sel: usize,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PopHit {
    Icon(&'static str),
    Tab(usize),
    RemoveIcon,
    Slash(usize),
    List(i64),
    Color(u16),
    /// The custom-hue strip.
    Hue,
    Shortcode(&'static str),
    Compact,
    Delete,
    /// Reminder menu: in N seconds, pick a day (offset), pick a time (index), repeat, remove.
    RemindIn(i64),
    RemindDay(i64),
    RemindTime(usize),
    /// The fine-tune row: move the pending time by this many minutes.
    RemindStep(i64),
    /// The fine-tune row's time: set the reminder for it.
    RemindSet,
    RemindRepeat(usize),
    RemindClear,
    /// Move the note to the desktop at this index of `desktop_list`.
    Desktop(u8),
    /// The formatting toolbar: Bold, Italic, Strikethrough, Code (by index).
    Fmt(u8),
    /// Opens or closes the toolbar's colours.
    FmtColors,
    /// A badge colour (an index into `markdown::HIGHLIGHTS`), or 255 for none.
    FmtColor(u8),
    /// A text colour (as `FmtColor`).
    FmtTextColor(u8),
    /// A table row or column menu item (see `table_menu_action`).
    TableOp(u8),
    /// Window mode.
    Mode(u8),
}

/// A popover laid out for drawing and hit-testing.
struct PopDraw {
    rect: (f32, f32, f32, f32),
    cells: Vec<((f32, f32, f32, f32), PopHit)>,
    /// Bordered tiles (slash items, the search field).
    tiles: Vec<(f32, f32, f32, f32)>,
    /// Soft highlighted backgrounds (the selected completion).
    selected: Vec<(f32, f32, f32, f32)>,
    /// Thin filled bars: the active tab's underline (accent) and dividers (faint).
    bars: Vec<((f32, f32, f32, f32), bool)>,
    /// Colour swatches: centre, radius, colour, current.
    dots: Vec<((f32, f32), f32, Rgb, bool)>,
    /// The custom-hue strip and the current custom hue, if any.
    hue: Option<(gfx::Rect, Option<f32>)>,
    labels: Vec<(Buffer, cosmic_text::Color, (f32, f32))>,
}

/// A resize in progress: the edge, the pointer's screen position at the press, and the
/// window's outer position and inner size then (physical px).
type ResizeStart = (ResizeDirection, (f64, f64), PhysicalPosition<i32>, PhysicalSize<u32>);

impl PopDraw {
    fn shift(&mut self, dy: f32) {
        let mv = |r: &mut (f32, f32, f32, f32)| r.1 += dy;
        mv(&mut self.rect);
        self.cells.iter_mut().for_each(|(r, _)| mv(r));
        self.tiles.iter_mut().for_each(mv);
        self.selected.iter_mut().for_each(mv);
        self.bars.iter_mut().for_each(|(r, _)| mv(r));
        self.dots.iter_mut().for_each(|(c, ..)| c.1 += dy);
        if let Some((r, _)) = self.hue.as_mut() {
            mv(r);
        }
        self.labels.iter_mut().for_each(|(_, _, at)| at.1 += dy);
    }
}

enum Zone {
    /// The transparent shadow margin around the card.
    Outside,
    Popover,
    Resize(ResizeDirection),
    Button(Action),
    Bar,
    Text,
}

pub struct Note {
    pub id: i64,
    pub window: Rc<Window>,
    surface: Surface,
    pub doc: Doc,
    /// A preset index or `theme::CUSTOM + hue`.
    pub color: u16,
    /// Page icon (an emoji), empty for none.
    pub icon: String,
    pub list: i64,
    /// Layout and look, from `[note_style]` in stickit.ini.
    st: NoteStyle,
    /// Icon picker state: search text, tab and first visible row.
    icon_query: String,
    icon_tab: usize,
    icon_row: usize,
    mode: Mode,
    zoom: u16,
    pub reminder: Option<Reminder>,
    pub ringing: bool,
    /// Text changed or geometry changed since the last save.
    pub dirty: bool,
    pub needs_paint: bool,
    /// Visible on screen (profiles and "hide all" hide notes without closing them).
    pub shown: bool,
    /// Hidden with × (persisted); reopened from the dock.
    pub user_hidden: bool,
    /// Last known virtual desktop. Windows forgets the desktop of a hidden window, so the dock
    /// uses this to file hidden notes under the right desktop.
    pub desktop: Option<String>,
    /// The desktops the note menu offers to move it to (id, name); kept up to date by the app.
    pub desktop_list: Vec<(String, String)>,
    /// Fading out after ×; the window is hidden once the fade ends.
    hiding: bool,
    /// Follows the system appearance.
    dark: bool,
    /// Last time the note had focus (the dock lists recent notes first).
    pub last_active: Instant,
    /// Last time the note was focused, pointed at or otherwise in use: `[notes.fade]` counts from here.
    in_use_at: Instant,
    /// Where the note last really was on screen (physical, top-left): what gets saved. A
    /// minimized window reports -32000, which must never be saved as its place.
    placed_at: Option<(i32, i32)>,

    /// The text, one buffer line per paragraph. Only lines near the view keep their shaping
    /// and layout (tens of bytes per glyph); the rest are freed and re-shaped when scrolled to.
    buffer: Buffer,
    /// Height of every paragraph once measured, so the note knows its full length (scrollbar,
    /// scroll limits, jumping to the caret) without keeping every line laid out.
    line_heights: Vec<Option<f32>>,
    /// Lines that may be holding shaping data.
    hot: Vec<bool>,
    /// (width, font px, line px) the heights were measured at.
    measured_for: (f32, f32, f32),
    layout_dirty: bool,
    layout_width: f32,
    line_starts: Vec<usize>,
    /// Byte length of the display text (doc text + preedit, without the layout sentinel).
    display_len: usize,
    preedit_len: usize,
    content_h: f32,
    scroll: f32,
    /// The page's scroll position when it folded into a compact line, restored when it opens.
    page_scroll: Option<f32>,
    goal_x: Option<f32>,
    decos: Vec<(std::ops::Range<usize>, Deco)>,
    tables: Vec<Table>,
    /// Tables as painted (window coordinates) with the display offset of their first row, and
    /// the one the pointer is near (it shows + buttons to add a row or a column).
    table_rects: Vec<(gfx::Rect, usize)>,
    table_hover: Option<usize>,
    /// Each painted table's rows (window y, height) and column widths, the cell the pointer is
    /// over (row as in `table::Grid`, column), its row and column handles (with the table's
    /// first-row offset, row, column), and the table whose menu is open (with its handle).
    table_geo: Vec<TableDraw>,
    table_cell: Option<(usize, usize)>,
    table_handles: Option<(gfx::Rect, gfx::Rect, usize, usize, usize)>,
    table_menu: Option<(usize, gfx::Rect)>,
    /// Measured table cell widths by text and font size: typing in a big table re-measures only
    /// the cell being typed in.
    cell_widths: std::collections::HashMap<(String, u32), f32>,
    /// Caret line the current layout was built for (its list syntax is shown raw).
    layout_caret_line: Option<usize>,

    focused: bool,
    /// The user has typed or clicked into the text since gaining focus; until then every
    /// line is shown as live preview.
    editing: bool,
    caret_on: bool,
    blink_at: Instant,
    last_input: Instant,
    mods: ModifiersState,
    pointer: (f64, f64),
    drag_select: bool,
    last_click: Option<(Instant, (f64, f64), u8)>,
    hits: Vec<((f32, f32, f32, f32), Action)>,
    hover: Option<Action>,
    popover: Option<Popover>,
    pop_rect: Option<(f32, f32, f32, f32)>,
    pop_hits: Vec<((f32, f32, f32, f32), PopHit)>,
    pop_hover: Option<PopHit>,
    /// Where the custom-hue strip was last drawn, and whether it's being dragged.
    hue_strip: Option<(f32, f32, f32, f32)>,
    hue_drag: bool,
    /// Floated above everything while hovered in the dock.
    peeking: bool,
    /// Pointer is inside the window.
    hovered: bool,
    /// Visibility of hover chrome (buttons, scrollbar), 0..1.
    chrome: Motion,
    /// Whole-window opacity: fades in on open and out on close.
    opacity: Motion,
    pub closing: bool,
    /// Resting position while shaking for attention; the shake is an x offset in logical px.
    wiggle: Option<PhysicalPosition<i32>>,
    shake: Motion,
    last_nudge: Instant,
    /// When the reminder started ringing (the glow breathes from here).
    ring_at: Instant,
    /// A checkbox that was just ticked (its line start) and its pop-in scale.
    pop: Option<(usize, Motion)>,
    /// Compact view: one line until clicked (`expanded`), folding back when left.
    compact: bool,
    expanded: bool,
    /// The window size to change to on the next paint.
    view_dirty: bool,
    /// The full page's size (kept while the window is a compact line).
    full_size: LogicalSize<f64>,
    /// Where a press on the compact line started (a drag moves the note, a click opens it).
    press_at: Option<(f64, f64)>,
    /// The note is being carried: the pointer's offset inside the window at the press. (The
    /// window moves with the pointer itself rather than by a native drag, which on macOS would
    /// force the arrow cursor and give no events until it ends.)
    moving: Option<(f64, f64)>,
    /// "Delete note" was clicked once.
    confirm_delete: bool,
    /// Reminder menu state: typed text, chosen day (days from today) and repeat.
    remind_query: String,
    remind_day: i64,
    remind_repeat: Repeat,
    /// The reminder menu's fine-tune time (for the chosen day).
    remind_time: chrono::NaiveTime,
    /// Transparent room added below the card so a popover taller than the note fits (px).
    extra_h: u32,
    /// The note springing in (0 → 1) when it appears, and out when hidden.
    appear: Motion,
    resizing: Option<ResizeStart>,
    /// The open popover springing in, and which kind it was started for.
    pop_in: Motion,
    pop_kind: Option<std::mem::Discriminant<Popover>>,
}

fn contains(r: (f32, f32, f32, f32), x: f32, y: f32) -> bool {
    x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
}

/// Whether a note whose top-left is at `pos` (physical) shows on one of `monitors`: enough of
/// its top strip to grab it.
fn on_screen(monitors: impl Iterator<Item = winit::monitor::MonitorHandle>, (x, y): (i32, i32)) -> bool {
    let (gx, gy) = (x + 60, y + 16);
    monitors.into_iter().any(|m| {
        let (p, s) = (m.position(), m.size());
        gx >= p.x && gy >= p.y && gx < p.x + s.width as i32 && gy < p.y + s.height as i32
    })
}

impl Note {
    pub fn open(
        el: &ActiveEventLoop,
        ctx: &surface::Context,
        rec: &NoteRecord,
        shared: &mut Shared,
    ) -> Result<Note, Box<dyn Error>> {
        let mode = Mode::from_u8(rec.mode);
        let mut attrs = Window::default_attributes()
            // macOS: a see-through window (our pixels carry the alpha).
            .with_transparent(cfg!(target_os = "macos"))
            .with_title("Stick-itt")
            .with_decorations(false)
            .with_inner_size(LogicalSize::new(rec.size.0, rec.size.1))
            .with_min_inner_size(LogicalSize::new(MIN_SIZE.0, MIN_SIZE.1))
            .with_window_level(mode.level())
            .with_window_icon(shared.icon.clone())
            // Never steal focus on launch; notes created on request are focused explicitly.
            .with_active(false)
            .with_visible(false);
        // A saved place no monitor shows (an unplugged screen, or a minimized window's -32000 from
        // an older version) is dropped: the system places the note instead.
        let placed_at = rec.pos.filter(|&p| on_screen(el.available_monitors(), p));
        if let Some((x, y)) = placed_at {
            attrs = attrs.with_position(PhysicalPosition::new(x, y));
        }
        #[cfg(windows)]
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            // Windows keeps a window on one virtual desktop only if it has a taskbar entry.
            attrs = attrs.with_skip_taskbar(!shared.config.desktops.per_desktop);
        }
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::WindowAttributesExtMacOS;
            // We draw our own shadow.
            attrs = attrs.with_has_shadow(false);
        }

        let window = Rc::new(el.create_window(attrs)?);
        window.set_ime_allowed(true);
        #[cfg(target_os = "macos")]
        crate::desktops::show_on_all(&window, !shared.config.desktops.per_desktop);
        let surface = Surface::new(ctx, &window, false)?;
        let dark = crate::theme::is_dark(window.theme());
        let now = Instant::now();
        let mut note = Note {
            id: rec.id,
            window,
            surface,
            doc: Doc::new(rec.body.clone()),
            color: rec.color,
            icon: rec.icon.clone(),
            list: rec.list,
            st: shared.config.notes.clone(),
            icon_query: String::new(),
            icon_tab: 0,
            icon_row: 0,
            mode,
            zoom: rec.zoom.clamp(50, 300),
            reminder: rec.reminder.clone(),
            ringing: false,
            dirty: false,
            needs_paint: true,
            shown: !rec.hidden,
            user_hidden: rec.hidden,
            desktop: rec.desktop.clone(),
            desktop_list: Vec::new(),
            hiding: false,
            dark,
            last_active: now,
            in_use_at: now,
            placed_at,
            buffer: Buffer::new_empty(Metrics::new(14.5, 22.0)),
            line_heights: Vec::new(),
            hot: Vec::new(),
            measured_for: (0.0, 0.0, 0.0),
            layout_dirty: true,
            layout_width: 0.0,
            line_starts: vec![0],
            display_len: 0,
            preedit_len: 0,
            content_h: 0.0,
            scroll: 0.0,
            page_scroll: None,
            goal_x: None,
            decos: Vec::new(),
            layout_caret_line: None,
            tables: Vec::new(),
            table_rects: Vec::new(),
            table_hover: None,
            table_geo: Vec::new(),
            table_cell: None,
            table_handles: None,
            table_menu: None,
            cell_widths: std::collections::HashMap::new(),
            focused: false,
            editing: false,
            caret_on: true,
            blink_at: now,
            last_input: now,
            mods: ModifiersState::empty(),
            pointer: (0.0, 0.0),
            drag_select: false,
            last_click: None,
            hits: Vec::new(),
            hover: None,
            popover: None,
            pop_rect: None,
            pop_hits: Vec::new(),
            pop_hover: None,
            hue_strip: None,
            hue_drag: false,
            peeking: false,
            hovered: false,
            chrome: Motion::new(0.0, Feel::Quick),
            opacity: Motion::new(0.0, Feel::Smooth),
            closing: false,
            wiggle: None,
            shake: Motion::new(0.0, Feel::Shake).with_epsilon(0.05),
            last_nudge: now,
            ring_at: now,
            pop: None,
            compact: rec.view == 1,
            expanded: false,
            view_dirty: rec.view == 1,
            full_size: LogicalSize::new(rec.size.0, rec.size.1),
            press_at: None,
            moving: None,
            confirm_delete: false,
            remind_query: String::new(),
            remind_day: 0,
            remind_repeat: Repeat::Once,
            remind_time: chrono::NaiveTime::from_hms_opt(9, 0, 0).unwrap_or_default(),
            extra_h: 0,
            resizing: None,
            appear: Motion::new(0.0, Feel::Snappy),
            pop_in: Motion::new(1.0, Feel::Snappy),
            pop_kind: None,
        };
        note.apply_level();
        // Paint before showing so the window never flashes white. Notes hidden with × stay hidden.
        note.render(shared);
        if note.shown {
            note.window.set_visible(true);
        }
        Ok(note)
    }

    pub fn record(&self) -> NoteRecord {
        NoteRecord {
            id: self.id,
            body: self.doc.text.clone(),
            pos: self.placed_at.or_else(|| {
                let minimized = self.window.is_minimized().unwrap_or(false);
                self.window
                    .outer_position()
                    .ok()
                    .map(|p| (p.x, p.y))
                    .filter(|&(x, y)| !minimized && x > -30000 && y > -30000)
            }),
            size: (self.full_size.width, self.full_size.height),
            color: self.color,
            mode: self.mode as u8,
            zoom: self.zoom,
            reminder: self.reminder.clone(),
            icon: self.icon.clone(),
            list: self.list,
            desktop: None,
            hidden: self.user_hidden,
            view: u8::from(self.compact),
        }
    }

    // ---- compact view -------------------------------------------------------------------

    /// Shown as one line right now.
    fn collapsed(&self) -> bool {
        self.compact && !self.expanded
    }

    fn set_compact(&mut self, on: bool) {
        self.compact = on;
        self.expanded = false;
        self.popover = None;
        self.view_dirty = true;
        self.dirty = true;
        self.needs_paint = true;
    }

    fn expand(&mut self) {
        self.expanded = true;
        self.view_dirty = true;
        self.needs_paint = true;
        self.window.focus_window();
    }

    fn collapse(&mut self) {
        // A compact line lays out nothing; the page picks up where it was when it opens again.
        self.page_scroll = Some(self.scroll);
        self.expanded = false;
        self.popover = None;
        self.view_dirty = true;
        self.needs_paint = true;
    }

    /// What the compact line shows on the right: the reminder, or to-do progress.
    fn compact_badge(&self) -> Option<(String, bool)> {
        if let Some(r) = &self.reminder {
            return Some((r.label(Local::now()), true));
        }
        if !self.st.compact.show_progress {
            return None;
        }
        let (mut done, mut total) = (0, 0);
        for l in self.doc.text.lines() {
            if let Some((_, checked)) = line_info(l).checkbox {
                total += 1;
                done += usize::from(checked);
            }
        }
        (total > 0).then(|| (format!("{done}/{total}"), false))
    }

    /// Width of the compact line's card (logical px): fits the title, within min/max width.
    fn compact_width(&self, fonts: &mut FontSystem) -> f32 {
        let c = &self.st.compact;
        let icon_w = if self.icon.is_empty() {
            8.0
        } else {
            label_width(&small_label(fonts, &self.icon, c.icon_size, Weight::NORMAL))
        };
        let title_w = label_width(&small_label(fonts, &title_of(&self.doc.text), c.font_size, Weight::MEDIUM));
        let badge_w = self
            .compact_badge()
            .map_or(0.0, |(t, _)| label_width(&small_label(fonts, &t, c.font_size * 0.86, Weight::MEDIUM)) + 10.0);
        (2.0 * c.padding + icon_w + 8.0 + title_w + badge_w + 2.0).clamp(c.min_width, c.max_width.max(c.min_width))
    }

    /// Sizes the window for the current view: a compact line, or the full page.
    fn apply_view(&mut self, fonts: &mut FontSystem) {
        let (mx, mt, mb) = (MARGIN.0 as f64, MARGIN.1 as f64, MARGIN.2 as f64);
        if self.collapsed() {
            let w = self.compact_width(fonts) as f64;
            self.window.set_min_inner_size(None::<LogicalSize<f64>>);
            self.resize(LogicalSize::new(w + 2.0 * mx, self.st.compact.height as f64 + mt + mb));
        } else {
            self.resize(self.full_size);
            self.window.set_min_inner_size(Some(LogicalSize::new(MIN_SIZE.0, MIN_SIZE.1)));
        }
        self.layout_dirty = true;
    }

    /// Resizes the window keeping its top-left corner in place. AppKit keeps the bottom-left one,
    /// so on macOS a note folding to one line would otherwise drop to the bottom of where it was
    /// (over the note below it), and one opening would grow upwards.
    pub fn resize(&self, size: impl Into<winit::dpi::Size>) {
        let top_left = self.window.outer_position().ok();
        let _ = self.window.request_inner_size(size);
        if cfg!(target_os = "macos") {
            if let Some(p) = top_left {
                self.window.set_outer_position(p);
            }
        }
    }

    /// Events while shown as one line: hover, drag to move, click (or Enter) to open.
    fn handle_compact(&mut self, event: &WindowEvent) -> Option<Option<Cmd>> {
        match event {
            WindowEvent::CursorEntered { .. } => {
                self.hovered = true;
                self.needs_paint = true;
            }
            WindowEvent::CursorLeft { .. } => {
                self.hovered = false;
                self.press_at = None;
                self.needs_paint = true;
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = (position.x, position.y);
                if self.moving.is_some() {
                    if crate::input::left_button_down() {
                        self.carry(position.x, position.y);
                    } else {
                        // The release is on its way, or went to another window (macOS).
                        self.moving = None;
                        self.window.set_cursor(CursorIcon::Pointer);
                        crate::input::show_cursor(CursorIcon::Pointer);
                    }
                    return Some(None);
                }
                if !self.hovered {
                    self.hovered = true;
                    self.needs_paint = true;
                }
                // A hand over the pill; the shadow around it isn't clickable (clicks there pass on).
                let icon = if self.on_card(self.pointer) { CursorIcon::Pointer } else { CursorIcon::Default };
                self.window.set_cursor(icon);
                if !crate::input::inside(position.x, position.y, self.window.inner_size()) {
                    self.press_at = None;
                    if std::mem::take(&mut self.hovered) {
                        self.needs_paint = true;
                    }
                    return Some(None);
                }
                if let Some((px, py)) = self.press_at {
                    if (position.x - px).hypot(position.y - py) > 4.0 * self.scale() as f64 {
                        // A press that moved starts a drag, unless its release was lost (macOS
                        // can send it to another window). One that stays put waits for its
                        // release, which makes the click.
                        self.press_at = None;
                        if crate::input::left_button_down() {
                            self.moving = Some((px, py));
                            self.window.set_cursor(CursorIcon::Move);
                            crate::input::show_cursor(CursorIcon::Move);
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                if *state == ElementState::Pressed {
                    if !self.on_card(self.pointer) {
                        return Some(self.screen_point().map(|(x, y)| Cmd::PassClick(x, y)));
                    }
                    self.press_at = Some(self.pointer);
                } else if self.moving.take().is_some() {
                    self.window.set_cursor(CursorIcon::Pointer);
                    crate::input::show_cursor(CursorIcon::Pointer);
                } else if self.press_at.take().is_some() {
                    self.expand();
                }
            }
            WindowEvent::KeyboardInput { event, is_synthetic: false, .. } => {
                if event.state == ElementState::Pressed && event.logical_key == Key::Named(NamedKey::Enter) {
                    self.expand();
                }
            }
            WindowEvent::Ime(_) | WindowEvent::MouseWheel { .. } => {}
            _ => return None,
        }
        Some(None)
    }

    fn render_compact(&mut self, shared: &mut Shared) {
        let PhysicalSize { width, height } = self.window.inner_size();
        let s = self.scale();
        let pal = self.palette();
        let c = self.st.compact.clone();
        let (kx, ky, kw, kh) = self.card();
        let fonts = &mut shared.fonts;
        let pad = c.padding * s;
        let icon = (!self.icon.is_empty()).then(|| small_label(fonts, &self.icon, c.icon_size * s, Weight::NORMAL));
        let icon_w = icon.as_ref().map_or(8.0 * s, label_width);
        let text_x = kx + pad + icon_w + 8.0 * s;
        let badge = self
            .compact_badge()
            .map(|(t, alert)| (small_label(fonts, &t, c.font_size * 0.86 * s, Weight::MEDIUM), alert));
        let badge_w = badge.as_ref().map_or(0.0, |(b, _)| label_width(b) + 10.0 * s);
        let title = ellipsized(
            fonts,
            &title_of(&self.doc.text),
            c.font_size * s,
            Weight::MEDIUM,
            kx + kw - pad - badge_w - text_x,
        );
        let radius = (c.radius * s).min(kh / 2.0);
        let opacity = (self.opacity.value().clamp(0.0, 1.0) * 255.0) as u8;
        let (hovered, ringing, shadow) = (self.hovered, self.ringing, self.st.shadow);
        let mid = ky + kh / 2.0;
        let swash = &mut shared.swash;
        self.surface.paint(width, height, opacity, |data| {
            if surface::ALPHA {
                let a = pal.shadow as f32 / 255.0 * shadow * if hovered { 1.25 } else { 1.0 };
                let layers = [(9.0 * s, 4.0 * s, a), (1.5 * s, 1.0 * s, a * 0.55)];
                let hole = (kx + 2.0 * s, ky + radius, kw - 4.0 * s, (kh - 2.0 * radius).max(0.0));
                gfx::shadow(
                    data,
                    width,
                    height,
                    (kx + 1.0 * s, ky + 1.0 * s, kw - 2.0 * s, kh - 2.0 * s),
                    &layers,
                    Some(hole),
                );
            }
            if let Some(mut pm) = PixmapMut::from_bytes(data, width, height) {
                if let Some(card) = gfx::squircle(kx, ky, kw, kh, radius) {
                    gfx::fill_path(&mut pm, &card, pal.base, 255);
                    gfx::fill_path(&mut pm, &card, pal.wash, (255.0 * c.tint.clamp(0.0, 1.0)) as u8);
                    if hovered {
                        gfx::fill_path(&mut pm, &card, pal.text, pal.soft / 2);
                    }
                    let (edge, edge_alpha) = if pal.dark { (theme::WHITE, 26) } else { (pal.text, 22) };
                    gfx::stroke_path(&mut pm, &card, 1.0 * s, edge, edge_alpha);
                    if ringing {
                        gfx::stroke_path(&mut pm, &card, 2.0 * s, pal.alert, 255);
                    }
                }
                if icon.is_none() {
                    if let Some(p) = gfx::circle(kx + pad + 4.0 * s, mid, 4.0 * s) {
                        gfx::fill_path(&mut pm, &p, pal.accent, 255);
                    }
                }
            }
            let clip = Clip { x0: kx as i32, y0: ky as i32, x1: (kx + kw) as i32, y1: (ky + kh) as i32 };
            let label_h = |b: &Buffer| b.layout_runs().next().map_or(0.0, |r| r.line_height);
            let at = |b: &Buffer, x: f32| (x.round() as i32, (mid - label_h(b) / 2.0).round() as i32);
            if let Some(mut b) = icon {
                let p = at(&b, kx + pad);
                gfx::draw_text(&mut b, fonts, swash, pal.text.text(255), data, width, clip, p);
            }
            let mut title = title;
            let p = at(&title, text_x);
            gfx::draw_text(&mut title, fonts, swash, pal.text.text(255), data, width, clip, p);
            if let Some((mut b, alert)) = badge {
                let p = at(&b, kx + kw - pad - label_width(&b));
                let color = if alert { pal.alert } else { pal.secondary };
                gfx::draw_text(&mut b, fonts, swash, color.text(255), data, width, clip, p);
            }
        });
    }

    // ---- geometry -----------------------------------------------------------------------

    fn scale(&self) -> f32 {
        self.window.scale_factor() as f32
    }

    fn size(&self) -> (f32, f32) {
        let s = self.window.inner_size();
        (s.width as f32, s.height as f32)
    }

    /// The visible card inside the window, in physical pixels: (x, y, w, h).
    fn card(&self) -> (f32, f32, f32, f32) {
        let s = self.scale();
        let (w, h) = self.size();
        let (mx, mt, mb) = (MARGIN.0 * s, MARGIN.1 * s, MARGIN.2 * s);
        (mx, mt, (w - 2.0 * mx).max(1.0), (h - mt - mb - self.extra_h as f32).max(1.0))
    }

    /// Whether a window point is on the card (not on the shadow around it).
    fn on_card(&self, (x, y): (f64, f64)) -> bool {
        let (kx, ky, kw, kh) = self.card();
        let (x, y) = (x as f32, y as f32);
        x >= kx && x < kx + kw && y >= ky && y < ky + kh
    }

    /// The pointer on screen (physical).
    fn screen_point(&self) -> Option<(i32, i32)> {
        let p = self.window.outer_position().ok()?;
        Some((p.x + self.pointer.0 as i32, p.y + self.pointer.1 as i32))
    }

    /// The card on screen (physical): where clicks are this note's.
    pub fn card_on_screen(&self) -> Option<(i32, i32, i32, i32)> {
        let p = self.window.outer_position().ok()?;
        let (kx, ky, kw, kh) = self.card();
        Some((p.x + kx as i32, p.y + ky as i32, kw as i32, kh as i32))
    }

    /// Shown as one line right now.
    pub fn is_collapsed(&self) -> bool {
        self.collapsed()
    }

    /// A click passed on from another note's shadow: a one-line note opens, others take focus.
    pub fn take_click(&mut self) {
        if self.collapsed() {
            self.expand();
        } else {
            self.window.focus_window();
        }
    }

    /// Text area in physical pixels: (x, y, w, h).
    fn content_rect(&self) -> (f32, f32, f32, f32) {
        let s = self.scale();
        let (kx, ky, kw, kh) = self.card();
        let st = &self.st;
        let top = st.top_bar + st.icon_row(!self.icon.is_empty()) + st.gap;
        let (x, y) = (kx + st.padding * s, ky + top * s);
        (x, y, (kw - 2.0 * st.padding * s).max(1.0), (kh - top * s - st.bottom_padding * s).max(1.0))
    }

    fn palette(&self) -> Palette {
        palette(self.color, self.dark)
    }

    fn to_display(&self, off: usize) -> usize {
        if self.preedit_len > 0 && off > self.doc.cursor {
            off + self.preedit_len
        } else {
            off
        }
    }

    fn to_doc(&self, off: usize) -> usize {
        let c = self.doc.cursor;
        let off = off.min(self.display_len);
        let off = if self.preedit_len == 0 || off <= c {
            off
        } else if off < c + self.preedit_len {
            c
        } else {
            off - self.preedit_len
        };
        self.doc.clamp(off)
    }

    fn cursor_at(&self, display_off: usize) -> Cursor {
        let line = self.line_starts.partition_point(|&s| s <= display_off).saturating_sub(1);
        Cursor::new(line, display_off - self.line_starts[line])
    }

    // ---- layout -------------------------------------------------------------------------

    /// The window is showing the page at its page size. Not while it is a compact line, nor
    /// while it grows back into a page: the note takes focus first, and text laid out at the
    /// line's width wraps so much that its scroll position means nothing on the page.
    fn page_ready(&self) -> bool {
        let h = self.window.inner_size().to_logical::<f64>(self.window.scale_factor()).height;
        !self.collapsed() && h >= MIN_SIZE.1 - 1.0
    }

    fn sync_layout(&mut self, fonts: &mut FontSystem) {
        if !self.page_ready() {
            return;
        }
        let (_, _, cw, _) = self.content_rect();
        if self.layout_dirty || cw != self.layout_width {
            self.relayout(fonts, cw);
        }
        if let Some(scroll) = self.page_scroll.take() {
            self.scroll = scroll;
            self.clamp_scroll();
        }
        self.sync_scroll(fonts);
    }

    fn relayout(&mut self, fonts: &mut FontSystem, width: f32) {
        let s = self.scale() * self.zoom as f32 / 100.0;
        let (size, line) = (self.st.font_size * s, self.st.line() * s);
        let pal = self.palette();
        let (display, preedit) = self.doc.display();
        let mut text = display.into_owned();
        self.display_len = text.len();
        self.preedit_len = preedit.as_ref().map_or(0, |r| r.len());

        let mut flags = markdown::style(&text);
        for tok in reminder::find_tokens(&text, Local::now()) {
            markdown::mark(&mut flags, tok.range, markdown::REMINDER);
        }
        let marks: Vec<markdown::Highlight> =
            markdown::highlights(&text).into_iter().filter(|h| flags[h.open.start] & markdown::CODE == 0).collect();
        for h in &marks {
            markdown::mark(&mut flags, h.open.clone(), markdown::MARKER);
            markdown::mark(&mut flags, h.close.clone(), markdown::MARKER);
        }
        // `{red: text}`: its syntax collapses like other marks; its text is coloured below.
        let inks: Vec<markdown::Highlight> =
            markdown::text_colors(&text).into_iter().filter(|h| flags[h.open.start] & markdown::CODE == 0).collect();
        for h in &inks {
            markdown::mark(&mut flags, h.open.clone(), markdown::MARKER);
            markdown::mark(&mut flags, h.close.clone(), markdown::MARKER);
        }
        if let Some(r) = preedit {
            markdown::mark(&mut flags, r, markdown::PREEDIT);
        }
        let caret_line = self.caret_line();
        self.decorate(&text, &mut flags, caret_line);
        self.decos.extend(marks.into_iter().map(|h| (h.text, Deco::Badge(h.color as u8))));
        self.layout_caret_line = caret_line;
        let (spacers, thin) = self.style_tables(fonts, &text, &mut flags, width, (&pal, size, line));
        // The shaper drops a trailing empty line; a sentinel space keeps it addressable.
        if text.is_empty() || text.ends_with('\n') {
            text.push(' ');
            flags.push(0);
        }
        self.line_starts = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();

        let runs = markdown::runs(&flags);
        let default = attrs_for(0, &pal, size, line);
        // One attribute list per paragraph.
        let count = self.line_starts.len();
        let mut lines: Vec<(std::ops::Range<usize>, AttrsList)> = Vec::with_capacity(count);
        let mut first = 0;
        for i in 0..count {
            let ls = self.line_starts[i];
            let le = self.line_starts.get(i + 1).map_or(text.len(), |&next| next - 1);
            while first < runs.len() && runs[first].0.end <= ls {
                first += 1;
            }
            let mut list = AttrsList::new(&default);
            for (r, f) in runs[first..].iter().take_while(|(r, _)| r.start < le) {
                let (a, b) = (r.start.max(ls), r.end.min(le));
                if *f != 0 && a < b {
                    list.add_span(a - ls..b - ls, &attrs_for(*f, &pal, size, line));
                }
            }
            // Coloured text keeps its other styles.
            for h in inks.iter().filter(|h| h.text.start < le && h.text.end > ls) {
                let (a, b) = (h.text.start.max(ls), h.text.end.min(le));
                let ink = theme::tint(HIGHLIGHT_TINT[h.color % HIGHLIGHT_TINT.len()], pal.dark).0;
                for (q, f) in markdown::runs(&flags[a..b]) {
                    let attrs = attrs_for(f, &pal, size, line).color(ink.text(255));
                    list.add_span(a + q.start - ls..a + q.end - ls, &attrs);
                }
            }
            // Table dividers stretched to line up the columns; separator rows made thin.
            for &(at, em, first) in spacers.iter().filter(|(at, _, _)| (ls..le).contains(at)) {
                let mut a = attrs_for(flags[at], &pal, size, line).letter_spacing(em);
                if first {
                    // The row's first (hidden) divider makes the whole row taller.
                    a = a.metrics(Metrics::new(size, line + TABLE_ROW * s));
                }
                list.add_span(at - ls..at - ls + 1, &a);
            }
            if le > ls && thin.contains(&ls) {
                let a = attrs_for(markdown::COLLAPSED, &pal, size, line).metrics(Metrics::new(0.05, TABLE_RULE * s));
                list.add_span(0..le - ls, &a);
            }
            lines.push((ls..le, list));
        }
        self.sync_lines(&text, lines);

        let (_, _, _, view_h) = self.content_rect();
        self.buffer.set_metrics(Metrics::new(size, line));
        self.buffer.set_size(Some(width), Some(view_h));
        if self.measured_for != (width, size, line) {
            self.measured_for = (width, size, line);
            self.line_heights.iter_mut().for_each(|h| *h = None);
        }
        // Apply pending invalidations before measuring.
        self.buffer.shape_until_scroll(fonts, false);
        self.measure(fonts);
        self.layout_width = width;
        self.layout_dirty = false;
        self.clamp_scroll();
    }

    /// Updates the buffer's lines to `lines` (ranges of `text` with their attributes), keeping
    /// the shaping of every line whose text and attributes are unchanged. Typing in one line of
    /// a long note re-shapes that line, not the whole note.
    fn sync_lines(&mut self, text: &str, lines: Vec<(std::ops::Range<usize>, AttrsList)>) {
        let (n_old, n_new) = (self.buffer.lines.len(), lines.len());
        self.line_heights.resize(n_old, None);
        self.hot.resize(n_old, true);
        let (p, q) = {
            let old = &self.buffer.lines;
            let same = |o: &BufferLine, r: &std::ops::Range<usize>| o.text() == &text[r.clone()];
            let mut p = 0;
            while p < n_old.min(n_new) && same(&old[p], &lines[p].0) {
                p += 1;
            }
            let mut q = 0;
            while q < (n_old - p).min(n_new - p) && same(&old[n_old - 1 - q], &lines[n_new - 1 - q].0) {
                q += 1;
            }
            (p, q)
        };
        let ending = |i: usize| if i + 1 == n_new { LineEnding::None } else { LineEnding::Lf };
        let mut middle = Vec::with_capacity(n_new - p - q);
        for (i, (r, attrs)) in lines.into_iter().enumerate() {
            let kept = if i < p {
                Some(i)
            } else if i >= n_new - q {
                Some(i + n_old - n_new)
            } else {
                None
            };
            match kept {
                // Same text: re-shaped only if its attributes changed (e.g. the caret moved onto it).
                Some(k) => {
                    if self.buffer.lines[k].set_text(&text[r], ending(i), attrs) {
                        self.line_heights[k] = None;
                    }
                }
                None => middle.push(BufferLine::new(&text[r], ending(i), attrs, Shaping::Advanced)),
            }
        }
        let added = middle.len();
        self.buffer.lines.splice(p..n_old - q, middle);
        self.line_heights.splice(p..n_old - q, std::iter::repeat_n(None, added));
        self.hot.splice(p..n_old - q, std::iter::repeat_n(false, added));
    }

    /// Lays out the lines whose height isn't known yet and totals the note's height. Lines far
    /// from the view are freed straight after measuring, so opening a long note never holds
    /// all of it laid out at once.
    fn measure(&mut self, fonts: &mut FontSystem) {
        let line = self.measured_for.2;
        let (_, _, _, vh) = self.content_rect();
        let (keep_top, keep_bottom) = (self.scroll - vh, self.scroll + 2.0 * vh);
        let mut top = 0.0;
        for i in 0..self.buffer.lines.len() {
            if self.line_heights[i].is_none() {
                let h = self
                    .buffer
                    .line_layout(fonts, i)
                    .map_or(line, |layout| layout.iter().map(|l| l.line_height_opt.unwrap_or(line)).sum());
                self.line_heights[i] = Some(h);
                self.hot[i] = true;
                if top + h < keep_top || top > keep_bottom {
                    self.free_line(i);
                }
            }
            top += self.line_heights[i].unwrap_or(line);
        }
        self.content_h = top;
    }

    /// Drops a line's shaping and layout (rebuilt from its own text and attributes, since
    /// cosmic-text's pruning keeps the allocations for reuse).
    fn free_line(&mut self, i: usize) {
        let l = &self.buffer.lines[i];
        let fresh = BufferLine::new(l.text().to_string(), l.ending(), l.attrs_list().clone(), Shaping::Advanced);
        self.buffer.lines[i] = fresh;
        self.hot[i] = false;
    }

    /// Top of paragraph `line` in content coordinates.
    fn line_top(&self, line: usize) -> f32 {
        let lh = self.measured_for.2;
        self.line_heights.iter().take(line).map(|h| h.unwrap_or(lh)).sum()
    }

    /// The paragraph at content height `y`.
    fn line_at(&self, y: f32) -> usize {
        let lh = self.measured_for.2;
        let mut top = 0.0;
        for (i, h) in self.line_heights.iter().enumerate() {
            top += h.unwrap_or(lh);
            if y < top {
                return i;
            }
        }
        self.line_heights.len().saturating_sub(1)
    }

    /// Points the buffer at the current scroll position and lays out what's in view. Lines
    /// more than a screen away give back their shaping data.
    fn sync_scroll(&mut self, fonts: &mut FontSystem) {
        if self.buffer.lines.is_empty() {
            return;
        }
        let first = self.line_at(self.scroll);
        let top = self.line_top(first);
        self.buffer.set_scroll(Scroll::new(first, self.scroll - top, 0.0));
        self.buffer.shape_until_scroll(fonts, false);
        let sc = self.buffer.scroll();
        self.scroll = self.line_top(sc.line) + sc.vertical;

        let (_, _, _, vh) = self.content_rect();
        let (keep_top, keep_bottom) = (self.scroll - vh, self.scroll + 2.0 * vh);
        let lh = self.measured_for.2;
        let mut top = 0.0;
        for i in 0..self.buffer.lines.len() {
            let h = self.line_heights[i].unwrap_or(lh);
            if top + h >= keep_top && top <= keep_bottom {
                // In or near the view: shaped by now, or soon.
                self.hot[i] = true;
            } else if self.hot[i] {
                self.free_line(i);
            }
            top += h;
        }
    }

    /// Highlight rectangles between two cursors, in content coordinates (visible lines only).
    fn span_rects(&self, a: Cursor, b: Cursor) -> Vec<(f32, f32, f32, f32)> {
        gfx::span_rects(&self.buffer, a, b).into_iter().map(|(x, y, w, h)| (x, y + self.scroll, w, h)).collect()
    }

    fn caret_line(&self) -> Option<usize> {
        (self.focused && self.editing).then(|| self.doc.text[..self.doc.cursor].matches('\n').count())
    }

    /// Live preview: on every line but the caret's, markdown syntax is collapsed or replaced
    /// by graphics (checkboxes, bullets, quote bars, rules). Code gets backgrounds everywhere.
    fn decorate(&mut self, text: &str, flags: &mut [Flags], caret_line: Option<usize>) {
        use markdown::{mark, CODE, COLLAPSED, HIDDEN, MARKER};
        self.decos.clear();
        self.tables.clear();
        // Tables: a header row, a `|---|` separator, then rows. They stay a grid while edited.
        let all: Vec<&str> = text.split('\n').collect();
        let mut in_table = vec![false; all.len()];
        let mut starts = Vec::with_capacity(all.len());
        let mut at = 0;
        for line in &all {
            starts.push(at);
            at += line.len() + 1;
        }
        let mut i = 0;
        while i + 1 < all.len() {
            if table_pipes(all[i]).is_some() && is_table_separator(all[i + 1]) {
                let mut j = i + 2;
                while j < all.len() && table_pipes(all[j]).is_some() && !is_table_separator(all[j]) {
                    j += 1;
                }
                in_table[i..j].iter_mut().for_each(|t| *t = true);
                let rows = (i..j).map(|k| starts[k]..starts[k] + all[k].len()).collect();
                self.tables.push(Table { rows, cols: Vec::new() });
                i = j;
            } else {
                i += 1;
            }
        }
        let mut ls = 0;
        let mut fence_start: Option<usize> = None;
        for (i, line) in text.split('\n').enumerate() {
            let le = ls + line.len();
            let pretty = Some(i) != caret_line && !in_table[i];
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") {
                if pretty {
                    mark(flags, ls..le, COLLAPSED);
                }
                match fence_start.take() {
                    Some(start) => self.decos.push((start..le, Deco::CodeBlock)),
                    None => fence_start = Some(ls),
                }
            } else if fence_start.is_none() {
                if pretty {
                    let info = line_info(line);
                    let marker = ls + info.indent;
                    if let Some((at, checked)) = info.checkbox {
                        mark(flags, marker..ls + at, COLLAPSED);
                        mark(flags, ls + at..ls + at + 3, HIDDEN);
                        self.decos.push((ls + at..ls + at + 3, Deco::Checkbox(checked)));
                    } else if info.is_list() && info.ordered.is_none() {
                        mark(flags, marker..marker + 1, HIDDEN);
                        self.decos.push((marker..marker + 1, Deco::Bullet));
                    } else if markdown::is_callout(line) {
                        mark(flags, marker..marker + 1, HIDDEN);
                        self.decos.push((ls..le.max(ls + 1), Deco::Callout));
                    } else if trimmed.starts_with('>') {
                        mark(flags, marker..marker + 1, HIDDEN);
                        self.decos.push((marker..le.max(marker + 1), Deco::Quote));
                    }
                    let rule = trimmed.len() >= 3 && flags[ls..le].iter().all(|f| f & MARKER != 0 && f & CODE == 0);
                    if rule && trimmed.chars().all(|c| matches!(c, '-' | '*' | '_' | ' ')) {
                        self.decos.push((ls..le, Deco::Rule));
                    }
                    for f in &mut flags[ls..le] {
                        if *f & MARKER != 0 && *f & HIDDEN == 0 {
                            *f |= COLLAPSED;
                        }
                    }
                }
                let mut j = ls;
                while j < le {
                    if flags[j] & CODE != 0 {
                        let start = j;
                        while j < le && flags[j] & CODE != 0 {
                            j += 1;
                        }
                        self.decos.push((start..j, Deco::CodePill));
                    } else {
                        j += 1;
                    }
                }
            }
            ls = le + 1;
        }
        if let Some(start) = fence_start {
            self.decos.push((start..text.len(), Deco::CodeBlock));
        }
    }

    /// Sizes each table found by `decorate` and styles it as a grid: dividers hidden and
    /// stretched (returned as `(byte, letter spacing in em, first in its row)`) so cells line up
    /// in columns, the separator row thin (returned by line start). A table wider than the note
    /// stays plain text.
    fn style_tables(
        &mut self,
        fonts: &mut FontSystem,
        text: &str,
        flags: &mut [Flags],
        width: f32,
        (pal, size, line): (&Palette, f32, f32),
    ) -> (Vec<(usize, f32, bool)>, Vec<usize>) {
        use markdown::{mark, COLLAPSED, HIDDEN, MARKER};
        let mut spacers = Vec::new();
        let mut thin = Vec::new();
        if self.tables.is_empty() {
            return (spacers, thin);
        }
        let s = self.scale() * self.zoom as f32 / 100.0;
        let pad = TABLE_PAD * s;
        let cache = &mut self.cell_widths;
        if cache.len() > 1024 {
            cache.clear();
        }
        let mut measure = |fonts: &mut FontSystem, f: &[Flags], r: std::ops::Range<usize>| -> f32 {
            if r.is_empty() {
                return 0.0;
            }
            let key = (text[r.clone()].to_string(), size.to_bits());
            if let Some(&w) = cache.get(&key) {
                return w;
            }
            let spans: Vec<(&str, Attrs)> = markdown::runs(&f[r.clone()])
                .into_iter()
                .map(|(q, fl)| (&text[r.start + q.start..r.start + q.end], attrs_for(fl, pal, size, line)))
                .collect();
            let mut b = Buffer::new_empty(Metrics::new(size, line));
            b.set_wrap(Wrap::None);
            b.set_rich_text(spans, &attrs_for(0, pal, size, line), Shaping::Advanced, None);
            b.shape_until_scroll(fonts, false);
            let w = label_width(&b);
            cache.insert(key, w);
            w
        };
        let pipe_w = {
            let mut b = Buffer::new_empty(Metrics::new(size, line));
            b.set_text("|", &attrs_for(0, pal, size, line), Shaping::Advanced, None);
            b.shape_until_scroll(fonts, false);
            label_width(&b)
        };
        for table in &mut self.tables {
            let span = table.rows[0].start..table.rows.last().map_or(0, |r| r.end);
            let mut f = flags[span.clone()].to_vec();
            let local = |r: &std::ops::Range<usize>| r.start - span.start..r.end - span.start;
            let mut rows: Vec<(Vec<usize>, Vec<std::ops::Range<usize>>)> = Vec::new();
            for (k, row) in table.rows.iter().enumerate() {
                let r = local(row);
                if k == 1 {
                    mark(&mut f, r, COLLAPSED);
                    rows.push((Vec::new(), Vec::new()));
                    continue;
                }
                let line_text = &text[row.clone()];
                let pipes = table_pipes(line_text).unwrap_or_default();
                let cells = table_cells(line_text, &pipes);
                // Everything but the cells' text is syntax: hide the dividers, collapse the rest.
                for b in &mut f[r.clone()] {
                    *b |= COLLAPSED;
                }
                for c in &cells {
                    for b in &mut f[r.start + c.start..r.start + c.end] {
                        *b &= !COLLAPSED;
                        if *b & MARKER != 0 {
                            *b |= COLLAPSED;
                        }
                    }
                }
                for &p in &pipes {
                    f[r.start + p] = (f[r.start + p] & !COLLAPSED) | HIDDEN;
                }
                let cells = cells.into_iter().map(|c| r.start + c.start..r.start + c.end).collect();
                let pipes = pipes.into_iter().map(|p| r.start + p).collect();
                rows.push((pipes, cells));
            }
            // Cells are relative to the table; `measure` works on the whole text.
            let mut all = flags.to_vec();
            all[span.clone()].copy_from_slice(&f);
            let widths: Vec<Vec<f32>> = rows
                .iter()
                .map(|(_, cells)| {
                    cells.iter().map(|c| measure(fonts, &all, span.start + c.start..span.start + c.end)).collect()
                })
                .collect();
            let n = widths.iter().map(Vec::len).max().unwrap_or(0);
            let mut cols = vec![TABLE_MIN_COL * s; n];
            for row in &widths {
                for (k, w) in row.iter().enumerate() {
                    cols[k] = cols[k].max(w + 2.0 * pad);
                }
            }
            if n == 0 || cols.iter().sum::<f32>() > width - 2.0 * s {
                continue;
            }
            // Each cell's text starts where its column's alignment puts it.
            let aligns = crate::table::aligns(&text[table.rows[1].clone()]);
            let total: f32 = cols.iter().sum();
            for ((pipes, cells), ws) in rows.iter().zip(&widths) {
                let start = |k: usize| -> f32 {
                    let x: f32 = cols[..k].iter().sum();
                    x + match aligns.get(k).copied().unwrap_or_default() {
                        crate::table::Align::Left => pad,
                        crate::table::Align::Center => (cols[k] - ws[k]) / 2.0,
                        crate::table::Align::Right => cols[k] - pad - ws[k],
                    }
                };
                for (j, &p) in pipes.iter().enumerate() {
                    let target = match (j, cells.len()) {
                        (0, 0) => total,
                        (_, 0) => 0.0,
                        (0, _) => start(0),
                        (j, m) if j < m => start(j) - (start(j - 1) + ws[j - 1]),
                        (_, m) => total - (start(m - 1) + ws[m - 1]),
                    };
                    spacers.push((span.start + p, (target - pipe_w) / size, j == 0));
                }
            }
            thin.push(table.rows[1].start);
            flags[span].copy_from_slice(&f);
            table.cols = cols;
        }
        (spacers, thin)
    }

    /// Caret position in buffer coordinates: (x, line_top, line_height).
    fn caret_geom(&self, display_off: usize) -> Option<(f32, f32, f32)> {
        let c = self.cursor_at(display_off);
        let mut fallback = None;
        for run in self.buffer.layout_runs() {
            if run.line_i < c.line {
                continue;
            }
            if run.line_i > c.line {
                break;
            }
            let inside = run.glyphs.is_empty() || run.glyphs.iter().any(|g| g.start <= c.index && c.index < g.end);
            if let Some(x) = run.cursor_position(&c) {
                if inside {
                    return Some((x, run.line_top + self.scroll, run.line_height));
                }
                fallback = Some((x, run.line_top + self.scroll, run.line_height));
            }
        }
        fallback
    }

    /// Doc offset under a buffer-space point.
    fn hit_buffer(&self, bx: f32, by: f32) -> usize {
        if by >= self.content_h {
            return self.doc.text.len();
        }
        let (_, _, _, vh) = self.content_rect();
        let local = by.max(0.0) - self.scroll;
        if (0.0..vh).contains(&local) {
            if let Some(c) = self.buffer.hit(bx, local) {
                return self.to_doc(self.line_starts.get(c.line).copied().unwrap_or(0) + c.index);
            }
        }
        // Outside the view (e.g. drag-selecting past an edge): the start of the line there.
        self.to_doc(self.line_starts.get(self.line_at(by.max(0.0))).copied().unwrap_or(0))
    }

    /// Doc offset under a window-space point.
    fn hit_window(&self, x: f64, y: f64) -> usize {
        let (cx, cy, _, _) = self.content_rect();
        self.hit_buffer(x as f32 - cx, y as f32 - cy + self.scroll)
    }

    fn clamp_scroll(&mut self) {
        let (_, _, _, vh) = self.content_rect();
        self.scroll = self.scroll.clamp(0.0, (self.content_h - vh).max(0.0));
    }

    fn ensure_caret_visible(&mut self, fonts: &mut FontSystem) {
        if !self.page_ready() {
            return;
        }
        let (cx, cy, _, vh) = self.content_rect();
        let mut geom = self.caret_geom(self.to_display(self.doc.cursor));
        if geom.is_none() {
            // The caret's line is out of view (and not laid out): scroll to it, then look again.
            let line = self.cursor_at(self.to_display(self.doc.cursor)).line;
            let top = self.line_top(line);
            let h = self.line_heights.get(line).copied().flatten().unwrap_or(0.0);
            self.scroll = if top < self.scroll { top } else { top + h - vh };
            self.clamp_scroll();
            self.sync_scroll(fonts);
            geom = self.caret_geom(self.to_display(self.doc.cursor));
        }
        if let Some((x, top, h)) = geom {
            if top < self.scroll {
                self.scroll = top;
            } else if top + h > self.scroll + vh {
                self.scroll = top + h - vh;
            }
            self.clamp_scroll();
            self.window.set_ime_cursor_area(
                PhysicalPosition::new((cx + x) as f64, (cy + top - self.scroll) as f64),
                PhysicalSize::new(1.0, h as f64),
            );
            self.sync_scroll(fonts);
        }
    }

    /// Call after anything that changes text, caret or selection.
    fn changed(&mut self, shared: &mut Shared, text: bool) {
        self.refresh(shared, text);
        self.ensure_caret_visible(&mut shared.fonts);
    }

    /// `changed` without bringing the caret into view: for focus changes, which shouldn't move
    /// the page away from where it was scrolled to.
    fn refresh(&mut self, shared: &mut Shared, text: bool) {
        if text {
            self.dirty = true;
            self.layout_dirty = true;
        }
        if self.caret_line() != self.layout_caret_line {
            self.layout_dirty = true;
        }
        self.validate_slash();
        self.validate_emoji();
        self.sync_format_bar();
        self.sync_layout(&mut shared.fonts);
        self.caret_on = true;
        self.blink_at = Instant::now();
        self.last_input = self.blink_at;
        self.needs_paint = true;
    }

    // ---- reminders ----------------------------------------------------------------------

    /// Turns finished `@time` tokens into the note's reminder. Returns true if any were found.
    fn consume_reminders(&mut self) -> bool {
        let tokens = reminder::find_tokens(&self.doc.text, Local::now());
        let Some(last) = tokens.last() else { return false };
        self.reminder = Some(last.reminder.clone());
        self.ringing = false;
        let ranges: Vec<_> = tokens.iter().map(|t| t.range.clone()).collect();
        self.doc.remove_ranges(&ranges);
        self.dirty = true;
        self.layout_dirty = true;
        true
    }

    pub fn due(&self, now: i64) -> bool {
        !self.ringing && self.reminder.as_ref().is_some_and(|r| r.due <= now)
    }

    /// The reminder is due: bring the note up, glowing and wiggling, with a chime if `sound`.
    pub fn ring(&mut self, sound: bool) {
        self.ringing = true;
        self.ring_at = Instant::now();
        if sound {
            crate::chime::play();
        }
        self.window.set_visible(true);
        self.window.set_minimized(false);
        self.apply_level();
        self.window.focus_window();
        self.window.request_user_attention(Some(UserAttentionType::Critical));
        self.start_wiggle();
        self.needs_paint = true;
    }

    fn start_wiggle(&mut self) {
        if self.wiggle.is_none() {
            self.wiggle = self.window.outer_position().ok();
        }
        if self.wiggle.is_some() {
            self.shake.kick(900.0);
        }
        self.last_nudge = Instant::now();
    }

    /// × : fades the note out and hides it. It stays in the dock, and comes back from there.
    pub fn hide(&mut self) {
        self.user_hidden = true;
        self.hiding = true;
        self.popover = None;
        self.dirty = true;
        self.opacity.animate_to(0.0, Feel::Smooth);
    }

    /// Fades the note out; the app drops it once `closed()` (used when deleting).
    pub fn begin_close(&mut self) {
        self.closing = true;
        self.opacity.animate_to(0.0, Feel::Smooth);
    }

    pub fn closed(&self) -> bool {
        self.closing && !self.opacity.moving()
    }

    fn pop_checkbox(&mut self, line_start: usize) {
        let mut m = Motion::new(0.45, Feel::Bouncy);
        m.to(1.0);
        self.pop = Some((line_start, m));
    }

    /// Stacking from the mode, unless a reminder is ringing or the dock is previewing the note.
    /// "On desktop" also pins the window under everything, even when clicked.
    fn apply_level(&mut self) {
        let raised = self.ringing || self.peeking;
        self.window.set_window_level(if raised { WindowLevel::AlwaysOnTop } else { self.mode.level() });
        self.surface.pin_to_bottom(!raised && self.mode == Mode::Desktop);
    }

    /// Keeps the note on its own virtual desktop (`per_desktop`), or shows it on all of them.
    /// Windows ties this to the taskbar entry; macOS to the window's Spaces behaviour.
    pub fn set_taskbar(&self, per_desktop: bool) {
        #[cfg(windows)]
        {
            use winit::platform::windows::WindowExtWindows;
            self.window.set_skip_taskbar(!per_desktop);
        }
        crate::desktops::show_on_all(&self.window, !per_desktop);
    }

    fn stop_ringing(&mut self) {
        self.ringing = false;
        self.apply_level();
        self.window.request_user_attention(None);
        self.dirty = true;
        self.needs_paint = true;
    }

    fn act(&mut self, action: Action, shared: &mut Shared) -> Option<Cmd> {
        let now = Local::now().timestamp();
        match action {
            Action::Icon => {
                self.popover = (self.popover != Some(Popover::Icons)).then_some(Popover::Icons);
                self.pop_hover = None;
                self.icon_query.clear();
                self.icon_row = 0;
            }
            Action::Colors => {
                self.popover = (self.popover != Some(Popover::Colors)).then_some(Popover::Colors);
                self.pop_hover = None;
            }
            Action::More => {
                self.popover = (self.popover != Some(Popover::More)).then_some(Popover::More);
                self.pop_hover = None;
                self.confirm_delete = false;
            }
            Action::Remind => {
                if self.popover == Some(Popover::Remind) {
                    self.popover = None;
                } else {
                    // Start from the current reminder's day and repeat.
                    let now = Local::now();
                    self.remind_query.clear();
                    self.remind_day =
                        self.reminder.as_ref().map_or(0, |r| reminder::day_offset(r.due, now).clamp(0, 6));
                    self.remind_repeat = match self.reminder.as_ref().map(|r| r.repeat) {
                        Some(Repeat::Every(_)) | None => Repeat::Once,
                        Some(r) => r,
                    };
                    self.remind_time = match &self.reminder {
                        Some(r) => reminder::local(r.due).time(),
                        None => reminder::suggested_time(now, shared.config.reminders.minute_step),
                    };
                    self.popover = Some(Popover::Remind);
                }
                self.pop_hover = None;
            }
            Action::Window => {
                self.popover = (self.popover != Some(Popover::Window)).then_some(Popover::Window);
                self.pop_hover = None;
            }
            Action::Close => return Some(Cmd::Close),
            Action::CycleMode => {
                self.mode = self.mode.next();
                self.apply_level();
                self.dirty = true;
            }
            Action::CycleColor => self.set_color(if self.color + 1 >= theme::PRESETS { 0 } else { self.color + 1 }),
            Action::Snooze(secs) => {
                if let Some(r) = &mut self.reminder {
                    r.due = now + secs;
                }
                self.stop_ringing();
            }
            Action::Dismiss => {
                self.reminder = self.reminder.as_ref().and_then(|r| r.advance(now));
                self.stop_ringing();
            }
            Action::ClearReminder => {
                self.reminder = None;
                self.stop_ringing();
            }
        }
        self.changed(shared, false);
        None
    }

    fn set_reminder(&mut self, r: Reminder) {
        self.reminder = Some(r);
        if self.ringing {
            self.stop_ringing();
        }
        self.dirty = true;
    }

    /// Enter in the reminder menu: the typed time, with the chosen repeat if it names none.
    fn apply_typed_reminder(&mut self, shared: &mut Shared) {
        let Some((r, _)) = reminder::parse(self.remind_query.trim(), Local::now()) else { return };
        let r = if r.repeat == Repeat::Once { r.repeating(self.remind_repeat) } else { r };
        self.set_reminder(r);
        self.popover = None;
        self.changed(shared, false);
    }

    fn set_color(&mut self, color: u16) {
        self.color = color;
        self.dirty = true;
        self.layout_dirty = true;
    }

    /// Advances animations and timers. Returns when this note next needs to wake up.
    pub fn tick(&mut self, now: Instant) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        let mut wake = |t: Instant| next = Some(next.map_or(t, |n: Instant| n.min(t)));

        self.chrome.to(if self.hovered || self.ringing { 1.0 } else { 0.0 });
        if !self.closing && !self.hiding {
            let target = match self.fade_at(now) {
                Some(at) if at <= now => self.st.fade.opacity.clamp(0.05, 1.0),
                Some(at) => {
                    wake(at);
                    1.0
                }
                None => 1.0,
            };
            if (self.opacity.target_value() - target).abs() > 0.001 {
                // In slowly and softly (also when fading for being idle); back quickly when pointed
                // at, so a faded note never feels in the way.
                let back = target > self.opacity.target_value() && self.opacity.value() > 0.04;
                self.opacity.animate_to(target, if back { Feel::Quick } else { Feel::Gentle });
            }
            if self.appear.target_value() < 1.0 {
                self.appear.animate_to(1.0, Feel::Gentle);
            }
        } else if self.appear.target_value() > 0.0 {
            self.appear.animate_to(0.0, Feel::Smooth);
        }
        let pop = self.pop.as_mut().map(|(_, m)| m);
        let motions =
            [Some(&mut self.chrome), Some(&mut self.opacity), Some(&mut self.appear), Some(&mut self.pop_in), pop];
        for m in motions.into_iter().flatten() {
            let before = m.value();
            if m.tick(now) {
                wake(now + FRAME);
            }
            if m.value() != before {
                self.needs_paint = true;
            }
        }
        if self.pop.as_ref().is_some_and(|(_, m)| !m.moving()) {
            self.pop = None;
        }
        if self.hiding && !self.opacity.moving() {
            self.hiding = false;
            self.shown = false;
            self.window.set_visible(false);
        }

        if let Some(base) = self.wiggle {
            let moving = self.shake.tick(now);
            let dx = (self.shake.value() * self.scale()).round() as i32;
            self.window.set_outer_position(PhysicalPosition::new(base.x + dx, base.y));
            if moving {
                wake(now + FRAME);
            } else {
                self.wiggle = None;
            }
        }
        if self.ringing {
            // Wiggle when it starts, again a few seconds later, then now and then.
            let due = RING_NUDGES
                .iter()
                .map(|&d| self.ring_at + d)
                .find(|&t| t > self.last_nudge)
                .unwrap_or(self.last_nudge + NUDGE_EVERY);
            if now >= due {
                self.start_wiggle();
                wake(now + FRAME);
            } else {
                wake(due);
            }
            // The glow breathes.
            self.needs_paint = true;
            wake(now + PULSE_FRAME);
        }

        let blinking = self.focused && self.doc.selection().is_none() && now - self.last_input < BLINK_IDLE;
        if blinking {
            if now - self.blink_at >= BLINK {
                self.caret_on = !self.caret_on;
                self.blink_at = now;
                self.needs_paint = true;
            }
            wake(self.blink_at + BLINK);
        } else if !self.caret_on {
            self.caret_on = true;
            self.needs_paint = true;
        }
        next
    }

    // ---- events -------------------------------------------------------------------------

    pub fn handle(&mut self, event: &WindowEvent, shared: &mut Shared) -> Option<Cmd> {
        if crate::input::debug() && !matches!(event, WindowEvent::RedrawRequested) {
            eprintln!(
                "note {}: {event:?} | resizing={} button_down={}",
                self.id,
                self.resizing.is_some(),
                crate::input::left_button_down()
            );
        }
        if self.collapsed() {
            if let Some(result) = self.handle_compact(event) {
                return result;
            }
        }
        match event {
            // Painting is paced by the app; an OS repaint request just marks the note dirty.
            WindowEvent::RedrawRequested => self.needs_paint = true,
            WindowEvent::CloseRequested => return Some(Cmd::SystemClose),
            WindowEvent::Resized(size) => {
                let logical = size.to_logical::<f64>(self.window.scale_factor());
                // A compact line is smaller than any page; never take its size as the page's.
                let page = !self.collapsed() && self.extra_h == 0;
                if page && logical.width >= MIN_SIZE.0 - 1.0 && logical.height >= MIN_SIZE.1 - 1.0 {
                    self.full_size = logical;
                }
                self.layout_dirty = true;
                self.dirty = true;
                self.needs_paint = true;
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                self.layout_dirty = true;
                self.dirty = true;
                self.needs_paint = true;
            }
            WindowEvent::Moved(p) => {
                let minimized = self.window.is_minimized().unwrap_or(false);
                if self.wiggle.is_none() && !minimized && p.x > -30000 && p.y > -30000 {
                    self.placed_at = Some((p.x, p.y));
                    self.dirty = true;
                }
            }
            WindowEvent::ThemeChanged(theme) => {
                self.dark = crate::theme::is_dark(Some(*theme));
                self.layout_dirty = true;
                self.needs_paint = true;
            }
            WindowEvent::Focused(focused) => {
                self.focused = *focused;
                self.last_active = Instant::now();
                self.editing = false;
                self.drag_select = false;
                if !focused && self.consume_reminders() {
                    self.refresh(shared, true);
                }
                self.refresh(shared, false);
                if !focused && self.compact && self.expanded && self.st.compact.collapse_on_blur {
                    self.collapse();
                }
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            // Synthetic presses replay keys held while focus arrives (e.g. the N of Ctrl+N
            // that just created this note); they must not type.
            WindowEvent::KeyboardInput { event, is_synthetic: false, .. } => return self.on_key(event, shared),
            WindowEvent::Ime(ime) => self.on_ime(ime, shared),
            WindowEvent::CursorMoved { position, .. } => self.on_pointer_move(position.x, position.y, shared),
            WindowEvent::CursorEntered { .. } => {
                self.hovered = true;
            }
            WindowEvent::CursorLeft { .. } => {
                self.hovered = false;
                if self.hover.take().is_some() {
                    self.needs_paint = true;
                }
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                if *state == ElementState::Pressed {
                    return self.on_press(shared);
                }
                self.drag_select = false;
                self.hue_drag = false;
                self.sync_format_bar();
                if self.moving.take().is_some() || matches!(self.zone(self.pointer.0, self.pointer.1), Zone::Bar) {
                    self.window.set_cursor(CursorIcon::Default);
                    crate::input::show_cursor(CursorIcon::Default);
                }
                if self.resizing.take().is_some() {
                    // The new size is saved with the next batch.
                    self.dirty = true;
                    self.needs_paint = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. }
                if self.popover == Some(Popover::Icons)
                    && self.pop_rect.is_some_and(|r| contains(r, self.pointer.0 as f32, self.pointer.1 as f32)) =>
            {
                let down = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y < 0.0,
                    MouseScrollDelta::PixelDelta(p) => p.y < 0.0,
                };
                self.icon_row = if down { self.icon_row + 1 } else { self.icon_row.saturating_sub(1) };
                self.needs_paint = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 3.0 * self.st.line() * self.scale(),
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                self.scroll -= dy;
                self.clamp_scroll();
                self.needs_paint = true;
            }
            _ => {}
        }
        None
    }

    fn on_ime(&mut self, ime: &Ime, shared: &mut Shared) {
        match ime {
            Ime::Preedit(text, cursor) => {
                self.doc.preedit = (!text.is_empty())
                    .then(|| crate::editor::Preedit { text: text.clone(), cursor: cursor.map(|c| c.0) });
                self.layout_dirty = true;
                self.changed(shared, false);
            }
            Ime::Commit(text) => {
                self.editing = true;
                self.doc.preedit = None;
                self.doc.insert(text);
                self.changed(shared, true);
            }
            Ime::Disabled => {
                self.doc.preedit = None;
                self.layout_dirty = true;
                self.changed(shared, false);
            }
            Ime::Enabled => {}
        }
    }

    fn on_key(&mut self, ev: &KeyEvent, shared: &mut Shared) -> Option<Cmd> {
        if ev.state != ElementState::Pressed {
            return None;
        }
        if !matches!(ev.logical_key, Key::Named(NamedKey::Control | NamedKey::Shift | NamedKey::Alt | NamedKey::Super))
        {
            self.editing = true;
        }
        if let Some(consumed) = self.popover_key(ev, shared) {
            return consumed;
        }
        if let Some(action) = shared.config.note_action(ev, self.mods) {
            return self.run_action(action, shared);
        }
        let m = self.mods;
        // AltGr arrives as Ctrl+Alt on Windows and must type text (e.g. '@' on German layouts).
        let primary = if cfg!(target_os = "macos") { m.super_key() } else { m.control_key() && !m.alt_key() };
        let word = if cfg!(target_os = "macos") { m.alt_key() } else { m.control_key() };
        let shift = m.shift_key();

        if !matches!(
            ev.logical_key,
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::PageUp | NamedKey::PageDown)
        ) {
            self.goal_x = None;
        }

        if !primary {
            if let Some(text_changed) = self.table_key(&ev.logical_key, shift) {
                self.changed(shared, text_changed);
                return None;
            }
        }

        let doc = &mut self.doc;
        let mut text_changed = false;
        match &ev.logical_key {
            Key::Named(named) => match named {
                NamedKey::ArrowLeft | NamedKey::ArrowRight => {
                    let left = *named == NamedKey::ArrowLeft;
                    let to = match doc.selection() {
                        Some(r) if !shift => {
                            if left {
                                r.start
                            } else {
                                r.end
                            }
                        }
                        _ if left && word => doc.prev_word(doc.cursor),
                        _ if left => doc.prev_grapheme(doc.cursor),
                        _ if word => doc.next_word(doc.cursor),
                        _ => doc.next_grapheme(doc.cursor),
                    };
                    doc.move_to(to, shift);
                }
                NamedKey::ArrowUp => self.vertical(&mut shared.fonts, -1.0, 1.0, shift),
                NamedKey::ArrowDown => self.vertical(&mut shared.fonts, 1.0, 1.0, shift),
                NamedKey::PageUp | NamedKey::PageDown => {
                    let lines = (self.content_rect().3 / (self.st.line() * self.scale())).floor().max(1.0);
                    self.vertical(&mut shared.fonts, if *named == NamedKey::PageUp { -1.0 } else { 1.0 }, lines, shift);
                }
                NamedKey::Home => {
                    let to = if primary { 0 } else { doc.line_bounds(doc.cursor).0 };
                    doc.move_to(to, shift);
                }
                NamedKey::End => {
                    let to = if primary { doc.text.len() } else { doc.line_bounds(doc.cursor).1 };
                    doc.move_to(to, shift);
                }
                NamedKey::Backspace => {
                    doc.backspace(word);
                    text_changed = true;
                }
                NamedKey::Delete => {
                    doc.delete(word);
                    text_changed = true;
                }
                NamedKey::Enter => {
                    self.consume_reminders();
                    self.doc.newline();
                    text_changed = true;
                }
                NamedKey::Tab => {
                    doc.indent(shift);
                    text_changed = true;
                }
                NamedKey::Escape => {
                    if self.ringing {
                        return self.act(Action::Dismiss, shared);
                    }
                    doc.anchor = None;
                }
                _ => return self.type_text(ev, primary, shared),
            },
            Key::Character(c) if primary => match c.to_lowercase().as_str() {
                "a" => doc.select_all(),
                "c" => self.copy(shared),
                "x" => {
                    self.copy(shared);
                    if let Some(r) = self.doc.selection() {
                        self.doc.replace(r, "", crate::editor::EditKind::Other);
                        text_changed = true;
                    }
                }
                "v" => {
                    if let Some(t) = shared.clipboard.as_mut().and_then(|c| c.get_text().ok()) {
                        self.doc.insert(&t);
                        text_changed = true;
                    }
                }
                "z" if shift => {
                    doc.redo();
                    text_changed = true;
                }
                "z" => {
                    doc.undo();
                    text_changed = true;
                }
                "y" => {
                    doc.redo();
                    text_changed = true;
                }
                d @ ("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9") => {
                    self.set_color(d.parse::<u16>().unwrap_or(1) - 1)
                }
                _ => return None,
            },
            _ => return self.type_text(ev, primary, shared),
        }
        // Home, End and word jumps never leave the caret on a table's syntax.
        if self.doc.selection().is_none() {
            if let Some(g) = crate::table::grid_at(&self.doc.text, self.doc.cursor) {
                self.doc.cursor = g.snap(self.doc.cursor, true);
            }
        }
        self.changed(shared, text_changed);
        None
    }

    /// Notion-style keys in a table: Tab and Shift+Tab move between cells (Tab past the last
    /// cell adds a row), Enter goes to the cell below (adding a row at the bottom), the arrows
    /// move between cells and rows, and Backspace and Delete stop at a cell's edges. Returns
    /// whether the text changed, or None when the key is not the table's.
    fn table_key(&mut self, key: &Key, shift: bool) -> Option<bool> {
        let Key::Named(named) = key else { return None };
        let grid = crate::table::grid_at(&self.doc.text, self.doc.cursor)?;
        let (row, col) = grid.cell_of(self.doc.cursor)?;
        let cell = grid.cells[row][col].clone();
        let selecting = self.doc.selection().is_some();
        let to = |g: &crate::table::Grid, r: usize, c: usize| g.caret(r, c.min(g.cells[r].len().saturating_sub(1)));
        match named {
            NamedKey::Tab => match grid.step(row, col, !shift) {
                Some((r, c)) => {
                    self.doc.move_to(to(&grid, r, c)?, false);
                    Some(false)
                }
                None if !shift => {
                    self.add_table_row(&grid, 0);
                    Some(true)
                }
                None => Some(false),
            },
            NamedKey::Enter if !shift => {
                match grid.below(row) {
                    Some(r) => self.doc.move_to(to(&grid, r, col)?, false),
                    // Enter on an empty last row takes it away and leaves the table, like
                    // pressing Enter on an empty list item.
                    None if row > 1 && grid.cells[row].iter().all(|c| c.is_empty()) => {
                        let line = grid.rows[row].clone();
                        self.doc.replace(line.start - 1..line.end, "", EditKind::Other);
                        self.leave_table(grid.rows[row - 1].end);
                        return Some(true);
                    }
                    None => {
                        self.add_table_row(&grid, col);
                        return Some(true);
                    }
                }
                Some(false)
            }
            NamedKey::ArrowUp | NamedKey::ArrowDown if !shift => {
                let down = *named == NamedKey::ArrowDown;
                match if down { grid.below(row) } else { grid.above(row) } {
                    Some(r) => self.doc.move_to(to(&grid, r, col)?, false),
                    None if down => return Some(self.leave_table(grid.rows.last()?.end)),
                    None => return None,
                }
                Some(false)
            }
            NamedKey::Escape if !selecting && !self.ringing => Some(self.leave_table(grid.rows.last()?.end)),
            NamedKey::ArrowLeft if !shift && !selecting && self.doc.cursor <= cell.start => {
                let (r, c) = grid.step(row, col, false)?;
                self.doc.move_to(grid.caret(r, c)?, false);
                Some(false)
            }
            NamedKey::ArrowRight if !shift && !selecting && self.doc.cursor >= cell.end => {
                let (r, c) = grid.step(row, col, true)?;
                self.doc.move_to(grid.cells[r][c].start, false);
                Some(false)
            }
            NamedKey::Backspace if !selecting && self.doc.cursor <= cell.start => Some(false),
            NamedKey::Delete if !selecting && self.doc.cursor >= cell.end => Some(false),
            _ => None,
        }
    }

    /// Moves the caret to the line after a table that ends at `end`, adding one if the table is
    /// the last thing in the note. Returns whether the text changed.
    fn leave_table(&mut self, end: usize) -> bool {
        if self.doc.text[end..].starts_with('\n') {
            self.doc.move_to(end + 1, false);
            false
        } else {
            self.doc.replace(end..end, "\n", EditKind::Other);
            true
        }
    }

    /// A table row or column menu item: rewrites the table (one undo step) and puts the caret
    /// in the cell the action was about.
    fn table_menu_action(&mut self, code: u8, shared: &mut Shared) {
        use crate::table::{Align, Model, TableOp};
        let (Some((first, _)), Some(pop)) = (self.table_menu, self.popover) else { return };
        let pos = self.to_doc(first);
        let Some(grid) = crate::table::grid_at(&self.doc.text, pos) else { return };
        let (row, col) = match pop {
            Popover::TableRow(r) => (r as usize, 0),
            Popover::TableCol(c) => (0, c as usize),
            _ => return,
        };
        let mut model = Model::read(&self.doc.text, &grid);
        let (n, rows) = (model.columns(), model.rows.len());
        // Model rows have no separator: grid row 2 is model row 1.
        let r = row.saturating_sub(usize::from(row > 0));
        let (op, target) = match code {
            0 => (TableOp::InsertRow { at: r }, (r, col)),
            1 => (TableOp::InsertRow { at: r + 1 }, (r + 1, col)),
            2 => (TableOp::DuplicateRow(r), (r + 1, col)),
            3 => (TableOp::MoveRow { from: r, to: r.saturating_sub(1) }, (r.saturating_sub(1), col)),
            4 => (TableOp::MoveRow { from: r, to: (r + 1).min(rows - 1) }, ((r + 1).min(rows - 1), col)),
            5 => (TableOp::DeleteRow(r), (r.min(rows.saturating_sub(2)), col)),
            6 => (TableOp::DeleteTable, (0, 0)),
            10 => (TableOp::InsertColumn { at: col }, (0, col)),
            11 => (TableOp::InsertColumn { at: col + 1 }, (0, col + 1)),
            12 => (TableOp::MoveColumn { from: col, to: col.saturating_sub(1) }, (0, col.saturating_sub(1))),
            13 => (TableOp::MoveColumn { from: col, to: (col + 1).min(n - 1) }, (0, (col + 1).min(n - 1))),
            14 => (TableOp::AlignColumn(col, Align::Left), (0, col)),
            15 => (TableOp::AlignColumn(col, Align::Center), (0, col)),
            16 => (TableOp::AlignColumn(col, Align::Right), (0, col)),
            17 => (TableOp::DeleteColumn(col), (0, col.min(n.saturating_sub(2)))),
            _ => return,
        };
        let span = grid.rows[0].start..grid.rows.last().map_or(0, |l| l.end);
        if model.apply(op) {
            self.doc.replace(span.clone(), &model.write(), EditKind::Other);
            if let Some(g) = crate::table::grid_at(&self.doc.text, span.start) {
                let (tr, tc) = target;
                let grid_row = if tr == 0 { 0 } else { tr + 1 };
                if let Some(p) = g.caret(grid_row.min(g.rows.len() - 1), tc) {
                    self.doc.move_to(p, false);
                }
            }
        } else {
            // The whole table goes, with the line break after it.
            let end = if self.doc.text[span.end..].starts_with('\n') { span.end + 1 } else { span.end };
            self.doc.replace(span.start..end, "", EditKind::Other);
        }
        self.popover = None;
        self.table_menu = None;
        self.editing = true;
        self.window.focus_window();
        self.changed(shared, true);
    }

    /// Adds an empty row at the bottom of `grid` and puts the caret in its cell `col`.
    fn add_table_row(&mut self, grid: &crate::table::Grid, col: usize) {
        let Some(at) = grid.rows.last().map(|r| r.end) else { return };
        self.doc.replace(at..at, &grid.new_row(), EditKind::Other);
        if let Some(g) = crate::table::grid_at(&self.doc.text, at + 1) {
            if let Some(p) = g.caret(g.rows.len() - 1, col) {
                self.doc.move_to(p, false);
            }
        }
    }

    /// The + buttons on a table: a row at the bottom, or a column at the right.
    fn table_add(&mut self, first: usize, column: bool, shared: &mut Shared) -> Option<Cmd> {
        let pos = self.to_doc(first);
        let grid = crate::table::grid_at(&self.doc.text, pos)?;
        if column {
            let span = grid.rows[0].start..grid.rows.last()?.end;
            let mut t = self.doc.text[span.clone()].to_string();
            for (at, cell) in grid.new_column() {
                t.insert_str(at - span.start, &cell);
            }
            self.doc.replace(span, &t, EditKind::Other);
            if let Some(g) = crate::table::grid_at(&self.doc.text, pos) {
                if let Some(p) = g.caret(0, g.columns() - 1) {
                    self.doc.move_to(p, false);
                }
            }
        } else {
            self.add_table_row(&grid, 0);
        }
        self.editing = true;
        self.window.focus_window();
        self.changed(shared, true);
        None
    }

    /// Shows the formatting toolbar while text is selected (once the drag is over), hides it
    /// when the selection goes.
    fn sync_format_bar(&mut self) {
        let selecting = self.focused && !self.drag_select && self.doc.selection().is_some_and(|r| !r.is_empty());
        match (self.popover, selecting) {
            (None, true) => {
                self.popover = Some(Popover::Format { colors: false });
                self.pop_hover = None;
                self.needs_paint = true;
            }
            (Some(Popover::Format { .. }), false) => {
                self.popover = None;
                self.needs_paint = true;
            }
            _ => {}
        }
    }

    /// Runs a user-bindable note action (see `config.rs` for names and defaults).
    fn run_action(&mut self, action: NoteAction, shared: &mut Shared) -> Option<Cmd> {
        let mut text = false;
        match action {
            NoteAction::NewNote => return Some(Cmd::NewNote),
            NoteAction::CloseNote => return Some(Cmd::Close),
            NoteAction::DeleteNote => return Some(Cmd::Delete),
            NoteAction::Quit => return Some(Cmd::Quit),
            NoteAction::CycleMode => return self.act(Action::CycleMode, shared),
            NoteAction::NextColor => return self.act(Action::CycleColor, shared),
            NoteAction::ClearReminder => return self.act(Action::ClearReminder, shared),
            NoteAction::PickIcon => return self.act(Action::Icon, shared),
            NoteAction::MoveToList => return self.act(Action::More, shared),
            NoteAction::ToggleCompact => {
                self.set_compact(!self.compact);
                return None;
            }
            NoteAction::PickColor => return self.act(Action::Colors, shared),
            NoteAction::OpenSettings => return Some(Cmd::OpenSettings),
            NoteAction::ToggleTodo => {
                self.doc.toggle_check();
                let ls = self.doc.line_bounds(self.doc.cursor).0;
                self.pop_checkbox(ls);
                text = true;
            }
            NoteAction::Bold => {
                self.doc.wrap("**");
                text = true;
            }
            NoteAction::Italic => {
                self.doc.wrap("*");
                text = true;
            }
            NoteAction::InlineCode => {
                self.doc.wrap("`");
                text = true;
            }
            NoteAction::Strikethrough => {
                self.doc.wrap("~~");
                text = true;
            }
            NoteAction::Highlight => {
                self.doc.wrap("==");
                text = true;
            }
            NoteAction::AddReminder => return self.act(Action::Remind, shared),
            NoteAction::ZoomIn => self.set_zoom(self.zoom + 10),
            NoteAction::ZoomOut => self.set_zoom(self.zoom.saturating_sub(10)),
            NoteAction::ZoomReset => self.set_zoom(100),
        }
        self.changed(shared, text);
        None
    }

    fn type_text(&mut self, ev: &KeyEvent, primary: bool, shared: &mut Shared) -> Option<Cmd> {
        if primary {
            return None;
        }
        if let Some(t) = ev.text.as_ref().filter(|t| t.chars().any(|c| !c.is_control())) {
            self.doc.insert(t);
            // A slash at the start of a line or after a space opens the block menu (Notion).
            let at = self.doc.cursor.saturating_sub(1);
            if t == "/" && (at == 0 || self.doc.text[..at].ends_with(char::is_whitespace)) {
                self.popover = Some(Popover::Slash { at, sel: 0 });
            }
            // ":fire:" becomes 🔥 as soon as the closing colon is typed.
            if t == ":" {
                if let Some((start, emoji)) = self.closed_shortcode() {
                    self.doc.replace(start..self.doc.cursor, emoji, EditKind::Other);
                    self.popover = None;
                }
            }
            self.changed(shared, true);
        }
        None
    }

    // ---- popovers -----------------------------------------------------------------------

    /// The `:query` being typed at the caret: (offset of the colon, query).
    fn shortcode_query(&self) -> Option<(usize, &str)> {
        let t = &self.doc.text;
        let c = self.doc.cursor;
        let start = t[..c].rfind(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '+' | '-')))?;
        if t.as_bytes()[start] != b':' {
            return None;
        }
        // The colon must start a word, so "10:30" and "http://" don't trigger it.
        if start > 0 && !t[..start].ends_with(char::is_whitespace) {
            return None;
        }
        Some((start, &t[start + 1..c]))
    }

    /// A just-closed ":name:" before the caret that names an emoji: (start offset, emoji).
    fn closed_shortcode(&self) -> Option<(usize, &'static str)> {
        let t = &self.doc.text;
        let c = self.doc.cursor;
        let inner = t[..c].strip_suffix(':')?;
        let open = inner.rfind(':')?;
        let name = &inner[open + 1..];
        let word_start = open == 0 || t[..open].ends_with(char::is_whitespace);
        let valid =
            !name.is_empty() && name.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '+' | '-'));
        if !(word_start && valid) {
            return None;
        }
        Some((open, popover::shortcode(name)?))
    }

    fn emoji_matches(&self) -> Vec<(&'static str, &'static str)> {
        match self.shortcode_query() {
            Some((_, q)) if q.len() >= 2 => popover::shortcode_matches(q, 7),
            _ => Vec::new(),
        }
    }

    /// Shows `:shortcode` completion while one is being typed (never over another popover).
    fn validate_emoji(&mut self) {
        if !matches!(self.popover, None | Some(Popover::Emoji { .. })) {
            return;
        }
        let sel = match self.popover {
            Some(Popover::Emoji { sel, .. }) => sel,
            _ => 0,
        };
        let n = self.emoji_matches().len();
        self.popover = match self.shortcode_query() {
            Some((at, _)) if n > 0 => Some(Popover::Emoji { at, sel: sel.min(n - 1) }),
            _ => None,
        };
    }

    fn apply_emoji(&mut self, emoji: &str, shared: &mut Shared) {
        if let Some((at, _)) = self.shortcode_query() {
            let c = self.doc.cursor;
            self.doc.replace(at..c, emoji, EditKind::Other);
        }
        self.popover = None;
        self.changed(shared, true);
    }

    fn slash_matches(&self) -> Vec<usize> {
        match self.popover {
            Some(Popover::Slash { at, .. }) if self.doc.cursor > at && at < self.doc.text.len() => {
                popover::filter(&self.doc.text[at + 1..self.doc.cursor])
            }
            _ => Vec::new(),
        }
    }

    /// Closes the slash menu once the caret leaves the `/query` or nothing matches.
    fn validate_slash(&mut self) {
        let Some(Popover::Slash { at, sel }) = self.popover else { return };
        let t = &self.doc.text;
        let c = self.doc.cursor;
        let valid = at < t.len()
            && t.as_bytes()[at] == b'/'
            && c > at
            && c - at <= 24
            && !t[at + 1..c].contains(char::is_whitespace);
        let n = if valid { self.slash_matches().len() } else { 0 };
        self.popover = if n > 0 { Some(Popover::Slash { at, sel: sel.min(n - 1) }) } else { None };
        self.needs_paint = true;
    }

    /// Keys while a popover is open. `Some` means the key was consumed.
    fn popover_key(&mut self, ev: &KeyEvent, shared: &mut Shared) -> Option<Option<Cmd>> {
        let pop = self.popover?;
        if matches!(pop, Popover::Format { .. }) {
            return None;
        }
        // The icon picker has its own search field: typing goes there, not into the note.
        if pop == Popover::Icons {
            match &ev.logical_key {
                Key::Named(NamedKey::Escape) => self.popover = None,
                Key::Named(NamedKey::Backspace) => {
                    self.icon_query.pop();
                    self.icon_row = 0;
                }
                Key::Named(NamedKey::Enter) => {
                    if let Some(first) = popover::icons(self.icon_tab, &self.icon_query).first() {
                        let _ = self.pop_click(PopHit::Icon(first), shared);
                    }
                }
                _ => {
                    let text = ev.text.as_deref().filter(|t| t.chars().all(|c| !c.is_control()))?;
                    self.icon_query.push_str(text);
                    self.icon_row = 0;
                }
            }
            self.needs_paint = true;
            return Some(None);
        }
        if pop == Popover::Remind {
            match &ev.logical_key {
                Key::Named(NamedKey::Escape) => self.popover = None,
                Key::Named(NamedKey::Backspace) => {
                    self.remind_query.pop();
                }
                Key::Named(NamedKey::Enter) => self.apply_typed_reminder(shared),
                _ => {
                    let text = ev.text.as_deref().filter(|t| t.chars().all(|c| !c.is_control()))?;
                    self.remind_query.push_str(text);
                }
            }
            self.needs_paint = true;
            return Some(None);
        }
        let Key::Named(named) = &ev.logical_key else { return None };
        match (pop, named) {
            (_, NamedKey::Escape) => self.popover = None,
            (Popover::Emoji { at, sel }, NamedKey::ArrowUp | NamedKey::ArrowDown) => {
                let n = self.emoji_matches().len().max(1);
                let sel = if *named == NamedKey::ArrowUp { (sel + n - 1) % n } else { (sel + 1) % n };
                self.popover = Some(Popover::Emoji { at, sel });
            }
            (Popover::Emoji { sel, .. }, NamedKey::Enter | NamedKey::Tab) => {
                if let Some(&(_, emoji)) = self.emoji_matches().get(sel) {
                    self.apply_emoji(emoji, shared);
                }
            }
            (Popover::Slash { at, sel }, NamedKey::ArrowUp | NamedKey::ArrowDown) => {
                let n = self.slash_matches().len().max(1);
                let sel = if *named == NamedKey::ArrowUp { (sel + n - 1) % n } else { (sel + 1) % n };
                self.popover = Some(Popover::Slash { at, sel });
            }
            (Popover::Slash { sel, .. }, NamedKey::Enter | NamedKey::Tab) => {
                if let Some(&i) = self.slash_matches().get(sel) {
                    self.apply_slash(i, shared);
                }
            }
            _ => return None,
        }
        self.needs_paint = true;
        Some(None)
    }

    /// Replaces the typed `/query` with the chosen block.
    fn apply_slash(&mut self, item: usize, shared: &mut Shared) {
        let Some(Popover::Slash { at, .. }) = self.popover.take() else { return };
        let end = self.doc.cursor.max(at + 1).min(self.doc.text.len());
        self.doc.replace(at..end, "", EditKind::Other);
        match SLASH[item].kind {
            SlashKind::Text => self.doc.set_line_prefix(""),
            SlashKind::Prefix(prefix) => self.doc.set_line_prefix(prefix),
            SlashKind::CodeBlock => {
                let c = self.doc.cursor;
                let lead = if c == 0 || self.doc.text[..c].ends_with('\n') { "" } else { "\n" };
                self.doc.replace(c..c, &format!("{lead}```\n\n```"), EditKind::Other);
                self.doc.cursor = c + lead.len() + 4;
            }
            SlashKind::Table => {
                self.doc.set_line_prefix("");
                let c = self.doc.cursor;
                let lead = if c == 0 || self.doc.text[..c].ends_with('\n') { "" } else { "\n" };
                self.doc.replace(c..c, &format!("{lead}| Column | Column |\n| --- | --- |\n|  |  |"), EditKind::Other);
                self.doc.cursor = c + lead.len() + 2;
            }
            SlashKind::Divider => {
                self.doc.set_line_prefix("");
                let c = self.doc.cursor;
                let ls = self.doc.line_bounds(c).0;
                self.doc.replace(ls..ls, "---\n", EditKind::Other);
                self.doc.cursor = c + 4;
            }
            SlashKind::Reminder => self.doc.insert("@"),
            SlashKind::Icon => self.popover = Some(Popover::Icons),
        }
        self.changed(shared, true);
    }

    fn pop_click(&mut self, hit: PopHit, shared: &mut Shared) -> Option<Cmd> {
        match hit {
            PopHit::Slash(i) => {
                self.apply_slash(i, shared);
                return None;
            }
            PopHit::Shortcode(emoji) => {
                self.apply_emoji(emoji, shared);
                return None;
            }
            PopHit::Tab(i) => {
                // Switching tab keeps the picker open.
                self.icon_tab = i;
                self.icon_query.clear();
                self.icon_row = 0;
                self.needs_paint = true;
                return None;
            }
            PopHit::Icon(emoji) => self.icon = emoji.to_string(),
            PopHit::RemoveIcon => self.icon.clear(),
            PopHit::List(id) => self.list = id,
            PopHit::Color(c) => self.set_color(c),
            PopHit::Desktop(i) => {
                let (id, _) = self.desktop_list.get(i as usize)?.clone();
                self.popover = None;
                self.needs_paint = true;
                return Some(Cmd::MoveToDesktop(id));
            }
            PopHit::Fmt(4) => {
                self.doc.link();
                self.popover = None;
                self.changed(shared, true);
                return None;
            }
            PopHit::Fmt(k) => {
                let marker = ["**", "*", "~~", "`"][k as usize % 4];
                self.doc.toggle_wrap(marker);
                self.changed(shared, true);
                return None;
            }
            PopHit::FmtColors => {
                if let Some(Popover::Format { colors }) = self.popover {
                    self.popover = Some(Popover::Format { colors: !colors });
                }
                self.needs_paint = true;
                return None;
            }
            PopHit::FmtTextColor(c) => {
                self.doc.set_text_color(markdown::HIGHLIGHTS.get(c as usize).copied());
                self.changed(shared, true);
                return None;
            }
            PopHit::TableOp(code) => {
                self.table_menu_action(code, shared);
                return None;
            }
            PopHit::FmtColor(c) => {
                self.doc.set_highlight(markdown::HIGHLIGHTS.get(c as usize).copied());
                self.changed(shared, true);
                return None;
            }
            PopHit::Hue => return None,
            PopHit::Compact => {
                self.set_compact(!self.compact);
                return None;
            }
            PopHit::RemindIn(secs) => self.set_reminder(Reminder::once(Local::now().timestamp() + secs)),
            PopHit::RemindDay(day) => {
                self.remind_day = day;
                self.needs_paint = true;
                return None;
            }
            PopHit::RemindStep(minutes) => {
                self.remind_time = reminder::nudge(self.remind_time, minutes);
                self.needs_paint = true;
                return None;
            }
            PopHit::RemindSet => {
                let due = reminder::at_day(Local::now(), self.remind_day, self.remind_time)?;
                self.set_reminder(Reminder::once(due).repeating(self.remind_repeat));
            }
            PopHit::RemindTime(i) => {
                let time = *shared.config.reminders.times.get(i)?;
                self.remind_time = time;
                let due = reminder::at_day(Local::now(), self.remind_day, time)?;
                self.set_reminder(Reminder::once(due).repeating(self.remind_repeat));
            }
            PopHit::RemindRepeat(i) => {
                // Applies to the reminder already set, and to the next time picked.
                self.remind_repeat = Repeat::CHOICES.get(i)?.0;
                if let Some(r) = self.reminder.take() {
                    self.reminder = Some(r.repeating(self.remind_repeat));
                    self.dirty = true;
                }
                self.needs_paint = true;
                return None;
            }
            PopHit::RemindClear => {
                self.reminder = None;
                self.stop_ringing();
            }
            PopHit::Mode(m) => {
                self.mode = Mode::from_u8(m);
                self.apply_level();
            }
            PopHit::Delete => {
                // Two clicks, so a stray one never deletes anything.
                if shared.config.general.confirm_delete && !self.confirm_delete {
                    self.confirm_delete = true;
                    self.needs_paint = true;
                    return None;
                }
                self.confirm_delete = false;
                self.popover = None;
                return Some(Cmd::Delete);
            }
        }
        self.popover = None;
        self.pop_hover = None;
        self.dirty = true;
        self.layout_dirty = true;
        self.changed(shared, false);
        None
    }

    fn pop_layout(&self, lists: &[List], rs: &ReminderSettings, fonts: &mut FontSystem) -> Option<PopDraw> {
        let pop = self.popover?;
        let s = self.scale();
        let pal = self.palette();
        let (kx, ky, kw, kh) = self.card();
        let mut d = PopDraw {
            rect: (0.0, 0.0, 0.0, 0.0),
            cells: Vec::new(),
            tiles: Vec::new(),
            selected: Vec::new(),
            bars: Vec::new(),
            dots: Vec::new(),
            hue: None,
            labels: Vec::new(),
        };
        let label_h = |b: &Buffer| b.layout_runs().next().map_or(0.0, |r| r.line_height);
        match pop {
            Popover::Icons => {
                let (cell, pad) = (32.0 * s, 8.0 * s);
                let (search_h, tabs_h) = (32.0 * s, 30.0 * s);
                let cols = ((kw - 24.0 * s - 2.0 * pad) / cell).floor().clamp(5.0, 9.0) as usize;
                let w = cols as f32 * cell + 2.0 * pad;
                let x = kx + 12.0 * s;
                let y = ky + (self.st.top_bar - 4.0 + self.st.icon_row(!self.icon.is_empty())) * s;
                let grid_top = y + pad + search_h + tabs_h;
                let fit = (((ky + kh - 8.0 * s) - grid_top - pad) / cell).floor().max(2.0) as usize;
                let list = popover::icons(self.icon_tab, &self.icon_query);
                let total = list.len().div_ceil(cols);
                let rows = total.min(fit).max(1);
                let first = self.icon_row.min(total.saturating_sub(rows));
                let h = grid_top - y + rows as f32 * cell + pad;
                d.rect = (x, y, w, h);

                // Search field, with "Remove" beside it when the note has an icon.
                let remove = (!self.icon.is_empty()).then(|| small_label(fonts, "Remove", 12.0 * s, Weight::NORMAL));
                let remove_w = remove.as_ref().map_or(0.0, |b| label_width(b) + 16.0 * s);
                let field = (x + pad, y + pad, w - 2.0 * pad - remove_w, search_h - 4.0 * s);
                d.tiles.push(field);
                let (text, color) = if self.icon_query.is_empty() {
                    (
                        format!("Search {}…", ICON_TABS[self.icon_tab.min(ICON_TABS.len() - 1)].1),
                        pal.secondary.text(220),
                    )
                } else {
                    (format!("{}|", self.icon_query), pal.text.text(255))
                };
                let q = small_label(fonts, &text, 12.5 * s, Weight::NORMAL);
                let at = (field.0 + 10.0 * s, field.1 + (field.3 - label_h(&q)) / 2.0);
                d.labels.push((q, color, at));
                if let Some(b) = remove {
                    let rx = x + w - pad - remove_w + 8.0 * s;
                    d.cells.push(((rx - 6.0 * s, field.1, remove_w - 4.0 * s, field.3), PopHit::RemoveIcon));
                    let at = (rx, field.1 + (field.3 - label_h(&b)) / 2.0);
                    d.labels.push((b, pal.secondary.text(255), at));
                }

                // Category tabs (hidden while searching): small, dimmed unless selected, the
                // selected one underlined, and a divider below so they don't read as icons.
                let tab_w = (w - 2.0 * pad) / ICON_TABS.len() as f32;
                let divider = (x + pad, y + pad + search_h + tabs_h - 3.0 * s, w - 2.0 * pad, 1.0 * s);
                if self.icon_query.is_empty() {
                    for (t, (glyph, _, _)) in ICON_TABS.iter().enumerate() {
                        let r = (x + pad + t as f32 * tab_w, y + pad + search_h, tab_w, tabs_h - 4.0 * s);
                        let active = t == self.icon_tab;
                        if active {
                            d.bars.push(((r.0 + r.2 * 0.2, divider.1 - 1.0 * s, r.2 * 0.6, 2.5 * s), true));
                        }
                        let b = small_label(fonts, glyph, 12.5 * s, Weight::NORMAL);
                        let at = (r.0 + (r.2 - label_width(&b)) / 2.0, r.1 + (r.3 - label_h(&b)) / 2.0);
                        d.labels.push((b, pal.text.text(if active { 255 } else { 110 }), at));
                        d.cells.push((r, PopHit::Tab(t)));
                    }
                    d.bars.push((divider, false));
                } else {
                    let found = format!("{} found", list.len());
                    let b = small_label(fonts, &found, 11.5 * s, Weight::MEDIUM);
                    let at = (x + pad + 4.0 * s, y + pad + search_h + (tabs_h - 4.0 * s - label_h(&b)) / 2.0);
                    d.labels.push((b, pal.secondary.text(255), at));
                }

                // The grid: whole rows only, scrolled with the wheel.
                for (k, e) in list.iter().enumerate().skip(first * cols).take(rows * cols) {
                    let k = k - first * cols;
                    let r = (x + pad + (k % cols) as f32 * cell, grid_top + (k / cols) as f32 * cell, cell, cell);
                    let b = small_label(fonts, e, 19.0 * s, Weight::NORMAL);
                    let at = (r.0 + (cell - label_width(&b)) / 2.0, r.1 + (cell - label_h(&b)) / 2.0);
                    d.labels.push((b, pal.text.text(255), at));
                    d.cells.push((r, PopHit::Icon(e)));
                }
            }
            Popover::Colors => {
                let (cols, pad, header, cell) = (6usize, 10.0 * s, 30.0 * s, 30.0 * s);
                let w = (cols as f32 * cell + 2.0 * pad).min(kw - 24.0 * s);
                let cell_w = (w - 2.0 * pad) / cols as f32;
                let rows = (theme::PRESETS as usize).div_ceil(cols);
                let strip_label = 22.0 * s;
                let strip_h = 14.0 * s;
                let h = header + rows as f32 * cell + strip_label + strip_h + 2.0 * pad;
                let x = kx + kw - 12.0 * s - w;
                let y = ky + (self.st.top_bar - 4.0) * s;
                d.rect = (x, y, w, h);
                // "Colour · Teal": the hovered swatch's name, else the current colour's.
                let shown = match self.pop_hover {
                    Some(PopHit::Color(c)) => c,
                    _ => self.color,
                };
                let title =
                    small_label(fonts, &format!("Colour · {}", theme::color_name(shown)), 11.5 * s, Weight::MEDIUM);
                d.labels.push((title, pal.secondary.text(255), (x + 12.0 * s, y + 9.0 * s)));
                for c in 0..theme::PRESETS {
                    let i = c as usize;
                    let r = (x + pad + (i % cols) as f32 * cell_w, y + header + (i / cols) as f32 * cell, cell_w, cell);
                    let (accent, _) = theme::tint(c, self.dark);
                    d.dots.push(((r.0 + r.2 / 2.0, r.1 + r.3 / 2.0), 8.5 * s, accent, c == self.color));
                    d.cells.push((r, PopHit::Color(c)));
                }
                let sy = y + header + rows as f32 * cell;
                let custom = small_label(fonts, "Custom", 11.5 * s, Weight::MEDIUM);
                let at = (x + pad + 2.0 * s, sy + (strip_label - label_h(&custom)) / 2.0 + 2.0 * s);
                d.labels.push((custom, pal.secondary.text(255), at));
                let strip = (x + pad, sy + strip_label, w - 2.0 * pad, strip_h);
                d.hue = Some((strip, theme::custom_hue(self.color)));
                d.cells.push(((strip.0, strip.1 - 6.0 * s, strip.2, strip.3 + 12.0 * s), PopHit::Hue));
            }
            Popover::Format { colors } => {
                let sel = self.doc.selection()?;
                let (a, b) = (self.cursor_at(self.to_display(sel.start)), self.cursor_at(self.to_display(sel.end)));
                let first = *self.span_rects(a, b).first()?;
                let (cx, cy, _, _) = self.content_rect();
                let (btn, pad, gap, color_w, sw, cap) = (28.0 * s, 4.0 * s, 2.0 * s, 40.0 * s, 24.0 * s, 18.0 * s);
                let row_w = 2.0 * pad + 5.0 * btn + 4.0 * gap + 9.0 * s + color_w;
                let w = if colors { row_w.max(2.0 * pad + 10.0 * sw) } else { row_w };
                let h = 2.0 * pad + btn + if colors { 2.0 * (cap + sw) + 6.0 * s } else { 0.0 };
                // Above the selection, or below it when there is no room.
                let top = cy + first.1 - self.scroll;
                let mut y = top - h - 6.0 * s;
                if y < ky + 4.0 * s {
                    y = top + first.3 + 6.0 * s;
                }
                let x = (cx + first.0 - 6.0 * s).clamp(kx + 6.0 * s, (kx + kw - 6.0 * s - w).max(kx + 6.0 * s));
                d.rect = (x, y, w, h);
                let doc = &self.doc;
                let active = [
                    doc.is_wrapped("**"),
                    doc.is_wrapped("*") && !doc.is_wrapped("**"),
                    doc.is_wrapped("~~"),
                    doc.is_wrapped("`"),
                    false,
                ];
                let looks = [
                    ("B", Weight::BOLD, false, false, false),
                    ("I", Weight::NORMAL, true, false, false),
                    ("S", Weight::NORMAL, false, true, false),
                    ("</>", Weight::MEDIUM, false, false, true),
                    ("🔗", Weight::NORMAL, false, false, false),
                ];
                let mut bx = x + pad;
                for (k, (text, weight, italic, strike, mono)) in looks.into_iter().enumerate() {
                    let r = (bx, y + pad, btn, btn);
                    if active[k] {
                        d.selected.push(r);
                    }
                    let size = if mono {
                        11.5 * s
                    } else if k == 4 {
                        12.5 * s
                    } else {
                        14.0 * s
                    };
                    let b = format_label(fonts, text, size, weight, (italic, strike, mono));
                    let at = (r.0 + (btn - label_width(&b)) / 2.0, r.1 + (btn - label_h(&b)) / 2.0);
                    let color = if active[k] { pal.accent } else { pal.text };
                    d.labels.push((b, color.text(255), at));
                    d.cells.push((r, PopHit::Fmt(k as u8)));
                    bx += btn + gap;
                }
                // A divider, then the colour button: the current colour and a caret.
                d.bars.push(((bx + 2.0 * s, y + pad + 7.0 * s, 1.0 * s, btn - 14.0 * s), false));
                bx += 9.0 * s;
                let named = |c: String| markdown::HIGHLIGHTS.iter().position(|h| *h == c);
                let background =
                    doc.highlight_around().and_then(|(_, _, c)| named(if c.is_empty() { "yellow".into() } else { c }));
                let ink = doc.text_color_around().and_then(|(_, _, c)| named(c));
                let r = (bx, y + pad, color_w, btn);
                if colors {
                    d.selected.push(r);
                }
                let dot = background.or(ink).map_or(pal.secondary, |i| theme::tint(HIGHLIGHT_TINT[i], self.dark).0);
                d.dots.push(((r.0 + 13.0 * s, r.1 + btn / 2.0), 6.5 * s, dot, false));
                let caret = small_label(fonts, "▾", 11.0 * s, Weight::NORMAL);
                let at = (r.0 + 25.0 * s, r.1 + (btn - label_h(&caret)) / 2.0);
                d.labels.push((caret, pal.secondary.text(255), at));
                d.cells.push((r, PopHit::FmtColors));
                if colors {
                    // Two rows, as in Notion: the text's colour, then its background.
                    let outline = if self.dark { Rgb(0x55, 0x55, 0x58) } else { Rgb(0xD9, 0xD8, 0xD4) };
                    let mut ry = y + pad + btn + 6.0 * s;
                    for (title, text_row) in [("COLOR", true), ("BACKGROUND", false)] {
                        let b = small_label(fonts, title, 9.5 * s, Weight::SEMIBOLD);
                        d.labels.push((b, pal.secondary.text(220), (x + pad + 4.0 * s, ry + 4.0 * s)));
                        ry += cap;
                        let current = if text_row { ink } else { background };
                        for k in 0..10 {
                            let r = (x + pad + k as f32 * sw, ry, sw, sw);
                            let center = (r.0 + sw / 2.0, r.1 + sw / 2.0);
                            let hit = if k == 0 { 255 } else { k as u8 - 1 };
                            let on = if k == 0 { current.is_none() } else { current == Some(k - 1) };
                            if on {
                                d.selected.push(r);
                            }
                            if text_row {
                                // "A" in each colour; the first is the default colour.
                                let color =
                                    if k == 0 { pal.text } else { theme::tint(HIGHLIGHT_TINT[k - 1], self.dark).0 };
                                let b = small_label(fonts, "A", 14.0 * s, Weight::SEMIBOLD);
                                let at = (center.0 - label_width(&b) / 2.0, center.1 - label_h(&b) / 2.0);
                                d.labels.push((b, color.text(255), at));
                                d.cells.push((r, PopHit::FmtTextColor(hit)));
                            } else if k == 0 {
                                let b = small_label(fonts, "⊘", 14.0 * s, Weight::NORMAL);
                                let at = (center.0 - label_width(&b) / 2.0, center.1 - label_h(&b) / 2.0);
                                d.labels.push((b, pal.secondary.text(255), at));
                                d.cells.push((r, PopHit::FmtColor(255)));
                            } else {
                                let (_, wash) = theme::tint(HIGHLIGHT_TINT[k - 1], self.dark);
                                d.dots.push((center, 8.0 * s, outline, false));
                                d.dots.push((center, 7.0 * s, wash, false));
                                d.cells.push((r, PopHit::FmtColor(hit)));
                            }
                        }
                        ry += sw;
                    }
                }
            }
            Popover::TableRow(_) | Popover::TableCol(_) => {
                let (_, anchor) = self.table_menu?;
                // Not placed yet (`--shots`): next to the handle under the pointer.
                let anchor = match (anchor.2 > 0.0, self.table_handles) {
                    (false, Some((rh, ch, ..))) => {
                        if matches!(pop, Popover::TableRow(_)) {
                            rh
                        } else {
                            ch
                        }
                    }
                    _ => anchor,
                };
                let row = matches!(pop, Popover::TableRow(_));
                let (item, pad, w) = (28.0 * s, 6.0 * s, 176.0 * s);
                // Items as (label, op code, danger); None is a divider.
                let items: Vec<Option<(&str, u8, bool)>> = if row {
                    vec![
                        Some(("Insert above", 0, false)),
                        Some(("Insert below", 1, false)),
                        Some(("Duplicate", 2, false)),
                        Some(("Move up", 3, false)),
                        Some(("Move down", 4, false)),
                        None,
                        Some(("Delete row", 5, true)),
                        Some(("Delete table", 6, true)),
                    ]
                } else {
                    vec![
                        Some(("Insert left", 10, false)),
                        Some(("Insert right", 11, false)),
                        Some(("Move left", 12, false)),
                        Some(("Move right", 13, false)),
                        None,
                        Some(("Delete column", 17, true)),
                    ]
                };
                let dividers = items.iter().filter(|i| i.is_none()).count() as f32;
                let aligns_h = if row { 0.0 } else { item + 20.0 * s };
                let h = 2.0 * pad + (items.len() as f32 - dividers) * item + dividers * 9.0 * s + aligns_h;
                let (x, y) = if row {
                    (anchor.0 + anchor.2 + 6.0 * s, anchor.1 - 8.0 * s)
                } else {
                    (anchor.0 + anchor.2 / 2.0 - w / 2.0, anchor.1 + anchor.3 + 6.0 * s)
                };
                let x = x.clamp(kx + 6.0 * s, (kx + kw - 6.0 * s - w).max(kx + 6.0 * s));
                let y = y.clamp(ky + 6.0 * s, (ky + kh - 6.0 * s - h).max(ky + 6.0 * s));
                d.rect = (x, y, w, h);
                let mut iy = y + pad;
                if !row {
                    // Alignment: three chips, the current one selected.
                    let current = self.table_menu.and_then(|(first, _)| {
                        let g = crate::table::grid_at(&self.doc.text, self.to_doc(first))?;
                        let c = match pop {
                            Popover::TableCol(c) => c as usize,
                            _ => 0,
                        };
                        crate::table::aligns(&self.doc.text[g.rows.get(1)?.clone()]).get(c).copied()
                    });
                    let b = small_label(fonts, "ALIGN", 9.5 * s, Weight::SEMIBOLD);
                    d.labels.push((b, pal.secondary.text(220), (x + pad + 4.0 * s, iy + 2.0 * s)));
                    iy += 16.0 * s;
                    let cw = (w - 2.0 * pad - 8.0 * s) / 3.0;
                    for (k, (label, code)) in [("Left", 14u8), ("Center", 15), ("Right", 16)].into_iter().enumerate() {
                        let r = (x + pad + k as f32 * (cw + 4.0 * s), iy, cw, item - 4.0 * s);
                        let on = current.unwrap_or_default()
                            == [crate::table::Align::Left, crate::table::Align::Center, crate::table::Align::Right][k];
                        if on {
                            d.selected.push(r);
                        } else {
                            d.tiles.push(r);
                        }
                        let b = small_label(fonts, label, 12.0 * s, Weight::MEDIUM);
                        let at = (r.0 + (r.2 - label_width(&b)) / 2.0, r.1 + (r.3 - label_h(&b)) / 2.0);
                        d.labels.push((b, if on { pal.accent } else { pal.text }.text(255), at));
                        d.cells.push((r, PopHit::TableOp(code)));
                    }
                    iy += item + 4.0 * s;
                }
                for it in items {
                    match it {
                        None => {
                            d.bars.push(((x + 10.0 * s, iy + 4.0 * s, w - 20.0 * s, 1.0 * s), false));
                            iy += 9.0 * s;
                        }
                        Some((label, code, danger)) => {
                            let r = (x + pad, iy, w - 2.0 * pad, item);
                            let b = small_label(fonts, label, 13.0 * s, Weight::NORMAL);
                            let at = (r.0 + 10.0 * s, r.1 + (item - label_h(&b)) / 2.0);
                            d.labels.push((b, if danger { pal.alert } else { pal.text }.text(255), at));
                            d.cells.push((r, PopHit::TableOp(code)));
                            iy += item;
                        }
                    }
                }
            }
            Popover::Window => {
                let (row, pad, header) = (46.0 * s, 6.0 * s, 28.0 * s);
                let w = (250.0 * s).min(kw - 24.0 * s);
                let x = kx + kw - 12.0 * s - w;
                let y = ky + (self.st.top_bar - 4.0) * s;
                const MODES: [(Mode, &str, &str); 3] = [
                    (Mode::Normal, "Normal", "Like any other window"),
                    (Mode::OnTop, "Always on top", "Floats above every app"),
                    (Mode::Desktop, "On desktop", "Behind your windows, like a widget"),
                ];
                d.rect = (x, y, w, header + MODES.len() as f32 * row + pad);
                let title = small_label(fonts, "Window", 11.5 * s, Weight::MEDIUM);
                d.labels.push((title, pal.secondary.text(255), (x + 12.0 * s, y + 8.0 * s)));
                for (i, (mode, name, about)) in MODES.iter().enumerate() {
                    let r = (x + pad, y + header + i as f32 * row, w - 2.0 * pad, row);
                    let current = *mode == self.mode;
                    let weight = if current { Weight::SEMIBOLD } else { Weight::NORMAL };
                    d.labels.push((
                        small_label(fonts, name, 13.5 * s, weight),
                        pal.text.text(255),
                        (r.0 + 28.0 * s, r.1 + 6.0 * s),
                    ));
                    d.labels.push((
                        small_label(fonts, about, 11.5 * s, Weight::NORMAL),
                        pal.secondary.text(255),
                        (r.0 + 28.0 * s, r.1 + 25.0 * s),
                    ));
                    if current {
                        let t = small_label(fonts, "✓", 13.0 * s, Weight::SEMIBOLD);
                        d.labels.push((t, pal.accent.text(255), (r.0 + 9.0 * s, r.1 + 6.0 * s)));
                    }
                    d.cells.push((r, PopHit::Mode(*mode as u8)));
                }
            }
            Popover::Remind => {
                let now = Local::now();
                let pad = 10.0 * s;
                let w = (276.0 * s).min(kw - 16.0 * s);
                let x = kx + kw - 8.0 * s - w;
                let y = ky + (self.st.top_bar - 4.0) * s;
                let inner = w - 2.0 * pad;
                let mut cy = y + pad;
                // A row of equal chips; `cells` is (label, hit, selected, enabled).
                let chips = |fonts: &mut FontSystem,
                             d: &mut PopDraw,
                             cy: f32,
                             h: f32,
                             gap: f32,
                             cells: Vec<(String, PopHit, bool, bool)>| {
                    let n = cells.len().max(1) as f32;
                    let cw = (inner - gap * (n - 1.0)) / n;
                    for (i, (text, hit, selected, enabled)) in cells.into_iter().enumerate() {
                        let r = (x + pad + i as f32 * (cw + gap), cy, cw, h);
                        if selected {
                            d.selected.push(r);
                        } else {
                            d.tiles.push(r);
                        }
                        let b = small_label(
                            fonts,
                            &text,
                            11.5 * s,
                            if selected { Weight::SEMIBOLD } else { Weight::NORMAL },
                        );
                        let color = match (enabled, selected) {
                            (false, _) => pal.secondary.text(110),
                            (true, true) => pal.accent.text(255),
                            (true, false) => pal.text.text(255),
                        };
                        let at = (r.0 + (cw - label_width(&b)) / 2.0, r.1 + (h - label_h(&b)) / 2.0);
                        d.labels.push((b, color, at));
                        if enabled {
                            d.cells.push((r, hit));
                        }
                    }
                };
                let caption = |fonts: &mut FontSystem, d: &mut PopDraw, cy: f32, text: &str| {
                    let b = small_label(fonts, text, 10.0 * s, Weight::SEMIBOLD);
                    d.labels.push((b, pal.secondary.text(220), (x + pad + 2.0 * s, cy)));
                };

                let title = small_label(fonts, "Remind me", 12.5 * s, Weight::SEMIBOLD);
                d.labels.push((title, pal.text.text(255), (x + pad + 2.0 * s, cy)));
                if let Some(r) = &self.reminder {
                    let cur = small_label(fonts, &r.label(now), 11.5 * s, Weight::MEDIUM);
                    let at = (x + w - pad - label_width(&cur), cy + 1.0 * s);
                    d.labels.push((cur, pal.accent.text(255), at));
                }
                cy += 24.0 * s;

                // Type anything the inline @ syntax understands; Enter sets it.
                let field = (x + pad, cy, inner, 30.0 * s);
                d.tiles.push(field);
                let (text, color) = if self.remind_query.is_empty() {
                    ("Type: 3pm, fri 9am, in 45m, daily 8am".to_string(), pal.secondary.text(200))
                } else {
                    (format!("{}|", self.remind_query), pal.text.text(255))
                };
                let q = small_label(fonts, &text, 12.5 * s, Weight::NORMAL);
                let at = (field.0 + 10.0 * s, field.1 + (field.3 - label_h(&q)) / 2.0);
                d.labels.push((q, color, at));
                cy += 34.0 * s;
                let (hint, color) = match (self.remind_query.trim(), reminder::parse(self.remind_query.trim(), now)) {
                    ("", _) => ("…or pick below".to_string(), pal.secondary.text(170)),
                    (_, Some((r, _))) => {
                        let r = if r.repeat == Repeat::Once { r.repeating(self.remind_repeat) } else { r };
                        (format!("→ {}   ⏎", r.describe(now)), pal.accent.text(255))
                    }
                    _ => ("Not a time yet".to_string(), pal.secondary.text(170)),
                };
                let b = small_label(fonts, &hint, 11.5 * s, Weight::MEDIUM);
                d.labels.push((b, color, (x + pad + 2.0 * s, cy)));
                cy += 24.0 * s;

                caption(fonts, &mut d, cy, "SOON");
                cy += 16.0 * s;
                let soon = rs
                    .soon
                    .iter()
                    .map(|&secs| {
                        (format!("In {}", reminder::duration_label(secs)), PopHit::RemindIn(secs), false, true)
                    })
                    .collect();
                chips(fonts, &mut d, cy, 28.0 * s, 6.0 * s, soon);
                cy += 38.0 * s;

                // A week strip: weekday over date; today's date in the accent colour.
                caption(fonts, &mut d, cy, "DAY");
                cy += 16.0 * s;
                let (gap, dh) = (4.0 * s, 40.0 * s);
                let dw = (inner - 6.0 * gap) / 7.0;
                for k in 0..7i64 {
                    let date = now.date_naive() + chrono::Duration::days(k);
                    let r = (x + pad + k as f32 * (dw + gap), cy, dw, dh);
                    let selected = k == self.remind_day;
                    if selected {
                        d.selected.push(r);
                    } else {
                        d.tiles.push(r);
                    }
                    let wd = small_label(fonts, &date.format("%a").to_string(), 10.0 * s, Weight::MEDIUM);
                    let num = small_label(fonts, &date.format("%-d").to_string(), 13.5 * s, Weight::SEMIBOLD);
                    let num_color = if k == 0 || selected { pal.accent } else { pal.text };
                    let (wd_at, num_at) = (
                        (r.0 + (dw - label_width(&wd)) / 2.0, r.1 + 4.0 * s),
                        (r.0 + (dw - label_width(&num)) / 2.0, r.1 + 18.0 * s),
                    );
                    d.labels.push((wd, pal.secondary.text(230), wd_at));
                    d.labels.push((num, num_color.text(255), num_at));
                    d.cells.push((r, PopHit::RemindDay(k)));
                }
                cy += dh + 10.0 * s;

                caption(fonts, &mut d, cy, "TIME");
                cy += 16.0 * s;
                let times = rs
                    .times
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let open = reminder::at_day(now, self.remind_day, *t).is_some();
                        (reminder::clock_label(*t), PopHit::RemindTime(i), false, open)
                    })
                    .collect();
                chips(fonts, &mut d, cy, 28.0 * s, 4.0 * s, times);
                cy += 32.0 * s;

                // Fine-tune: [-1h] [-5m] [ 9:45 AM ✓ ] [+5m] [+1h]. The middle sets it.
                let step = rs.minute_step.clamp(1, 60) as i64;
                let (bw, gap, bh) = (38.0 * s, 4.0 * s, 28.0 * s);
                let mid = inner - 4.0 * bw - 4.0 * gap;
                let mut bx = x + pad;
                for (i, minutes) in [-60, -step, 0, step, 60].into_iter().enumerate() {
                    let width = if i == 2 { mid } else { bw };
                    let r = (bx, cy, width, bh);
                    bx += width + gap;
                    if i == 2 {
                        let open = reminder::at_day(now, self.remind_day, self.remind_time).is_some();
                        let text = format!("{}  ✓", reminder::clock_label(self.remind_time));
                        let b = small_label(fonts, &text, 12.5 * s, Weight::SEMIBOLD);
                        let at = (r.0 + (r.2 - label_width(&b)) / 2.0, r.1 + (r.3 - label_h(&b)) / 2.0);
                        if open {
                            d.selected.push(r);
                            d.labels.push((b, pal.accent.text(255), at));
                            d.cells.push((r, PopHit::RemindSet));
                        } else {
                            // Already past on the chosen day.
                            d.tiles.push(r);
                            d.labels.push((b, pal.secondary.text(150), at));
                        }
                        continue;
                    }
                    let text = match minutes.abs() {
                        60 => format!("{}1h", if minutes < 0 { "−" } else { "+" }),
                        m => format!("{}{m}m", if minutes < 0 { "−" } else { "+" }),
                    };
                    let b = small_label(fonts, &text, 11.5 * s, Weight::MEDIUM);
                    let at = (r.0 + (r.2 - label_width(&b)) / 2.0, r.1 + (r.3 - label_h(&b)) / 2.0);
                    d.tiles.push(r);
                    d.labels.push((b, pal.text.text(230), at));
                    d.cells.push((r, PopHit::RemindStep(minutes)));
                }
                cy += bh + 10.0 * s;

                caption(fonts, &mut d, cy, "REPEAT");
                cy += 16.0 * s;
                let repeats = Repeat::CHOICES
                    .iter()
                    .enumerate()
                    .map(|(i, (r, name))| (name.to_string(), PopHit::RemindRepeat(i), *r == self.remind_repeat, true))
                    .collect();
                chips(fonts, &mut d, cy, 28.0 * s, 4.0 * s, repeats);
                cy += 28.0 * s;

                if self.reminder.is_some() {
                    d.bars.push(((x + 10.0 * s, cy + 6.0 * s, w - 20.0 * s, 1.0 * s), false));
                    cy += 12.0 * s;
                    let r = (x + pad, cy, inner, 28.0 * s);
                    let b = small_label(fonts, "Remove reminder", 12.5 * s, Weight::MEDIUM);
                    let at = (r.0 + 8.0 * s, r.1 + (r.3 - label_h(&b)) / 2.0);
                    d.labels.push((b, pal.alert.text(255), at));
                    d.cells.push((r, PopHit::RemindClear));
                    cy += 28.0 * s;
                }
                d.rect = (x, y, w, cy + pad - y);
            }
            Popover::Emoji { sel, .. } => {
                let items = self.emoji_matches();
                let (cx, cy, _, _) = self.content_rect();
                let (row, pad) = (30.0 * s, 6.0 * s);
                let w = (220.0 * s).min(kw - 16.0 * s);
                let h = items.len() as f32 * row + 2.0 * pad;
                let (caret_x, caret_top, caret_h) =
                    self.caret_geom(self.to_display(self.doc.cursor)).unwrap_or((0.0, 0.0, 0.0));
                let x = (cx + caret_x - 12.0 * s).clamp(kx + 8.0 * s, (kx + kw - 8.0 * s - w).max(kx + 8.0 * s));
                let line_y = cy + caret_top - self.scroll;
                let mut y = line_y + caret_h + 4.0 * s;
                if y + h > ky + kh - 6.0 * s {
                    y = line_y - 4.0 * s - h;
                }
                let y = y.max(ky + 6.0 * s);
                d.rect = (x, y, w, h);
                for (j, (code, emoji)) in items.iter().enumerate() {
                    let r = (x + pad, y + pad + j as f32 * row, w - 2.0 * pad, row);
                    if j == sel {
                        d.selected.push(r);
                    }
                    let e = small_label(fonts, emoji, 16.0 * s, Weight::NORMAL);
                    let at = (r.0 + 8.0 * s, r.1 + (row - label_h(&e)) / 2.0);
                    d.labels.push((e, pal.text.text(255), at));
                    let l = small_label(fonts, &format!(":{code}:"), 13.0 * s, Weight::NORMAL);
                    let at = (r.0 + 36.0 * s, r.1 + (row - label_h(&l)) / 2.0);
                    d.labels.push((l, pal.text.text(255), at));
                    d.cells.push((r, PopHit::Shortcode(emoji)));
                }
            }
            Popover::More => {
                // Move to list · compact view · (divider) · delete, kept at the bottom and
                // guarded by a second click.
                let (row, pad, header, sep) = (30.0 * s, 6.0 * s, 28.0 * s, 11.0 * s);
                let w = (210.0 * s).min(kw - 24.0 * s);
                let desks = if self.desktop_list.len() > 1 { self.desktop_list.len() } else { 0 };
                let desk_h = if desks > 0 { sep + 22.0 * s + desks as f32 * row } else { 0.0 };
                let fixed = header + 2.0 * row + 2.0 * sep + pad + desk_h;
                let room = (ky + kh - 8.0 * s) - (ky + self.st.top_bar * s) - fixed;
                let shown = lists.len().min((room / row).floor().max(1.0) as usize);
                let h = fixed + shown as f32 * row;
                let x = kx + kw - 12.0 * s - w;
                let y = ky + (self.st.top_bar - 4.0) * s;
                d.rect = (x, y, w, h);
                let title = small_label(fonts, "Move to list", 11.5 * s, Weight::MEDIUM);
                d.labels.push((title, pal.secondary.text(255), (x + 12.0 * s, y + 8.0 * s)));
                let tick = |fonts: &mut FontSystem, d: &mut PopDraw, r: (f32, f32, f32, f32)| {
                    let t = small_label(fonts, "✓", 13.0 * s, Weight::SEMIBOLD);
                    let at = (r.0 + 9.0 * s, r.1 + (row - label_h(&t)) / 2.0);
                    d.labels.push((t, pal.accent.text(255), at));
                };
                let mut ry = y + header;
                for list in lists.iter().take(shown) {
                    let r = (x + pad, ry, w - 2.0 * pad, row);
                    let current = list.id == self.list;
                    let name = small_label(
                        fonts,
                        &list.name,
                        13.5 * s,
                        if current { Weight::SEMIBOLD } else { Weight::NORMAL },
                    );
                    let at = (r.0 + 28.0 * s, r.1 + (row - label_h(&name)) / 2.0);
                    d.labels.push((name, pal.text.text(255), at));
                    if current {
                        tick(fonts, &mut d, r);
                    }
                    d.cells.push((r, PopHit::List(list.id)));
                    ry += row;
                }
                let divider = |d: &mut PopDraw, at: f32| {
                    d.bars.push(((x + 10.0 * s, at + sep / 2.0, w - 20.0 * s, 1.0 * s), false))
                };
                if desks > 0 {
                    // Move to another virtual desktop: just this note, without dragging it.
                    divider(&mut d, ry);
                    ry += sep;
                    let t = small_label(fonts, "Move to desktop", 11.5 * s, Weight::MEDIUM);
                    d.labels.push((t, pal.secondary.text(255), (x + 12.0 * s, ry + 4.0 * s)));
                    ry += 22.0 * s;
                    for (i, (id, name)) in self.desktop_list.iter().enumerate() {
                        let r = (x + pad, ry, w - 2.0 * pad, row);
                        let current = self.desktop.as_deref() == Some(id.as_str());
                        let weight = if current { Weight::SEMIBOLD } else { Weight::NORMAL };
                        let label = small_label(fonts, name, 13.5 * s, weight);
                        let at = (r.0 + 28.0 * s, r.1 + (row - label_h(&label)) / 2.0);
                        d.labels.push((label, pal.text.text(255), at));
                        if current {
                            tick(fonts, &mut d, r);
                        } else {
                            d.cells.push((r, PopHit::Desktop(i as u8)));
                        }
                        ry += row;
                    }
                }
                divider(&mut d, ry);
                ry += sep;
                let r = (x + pad, ry, w - 2.0 * pad, row);
                let label = small_label(fonts, "Compact view", 13.5 * s, Weight::NORMAL);
                let at = (r.0 + 28.0 * s, r.1 + (row - label_h(&label)) / 2.0);
                d.labels.push((label, pal.text.text(255), at));
                if self.compact {
                    tick(fonts, &mut d, r);
                }
                d.cells.push((r, PopHit::Compact));
                ry += row;
                divider(&mut d, ry);
                ry += sep;
                let r = (x + pad, ry, w - 2.0 * pad, row);
                let text = if self.confirm_delete { "Click again to delete" } else { "Delete note" };
                let label = small_label(fonts, text, 13.5 * s, Weight::MEDIUM);
                let at = (r.0 + 28.0 * s, r.1 + (row - label_h(&label)) / 2.0);
                d.labels.push((label, pal.alert.text(255), at));
                let bin = small_label(fonts, "🗑", 12.0 * s, Weight::NORMAL);
                let at = (r.0 + 8.0 * s, r.1 + (row - label_h(&bin)) / 2.0);
                d.labels.push((bin, pal.alert.text(255), at));
                d.cells.push((r, PopHit::Delete));
            }
            Popover::Slash { sel, .. } => {
                let items = self.slash_matches();
                let (cx, cy, _, _) = self.content_rect();
                let (row, pad, header) = (32.0 * s, 6.0 * s, 28.0 * s);
                let start = sel.saturating_sub(6);
                let shown = items.len().min(7);
                let w = (236.0 * s).min(kw - 16.0 * s);
                let h = header + shown as f32 * row + pad;
                let (caret_x, caret_top, caret_h) =
                    self.caret_geom(self.to_display(self.doc.cursor)).unwrap_or((0.0, 0.0, 0.0));
                let x = (cx + caret_x - 12.0 * s).clamp(kx + 8.0 * s, (kx + kw - 8.0 * s - w).max(kx + 8.0 * s));
                let line_y = cy + caret_top - self.scroll;
                let mut y = line_y + caret_h + 4.0 * s;
                if y + h > ky + kh - 6.0 * s {
                    y = line_y - 4.0 * s - h;
                }
                let y = y.max(ky + 6.0 * s);
                d.rect = (x, y, w, h);
                let title = small_label(fonts, "Basic blocks", 11.5 * s, Weight::MEDIUM);
                d.labels.push((title, pal.secondary.text(255), (x + 12.0 * s, y + 8.0 * s)));
                for (j, &i) in items.iter().enumerate().skip(start).take(shown) {
                    let r = (x + pad, y + header + (j - start) as f32 * row, w - 2.0 * pad, row);
                    let tile = (r.0 + 6.0 * s, r.1 + 5.0 * s, 22.0 * s, 22.0 * s);
                    let t = small_label(fonts, SLASH[i].tile, 11.0 * s, Weight::SEMIBOLD);
                    let at = (tile.0 + (tile.2 - label_width(&t)) / 2.0, tile.1 + (tile.3 - label_h(&t)) / 2.0);
                    d.labels.push((t, pal.text.text(230), at));
                    let l = small_label(fonts, SLASH[i].label, 13.5 * s, Weight::NORMAL);
                    let at = (r.0 + 38.0 * s, r.1 + (row - label_h(&l)) / 2.0);
                    d.labels.push((l, pal.text.text(255), at));
                    d.tiles.push(tile);
                    d.cells.push((r, PopHit::Slash(i)));
                }
            }
        }
        // Keep it inside the card: one that would run off the bottom moves up if that makes
        // it fit. A taller one stays put and the window grows below the card (`fit_popover`).
        let max_y = ky + kh - 6.0 * s - d.rect.3;
        if d.rect.1 > max_y && max_y >= ky + 4.0 * s {
            d.shift(max_y - d.rect.1);
        }
        Some(d)
    }

    /// Adds transparent room below the card while a popover is taller than the note, and
    /// removes it when the popover closes. The note itself keeps its size.
    fn fit_popover(&mut self, shared: &mut Shared) {
        let size = self.window.inner_size();
        let base = size.height.saturating_sub(self.extra_h);
        let need = match self.pop_layout(&shared.lists, &shared.config.reminders, &mut shared.fonts) {
            Some(p) => (p.rect.1 + p.rect.3 + 10.0 * self.scale() - base as f32).max(0.0).ceil() as u32,
            None => 0,
        };
        if need != self.extra_h {
            self.extra_h = need;
            self.resize(PhysicalSize::new(size.width, base + need));
        }
    }

    fn set_zoom(&mut self, zoom: u16) {
        self.zoom = zoom.clamp(50, 300);
        self.dirty = true;
        self.layout_dirty = true;
    }

    fn copy(&mut self, shared: &mut Shared) {
        if let (Some(t), Some(cb)) = (self.doc.selected_text(), shared.clipboard.as_mut()) {
            let _ = cb.set_text(t.to_string());
        }
    }

    fn vertical(&mut self, fonts: &mut FontSystem, dir: f32, lines: f32, extend: bool) {
        if self.caret_geom(self.to_display(self.doc.cursor)).is_none() {
            self.ensure_caret_visible(fonts);
        }
        let Some((x, top, h)) = self.caret_geom(self.to_display(self.doc.cursor)) else { return };
        let gx = *self.goal_x.get_or_insert(x);
        let y = if dir < 0.0 { top - 1.0 - (lines - 1.0) * h } else { top + h + 1.0 + (lines - 1.0) * h };
        // Only lines in view are laid out to hit-test: bring the target line into view first.
        let (_, _, _, vh) = self.content_rect();
        if y < self.scroll {
            self.scroll = y.max(0.0);
        } else if y + h > self.scroll + vh {
            self.scroll = y + h - vh;
        }
        self.clamp_scroll();
        self.sync_scroll(fonts);
        let to = if y < 0.0 { 0 } else { self.hit_buffer(gx, y) };
        self.doc.move_to(to, extend);
    }

    // ---- pointer ------------------------------------------------------------------------

    fn zone(&self, x: f64, y: f64) -> Zone {
        let (x, y) = (x as f32, y as f32);
        let s = self.scale();
        let (kx, ky, kw, kh) = self.card();
        let e = EDGE * s;
        // Resize handles straddle the card edge, reaching a little into the shadow margin.
        if self.pop_rect.is_some_and(|r| contains(r, x, y)) {
            return Zone::Popover;
        }
        if x < kx - e || x > kx + kw + e || y < ky - e || y > ky + kh + e {
            return Zone::Outside;
        }
        let (l, r, t, b) = (x < kx + e, x > kx + kw - e, y < ky + e * 0.6, y > ky + kh - e);
        let dir = match (l, r, t, b) {
            (true, _, true, _) => Some(ResizeDirection::NorthWest),
            (_, true, true, _) => Some(ResizeDirection::NorthEast),
            (true, _, _, true) => Some(ResizeDirection::SouthWest),
            (_, true, _, true) => Some(ResizeDirection::SouthEast),
            (true, ..) => Some(ResizeDirection::West),
            (_, true, ..) => Some(ResizeDirection::East),
            (_, _, true, _) => Some(ResizeDirection::North),
            (.., true) => Some(ResizeDirection::South),
            _ => None,
        };
        if let Some(d) = dir {
            return Zone::Resize(d);
        }
        if let Some((_, a)) = self.hits.iter().rev().find(|(r, _)| contains(*r, x, y)) {
            return Zone::Button(*a);
        }
        if y < ky + self.st.top_bar * s {
            return Zone::Bar;
        }
        Zone::Text
    }

    /// Byte range of the checkbox under a doc offset, if any.
    fn checkbox_at(&self, off: usize) -> Option<(usize, usize)> {
        let (ls, le) = self.doc.line_bounds(off);
        let info = line_info(&self.doc.text[ls..le]);
        let (at, _) = info.checkbox?;
        (ls + info.indent <= off && off <= ls + at + 3).then_some((ls, le))
    }

    /// A custom colour from a point on the hue strip.
    fn set_hue_from(&mut self, x: f32) {
        if let Some((sx, _, sw, _)) = self.hue_strip {
            let hue = ((x - sx) / sw).clamp(0.0, 1.0) * 359.0;
            self.set_color(theme::CUSTOM + hue.round() as u16);
            self.needs_paint = true;
        }
    }

    /// Follows the pointer while resizing: moves the dragged edge(s), keeps the opposite ones
    /// still, and repaints straight away so the note tracks the pointer.
    fn resize_to(&mut self, x: f64, y: f64, shared: &mut Shared) {
        let Some((dir, (sx, sy), pos, size)) = self.resizing else { return };
        let Ok(now) = self.window.outer_position() else { return };
        let (dx, dy) = ((now.x as f64 + x - sx).round() as i32, (now.y as f64 + y - sy).round() as i32);
        let scale = self.window.scale_factor();
        let (min_w, min_h) = ((MIN_SIZE.0 * scale) as i32, (MIN_SIZE.1 * scale) as i32);
        let (w0, h0) = (size.width as i32, size.height as i32);
        use ResizeDirection as D;
        let (west, east) = (
            matches!(dir, D::West | D::NorthWest | D::SouthWest),
            matches!(dir, D::East | D::NorthEast | D::SouthEast),
        );
        let (north, south) = (
            matches!(dir, D::North | D::NorthWest | D::NorthEast),
            matches!(dir, D::South | D::SouthWest | D::SouthEast),
        );
        let w = if east {
            (w0 + dx).max(min_w)
        } else if west {
            (w0 - dx).max(min_w)
        } else {
            w0
        };
        let h = if south {
            (h0 + dy).max(min_h)
        } else if north {
            (h0 - dy).max(min_h)
        } else {
            h0
        };
        // Dragging a left or top edge moves the window so the opposite edge stays put. The size
        // goes first: macOS resizes from the bottom-left corner, so the place is set after it.
        let nx = if west { pos.x + w0 - w } else { pos.x };
        let ny = if north { pos.y + h0 - h } else { pos.y };
        let resized = self.window.inner_size() != PhysicalSize::new(w as u32, h as u32);
        if resized {
            let _ = self.window.request_inner_size(PhysicalSize::new(w as u32, h as u32));
        }
        if (nx, ny) != (now.x, now.y) || (resized && cfg!(target_os = "macos")) {
            self.window.set_outer_position(PhysicalPosition::new(nx, ny));
        }
        if resized {
            self.layout_dirty = true;
            self.dirty = true;
            self.render(shared);
        }
    }

    /// Moves the window so the pointer keeps its offset from the press.
    fn carry(&mut self, x: f64, y: f64) {
        let Some((gx, gy)) = self.moving else { return };
        if let Ok(pos) = self.window.outer_position() {
            let to = PhysicalPosition::new(pos.x + (x - gx).round() as i32, pos.y + (y - gy).round() as i32);
            self.window.set_outer_position(to);
        }
    }

    fn on_pointer_move(&mut self, x: f64, y: f64, shared: &mut Shared) {
        // A resize, selection or hue drag whose button is no longer held lost its release
        // (macOS can send it to another window): end it rather than follow the pointer.
        if !crate::input::left_button_down() {
            if self.resizing.take().is_some() {
                self.dirty = true;
                self.needs_paint = true;
            }
            if self.moving.take().is_some() {
                self.window.set_cursor(CursorIcon::Default);
                crate::input::show_cursor(CursorIcon::Default);
            }
            self.drag_select = false;
            self.hue_drag = false;
        }
        // macOS keeps sending moves to a focused window from anywhere on screen: outside the
        // note, it is simply not hovered.
        let busy = self.resizing.is_some() || self.moving.is_some() || self.drag_select || self.hue_drag;
        if !busy && !crate::input::inside(x, y, self.window.inner_size()) {
            if std::mem::take(&mut self.hovered) || self.hover.take().is_some() {
                self.needs_paint = true;
            }
            return;
        }
        self.pointer = (x, y);
        // Near a table: show its + buttons.
        let m = 14.0 * self.scale();
        let (px, py) = (x as f32, y as f32);
        let near = self
            .table_rects
            .iter()
            .position(|&((tx, ty, tw, th), _)| px >= tx - m && px <= tx + tw + m && py >= ty - m && py <= ty + th + m);
        if near != self.table_hover {
            self.table_hover = near;
            self.needs_paint = true;
        }
        // The cell under the pointer (by row band and column), for the row and column handles.
        let cell = near.and_then(|i| self.table_geo.get(i)).map(|(rows, cols)| {
            let row = rows.iter().rposition(|&(top, _)| py >= top).unwrap_or(0);
            let row = if row == 1 { 0 } else { row };
            let tx = self.table_rects[near.unwrap_or(0)].0 .0;
            let mut x = tx;
            let col = cols
                .iter()
                .position(|w| {
                    x += w;
                    px < x
                })
                .unwrap_or(cols.len().saturating_sub(1));
            (row, col)
        });
        if cell != self.table_cell {
            self.table_cell = cell;
            self.needs_paint = true;
        }
        if self.moving.is_some() {
            self.carry(x, y);
            return;
        }
        if self.resizing.is_some() {
            self.resize_to(x, y, shared);
            return;
        }
        if self.hue_drag {
            self.set_hue_from(x as f32);
            return;
        }
        if !self.hovered {
            self.hovered = true;
        }
        if self.drag_select {
            let to = self.hit_window(x, y);
            self.doc.move_to(to, true);
            self.changed(shared, false);
            return;
        }
        let zone = self.zone(x, y);
        let hover = match zone {
            Zone::Button(a) => Some(a),
            _ => None,
        };
        if hover != self.hover {
            self.hover = hover;
            self.needs_paint = true;
        }
        let pop_hover = match zone {
            Zone::Popover => self.pop_hits.iter().find(|(r, _)| contains(*r, x as f32, y as f32)).map(|(_, h)| *h),
            _ => None,
        };
        if pop_hover != self.pop_hover {
            self.pop_hover = pop_hover;
            if let (Some(PopHit::Slash(i)), Some(Popover::Slash { at, .. })) = (pop_hover, self.popover) {
                if let Some(sel) = self.slash_matches().iter().position(|&m| m == i) {
                    self.popover = Some(Popover::Slash { at, sel });
                }
            }
            self.needs_paint = true;
        }
        let icon = match zone {
            Zone::Resize(d) => match d {
                ResizeDirection::East | ResizeDirection::West => CursorIcon::EwResize,
                ResizeDirection::North | ResizeDirection::South => CursorIcon::NsResize,
                ResizeDirection::NorthEast | ResizeDirection::SouthWest => CursorIcon::NeswResize,
                ResizeDirection::NorthWest | ResizeDirection::SouthEast => CursorIcon::NwseResize,
            },
            Zone::Button(_) | Zone::Popover => CursorIcon::Pointer,
            Zone::Bar | Zone::Outside => CursorIcon::Default,
            Zone::Text if self.checkbox_at(self.hit_window(x, y)).is_some() => CursorIcon::Pointer,
            Zone::Text => CursorIcon::Text,
        };
        self.window.set_cursor(icon);
    }

    fn on_press(&mut self, shared: &mut Shared) -> Option<Cmd> {
        let (x, y) = self.pointer;
        let zone = self.zone(x, y);
        if self.popover.is_some() && !matches!(zone, Zone::Popover) {
            // Like Notion: a click outside only dismisses the popover (but a click away from
            // the formatting toolbar also places the caret).
            let toolbar = matches!(self.popover, Some(Popover::Format { .. }));
            self.popover = None;
            self.pop_hover = None;
            self.needs_paint = true;
            if !toolbar {
                return None;
            }
        }
        if let Some((rh, ch, first, row, col)) = self.table_handles {
            let (px, py) = (x as f32, y as f32);
            let inside =
                |r: gfx::Rect| px >= r.0 - 3.0 && px <= r.0 + r.2 + 3.0 && py >= r.1 - 3.0 && py <= r.1 + r.3 + 3.0;
            if inside(rh) || inside(ch) {
                let (pop, anchor) =
                    if inside(rh) { (Popover::TableRow(row as u16), rh) } else { (Popover::TableCol(col as u16), ch) };
                self.table_menu = Some((first, anchor));
                self.popover = Some(pop);
                self.pop_hover = None;
                self.needs_paint = true;
                return None;
            }
        }
        if let Some(&(r, first)) =
            self.table_hover.and_then(|i| self.table_rects.get(i)).filter(|_| self.st.table_buttons)
        {
            let s = self.scale();
            let near = |cx: f32, cy: f32| (x as f32 - cx).hypot(y as f32 - cy) <= 11.0 * s;
            if near(r.0 + r.2 / 2.0, r.1 + r.3) {
                return self.table_add(first, false, shared);
            }
            if near(r.0 + r.2, r.1 + r.3 / 2.0) {
                return self.table_add(first, true, shared);
            }
        }
        match zone {
            Zone::Outside => return self.screen_point().map(|(x, y)| Cmd::PassClick(x, y)),
            Zone::Popover => {
                let hit = self.pop_hits.iter().find(|(r, _)| contains(*r, x as f32, y as f32)).map(|(_, h)| *h);
                if hit == Some(PopHit::Hue) {
                    self.hue_drag = true;
                    self.set_hue_from(x as f32);
                } else if let Some(hit) = hit {
                    return self.pop_click(hit, shared);
                }
            }
            Zone::Resize(d) => {
                if let Ok(pos) = self.window.outer_position() {
                    let screen = (pos.x as f64 + x, pos.y as f64 + y);
                    self.resizing = Some((d, screen, pos, self.window.inner_size()));
                    self.popover = None;
                    self.needs_paint = true;
                }
            }
            Zone::Button(a) => return self.act(a, shared),
            Zone::Bar => {
                // The move cursor for as long as the note is carried; the release puts it back.
                self.moving = Some((x, y));
                self.window.set_cursor(CursorIcon::Move);
                crate::input::show_cursor(CursorIcon::Move);
            }
            Zone::Text => {
                let now = Instant::now();
                let count = match self.last_click {
                    Some((t, p, n))
                        if now - t < Duration::from_millis(450) && (p.0 - x).abs() < 4.0 && (p.1 - y).abs() < 4.0 =>
                    {
                        n % 3 + 1
                    }
                    _ => 1,
                };
                self.last_click = Some((now, (x, y), count));
                let off = self.hit_window(x, y);
                let open = if cfg!(target_os = "macos") { self.mods.super_key() } else { self.mods.control_key() };
                if open && count == 1 {
                    let (ls, le) = self.doc.line_bounds(off);
                    if let Some(url) = markdown::link_at(&self.doc.text[ls..le], off - ls) {
                        crate::input::open_url(url);
                        return None;
                    }
                }
                let shift = self.mods.shift_key();
                if let (1, false, Some((ls, le))) = (count, shift, self.checkbox_at(off)) {
                    self.doc.toggle_check_at_line(ls, le);
                    self.pop_checkbox(ls);
                    self.changed(shared, true);
                    return None;
                }
                self.editing = true;
                match count {
                    2 => self.doc.select_word_at(off),
                    3 => self.doc.select_line_at(off),
                    _ => {
                        // Below a table that ends the note: a new line there, to write on.
                        let (_, cy, _, _) = self.content_rect();
                        let below = y as f32 > cy + self.content_h - self.scroll;
                        let end = self.doc.text.len();
                        if below && !shift && crate::table::grid_at(&self.doc.text, end).is_some() {
                            self.leave_table(end);
                            self.changed(shared, true);
                            return None;
                        }
                        // In a table, onto the cell on that side of the divider.
                        let off = match crate::table::grid_at(&self.doc.text, off) {
                            Some(g) if !shift => g.nearest(off),
                            _ => off,
                        };
                        self.doc.move_to(off, shift);
                        self.drag_select = true;
                    }
                }
                self.changed(shared, false);
            }
        }
        None
    }

    // ---- dock support -------------------------------------------------------------------

    /// Title and subtitle shown for this note in the edge dock.
    pub fn summary(&self) -> (String, String) {
        let mut lines = text_lines(&self.doc.text);
        let title = lines.next().unwrap_or_else(|| "New note".to_string());
        let sub = match &self.reminder {
            Some(r) => r.label(Local::now()),
            None => {
                let (mut done, mut total) = (0, 0);
                for l in self.doc.text.lines() {
                    if let Some((_, checked)) = line_info(l).checkbox {
                        total += 1;
                        done += usize::from(checked);
                    }
                }
                if total > 0 {
                    format!("{done} of {total} done")
                } else {
                    lines.next().unwrap_or_default()
                }
            }
        };
        (title, sub)
    }

    /// Picks up a changed `[general] theme`.
    pub fn refresh_theme(&mut self) {
        let dark = crate::theme::is_dark(self.window.theme());
        if dark != self.dark {
            // Text colours are part of the layout.
            self.dark = dark;
            self.layout_dirty = true;
            self.needs_paint = true;
        }
    }

    pub fn set_style(&mut self, st: &NoteStyle) {
        self.view_dirty |= self.collapsed();
        if *st != self.st {
            self.st = st.clone();
            self.layout_dirty = true;
            self.needs_paint = true;
        }
    }

    /// Undoes a × hide that is still fading, leaving visibility to the app ("hide all").
    pub fn unhide(&mut self) {
        self.user_hidden = false;
        self.hiding = false;
        self.dirty = true;
    }

    pub fn set_shown(&mut self, shown: bool) {
        if shown && !self.shown {
            self.appear.snap_to(0.0);
            self.opacity.snap_to(0.0);
        }
        if shown != self.shown {
            self.shown = shown;
            self.window.set_visible(shown);
            self.needs_paint = shown;
        }
    }

    /// Shows a hidden note; `focus` also brings it to the front with a wiggle.
    pub fn reveal(&mut self, focus: bool) {
        if !self.shown || self.user_hidden || self.hiding {
            self.appear.snap_to(self.appear.value().min(0.6));
        }
        if self.user_hidden || self.hiding {
            self.user_hidden = false;
            self.hiding = false;
            self.dirty = true;
            // Fade back in from wherever the fade-out got to.
            self.opacity.animate_to(1.0, Feel::Smooth);
        }
        self.shown = true;
        self.window.set_visible(true);
        self.window.set_minimized(false);
        self.bring_on_screen();
        self.peeking = false;
        self.apply_level();
        if focus {
            self.window.focus_window();
            self.start_wiggle();
        }
        self.needs_paint = true;
    }

    /// Moves the note onto the primary screen if no screen shows it (see `on_screen`).
    fn bring_on_screen(&mut self) {
        let Ok(pos) = self.window.outer_position() else { return };
        if on_screen(self.window.available_monitors(), (pos.x, pos.y)) {
            return;
        }
        let Some(m) = self.window.primary_monitor().or_else(|| self.window.available_monitors().next()) else { return };
        let (mp, ms, size) = (m.position(), m.size(), self.window.outer_size());
        let x = mp.x + (ms.width as i32 - size.width as i32).max(0) / 2;
        let y = mp.y + (ms.height as i32 - size.height as i32).max(0) / 3;
        self.window.set_outer_position(PhysicalPosition::new(x, y));
        self.placed_at = Some((x, y));
        self.dirty = true;
    }

    /// Sets the note up for a `--shots` picture: light or dark, hovered (showing its controls),
    /// and with one of its menus open: "more", "remind", "colors" or "window" (or "ring": its
    /// reminder ringing, the glow at its brightest).
    pub fn stage(&mut self, dark: bool, hovered: bool, menu: Option<&str>, shared: &mut Shared) {
        if self.dark != dark {
            // Text colours are part of the layout.
            self.dark = dark;
            self.layout_dirty = true;
        }
        self.hovered = hovered;
        self.popover = None;
        self.ringing = menu == Some("ring");
        if let Some(at) = self.doc.text.find("Won").filter(|_| menu == Some("format")) {
            // Text selected, with the formatting toolbar and its colours open.
            self.doc.anchor = Some(at);
            self.doc.cursor = at + 3;
            self.focused = true;
            self.popover = Some(Popover::Format { colors: true });
            self.layout_dirty = true;
        }
        if let Some(first) = self.doc.text.find("| Deal").filter(|_| matches!(menu, Some("row-menu" | "col-menu"))) {
            // A table's row or column menu open (anchored to its handle once laid out).
            let row = menu == Some("row-menu");
            self.table_hover = Some(0);
            self.table_cell = Some((2, if row { 0 } else { 2 }));
            self.table_menu = Some((first, (0.0, 0.0, 0.0, 0.0)));
            self.popover = Some(if row { Popover::TableRow(2) } else { Popover::TableCol(2) });
            self.layout_dirty = true;
        }
        if let Some(at) = self.doc.text.find("Acme Corp").filter(|_| menu == Some("edit-table")) {
            // Typing in a cell, the pointer near the table (its + buttons showing).
            self.doc.anchor = None;
            self.doc.cursor = at + "Acme Corp".len();
            self.focused = true;
            self.editing = true;
            self.caret_on = true;
            self.table_hover = Some(0);
            self.layout_dirty = true;
        }
        if self.ringing {
            self.ring_at = Instant::now() - Duration::from_secs_f32(PULSE / 2.0);
        }
        let action = match menu {
            Some("more") => Some(Action::More),
            Some("remind") => Some(Action::Remind),
            Some("colors") => Some(Action::Colors),
            Some("window") => Some(Action::Window),
            _ => None,
        };
        if let Some(action) = action {
            self.act(action, shared);
        }
        self.needs_paint = true;
    }

    /// Ends every animation at its target, so the next frame shows the note at rest (`--shots`).
    pub fn settle(&mut self) {
        self.pop_kind = self.popover.as_ref().map(std::mem::discriminant);
        for m in [&mut self.chrome, &mut self.opacity, &mut self.appear, &mut self.pop_in] {
            m.settle();
        }
        self.pop = None;
        self.needs_paint = true;
    }

    /// When this note fades for being idle (`[notes.fade]`), or None if it doesn't. Being in use
    /// (focused, pointed at, a menu open, ringing, shown from the dock) restarts the count.
    fn fade_at(&mut self, now: Instant) -> Option<Instant> {
        use crate::config::FadeWhich;
        let f = &self.st.fade;
        let applies =
            f.enabled && (f.which == FadeWhich::All || self.mode == Mode::OnTop) && (f.compact || !self.collapsed());
        if !applies {
            return None;
        }
        let busy = self.focused
            || self.hovered
            || self.ringing
            || self.peeking
            || self.popover.is_some()
            || self.moving.is_some()
            || self.resizing.is_some();
        if busy {
            self.in_use_at = now;
        }
        Some(self.in_use_at + Duration::from_secs_f32(f.after.max(1.0)))
    }

    /// While the app is idle: free the window's pixel buffer (made again at the next paint),
    /// fold the undo history into compact steps, and keep the table width cache small.
    pub fn trim(&mut self) {
        self.surface.trim();
        self.doc.fold();
        if self.cell_widths.len() > 256 {
            self.cell_widths.clear();
        }
    }

    /// Temporarily floats the note above everything while it's hovered in the dock.
    pub fn peek(&mut self, on: bool) {
        self.peeking = on;
        self.apply_level();
    }

    // ---- painting -----------------------------------------------------------------------

    fn layout_bar(&self, fonts: &mut FontSystem) -> Vec<BarItem> {
        let s = self.scale();
        let pal = self.palette();
        let (kx, ky, kw, _) = self.card();
        let btn = 24.0 * s;
        let by = ky + (self.st.top_bar * s - btn) / 2.0;
        let mut items = Vec::new();
        let button = |x: f32, icon: Icon, action: Action| BarItem {
            rect: (x, by, btn, btn),
            action: Some(action),
            draw: Draw::Icon(icon),
            color: pal.secondary,
            alpha: 255,
            pill: None,
            fade: true,
        };

        // Controls cluster in the top-right corner, laid out right to left.
        let mut right = kx + kw - 10.0 * s - btn;
        items.push(button(right, Icon::Close, Action::Close));
        if self.ringing {
            for (text, action) in
                [("Done", Action::Dismiss), ("1h", Action::Snooze(3600)), ("10m", Action::Snooze(600))]
            {
                let label = small_label(fonts, text, 12.0 * s, Weight::SEMIBOLD);
                let lw = label_width(&label) + 18.0 * s;
                right -= lw + 4.0 * s;
                items.push(BarItem {
                    rect: (right, by + 1.0 * s, lw, btn - 2.0 * s),
                    action: Some(action),
                    draw: Draw::Label(label),
                    color: pal.alert,
                    alpha: 255,
                    pill: Some((pal.alert, 34)),
                    fade: false,
                });
            }
        } else {
            // With a reminder set, its badge opens the reminder menu instead of a button.
            let clock = self.reminder.is_none().then_some((Icon::Clock, Action::Remind));
            for (icon, action) in [
                Some((Icon::More, Action::More)),
                clock,
                Some((Icon::Mode(self.mode), Action::Window)),
                Some((Icon::Swatch(theme::tint(self.color, self.dark).0), Action::Colors)),
            ]
            .into_iter()
            .flatten()
            {
                right -= btn + 2.0 * s;
                let mut item = button(right, icon, action);
                if action == Action::Window && self.mode != Mode::Normal {
                    // "On top" / "on desktop" is shown even while the controls are hidden.
                    item.fade = false;
                    item.color = pal.accent;
                }
                items.push(item);
            }
        }

        // The note's icon: top-left in the bar, or large above the text (`icon_position`).
        let mut left = kx + 10.0 * s;
        if !self.icon.is_empty() {
            let st = &self.st;
            let (glyph, rect) = match st.icon_position {
                IconPosition::Bar => {
                    let b = (st.icon_size + 8.0) * s;
                    let r = (left, ky + (st.top_bar * s - b) / 2.0, b, b);
                    left += b + 4.0 * s;
                    (st.icon_size * s, r)
                }
                IconPosition::Page => {
                    let b = (st.icon_size + 10.0) * s;
                    (st.icon_size * s, (kx + st.padding * s - 5.0 * s, ky + st.top_bar * s - 2.0 * s, b, b))
                }
            };
            items.push(BarItem {
                rect,
                action: Some(Action::Icon),
                draw: Draw::Label(small_label(fonts, &self.icon, glyph, Weight::NORMAL)),
                color: pal.text,
                alpha: 255,
                pill: None,
                fade: false,
            });
        }
        // The controls are hidden while the pointer is away; leave their room so nothing jumps.
        let limit = if self.ringing { right } else { right - 4.0 * s };

        // Reminder badge on the left: the active reminder, or a faint preview while typing one.
        let now = Local::now();
        let (rem, pending) = match &self.reminder {
            Some(r) => (Some(r.clone()), false),
            None => (reminder::find_tokens(&self.doc.text, now).pop().map(|t| t.reminder), true),
        };
        if let Some(r) = rem {
            left = self.layout_badge(fonts, &mut items, &r, pending, left, limit);
        }

        // "Add icon" ghost button, shown on hover (Notion).
        if self.icon.is_empty() {
            let label = small_label(fonts, "Add icon", 12.5 * s, Weight::NORMAL);
            let w = 18.0 * s + label_width(&label) + 12.0 * s;
            if left + w <= limit {
                let h = 24.0 * s;
                let y = ky + (self.st.top_bar * s - h) / 2.0;
                let ghost = |rect, draw, action| BarItem {
                    rect,
                    action,
                    draw,
                    color: pal.secondary,
                    alpha: 255,
                    pill: None,
                    fade: true,
                };
                items.push(ghost((left, y, w, h), Draw::Nothing, Some(Action::Icon)));
                items.push(ghost((left + 4.0 * s, y, 16.0 * s, h), Draw::Icon(Icon::Smiley), None));
                items.push(ghost((left + 20.0 * s, y, w - 24.0 * s, h), Draw::Label(label), None));
            }
        }
        items
    }

    /// Lays out the reminder badge at `left`; returns the x after it.
    fn layout_badge(
        &self,
        fonts: &mut FontSystem,
        items: &mut Vec<BarItem>,
        r: &Reminder,
        pending: bool,
        left: f32,
        limit: f32,
    ) -> f32 {
        let s = self.scale();
        let pal = self.palette();
        let (_, ky, _, _) = self.card();
        let now = Local::now();
        let text = if pending { format!("{} ⏎", r.label(now)) } else { r.label(now) };
        let label = small_label(fonts, &text, 12.0 * s, Weight::SEMIBOLD);
        let (pad, icon_w, gap) = (9.0 * s, 13.0 * s, 5.0 * s);
        let x_w = if pending || self.ringing { 0.0 } else { 16.0 * s };
        let lw = label_width(&label).min(limit - left - 2.0 * pad - icon_w - gap - x_w - 4.0 * s);
        if lw < 24.0 * s {
            return left;
        }
        let chip_w = pad + icon_w + gap + lw + if x_w > 0.0 { x_w + 2.0 * s } else { pad };
        let chip_h = 22.0 * s;
        let chip_y = ky + (self.st.top_bar * s - chip_h) / 2.0;
        let (color, pill) = match (self.ringing, pending) {
            (true, _) => (theme::WHITE, Some((pal.alert, 255))),
            (false, true) => (pal.accent, Some((pal.wash, 150))),
            (false, false) => (pal.accent, Some((pal.wash, 255))),
        };
        let alpha = if pending { 160 } else { 255 };
        let part = |rect, draw, action, alpha| BarItem { rect, action, draw, color, alpha, pill: None, fade: false };
        let open = (!pending).then_some(Action::Remind);
        items.push(BarItem { pill, ..part((left, chip_y, chip_w, chip_h), Draw::Nothing, open, 0) });
        items.push(part((left + pad, chip_y, icon_w, chip_h), Draw::Icon(Icon::Clock), None, alpha));
        items.push(part((left + pad + icon_w + gap, chip_y, lw, chip_h), Draw::Label(label), None, alpha));
        if x_w > 0.0 {
            let xr = (left + pad + icon_w + gap + lw, chip_y + 3.0 * s, x_w, chip_h - 6.0 * s);
            items.push(part(xr, Draw::Icon(Icon::SmallX), Some(Action::ClearReminder), 200));
        }
        left + chip_w + 6.0 * s
    }

    pub fn render(&mut self, shared: &mut Shared) {
        self.needs_paint = false;
        if self.view_dirty {
            self.view_dirty = false;
            self.apply_view(&mut shared.fonts);
        }
        let PhysicalSize { width, height } = self.window.inner_size();
        if width == 0 || height == 0 {
            return;
        }
        if self.collapsed() {
            return self.render_compact(shared);
        }
        if self.popover.is_some() || self.extra_h > 0 {
            self.fit_popover(shared);
        }
        let PhysicalSize { width, height } = self.window.inner_size();
        self.sync_layout(&mut shared.fonts);

        let s = self.scale();
        let pal = self.palette();
        let (kx, ky, kw, kh) = self.card();
        let (cx, cy, cw, ch) = self.content_rect();
        let bar = self.layout_bar(&mut shared.fonts);
        self.hits = bar.iter().filter_map(|i| i.action.map(|a| (i.rect, a))).collect();

        // Everything that needs `&self` is computed before the surface is borrowed.
        let sel_rects = match self.doc.selection() {
            Some(r) => {
                let (a, b) = (self.cursor_at(self.to_display(r.start)), self.cursor_at(self.to_display(r.end)));
                self.span_rects(a, b)
            }
            None => Vec::new(),
        };
        let caret = (self.focused && self.caret_on && self.doc.selection().is_none())
            .then(|| {
                let pre = self.doc.preedit.as_ref().and_then(|p| p.cursor).unwrap_or(0);
                self.caret_geom(self.to_display(self.doc.cursor) + pre.min(self.preedit_len))
            })
            .flatten();
        let scrollbar = (self.content_h > ch + 1.0).then(|| {
            let thumb = (ch / self.content_h * ch).max(20.0 * s);
            let y = cy + self.scroll / (self.content_h - ch) * (ch - thumb);
            (kx + kw - 7.0 * s, y, 3.0 * s, thumb)
        });
        // Tables: each row's top and height, and the column widths.
        let shown: Vec<&Table> = self.tables.iter().filter(|t| !t.cols.is_empty()).collect();
        let tables: Vec<TableDraw> = shown
            .iter()
            .map(|t| {
                let rows = t.rows.iter().filter_map(|r| self.caret_geom(r.start).map(|(_, top, h)| (top, h))).collect();
                (rows, t.cols.clone())
            })
            .collect();
        // Where each table is on screen, for the + buttons shown near it.
        let rects: Vec<(gfx::Rect, usize)> = tables
            .iter()
            .zip(&shown)
            .filter_map(|((rows, cols), t)| {
                let (&(top, _), &(last, lh)) = (rows.first()?, rows.last()?);
                let (y0, y1) = (cy + top - self.scroll, cy + last + lh - self.scroll);
                Some(((cx, y0, cols.iter().sum(), y1 - y0), t.rows[0].start))
            })
            .collect();
        self.table_rects = rects;
        self.table_geo = tables
            .iter()
            .map(|(rows, cols)| (rows.iter().map(|&(t, h)| (cy + t - self.scroll, h)).collect(), cols.clone()))
            .collect();
        let plus = self
            .table_hover
            .and_then(|i| self.table_rects.get(i))
            .map(|&(r, _)| r)
            .filter(|_| self.hovered && self.st.table_buttons);
        // Handles for the row and column under the pointer: ⋮⋮ left of the row, ⋯ above the column.
        self.table_handles =
            self.table_hover.zip(self.table_cell).filter(|_| self.hovered).and_then(|(i, (row, col))| {
                let (&((tx, ty, _, _), first), (rows, cols)) = (self.table_rects.get(i)?, self.table_geo.get(i)?);
                let &(ry, rh) = rows.get(row)?;
                let cx0 = tx + cols[..col.min(cols.len())].iter().sum::<f32>();
                let row_h = (tx - 16.0 * s, ry + (rh - 18.0 * s) / 2.0, 12.0 * s, 18.0 * s);
                let col_h = (cx0 + cols.get(col)? / 2.0 - 9.0 * s, ty - 6.0 * s, 18.0 * s, 12.0 * s);
                Some((row_h, col_h, first, row, col))
            });
        let handles = self.table_handles.map(|(r, c, ..)| (r, c));
        let mut decos: Vec<(gfx::Rect, Deco, f32)> = Vec::new();
        for (r, d) in &self.decos {
            let rects = self.span_rects(self.cursor_at(r.start), self.cursor_at(r.end));
            let scale = match &self.pop {
                Some((ls, m)) if self.doc.line_bounds(self.to_doc(r.start)).0 == *ls => m.value(),
                _ => 1.0,
            };
            if matches!(d, Deco::CodePill | Deco::Badge(_)) {
                decos.extend(rects.iter().map(|&q| (q, *d, 1.0)));
            } else if let Some(first) = rects.first() {
                // Union across wrapped runs.
                let (mut x0, mut y0, mut x1, mut y1) = (first.0, first.1, first.0 + first.2, first.1 + first.3);
                for &(x, y, w, h) in &rects[1..] {
                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + w), y1.max(y + h));
                }
                decos.push(((x0, y0, x1 - x0, y1 - y0), *d, scale));
            }
        }
        let font_px = self.st.font_size * s * self.zoom as f32 / 100.0;
        let pop = self.pop_layout(&shared.lists, &shared.config.reminders, &mut shared.fonts);
        self.pop_rect = pop.as_ref().map(|p| p.rect);
        self.hue_strip = pop.as_ref().and_then(|p| p.hue.map(|(r, _)| r));
        self.pop_hits = pop.as_ref().map(|p| p.cells.clone()).unwrap_or_default();
        let pop_hover = self.pop_hover;
        let pop_sel = match self.popover {
            Some(Popover::Slash { sel, .. }) => self.slash_matches().get(sel).map(|&i| PopHit::Slash(i)),
            _ => None,
        };
        let kind = self.popover.as_ref().map(std::mem::discriminant);
        if kind != self.pop_kind {
            self.pop_kind = kind;
            if kind.is_some() {
                self.pop_in.snap_to(0.0);
                self.pop_in.to(1.0);
            }
        }
        let pop_t = self.pop_in.value();
        // Grows from `appear_scale` (97% by default) about the card's centre.
        let from = self.st.appear_scale.clamp(0.5, 1.0);
        let appear = from + (1.0 - from) * self.appear.value().clamp(0.0, 1.0);
        let resizing = self.resizing.is_some();
        let (hover, scroll, ringing) = (self.hover, self.scroll, self.ringing);
        // 0 → 1 → 0 once per breath while ringing.
        let pulse = if ringing {
            let t = self.ring_at.elapsed().as_secs_f32() / PULSE;
            0.5 - 0.5 * (t * std::f32::consts::TAU).cos()
        } else {
            0.0
        };
        let chrome = self.chrome.value().clamp(0.0, 1.0);
        let opacity = (self.opacity.value().clamp(0.0, 1.0) * 255.0) as u8;
        let fade = |item: &BarItem, alpha: u8| if item.fade { (alpha as f32 * chrome) as u8 } else { alpha };
        let buffer = &mut self.buffer;
        let (fonts, swash) = (&mut shared.fonts, &mut shared.swash);

        self.surface.paint(width, height, opacity, |data| {
            if surface::ALPHA {
                let a = pal.shadow as f32 / 255.0 * self.st.shadow;
                let layers = [(13.0 * s, 7.0 * s, a), (2.0 * s, 1.0 * s, a * 0.55)];
                let hole = (kx + 2.0 * s, ky + self.st.radius * s, kw - 4.0 * s, kh - 2.0 * self.st.radius * s);
                gfx::shadow(
                    data,
                    width,
                    height,
                    (kx + 1.0 * s, ky + 1.0 * s, kw - 2.0 * s, kh - 2.0 * s),
                    &layers,
                    Some(hole),
                );
            }
            {
                let Some(mut pm) = PixmapMut::from_bytes(data, width, height) else { return };
                if ringing {
                    // A soft halo in the alert colour, breathing.
                    for i in 0..5 {
                        let g = (i as f32 + 1.0) * 2.0 * s;
                        let r = self.st.radius * s + g;
                        if let Some(p) = gfx::squircle(kx - g, ky - g, kw + 2.0 * g, kh + 2.0 * g, r) {
                            let a = (70.0 - i as f32 * 13.0) * (0.3 + 0.7 * pulse);
                            gfx::stroke_path(&mut pm, &p, 2.2 * s, pal.alert, a.max(0.0) as u8);
                        }
                    }
                }
                if let Some(card) = gfx::squircle(kx, ky, kw, kh, self.st.radius * s) {
                    gfx::fill_path(&mut pm, &card, pal.base, 255);
                    // A soft "cover" in the note's colour, fading into the page.
                    let cover = (kh * 0.5).min(140.0 * s);
                    let strength = (255.0 * self.st.cover.clamp(0.0, 1.0)) as u8;
                    gfx::fill_path_gradient(&mut pm, &card, pal.wash, (ky, strength), (ky + cover, 0));
                    let (edge, edge_alpha) = if pal.dark { (theme::WHITE, 26) } else { (pal.text, 22) };
                    gfx::stroke_path(&mut pm, &card, 1.0 * s, edge, edge_alpha);
                    if ringing {
                        // The top of the note warms to the alert colour with each breath.
                        let warm = (36.0 + 54.0 * pulse) as u8;
                        gfx::fill_path_gradient(&mut pm, &card, pal.alert, (ky, warm), (ky + cover, 0));
                        gfx::stroke_path(&mut pm, &card, 2.0 * s, pal.alert, (165.0 + 90.0 * pulse) as u8);
                    }
                    if resizing {
                        // A dotted outline: "you're resizing this".
                        gfx::stroke_dashed(&mut pm, &card, 1.5 * s, pal.accent, 230, 5.0 * s, 4.0 * s);
                    }
                }
                for &((x, top, rw, lh), deco, _) in decos.iter().filter(|(_, d, _)| d.is_background()) {
                    let y = cy + top - scroll;
                    if y + lh < ky + self.st.top_bar * s - 4.0 * s || y > ky + kh {
                        continue;
                    }
                    let (path, color, alpha) = match deco {
                        Deco::CodePill => {
                            let ph = (font_px * 1.45).min(lh);
                            let p = gfx::rounded_rect(cx + x - 4.0 * s, y + (lh - ph) / 2.0, rw + 8.0 * s, ph, 6.0 * s);
                            (p, pal.text, pal.soft)
                        }
                        Deco::CodeBlock => {
                            let p =
                                gfx::rounded_rect(cx - 10.0 * s, y - 3.0 * s, cw + 20.0 * s, lh + 6.0 * s, 10.0 * s);
                            (p, pal.text, pal.soft)
                        }
                        Deco::Quote => {
                            (gfx::rounded_rect(cx + x, y + 3.0 * s, 3.0 * s, lh - 6.0 * s, 1.5 * s), pal.text, 230)
                        }
                        Deco::Callout => {
                            let p = gfx::rounded_rect(cx - 8.0 * s, y - 2.0 * s, cw + 16.0 * s, lh + 4.0 * s, 8.0 * s);
                            (p, pal.text, pal.soft + 4)
                        }
                        Deco::Badge(c) => {
                            // Notion's background colours: the matching note tints' washes.
                            let (_, wash) = theme::tint(HIGHLIGHT_TINT[c as usize % HIGHLIGHT_TINT.len()], pal.dark);
                            let ph = (font_px * 1.4).min(lh);
                            let p = gfx::rounded_rect(cx + x - 3.0 * s, y + (lh - ph) / 2.0, rw + 6.0 * s, ph, 5.0 * s);
                            (p, wash, 255)
                        }
                        Deco::Rule => {
                            (gfx::rounded_rect(cx, y + lh / 2.0 - 0.5 * s, cw, s.max(1.0), 0.5 * s), pal.secondary, 90)
                        }
                        Deco::Checkbox(_) | Deco::Bullet => (None, pal.text, 0),
                    };
                    if let Some(p) = path {
                        gfx::fill_path(&mut pm, &p, color, alpha);
                    }
                }
                for (rows, cols) in &tables {
                    let (Some(&(top, hh)), Some(&(last, lh))) = (rows.first(), rows.last()) else { continue };
                    if rows.len() < 2 {
                        continue;
                    }
                    let (y0, y1) = (cy + top - scroll, cy + last + lh - scroll);
                    if y1 < ky + self.st.top_bar * s || y0 > ky + kh {
                        continue;
                    }
                    let w: f32 = cols.iter().sum();
                    let hair = s.max(1.0);
                    let (ink, line_a, head_a) = if pal.dark { (theme::WHITE, 26, 10) } else { (pal.text, 26, 9) };
                    // The header row's soft background, down to the separator.
                    let head = hh + rows[1].1;
                    gfx::fill_rect(&mut pm, cx, y0, w, head, ink, head_a);
                    // Horizontal hairlines: the top, under the header, between rows, the bottom.
                    gfx::fill_rect(&mut pm, cx, y0, w, hair, ink, line_a);
                    gfx::fill_rect(&mut pm, cx, y0 + head - hair, w, hair, ink, line_a);
                    for &(t, _) in rows.iter().skip(3) {
                        gfx::fill_rect(&mut pm, cx, cy + t - scroll, w, hair, ink, line_a);
                    }
                    gfx::fill_rect(&mut pm, cx, y1 - hair, w, hair, ink, line_a);
                    // Vertical hairlines: the edges and between columns.
                    let mut x = cx;
                    gfx::fill_rect(&mut pm, x, y0, hair, y1 - y0, ink, line_a);
                    for c in cols {
                        x += c;
                        gfx::fill_rect(&mut pm, x - hair, y0, hair, y1 - y0, ink, line_a);
                    }
                }
                if let Some((row_h, col_h)) = handles {
                    for (r, vertical) in [(row_h, true), (col_h, false)] {
                        if let Some(p) = gfx::rounded_rect(r.0, r.1, r.2, r.3, 3.0 * s) {
                            gfx::fill_path(&mut pm, &p, pal.base, 255);
                            gfx::stroke_path(&mut pm, &p, 1.0 * s, pal.text, 50);
                        }
                        // Grip dots: 2 × 3 for a row, 3 × 2 for a column.
                        let (nx, ny) = if vertical { (2, 3) } else { (3, 2) };
                        let gap = 3.6 * s;
                        for i in 0..nx {
                            for j in 0..ny {
                                let dx = r.0 + r.2 / 2.0 + (i as f32 - (nx - 1) as f32 / 2.0) * gap;
                                let dy = r.1 + r.3 / 2.0 + (j as f32 - (ny - 1) as f32 / 2.0) * gap;
                                if let Some(c) = gfx::circle(dx, dy, 1.1 * s) {
                                    gfx::fill_path(&mut pm, &c, pal.secondary, 230);
                                }
                            }
                        }
                    }
                }
                if let Some((tx, ty, tw, th)) = plus {
                    // Add a row (bottom edge) or a column (right edge).
                    for (bx, by) in [(tx + tw / 2.0, ty + th), (tx + tw, ty + th / 2.0)] {
                        if let Some(c) = gfx::circle(bx, by, 9.0 * s) {
                            gfx::fill_path(&mut pm, &c, pal.base, 255);
                            gfx::stroke_path(&mut pm, &c, 1.0 * s, pal.text, 60);
                        }
                        let (l, t) = (4.5 * s, 1.4 * s);
                        gfx::fill_rect(&mut pm, bx - l, by - t / 2.0, 2.0 * l, t, pal.secondary, 255);
                        gfx::fill_rect(&mut pm, bx - t / 2.0, by - l, t, 2.0 * l, pal.secondary, 255);
                    }
                }
                for &(x, y, rw, rh) in &sel_rects {
                    let top = (cy + y - scroll).max(cy - 4.0 * s);
                    let bottom = (cy + y - scroll + rh).min(cy + ch + 4.0 * s);
                    if bottom > top {
                        gfx::fill_rect(&mut pm, cx + x, top, rw, bottom - top, pal.selection.0, pal.selection.1);
                    }
                }
                for item in &bar {
                    let (x, y, iw, ih) = item.rect;
                    if let Some((c, a)) = item.pill {
                        if let Some(p) = gfx::rounded_rect(x, y, iw, ih, 6.0 * s) {
                            gfx::fill_path(&mut pm, &p, c, fade(item, a));
                        }
                    }
                    if item.action.is_some() && item.action == hover {
                        if let Some(p) = gfx::rounded_rect(x, y, iw, ih, 6.0 * s) {
                            gfx::fill_path(&mut pm, &p, pal.text, fade(item, pal.soft + 10));
                        }
                    }
                    if let Draw::Icon(icon) = item.draw {
                        let a = fade(item, item.alpha);
                        if a > 0 {
                            draw_icon(&mut pm, icon, x + iw / 2.0, y + ih / 2.0, s, item.color, a);
                        }
                    }
                }
                if let Some((x, y, sw, sh)) = scrollbar.filter(|_| chrome > 0.0) {
                    if let Some(p) = gfx::rounded_rect(x, y, sw, sh, sw / 2.0) {
                        gfx::fill_path(&mut pm, &p, pal.secondary, (120.0 * chrome) as u8);
                    }
                }
            }

            // Text: glyph coverage is blended straight into the buffer, clipped to the text area.
            let text_clip = Clip {
                x0: (kx + 4.0 * s) as i32,
                y0: (ky + self.st.top_bar * s - 4.0 * s) as i32,
                x1: (kx + kw - 4.0 * s) as i32,
                y1: (ky + kh - 6.0 * s) as i32,
            };
            // The buffer is scrolled itself: its runs are relative to the top of the view.
            let origin = (cx as i32, cy.round() as i32);
            gfx::draw_text(buffer, fonts, swash, pal.text.text(255), data, width, text_clip, origin);

            let full = Clip { x0: 0, y0: 0, x1: width as i32, y1: height as i32 };
            for item in bar {
                let tc = item.color.text(fade(&item, item.alpha));
                if let Draw::Label(mut label) = item.draw {
                    let (x, y, iw, ih) = item.rect;
                    let lw = label_width(&label);
                    let lh = label.layout_runs().next().map_or(ih, |r| r.line_height);
                    let lx = (x + (iw - lw).max(0.0) / 2.0).round() as i32;
                    let ly = (y + (ih - lh) / 2.0).round() as i32;
                    let clip = Clip { x1: (x + iw) as i32, ..full };
                    gfx::draw_text(&mut label, fonts, swash, tc, data, width, clip, (lx, ly));
                }
            }

            let Some(mut pm) = PixmapMut::from_bytes(data, width, height) else { return };
            let visible = |y: f32, lh: f32| y + lh > cy - 4.0 * s && y < cy + ch + 4.0 * s;
            for ((x, top, rw, lh), deco, pop) in decos {
                let y = cy + top - scroll;
                if !visible(y, lh) {
                    continue;
                }
                let (mx, my) = (cx + x + rw / 2.0, y + lh / 2.0);
                match deco {
                    Deco::Bullet => {
                        if let Some(p) = gfx::circle(mx, my, font_px * 0.14) {
                            gfx::fill_path(&mut pm, &p, pal.text, 255);
                        }
                    }
                    Deco::Checkbox(checked) => {
                        // Notion's to-do box: a small rounded square.
                        let r = (font_px * 0.5).min(lh * 0.38) * pop;
                        let Some(p) = gfx::rounded_rect(mx - r, my - r, 2.0 * r, 2.0 * r, 3.0 * s * pop) else {
                            continue;
                        };
                        if checked {
                            gfx::fill_path(&mut pm, &p, pal.accent, 255);
                            let tick = [
                                (mx - r * 0.5, my + r * 0.02),
                                (mx - r * 0.12, my + r * 0.4),
                                (mx + r * 0.52, my - r * 0.38),
                            ];
                            if let Some(t) = gfx::polyline(&tick, false) {
                                gfx::stroke_path(&mut pm, &t, 1.7 * s, theme::WHITE, 255);
                            }
                        } else {
                            gfx::stroke_path(&mut pm, &p, 1.5 * s, pal.text, 190);
                        }
                    }
                    _ => {}
                }
            }
            if let Some((x, top, lh)) = caret {
                let y = cy + top - scroll;
                if visible(y, lh) {
                    let inset = lh * 0.14;
                    if let Some(p) =
                        gfx::rounded_rect(cx + x - 0.75 * s, y + inset, 1.75 * s, lh - 2.0 * inset, 0.9 * s)
                    {
                        gfx::fill_path(&mut pm, &p, pal.text, 255);
                    }
                }
            }

            // Popovers float above everything; a new one springs open from its top-right corner.
            if let Some(pop) = pop {
                let look = PopLook { s, pal, hover: pop_hover, sel: pop_sel };
                if pop_t >= 0.999 {
                    draw_popover(data, width, height, pop, &look, fonts, swash);
                } else {
                    let anchor = (pop.rect.0 + pop.rect.2, pop.rect.1);
                    let mut layer = vec![0u8; data.len()];
                    draw_popover(&mut layer, width, height, pop, &look, fonts, swash);
                    let k = 0.95 + 0.05 * pop_t.min(1.02);
                    gfx::composite(data, &layer, width, height, pop_t, k, anchor, (1.0 - pop_t) * -6.0 * s);
                }
            }
            gfx::rescale(data, width, height, appear, (kx + kw / 2.0, ky + kh / 2.0));
        });
    }
}

/// How to paint a popover: scale, palette and which cell is highlighted.
struct PopLook {
    s: f32,
    pal: Palette,
    hover: Option<PopHit>,
    sel: Option<PopHit>,
}

fn draw_popover(
    data: &mut [u8],
    width: u32,
    height: u32,
    pop: PopDraw,
    look: &PopLook,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
) {
    let (s, pal, pop_hover, pop_sel) = (look.s, look.pal, look.hover, look.sel);
    let Some(mut pm) = PixmapMut::from_bytes(data, width, height) else { return };
    let (x, y, w, h) = pop.rect;
    for (grow, alpha) in [(8.0, 8), (4.0, 12), (1.5, 18)] {
        let g = grow * s;
        if let Some(p) = gfx::rounded_rect(x - g, y - g + 3.0 * s, w + 2.0 * g, h + 2.0 * g, 8.0 * s + g) {
            gfx::fill_path(&mut pm, &p, Rgb(0, 0, 0), if pal.dark { alpha * 3 } else { alpha });
        }
    }
    if let Some(p) = gfx::rounded_rect(x, y, w, h, 8.0 * s) {
        let base = if pal.dark { Rgb(0x25, 0x25, 0x25) } else { pal.base };
        gfx::fill_path(&mut pm, &p, base, 255);
        gfx::stroke_path(&mut pm, &p, 1.0 * s, pal.text, 28);
    }
    for &(r, hit) in &pop.cells {
        if Some(hit) == pop_hover || Some(hit) == pop_sel {
            if let Some(p) = gfx::rounded_rect(r.0, r.1, r.2, r.3, 5.0 * s) {
                gfx::fill_path(&mut pm, &p, pal.text, pal.soft);
            }
        }
    }
    for &(tx, ty, tw, th) in &pop.tiles {
        if let Some(p) = gfx::rounded_rect(tx, ty, tw, th, 5.0 * s) {
            gfx::stroke_path(&mut pm, &p, 1.0 * s, pal.text, 40);
        }
    }
    for &((bx, by, bw, bh), accent) in &pop.bars {
        if let Some(p) = gfx::rounded_rect(bx, by, bw, bh, bh / 2.0) {
            if accent {
                gfx::fill_path(&mut pm, &p, pal.accent, 255);
            } else {
                gfx::fill_path(&mut pm, &p, pal.text, 30);
            }
        }
    }
    for &(rx, ry, rw, rh) in &pop.selected {
        if let Some(p) = gfx::rounded_rect(rx, ry, rw, rh, 5.0 * s) {
            gfx::fill_path(&mut pm, &p, pal.text, pal.soft + 8);
        }
    }
    if let Some(((hx, hy, hw, hh), marker)) = pop.hue {
        // The strip: 60 slices of the hue wheel, then a rounded outline over them.
        let slices = 60;
        for k in 0..slices {
            let x0 = hx + hw * k as f32 / slices as f32;
            let c = theme::hue_sample(360.0 * (k as f32 + 0.5) / slices as f32, pal.dark);
            gfx::fill_rect(&mut pm, x0, hy, hw / slices as f32 + 0.6, hh, c, 255);
        }
        if let Some(p) = gfx::rounded_rect(hx, hy, hw, hh, hh / 2.0) {
            gfx::stroke_path(&mut pm, &p, 1.0 * s, pal.text, 40);
        }
        if let Some(h) = marker {
            let mx = hx + hw * h / 359.0;
            if let Some(p) = gfx::circle(mx, hy + hh / 2.0, hh * 0.75) {
                gfx::fill_path(&mut pm, &p, theme::hue_sample(h, pal.dark), 255);
                gfx::stroke_path(&mut pm, &p, 2.0 * s, theme::WHITE, 255);
            }
        }
    }
    for &((dx, dy), r, color, current) in &pop.dots {
        if let Some(p) = gfx::circle(dx, dy, r) {
            gfx::fill_path(&mut pm, &p, color, 255);
        }
        if current {
            if let Some(p) = gfx::circle(dx, dy, r + 3.0 * s) {
                gfx::stroke_path(&mut pm, &p, 1.5 * s, color, 255);
            }
        }
    }
    let clip = Clip { x0: x as i32, y0: y as i32, x1: (x + w) as i32, y1: (y + h) as i32 };
    for (mut b, color, (lx, ly)) in pop.labels {
        gfx::draw_text(&mut b, fonts, swash, color, data, width, clip, (lx.round() as i32, ly.round() as i32));
    }
}

fn attrs_for(f: Flags, pal: &Palette, size: f32, line: f32) -> Attrs<'static> {
    use markdown::*;
    let mut a = Attrs::new().family(Family::SansSerif).color(pal.text.text(255));
    let level = heading_level(f);
    if level > 0 {
        let k = [1.0, 1.6, 1.35, 1.15, 1.0, 1.0, 1.0][level as usize];
        a = a.metrics(Metrics::new(size * k, line * k)).weight(Weight::BOLD).letter_spacing(-0.01);
    }
    if f & BOLD != 0 {
        a = a.weight(Weight::SEMIBOLD);
    }
    if f & ITALIC != 0 {
        a = a.style(Style::Italic);
    }
    if f & CODE != 0 {
        a = a.family(Family::Monospace).metrics(Metrics::new(size * 0.86, line)).color(pal.code.text(255));
    }
    if f & LINK != 0 {
        a = a.underline(UnderlineStyle::Single).underline_color(pal.secondary.text(140));
    }
    if f & (STRIKE | DONE) != 0 {
        a = a.strikethrough().color(pal.secondary.text(255));
    }
    if f & MARKER != 0 {
        a = a.color(pal.secondary.text(150));
    }
    if f & REMINDER != 0 {
        a = a.color(pal.accent.text(255)).weight(Weight::SEMIBOLD);
    }
    if f & PREEDIT != 0 {
        a = a.underline(UnderlineStyle::Single);
    }
    if f & HIDDEN != 0 {
        a = a.color(pal.text.text(0));
    }
    if f & COLLAPSED != 0 {
        a = a.color(pal.text.text(0)).metrics(Metrics::new(0.05, line));
    }
    a
}

/// A note's title: its first line of text without markdown marks.
pub fn title_of(text: &str) -> String {
    text_lines(text).next().unwrap_or_else(|| "New note".to_string())
}

/// The note's non-empty lines without markdown marks (rules skipped).
fn text_lines(text: &str) -> impl Iterator<Item = String> + '_ {
    let clean = |line: &str| -> String {
        let line = &markdown::strip_marks(line);
        if is_table_separator(line) {
            return String::new();
        }
        if let Some(pipes) = table_pipes(line) {
            let cells: Vec<&str> =
                table_cells(line, &pipes).into_iter().map(|r| &line[r]).filter(|c| !c.is_empty()).collect();
            return cells.join(" · ").chars().filter(|c| !matches!(c, '*' | '`' | '~')).collect();
        }
        let info = line_info(line);
        let body = if info.is_list() { &line[info.content_start(line)..] } else { line };
        let body: String =
            body.trim_start_matches(['#', '>', ' ']).chars().filter(|c| !matches!(c, '*' | '`' | '~')).collect();
        body.trim().to_string()
    };
    text.lines().map(clean).filter(|l| !l.is_empty() && !l.chars().all(|c| matches!(c, '-' | '_')))
}

/// One line of text cut off with "…" at `width`.
pub fn ellipsized(fonts: &mut FontSystem, text: &str, size: f32, weight: Weight, width: f32) -> Buffer {
    let mut b = Buffer::new_empty(Metrics::new(size, size * 1.3));
    b.set_wrap(Wrap::None);
    b.set_ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)));
    b.set_size(Some(width.max(1.0)), None);
    b.set_text(text, &Attrs::new().family(Family::SansSerif).weight(weight), Shaping::Advanced, None);
    b.shape_until_scroll(fonts, false);
    b
}

/// A formatting toolbar label: `text` in `weight`, and (italic, struck through, monospaced).
fn format_label(
    fonts: &mut FontSystem,
    text: &str,
    size: f32,
    weight: Weight,
    (italic, strike, mono): (bool, bool, bool),
) -> Buffer {
    let mut a = Attrs::new().family(if mono { Family::Monospace } else { Family::SansSerif }).weight(weight);
    if italic {
        a = a.style(Style::Italic);
    }
    if strike {
        a = a.strikethrough();
    }
    let mut b = Buffer::new_empty(Metrics::new(size, size * 1.3));
    b.set_text(text, &a, Shaping::Advanced, None);
    b.shape_until_scroll(fonts, false);
    b
}

pub fn small_label(fonts: &mut FontSystem, text: &str, size: f32, weight: Weight) -> Buffer {
    let mut b = Buffer::new_empty(Metrics::new(size, size * 1.3));
    b.set_text(text, &Attrs::new().family(Family::SansSerif).weight(weight), Shaping::Advanced, None);
    b.shape_until_scroll(fonts, false);
    b
}

pub fn label_width(b: &Buffer) -> f32 {
    b.layout_runs().map(|r| r.line_w).fold(0.0, f32::max)
}

fn draw_icon(pm: &mut PixmapMut, icon: Icon, cx: f32, cy: f32, s: f32, c: Rgb, alpha: u8) {
    let lw = 1.5 * s;
    let line = |pm: &mut PixmapMut, pts: &[(f32, f32)]| {
        if let Some(p) = gfx::polyline(pts, false) {
            gfx::stroke_path(pm, &p, lw, c, alpha);
        }
    };
    match icon {
        Icon::Close | Icon::SmallX => {
            let r = if matches!(icon, Icon::Close) { 4.0 * s } else { 3.0 * s };
            line(pm, &[(cx - r, cy - r), (cx + r, cy + r)]);
            line(pm, &[(cx - r, cy + r), (cx + r, cy - r)]);
        }
        Icon::Mode(Mode::Normal) => {
            // Two stacked cards: an ordinary window.
            if let Some(p) = gfx::rounded_rect(cx - 5.0 * s, cy - 3.0 * s, 9.0 * s, 8.0 * s, 2.0 * s) {
                gfx::stroke_path(pm, &p, lw, c, alpha);
            }
            line(pm, &[(cx - 2.5 * s, cy - 5.5 * s), (cx + 6.5 * s, cy - 5.5 * s), (cx + 6.5 * s, cy + 2.0 * s)]);
        }
        Icon::Mode(m) => {
            // Arrow towards a bar: up = always on top, down = pinned to desktop.
            let d = if m == Mode::OnTop { -1.0 } else { 1.0 };
            let r = 5.0 * s;
            line(pm, &[(cx - r, cy + d * r), (cx + r, cy + d * r)]);
            line(pm, &[(cx, cy - d * r * 0.9), (cx, cy + d * r * 0.4)]);
            let tip = cy + d * r * 0.4;
            line(pm, &[(cx - 3.0 * s, tip - d * 3.0 * s), (cx, tip), (cx + 3.0 * s, tip - d * 3.0 * s)]);
        }
        Icon::Swatch(color) => {
            if let Some(p) = gfx::circle(cx, cy, 5.5 * s) {
                gfx::fill_path(pm, &p, color, alpha);
            }
        }
        Icon::More => {
            for dx in [-4.5, 0.0, 4.5] {
                if let Some(p) = gfx::circle(cx + dx * s, cy, 1.25 * s) {
                    gfx::fill_path(pm, &p, c, alpha);
                }
            }
        }
        Icon::Smiley => {
            if let Some(p) = gfx::circle(cx, cy, 6.0 * s) {
                gfx::stroke_path(pm, &p, 1.3 * s, c, alpha);
            }
            for dx in [-2.2, 2.2] {
                if let Some(p) = gfx::circle(cx + dx * s, cy - 1.5 * s, 0.9 * s) {
                    gfx::fill_path(pm, &p, c, alpha);
                }
            }
            line(pm, &[(cx - 2.6 * s, cy + 1.6 * s), (cx, cy + 3.0 * s), (cx + 2.6 * s, cy + 1.6 * s)]);
        }
        Icon::Clock => {
            if let Some(p) = gfx::circle(cx, cy, 5.0 * s) {
                gfx::stroke_path(pm, &p, 1.3 * s, c, alpha);
            }
            line(pm, &[(cx, cy - 2.8 * s), (cx, cy), (cx + 2.2 * s, cy + 1.3 * s)]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_skip_markdown_and_rules() {
        assert_eq!(title_of("# Groceries\n- [ ] milk"), "Groceries");
        assert_eq!(title_of("---\n\n- [x] **done** thing"), "done thing");
        assert_eq!(title_of("> quoted `code`"), "quoted code");
        assert_eq!(title_of("   \n"), "New note");
    }
}
