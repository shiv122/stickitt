//! The edge dock: a slim tab on a screen edge that slides out your notes.
//!
//! List views (compact, detailed) show each note as its own floating card: an icon tile and
//! the title. Cards cascade in when the dock opens, and hovering pushes the card under the
//! pointer out from the edge with its neighbours following less, like the macOS Dock's
//! magnification turned sideways. The icon view is the Dock proper: tiles that magnify. Cards
//! are grouped by the note lists of the active profile; notes on other virtual desktops get
//! their own group, and a "New note" card ends the stack. There is no header: right-click
//! opens a menu (profiles, lists, view) and dragging any card moves the dock to another edge.
//! Overflow scrolls on a spring with no scrollbar: cards fade out at the edges. Sizes come
//! from `[dock]` in `stickit.ini`. While tucked away the window shrinks to a few pixels.

use crate::config::{DockStyle, ListHeaders, Side};
use crate::gfx::{self, Clip, Rect};
use crate::motion::{Feel, Motion};
use crate::note::{ellipsized, label_width, small_label, Shared};
use crate::store::{List, Profile, DEFAULT_LIST, DEFAULT_PROFILE};
use crate::surface::{self, Surface};
use crate::theme::{self, palette, Palette, Rgb};
use cosmic_text::{Buffer, FontSystem, Weight};
use std::collections::HashSet;
use std::error::Error;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tiny_skia::PixmapMut;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorIcon, Window, WindowId, WindowLevel};

// Logical pixels.
/// Pointer travel that turns a press on a card into a drag of the whole dock.
const DRAG_SLOP: f64 = 6.0;
/// A group label row: the label pill plus space above it.
const SECTION_H: f32 = 34.0;
const SECTION_PILL_H: f32 = 22.0;
const MENU_ROW_H: f32 = 30.0;
const MENU_PAD: f32 = 8.0;
const SEPARATOR: f32 = 14.0;
/// Icon view: room beside the strip for magnified tiles and the name bubble.
const LABEL_ROOM: f32 = 230.0;
/// Pointer distance over which neighbours magnify (icons) or slide out (cards).
const REACH: f32 = 115.0;
/// Cards cascade in: each starts this much (of the open animation) after the one above.
const CASCADE: f32 = 0.035;
const SHADOW: f32 = if surface::ALPHA { 26.0 } else { 0.0 };
const CLOSE_DELAY: Duration = Duration::from_millis(320);
/// Tucked away by a click, the strip settles under the still pointer with a flurry of leave/enter
/// pairs (macOS); coming back sooner than this after leaving is not a return.
const SETTLE: Duration = Duration::from_millis(200);
/// A note is floated to the front only once the pointer rests on its card this long; sweeping
/// across the stack would otherwise reshuffle every window it passes.
const PEEK_DELAY: Duration = Duration::from_millis(140);
const FRAME: Duration = Duration::from_millis(16);
/// Group id used for "Other desktops".
const OTHER_DESKTOPS: i64 = -1;

#[derive(Clone, PartialEq)]
pub struct DockItem {
    pub id: WindowId,
    pub title: String,
    pub sub: String,
    pub tint: u16,
    pub icon: String,
    pub reminder: bool,
    pub list: i64,
    /// On the virtual desktop the user is looking at.
    pub here: bool,
    /// Hidden with × (clicking it brings it back).
    pub hidden: bool,
    /// Database id.
    pub note: i64,
    /// In the trash (no window; clicking it restores it).
    pub trashed: bool,
}

#[derive(Clone, Default, PartialEq)]
pub struct DockModel {
    pub lists: Vec<List>,
    pub profiles: Vec<Profile>,
    pub active: i64,
    /// List notes from other virtual desktops too (in their own group).
    pub show_others: bool,
}

impl DockModel {
    fn profile(&self) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == self.active)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Compact,
    Detailed,
    Icons,
}

impl View {
    pub fn parse(s: &str) -> View {
        match s {
            "detailed" => View::Detailed,
            "icons" => View::Icons,
            _ => View::Compact,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            View::Compact => "compact",
            View::Detailed => "detailed",
            View::Icons => "icons",
        }
    }
}

pub enum DockCmd {
    Focus(WindowId),
    Peek(Option<WindowId>),
    NewNote,
    /// The dock started sliding out (a good moment to note the current desktop).
    Opening,
    SetView(View),
    SelectProfile(i64),
    NewProfile(String),
    DeleteProfile(i64),
    ToggleList(i64, bool),
    NewList(String),
    /// Delete a list; its notes go to the default list, or to the trash.
    DeleteList {
        id: i64,
        trash_notes: bool,
    },
    OpenSettings,
    Quit,
    /// Take a note out of the trash (database id).
    Restore(i64),
    /// Delete a trashed note for good (database id).
    Purge(i64),
    EmptyTrash,
    /// The user dragged the dock to a new edge/height.
    Moved {
        side: Side,
        position: f32,
    },
    /// Ctrl+wheel changed the icon tile size or the card width.
    Resized {
        tile_size: f32,
        width: f32,
    },
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Note(usize),
    New,
    SetView(View),
    Section(i64),
    Profile(i64),
    DeleteProfile(i64),
    AddProfile,
    ListToggle(i64),
    DeleteList(i64),
    /// Answers to "what happens to this list's notes?"
    KeepNotes(i64),
    TrashNotes(i64),
    CancelDelete,
    AddList,
    Settings,
    Quit,
    Input,
    OpenTrash,
    CloseTrash,
    /// Delete one trashed note for good (item index).
    Purge(usize),
    EmptyTrash,
}

#[derive(Clone, Copy, PartialEq)]
enum InputKind {
    Profile,
    List,
}

enum RowKind {
    Section {
        title: String,
        count: usize,
        collapsed: bool,
    },
    Note(usize),
    Caption(String),
    Choice {
        label: String,
        on: bool,
        radio: bool,
        delete: Option<Hit>,
    },
    Add(&'static str),
    Input(&'static str),
    Empty(&'static str),
    NewNote,
    Search,
    /// Menu: opens the trash page, with the number of notes in it.
    TrashLink(usize),
    Back,
    /// Menu: a line of explanation, indented under the row it's about.
    Prompt(String),
    /// Menu: one of the answers to a prompt.
    Action {
        label: String,
        danger: bool,
    },
    /// Menu: a trashed note (item index).
    Trashed(usize),
    EmptyTrash,
}

/// A row in content coordinates (physical px, scroll not applied).
struct Row {
    y: f32,
    h: f32,
    hit: Option<Hit>,
    kind: RowKind,
}

/// A row placed on screen as a card.
struct Card {
    rect: (f32, f32, f32, f32),
    row: usize,
    /// Fades at the edges of the scroll area.
    alpha: f32,
    /// How strongly the pointer is lifting this card, 0..1.
    lift: f32,
}

struct Tile {
    rect: (f32, f32, f32, f32),
    hit: Hit,
}

pub struct Dock {
    pub window: Rc<Window>,
    surface: Surface,
    items: Vec<DockItem>,
    model: DockModel,
    pub view: View,
    style: DockStyle,
    /// 0 = tucked away, 1 = out (springs a touch past 1 on the way out).
    open: Motion,
    want_open: bool,
    /// The window is at full size (it shrinks to a strip once tucked away).
    expanded: bool,
    open_at: Option<Instant>,
    close_at: Option<Instant>,
    /// Strength of the hover push/magnification, and the spring-smoothed pointer y.
    magnify: Motion,
    follow: Motion,
    scroll: Motion,
    pointer: Option<(f64, f64)>,
    hover: Option<Hit>,
    /// A pending peek (when, which item) and whether a note is currently peeked.
    peek_at: Option<(Instant, usize)>,
    peeking: bool,
    menu: bool,
    /// Left button down on the dock: where, and what was under it (click or drag decides later).
    press: Option<((f64, f64), Option<Hit>)>,
    collapsed: HashSet<i64>,
    /// The menu shows the trash instead of profiles and lists.
    trash_page: bool,
    /// "Empty trash" was clicked once; the next click empties it.
    confirm_empty: bool,
    /// The list being deleted, while the menu asks what happens to its notes.
    deleting: Option<i64>,
    input: Option<(InputKind, String)>,
    /// Type-to-filter text (when the dock has keyboard focus).
    query: String,
    /// Pointer offset inside the window while the dock is being dragged.
    drag: Option<(f64, f64)>,
    /// The tab being dragged along its edge: pointer's screen y and the position at the press,
    /// and whether it has moved yet (a press that doesn't move is a click).
    tab_drag: Option<(f64, f32, bool)>,
    /// Tucked away by a click on the tab: hovering opens it again only once the pointer has
    /// left and come back (macOS reports it entering the shrunken strip straight away).
    rest: bool,
    /// When the pointer last left while resting.
    left_at: Option<Instant>,
    /// Where the tab sits inside the window (physical px from the top). The window is clamped to
    /// the screen, so near an edge the tab is not at its middle.
    anchor: f32,
    /// Spring-animated window position after a drop.
    snap: Option<(Motion, Motion)>,
    dark: bool,
    pub needs_paint: bool,
    /// A command produced while animating (the hovered note changed under a still pointer).
    pub pending: Option<DockCmd>,
    /// Monitor origin and size (physical) and scale.
    monitor: (PhysicalPosition<i32>, PhysicalSize<u32>, f64),
}

/// Text to draw: buffer, colour, position and clip.
type Label = (Buffer, cosmic_text::Color, (f32, f32), Clip);

fn line_h(b: &Buffer) -> f32 {
    b.layout_runs().next().map_or(0.0, |r| r.line_height)
}

fn inside(r: (f32, f32, f32, f32), x: f32, y: f32) -> bool {
    x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
}

/// Raised-cosine falloff: 1 at distance 0, 0 at `reach` and beyond.
fn falloff(distance: f32, reach: f32) -> f32 {
    let d = (distance.abs() / reach).min(1.0);
    (1.0 + (std::f32::consts::PI * d).cos()) / 2.0
}

impl Dock {
    pub fn open(
        el: &ActiveEventLoop,
        ctx: &surface::Context,
        style: DockStyle,
        view: View,
    ) -> Result<Dock, Box<dyn Error>> {
        let mon = el.primary_monitor().or_else(|| el.available_monitors().next()).ok_or("no monitor")?;
        let monitor = (mon.position(), mon.size(), mon.scale_factor());
        let attrs = Window::default_attributes()
            // macOS: a see-through window (our pixels carry the alpha).
            .with_transparent(cfg!(target_os = "macos"))
            .with_title("Stick-itt")
            .with_decorations(false)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_active(false)
            .with_visible(false)
            .with_inner_size(PhysicalSize::new(1, 1));
        #[cfg(windows)]
        let attrs = {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs.with_skip_taskbar(true)
        };
        let window = Rc::new(el.create_window(attrs)?);
        // The edge tab is there on every desktop (macOS Spaces, full-screen apps included).
        crate::desktops::show_on_all(&window, true);
        let surface = Surface::new(ctx, &window, true)?;
        let dark = crate::theme::is_dark(window.theme());
        let mut dock = Dock {
            window,
            surface,
            items: Vec::new(),
            model: DockModel::default(),
            view,
            style,
            open: Motion::new(0.0, Feel::Snappy),
            want_open: false,
            expanded: false,
            open_at: None,
            close_at: None,
            magnify: Motion::new(0.0, Feel::Quick),
            follow: Motion::new(0.0, Feel::Quick).with_epsilon(0.5),
            scroll: Motion::new(0.0, Feel::Smooth).with_epsilon(0.5),
            pointer: None,
            hover: None,
            peek_at: None,
            peeking: false,
            menu: false,
            press: None,
            collapsed: HashSet::new(),
            trash_page: false,
            confirm_empty: false,
            deleting: None,
            input: None,
            query: String::new(),
            drag: None,
            tab_drag: None,
            rest: false,
            left_at: None,
            anchor: 0.0,
            snap: None,
            dark,
            needs_paint: true,
            pending: None,
            monitor,
        };
        dock.apply_geometry();
        dock.window.set_visible(true);
        Ok(dock)
    }

    /// Sets the dock up for a `--shots` picture: light or dark, open or tucked away, in `view`,
    /// with or without the right-click menu.
    pub fn stage(&mut self, dark: bool, open: bool, view: View, menu: bool) {
        self.dark = dark;
        self.view = view;
        self.start(open);
        if self.menu != menu {
            self.toggle_menu();
        }
        self.apply_geometry();
        self.needs_paint = true;
    }

    /// Ends every animation at its target, so the next frame shows the dock at rest (`--shots`).
    pub fn settle(&mut self) {
        for m in [&mut self.open, &mut self.magnify, &mut self.follow, &mut self.scroll] {
            m.settle();
        }
        self.needs_paint = true;
    }

    /// Picks up a changed `[general] theme`.
    pub fn refresh_theme(&mut self) {
        let dark = crate::theme::is_dark(self.window.theme());
        if dark != self.dark {
            self.dark = dark;
            self.needs_paint = true;
        }
    }

    /// While the app is idle: free the window's pixel buffer (made again at the next paint).
    pub fn trim(&mut self) {
        self.surface.trim();
    }

    pub fn is_open(&self) -> bool {
        self.want_open || self.expanded
    }

    pub fn set_items(&mut self, items: Vec<DockItem>) {
        if items != self.items {
            self.items = items;
            self.refresh();
        }
    }

    pub fn set_model(&mut self, model: DockModel) {
        if model != self.model {
            self.model = model;
            self.refresh();
        }
    }

    pub fn set_style(&mut self, style: DockStyle) {
        if style != self.style {
            self.style = style;
            self.apply_geometry();
        }
    }

    /// Ctrl+wheel: the icon tiles (or, in the list views, the cards) one step bigger or smaller.
    fn resize_by(&mut self, steps: f32) -> Option<DockCmd> {
        if self.icons() {
            self.style.tile_size = (self.style.tile_size + 2.0 * steps).clamp(16.0, 96.0);
        } else {
            self.style.width = (self.style.width + 10.0 * steps).clamp(140.0, 520.0);
        }
        self.apply_geometry();
        Some(DockCmd::Resized { tile_size: self.style.tile_size, width: self.style.width })
    }

    /// Slides the dock out (or tucks it away) without the pointer, e.g. from a hotkey.
    pub fn toggle(&mut self) {
        let open = !self.want_open;
        self.start(open);
        if open {
            self.window.focus_window();
        }
    }

    fn refresh(&mut self) {
        if self.expanded {
            self.apply_geometry();
        }
        self.needs_paint = true;
    }

    fn scale(&self) -> f32 {
        self.monitor.2 as f32
    }

    fn right(&self) -> bool {
        self.style.side == Side::Right
    }

    fn pal(&self) -> Palette {
        palette(5, self.dark)
    }

    fn card_bg(&self) -> Rgb {
        if self.dark {
            Rgb(0x23, 0x23, 0x26)
        } else {
            Rgb(0xFF, 0xFF, 0xFF)
        }
    }

    fn window_size(&self) -> (f32, f32) {
        let size = self.window.inner_size();
        (size.width as f32, size.height as f32)
    }

    // ---- rows -------------------------------------------------------------------------------

    /// A note card's height (without the gap below it).
    fn note_h(&self) -> f32 {
        let st = &self.style;
        let text = st.font_size * 1.35;
        let body = match self.view {
            View::Detailed => text + st.font_size * 0.86 * 1.3,
            _ => text,
        };
        (body + 2.0 * st.row_padding).max(st.icon_size + 12.0)
    }

    /// Rows in content coordinates: note groups, or the profile menu.
    fn rows(&self) -> Vec<Row> {
        let s = self.scale();
        let mut rows = Vec::new();
        let mut y = 0.0;
        let mut push = |h: f32, hit: Option<Hit>, kind: RowKind, rows: &mut Vec<Row>| {
            rows.push(Row { y, h: h * s, hit, kind });
            y += h * s;
        };
        if self.menu && self.trash_page {
            push(MENU_ROW_H, Some(Hit::CloseTrash), RowKind::Back, &mut rows);
            let trash: Vec<usize> = (0..self.items.len()).filter(|&i| self.items[i].trashed).collect();
            if trash.is_empty() {
                push(MENU_ROW_H + 6.0, None, RowKind::Caption("THE TRASH IS EMPTY".into()), &mut rows);
                return rows;
            }
            push(MENU_ROW_H - 4.0, None, RowKind::Caption("CLICK A NOTE TO RESTORE IT".into()), &mut rows);
            for i in trash {
                push(MENU_ROW_H, Some(Hit::Note(i)), RowKind::Trashed(i), &mut rows);
            }
            push(MENU_ROW_H + 6.0, Some(Hit::EmptyTrash), RowKind::EmptyTrash, &mut rows);
            return rows;
        }
        if self.menu {
            push(MENU_ROW_H - 4.0, None, RowKind::Caption("PROFILES".into()), &mut rows);
            for p in &self.model.profiles {
                let delete = (p.id != DEFAULT_PROFILE).then_some(Hit::DeleteProfile(p.id));
                let kind =
                    RowKind::Choice { label: p.name.clone(), on: p.id == self.model.active, radio: true, delete };
                push(MENU_ROW_H, Some(Hit::Profile(p.id)), kind, &mut rows);
            }
            match &self.input {
                Some((InputKind::Profile, _)) => {
                    push(MENU_ROW_H + 4.0, Some(Hit::Input), RowKind::Input("Profile name"), &mut rows)
                }
                _ => push(MENU_ROW_H, Some(Hit::AddProfile), RowKind::Add("New profile"), &mut rows),
            }
            let name = self.model.profile().map_or("PROFILE".to_string(), |p| p.name.to_uppercase());
            push(MENU_ROW_H + 2.0, None, RowKind::Caption(format!("LISTS IN {name}")), &mut rows);
            let active = self.model.profile().map(|p| p.lists.clone()).unwrap_or_default();
            for l in &self.model.lists {
                let delete = (l.id != DEFAULT_LIST).then_some(Hit::DeleteList(l.id));
                let kind = RowKind::Choice { label: l.name.clone(), on: active.contains(&l.id), radio: false, delete };
                push(MENU_ROW_H, Some(Hit::ListToggle(l.id)), kind, &mut rows);
                if self.deleting == Some(l.id) {
                    let n = self.notes_in(l.id);
                    let home = self.model.lists.iter().find(|l| l.id == DEFAULT_LIST).map_or("Notes", |l| &l.name);
                    let notes = if n == 1 { "1 note".to_string() } else { format!("{n} notes") };
                    let ask = format!("Delete “{}” and its {notes}?", l.name);
                    push(MENU_ROW_H, None, RowKind::Prompt(ask), &mut rows);
                    let keep = RowKind::Action { label: format!("Delete list, keep notes in {home}"), danger: false };
                    push(MENU_ROW_H, Some(Hit::KeepNotes(l.id)), keep, &mut rows);
                    let trash = RowKind::Action { label: "Delete list and notes (to Trash)".into(), danger: true };
                    push(MENU_ROW_H, Some(Hit::TrashNotes(l.id)), trash, &mut rows);
                    let cancel = RowKind::Action { label: "Cancel".into(), danger: false };
                    push(MENU_ROW_H, Some(Hit::CancelDelete), cancel, &mut rows);
                }
            }
            match &self.input {
                Some((InputKind::List, _)) => {
                    push(MENU_ROW_H + 4.0, Some(Hit::Input), RowKind::Input("List name"), &mut rows)
                }
                _ => push(MENU_ROW_H, Some(Hit::AddList), RowKind::Add("New list"), &mut rows),
            }
            push(MENU_ROW_H + 2.0, None, RowKind::Caption("VIEW".into()), &mut rows);
            for (view, label) in
                [(View::Compact, "Compact list"), (View::Detailed, "Detailed list"), (View::Icons, "Icons")]
            {
                let kind = RowKind::Choice { label: label.into(), on: self.view == view, radio: true, delete: None };
                push(MENU_ROW_H, Some(Hit::SetView(view)), kind, &mut rows);
            }
            let trashed = self.items.iter().filter(|it| it.trashed).count();
            push(MENU_ROW_H + 6.0, Some(Hit::OpenTrash), RowKind::TrashLink(trashed), &mut rows);
            push(MENU_ROW_H, Some(Hit::Settings), RowKind::Add("Settings…"), &mut rows);
            push(MENU_ROW_H, Some(Hit::Quit), RowKind::Add("Quit Stick-itt"), &mut rows);
            return rows;
        }

        let note_h = self.note_h();
        if !self.query.is_empty() {
            push(SECTION_H, None, RowKind::Search, &mut rows);
        }
        let active = self.model.profile().map(|p| p.lists.clone()).unwrap_or_default();
        let mut groups: Vec<(i64, String, Vec<usize>)> = self
            .model
            .lists
            .iter()
            .filter(|l| active.contains(&l.id))
            .map(|l| {
                let notes = (0..self.items.len())
                    .filter(|&i| {
                        let it = &self.items[i];
                        !it.trashed && it.list == l.id && it.here && self.matches(i)
                    })
                    .collect();
                (l.id, l.name.clone(), notes)
            })
            .collect();
        let elsewhere: Vec<usize> = (0..self.items.len())
            .filter(|&i| {
                let it = &self.items[i];
                self.model.show_others && !it.trashed && !it.here && active.contains(&it.list) && self.matches(i)
            })
            .collect();
        groups.push((OTHER_DESKTOPS, "Other desktops".into(), elsewhere));
        groups.retain(|(_, _, notes)| !notes.is_empty());
        let none = groups.is_empty();
        // A single group needs no label.
        let headers = self.style.list_headers;
        let labelled =
            headers != ListHeaders::None && (groups.len() > 1 || groups.first().is_some_and(|g| g.0 == OTHER_DESKTOPS));
        for (id, title, notes) in &groups {
            let collapsed = self.collapsed.contains(id);
            if labelled {
                let kind = RowKind::Section { title: title.clone(), count: notes.len(), collapsed };
                // A header card takes a note's place in the column; the others sit above the list.
                let h = if headers == ListHeaders::Card { note_h + self.style.card_gap } else { SECTION_H };
                push(h, Some(Hit::Section(*id)), kind, &mut rows);
            }
            if !collapsed || !labelled {
                for &i in notes {
                    push(note_h + self.style.card_gap, Some(Hit::Note(i)), RowKind::Note(i), &mut rows);
                }
            }
        }
        if none {
            let text = if self.query.is_empty() { "No notes in this profile yet" } else { "No matching notes" };
            push(note_h + self.style.card_gap, None, RowKind::Empty(text), &mut rows);
        }
        push(note_h + self.style.card_gap, Some(Hit::New), RowKind::NewNote, &mut rows);
        rows
    }

    /// The icon strip is used unless the menu is open (the menu is always a list).
    fn icons(&self) -> bool {
        self.view == View::Icons && !self.menu
    }

    /// Notes (not in the trash) in a list.
    fn notes_in(&self, list: i64) -> usize {
        self.items.iter().filter(|it| !it.trashed && it.list == list).count()
    }

    fn matches(&self, i: usize) -> bool {
        if self.query.is_empty() {
            return true;
        }
        let q = self.query.to_lowercase();
        let item = &self.items[i];
        item.title.to_lowercase().contains(&q) || item.sub.to_lowercase().contains(&q)
    }

    fn content_h(rows: &[Row]) -> f32 {
        rows.last().map_or(0.0, |r| r.y + r.h)
    }

    // ---- geometry ---------------------------------------------------------------------------

    fn max_body(&self) -> f32 {
        self.monitor.1.height as f32 * 0.75
    }

    /// How far a column item slides off-screen while tucked away.
    fn slide_distance(&self) -> f32 {
        let st = &self.style;
        (st.width + st.edge_gap + st.hover_push + 16.0) * self.scale()
    }

    /// Left x of a column item `width` wide that is `progress` of the way out, pushed `push` px.
    fn column_x(&self, width: f32, progress: f32, push: f32) -> f32 {
        let (w, _) = self.window_size();
        let gap = self.style.edge_gap * self.scale();
        let off = (1.0 - progress) * self.slide_distance();
        if self.right() {
            w - gap - width + off - push
        } else {
            gap - off + push
        }
    }

    /// The scroll area: the card stack, or the menu panel's inside.
    fn body_rect(&self) -> (f32, f32, f32, f32) {
        let s = self.scale();
        let w = self.style.width * s;
        let x = self.column_x(w, self.open.value(), 0.0);
        let (_, wh) = self.window_size();
        let pad = if self.menu { MENU_PAD * s } else { 0.0 };
        let top = SHADOW * s + pad;
        (x, top, w, (wh - SHADOW * s - top - pad).max(0.0))
    }

    fn full_size(&self) -> (u32, u32) {
        let s = self.scale();
        let st = &self.style;
        let (w, h) = if self.icons() {
            let n = self.shown_tiles() as f32 + 1.0;
            let column = n * st.tile_size + (n - 1.0) * self.style.tile_gap + SEPARATOR + 2.0 * self.style.tile_padding;
            let grow = st.tile_size * (st.magnify - 1.0) * 2.4;
            let strip_w = st.tile_size + 2.0 * self.style.tile_padding;
            (
                (LABEL_ROOM + st.tile_size * (st.magnify - 1.0) + strip_w + st.edge_gap + SHADOW) * s,
                (column + grow + 2.0 * SHADOW) * s,
            )
        } else {
            let pad = if self.menu { 2.0 * MENU_PAD } else { 0.0 };
            let body = Self::content_h(&self.rows()).min(self.max_body());
            let w = (st.width + st.edge_gap + st.hover_push + SHADOW + 4.0) * s;
            (w, body + (pad + 2.0 * SHADOW) * s)
        };
        (w.ceil() as u32, h.ceil() as u32)
    }

    fn apply_geometry(&mut self) {
        let s = self.scale();
        let (w, h) = if self.expanded {
            self.full_size()
        } else {
            let st = &self.style;
            let w = st.trigger_width.max(st.tab_width + 2.0);
            ((w * s).ceil() as u32, (st.trigger_height.max(st.tab_height) * s).ceil() as u32)
        };
        let (pos, size, _) = self.monitor;
        let x = if self.right() { pos.x + size.width as i32 - w as i32 } else { pos.x };
        let centre = pos.y + (size.height as f32 * self.style.position) as i32;
        let y = (centre - h as i32 / 2).clamp(pos.y, (pos.y + size.height as i32 - h as i32).max(pos.y));
        self.anchor = (centre - y) as f32;
        if self.window.inner_size() != PhysicalSize::new(w, h) {
            // macOS resizes from the bottom-left corner: put the top-left back at once, or the
            // dock jumps for a frame while it slides to its place.
            let top_left = self.window.outer_position().ok();
            let _ = self.window.request_inner_size(PhysicalSize::new(w, h));
            if let Some(p) = top_left.filter(|_| cfg!(target_os = "macos")) {
                self.window.set_outer_position(p);
            }
        }
        match self.snap.as_mut() {
            Some((sx, sy)) => {
                sx.to(x as f32);
                sy.to(y as f32);
            }
            None => self.window.set_outer_position(PhysicalPosition::new(x, y)),
        }
        self.clamp_scroll();
        self.needs_paint = true;
    }

    fn clamp_scroll(&mut self) {
        let max = (Self::content_h(&self.rows()) - self.body_rect().3).max(0.0);
        if self.scroll.value() > max {
            self.scroll.to(max);
        }
    }

    /// Places note rows as cards: cascading slide-in, pointer push, edge fades.
    fn cards(&self, rows: &[Row]) -> Vec<Card> {
        let s = self.scale();
        let body = self.body_rect();
        let scroll = self.scroll.value();
        let overflow = Self::content_h(rows) > body.3 + 0.5;
        let fade = 28.0 * s;
        let open = self.open.value();
        let n = rows.len().max(1) as f32;
        let m = self.magnify.value().clamp(0.0, 1.0);
        let py = self.follow.value();
        let width = self.style.width * s;
        let card_headers = self.style.list_headers == ListHeaders::Card;
        let mut cards = Vec::with_capacity(rows.len());
        for (i, row) in rows.iter().enumerate() {
            let (top, h) = match row.kind {
                RowKind::Section { .. } if card_headers => (row.y, row.h - self.style.card_gap * s),
                RowKind::Section { .. } | RowKind::Search => {
                    (row.y + row.h - SECTION_PILL_H * s - 4.0 * s, SECTION_PILL_H * s)
                }
                _ => (row.y, row.h - self.style.card_gap * s),
            };
            let y = body.1 + top - scroll;
            if y + h < body.1 - fade || y > body.1 + body.3 + fade {
                continue;
            }
            let centre = y + h / 2.0;
            let alpha = if overflow {
                ((centre - body.1) / fade).clamp(0.0, 1.0) * ((body.1 + body.3 - centre) / fade).clamp(0.0, 1.0)
            } else {
                1.0
            };
            if alpha <= 0.0 {
                continue;
            }
            let lift = if matches!(row.kind, RowKind::Note(_) | RowKind::NewNote)
                || (card_headers && matches!(row.kind, RowKind::Section { .. }))
            {
                m * falloff(py - centre, REACH * s)
            } else {
                0.0
            };
            let progress = (open * (1.0 + CASCADE * (n - 1.0)) - CASCADE * i as f32).min(open);
            let x = self.column_x(width, progress, lift * self.style.hover_push * s);
            cards.push(Card { rect: (x, y, width, h), row: i, alpha, lift });
        }
        cards
    }

    // ---- icon view --------------------------------------------------------------------------

    /// Notes shown as tiles: on this desktop and in the active profile.
    fn tile_items(&self) -> Vec<usize> {
        let active = self.model.profile().map(|p| p.lists.clone()).unwrap_or_default();
        (0..self.items.len())
            .filter(|&i| !self.items[i].trashed && self.items[i].here && active.contains(&self.items[i].list))
            .collect()
    }

    fn shown_tiles(&self) -> usize {
        let st = &self.style;
        let fit = (self.monitor.1.height as f32 / self.scale() * 0.75
            - 2.0 * self.style.tile_padding
            - SEPARATOR
            - st.tile_size)
            / (st.tile_size + self.style.tile_gap);
        self.tile_items().len().min(fit.max(1.0) as usize)
    }

    /// Lays out the icon strip in "right edge" coordinates, then mirrors for the left edge.
    fn tiles(&self) -> ((f32, f32, f32, f32), Vec<Tile>, f32) {
        let s = self.scale();
        let st = &self.style;
        let (w, h) = self.window_size();
        let notes = self.tile_items();
        let hits: Vec<Hit> =
            notes.iter().take(self.shown_tiles()).map(|&i| Hit::Note(i)).chain(std::iter::once(Hit::New)).collect();
        let gap = self.style.tile_gap * s;
        let total = |sizes: &[f32]| sizes.iter().sum::<f32>() + gap * (sizes.len() as f32 - 1.0) + SEPARATOR * s;
        let base = vec![st.tile_size * s; hits.len()];
        let mut y = (h - total(&base)) / 2.0;
        let mut centres = Vec::with_capacity(hits.len());
        for (i, size) in base.iter().enumerate() {
            if hits[i] == Hit::New {
                y += SEPARATOR * s;
            }
            centres.push(y + size / 2.0);
            y += size + gap;
        }
        let m = self.magnify.value().clamp(0.0, 1.0);
        let py = self.follow.value();
        let sizes: Vec<f32> = centres
            .iter()
            .map(|c| st.tile_size * s * (1.0 + (st.magnify - 1.0) * m * falloff(py - c, REACH * s)))
            .collect();
        let strip_w = (st.tile_size + 2.0 * self.style.tile_padding) * s;
        let slide = (1.0 - self.open.value()) * (strip_w / s + st.edge_gap + 12.0) * s;
        let strip_right = w - st.edge_gap * s + slide;
        let tile_right = strip_right - self.style.tile_padding * s;
        let mut y = (h - total(&sizes)) / 2.0;
        let top = y;
        let mut tiles = Vec::with_capacity(hits.len());
        let mut sep_y = 0.0;
        for (i, size) in sizes.iter().enumerate() {
            if hits[i] == Hit::New {
                sep_y = y + (SEPARATOR * s - gap) / 2.0;
                y += SEPARATOR * s;
            }
            tiles.push(Tile { rect: (tile_right - size, y, *size, *size), hit: hits[i] });
            y += size + gap;
        }
        let mut strip = (
            strip_right - strip_w,
            top - self.style.tile_padding * s,
            strip_w,
            y - gap - top + 2.0 * self.style.tile_padding * s,
        );
        if !self.right() {
            let mirror = |r: (f32, f32, f32, f32)| (w - r.0 - r.2, r.1, r.2, r.3);
            strip = mirror(strip);
            for t in &mut tiles {
                t.rect = mirror(t.rect);
            }
        }
        (strip, tiles, sep_y)
    }

    // ---- input ------------------------------------------------------------------------------

    fn hit(&self, x: f64, y: f64) -> Option<Hit> {
        if !self.want_open || self.open.value() < 0.5 {
            return None;
        }
        let (x, y) = (x as f32, y as f32);
        let s = self.scale();
        if self.icons() {
            let (strip, tiles, _) = self.tiles();
            let half_gap = self.style.tile_gap * s / 2.0;
            return tiles
                .iter()
                .find(|t| {
                    let across = if self.right() {
                        x >= t.rect.0.min(strip.0) - 4.0
                    } else {
                        x <= (t.rect.0 + t.rect.2).max(strip.0 + strip.2) + 4.0
                    };
                    across && y >= t.rect.1 - half_gap && y < t.rect.1 + t.rect.3 + half_gap
                })
                .map(|t| t.hit);
        }
        let body = self.body_rect();
        let rows = self.rows();
        if self.menu {
            if !inside(body, x, y) {
                return None;
            }
            let cy = y - body.1 + self.scroll.value();
            let row = rows.into_iter().find(|r| cy >= r.y && cy < r.y + r.h)?;
            let delete = match row.kind {
                RowKind::Choice { delete: Some(del), .. } => Some(del),
                RowKind::Trashed(i) => Some(Hit::Purge(i)),
                _ => None,
            };
            if let Some(del) = delete.filter(|_| x > body.0 + body.2 - 40.0 * s) {
                return Some(del);
            }
            return row.hit;
        }
        // Cards: the hit area spans the card's band so the push doesn't make them jumpy.
        self.cards(&rows)
            .into_iter()
            .find(|c| {
                let band = (
                    c.rect.0 - self.style.hover_push * s,
                    c.rect.1,
                    c.rect.2 + 2.0 * self.style.hover_push * s,
                    c.rect.3 + self.style.card_gap * s,
                );
                inside(band, x, y)
            })
            .and_then(|c| rows[c.row].hit)
    }

    fn start(&mut self, open: bool) {
        if self.want_open == open {
            return;
        }
        self.want_open = open;
        if open {
            self.rest = false;
            self.pending = Some(DockCmd::Opening);
            if !self.expanded {
                self.expanded = true;
                self.apply_geometry();
            }
            self.open.animate_to(1.0, Feel::Snappy);
        } else {
            self.open.animate_to(0.0, Feel::Smooth);
            self.magnify.to(0.0);
            self.menu = false;
            self.input = None;
        }
    }

    /// On the tab: the whole strip while tucked away, the edge band around it while open.
    fn on_tab(&self, x: f64, y: f64) -> bool {
        if !self.expanded {
            return true;
        }
        let s = self.scale();
        let st = &self.style;
        let w = self.window.inner_size().width as f32;
        let h = self.window.inner_size().height as f32;
        let reach = st.trigger_width.max(st.tab_width + 4.0) * s;
        let band = st.trigger_height.max(st.tab_height) * s;
        let edge = if self.right() { x as f32 >= w - reach } else { (x as f32) <= reach };
        edge && (y as f32 - tab_centre(self.anchor, band, h)).abs() <= band / 2.0
    }

    /// `(x, y)` is over the dock's visible part: the strip or card column, out to the screen
    /// edge. The window is wider than that (label room, hover push, shadow); Windows lets the
    /// pointer through its transparent part, but on macOS it still counts as inside, which kept
    /// the dock out with the pointer inches away from it.
    fn occupied(&self, x: f64, y: f64) -> bool {
        if !self.expanded || self.on_tab(x, y) {
            return true;
        }
        let st = &self.style;
        let content = if self.icons() {
            st.tile_size + 2.0 * st.tile_padding + st.tile_size * (st.magnify - 1.0)
        } else {
            st.width + st.hover_push
        };
        let depth = (st.edge_gap + content) * self.scale();
        let w = self.window.inner_size().width as f32;
        if self.right() {
            x as f32 >= w - depth
        } else {
            x as f32 <= depth
        }
    }

    /// The pointer is no longer over the dock: tuck it away after a moment.
    fn pointer_left(&mut self) -> Option<DockCmd> {
        self.pointer = None;
        self.press = None;
        if self.rest {
            self.left_at = Some(Instant::now());
        }
        self.open_at = None;
        self.magnify.to(0.0);
        if self.want_open && self.input.is_none() {
            self.close_at = Some(Instant::now() + CLOSE_DELAY);
        }
        self.peek_at = None;
        if self.hover.take().is_some() {
            self.needs_paint = true;
            return self.end_peek();
        }
        None
    }

    fn pointer_moved(&mut self, x: f64, y: f64) -> Option<DockCmd> {
        let held = crate::input::left_button_down();
        if !held {
            // The button is up. A press that hasn't moved keeps waiting for its release: macOS
            // reports a move at the same spot just before it, and the release makes the click.
            // A press that moved, and any drag, lost its release (macOS can send it to another
            // window): finish it instead of letting the dock follow the pointer.
            if self.press.is_some_and(|((px, py), _)| (x - px).hypot(y - py) > DRAG_SLOP) {
                self.press = None;
            }
            if let Some((start_y, _, moved)) = self.tab_drag {
                let top = self.window.outer_position().map_or(0, |p| p.y) as f64;
                if moved || (top + y - start_y).abs() > DRAG_SLOP * self.scale() as f64 {
                    self.tab_drag = None;
                    self.window.set_cursor(CursorIcon::Grab);
                    crate::input::show_cursor(CursorIcon::Grab);
                    if moved {
                        return Some(DockCmd::Moved { side: self.style.side, position: self.style.position });
                    }
                }
            }
            if self.drag.is_some() {
                return self.drop_dock();
            }
        }
        // macOS keeps sending moves to a focused window from anywhere on screen; one outside the
        // dock's visible part means the pointer left it.
        let dragging = self.drag.is_some() || self.tab_drag.is_some();
        let over = crate::input::inside(x, y, self.window.inner_size()) && self.occupied(x, y);
        if !dragging && !over {
            return if self.pointer.is_some() { self.pointer_left() } else { None };
        }
        self.pointer = Some((x, y));
        if let Some((start_y, start_pos, moved)) = self.tab_drag {
            // Slide along the edge; the window follows, so work in screen coordinates.
            let top = self.window.outer_position().map_or(0, |p| p.y) as f64;
            let dy = top + y - start_y;
            if moved || dy.abs() > DRAG_SLOP * self.scale() as f64 {
                self.tab_drag = Some((start_y, start_pos, true));
                self.window.set_cursor(CursorIcon::Move);
                crate::input::show_cursor(CursorIcon::Move);
                let height = self.monitor.1.height.max(1) as f32;
                self.style.position = (start_pos + dy as f32 / height).clamp(0.05, 0.95);
                self.apply_geometry();
            }
            return None;
        }
        if held && self.drag.is_none() {
            if let Some(((px, py), _)) = self.press {
                if (x - px).hypot(y - py) > DRAG_SLOP {
                    self.press = None;
                    self.drag = Some((px, py));
                    self.close_at = None;
                    self.hover = None;
                    self.window.set_cursor(CursorIcon::Move);
                    crate::input::show_cursor(CursorIcon::Move);
                }
            }
        }
        if let Some((gx, gy)) = self.drag {
            if let Ok(pos) = self.window.outer_position() {
                let nx = pos.x + (x - gx).round() as i32;
                let ny = pos.y + (y - gy).round() as i32;
                self.window.set_outer_position(PhysicalPosition::new(nx, ny));
            }
            return None;
        }
        self.close_at = None;
        if !self.want_open && !self.rest && self.open_at.is_none() {
            self.open_at = Some(Instant::now() + Duration::from_millis(self.style.open_delay_ms));
        }
        if self.want_open {
            let s = self.scale();
            let over = if self.icons() {
                let (strip, _, _) = self.tiles();
                let reach = self.style.tile_size * (self.style.magnify - 1.0) * s;
                if self.right() {
                    x as f32 >= strip.0 - reach
                } else {
                    (x as f32) <= strip.0 + strip.2 + reach
                }
            } else {
                !self.menu && inside(self.body_rect(), x as f32, y as f32)
            };
            self.magnify.to(if over { 1.0 } else { 0.0 });
            if self.magnify.value() < 0.01 {
                // Start the glide where the pointer entered, not from the top.
                self.follow = Motion::new(y as f32, Feel::Quick).with_epsilon(0.5);
            }
            self.follow.to(y as f32);
        }
        self.needs_paint = true;
        let cmd = self.update_hover(x, y);
        if self.hover.is_none() && self.on_tab(x, y) {
            // The tab can be grabbed and slid along the edge.
            self.window.set_cursor(CursorIcon::Grab);
        }
        cmd
    }

    fn update_hover(&mut self, x: f64, y: f64) -> Option<DockCmd> {
        let hover = self.hit(x, y);
        let cursor = match hover {
            Some(Hit::Input) | None => CursorIcon::Default,
            Some(_) => CursorIcon::Pointer,
        };
        self.window.set_cursor(if self.drag.is_some() { CursorIcon::Move } else { cursor });
        if hover == self.hover {
            return None;
        }
        self.hover = hover;
        self.needs_paint = true;
        if hover != Some(Hit::EmptyTrash) {
            // "Click again" only holds while the pointer stays on it.
            self.confirm_empty = false;
        }
        self.peek_at = match hover {
            Some(Hit::Note(i)) => Some((Instant::now() + PEEK_DELAY, i)),
            _ => None,
        };
        self.end_peek()
    }

    fn end_peek(&mut self) -> Option<DockCmd> {
        std::mem::take(&mut self.peeking).then_some(DockCmd::Peek(None))
    }

    fn click(&mut self, hit: Hit) -> Option<DockCmd> {
        self.needs_paint = true;
        let cmd = match hit {
            Hit::Note(i) => {
                let item = self.items.get(i)?;
                let cmd = if item.trashed { DockCmd::Restore(item.note) } else { DockCmd::Focus(item.id) };
                if self.menu {
                    self.toggle_menu();
                }
                self.hover = None;
                self.start(false);
                cmd
            }
            Hit::OpenTrash | Hit::CloseTrash => {
                self.trash_page = hit == Hit::OpenTrash;
                self.confirm_empty = false;
                self.hover = None;
                self.scroll.snap_to(0.0);
                self.apply_geometry();
                return None;
            }
            Hit::Purge(i) => DockCmd::Purge(self.items.get(i)?.note),
            Hit::EmptyTrash => {
                if !self.confirm_empty {
                    self.confirm_empty = true;
                    return None;
                }
                self.confirm_empty = false;
                DockCmd::EmptyTrash
            }
            Hit::New => {
                self.start(false);
                DockCmd::NewNote
            }
            Hit::SetView(view) => {
                self.view = view;
                self.menu = false;
                self.apply_geometry();
                DockCmd::SetView(view)
            }
            Hit::Section(id) => {
                if !self.collapsed.remove(&id) {
                    self.collapsed.insert(id);
                }
                self.apply_geometry();
                return None;
            }
            Hit::Profile(id) => DockCmd::SelectProfile(id),
            Hit::DeleteProfile(id) => DockCmd::DeleteProfile(id),
            Hit::ListToggle(id) => {
                self.deleting = None;
                let on = self.model.profile().is_some_and(|p| p.lists.contains(&id));
                DockCmd::ToggleList(id, !on)
            }
            // An empty list just goes; one with notes asks what happens to them first.
            Hit::DeleteList(id) if self.notes_in(id) == 0 => DockCmd::DeleteList { id, trash_notes: false },
            Hit::DeleteList(id) => {
                self.deleting = Some(id);
                self.apply_geometry();
                return None;
            }
            Hit::KeepNotes(id) | Hit::TrashNotes(id) => {
                self.deleting = None;
                self.hover = None;
                DockCmd::DeleteList { id, trash_notes: matches!(hit, Hit::TrashNotes(_)) }
            }
            Hit::CancelDelete => {
                self.deleting = None;
                self.hover = None;
                self.apply_geometry();
                return None;
            }
            Hit::Settings => {
                self.toggle_menu();
                self.start(false);
                DockCmd::OpenSettings
            }
            Hit::Quit => DockCmd::Quit,
            Hit::AddProfile | Hit::AddList => {
                self.deleting = None;
                let kind = if hit == Hit::AddProfile { InputKind::Profile } else { InputKind::List };
                self.input = Some((kind, String::new()));
                // Typing needs keyboard focus; the dock never takes it otherwise.
                self.window.focus_window();
                self.apply_geometry();
                return None;
            }
            Hit::Input => return None,
        };
        Some(cmd)
    }

    /// Drops a dragged dock: nearest left/right edge of the monitor under the pointer.
    fn drop_dock(&mut self) -> Option<DockCmd> {
        let (gx, gy) = self.drag.take()?;
        self.window.set_cursor(CursorIcon::Default);
        crate::input::show_cursor(CursorIcon::Default);
        let pos = self.window.outer_position().ok()?;
        let (px, py) = (pos.x + gx as i32, pos.y + gy as i32);
        if let Some(m) = self.window.available_monitors().find(|m| {
            let (p, s) = (m.position(), m.size());
            px >= p.x && px < p.x + s.width as i32 && py >= p.y && py < p.y + s.height as i32
        }) {
            self.monitor = (m.position(), m.size(), m.scale_factor());
        }
        let (mp, ms, _) = self.monitor;
        let side = if px < mp.x + ms.width as i32 / 2 { Side::Left } else { Side::Right };
        let position = ((py - mp.y) as f32 / ms.height as f32).clamp(0.08, 0.92);
        self.style.side = side;
        self.style.position = position;
        let s = |v: i32| Motion::new(v as f32, Feel::Snappy).with_epsilon(0.5);
        self.snap = Some((s(pos.x), s(pos.y)));
        self.apply_geometry();
        Some(DockCmd::Moved { side, position })
    }

    /// Keyboard navigation (when the dock has focus): ↑/↓ select, Enter opens, typing filters.
    fn navigate(&mut self, key: &Key, text: Option<&str>) -> Option<DockCmd> {
        let notes: Vec<(usize, f32, f32)> = self
            .rows()
            .iter()
            .filter_map(|r| match r.kind {
                RowKind::Note(i) => Some((i, r.y, r.h)),
                _ => None,
            })
            .collect();
        let current = match self.hover {
            Some(Hit::Note(i)) => notes.iter().position(|n| n.0 == i),
            _ => None,
        };
        self.needs_paint = true;
        let select = |dock: &mut Dock, k: usize| -> Option<DockCmd> {
            let (i, y, h) = *notes.get(k)?;
            // Bring it into view and lift it, as if the pointer were on it.
            let body = dock.body_rect();
            let scroll = dock.scroll.value();
            if y < scroll {
                dock.scroll.to(y);
            } else if y + h > scroll + body.3 {
                dock.scroll.to(y + h - body.3);
            }
            let target_scroll = dock.scroll.target_value();
            dock.follow.to(body.1 + y - target_scroll + h / 2.0);
            dock.magnify.to(1.0);
            dock.hover = Some(Hit::Note(i));
            dock.peek_at = None;
            dock.peeking = true;
            Some(DockCmd::Peek(dock.items.get(i).map(|it| it.id)))
        };
        match key {
            Key::Named(NamedKey::ArrowDown) => {
                select(self, current.map_or(0, |c| (c + 1).min(notes.len().saturating_sub(1))))
            }
            Key::Named(NamedKey::ArrowUp) => select(self, current.map_or(0, |c| c.saturating_sub(1))),
            Key::Named(NamedKey::Enter) => {
                let k = current.unwrap_or(0);
                let i = notes.get(k)?.0;
                self.click(Hit::Note(i))
            }
            Key::Named(NamedKey::Escape) if self.menu => {
                self.toggle_menu();
                None
            }
            Key::Named(NamedKey::Escape) => {
                if self.query.is_empty() {
                    self.start(false);
                } else {
                    self.query.clear();
                    self.apply_geometry();
                }
                None
            }
            Key::Named(NamedKey::Backspace) => {
                self.query.pop();
                self.apply_geometry();
                None
            }
            _ => {
                let t = text.filter(|t| t.chars().all(|c| !c.is_control()))?;
                self.query.push_str(t);
                self.scroll.to(0.0);
                self.apply_geometry();
                None
            }
        }
    }

    fn toggle_menu(&mut self) {
        self.menu = !self.menu;
        self.trash_page = false;
        self.deleting = None;
        self.input = None;
        self.hover = None;
        self.scroll.snap_to(0.0);
        self.apply_geometry();
    }

    fn key(&mut self, key: &Key, text: Option<&str>) -> Option<DockCmd> {
        if self.input.is_none() {
            return if self.want_open { self.navigate(key, text) } else { None };
        }
        let (kind, name) = self.input.as_mut()?;
        self.needs_paint = true;
        match key {
            Key::Named(NamedKey::Enter) => {
                let name = name.trim().to_string();
                let kind = *kind;
                self.input = None;
                self.apply_geometry();
                if name.is_empty() {
                    return None;
                }
                return Some(match kind {
                    InputKind::Profile => DockCmd::NewProfile(name),
                    InputKind::List => DockCmd::NewList(name),
                });
            }
            Key::Named(NamedKey::Escape) => {
                self.input = None;
                self.apply_geometry();
            }
            Key::Named(NamedKey::Backspace) => {
                name.pop();
            }
            _ => {
                if let Some(t) = text.filter(|t| t.chars().all(|c| !c.is_control())) {
                    if name.chars().count() < 40 {
                        name.push_str(t);
                    }
                }
            }
        }
        None
    }

    pub fn handle(&mut self, event: &WindowEvent) -> Option<DockCmd> {
        if crate::input::debug() && !matches!(event, WindowEvent::RedrawRequested) {
            eprintln!(
                "dock: {event:?} | open={} expanded={} press={} drag={} tab_drag={} button_down={}",
                self.want_open,
                self.expanded,
                self.press.is_some(),
                self.drag.is_some(),
                self.tab_drag.is_some(),
                crate::input::left_button_down()
            );
        }
        match event {
            // Painting is paced by the app; an OS repaint request just marks the dock dirty.
            WindowEvent::RedrawRequested => self.needs_paint = true,
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. }
                if self.pointer.is_some_and(|(x, y)| self.on_tab(x, y)) =>
            {
                let (_, y) = self.pointer?;
                let top = self.window.outer_position().map_or(0, |p| p.y) as f64;
                self.tab_drag = Some((top + y, self.style.position, false));
                self.open_at = None;
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. }
                if self.tab_drag.is_some() =>
            {
                let (_, _, moved) = self.tab_drag.take()?;
                self.window.set_cursor(CursorIcon::Grab);
                crate::input::show_cursor(CursorIcon::Grab);
                if moved {
                    return Some(DockCmd::Moved { side: self.style.side, position: self.style.position });
                }
                // A click on the tab opens (or tucks away) the dock.
                self.start(!self.want_open);
                self.rest = !self.want_open;
                self.left_at = None;
            }
            WindowEvent::ThemeChanged(theme) => {
                self.dark = crate::theme::is_dark(Some(*theme));
                self.needs_paint = true;
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.monitor.2 = *scale_factor;
                self.apply_geometry();
            }
            WindowEvent::CursorEntered { .. } => {
                self.close_at = None;
                if self.rest && self.left_at.is_some_and(|t| t.elapsed() >= SETTLE) {
                    self.rest = false;
                }
                if !self.want_open && !self.rest && self.open_at.is_none() {
                    self.open_at = Some(Instant::now() + Duration::from_millis(self.style.open_delay_ms));
                }
            }
            WindowEvent::CursorMoved { position, .. } => return self.pointer_moved(position.x, position.y),
            // A drag carries on outside the window while the button is held. Without it the
            // drag lost its release, so it ends here and the pointer has left.
            WindowEvent::CursorLeft { .. }
                if (self.drag.is_some() || self.tab_drag.is_some()) && crate::input::left_button_down() => {}
            WindowEvent::CursorLeft { .. } => {
                let slid = self.tab_drag.take().is_some_and(|(_, _, moved)| moved);
                let dropped = if self.drag.is_some() { self.drop_dock() } else { None };
                let left = self.pointer_left();
                let moved = || DockCmd::Moved { side: self.style.side, position: self.style.position };
                return dropped.or_else(|| slid.then(moved)).or(left);
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. }
                if self.drag.is_some() =>
            {
                return self.drop_dock();
            }
            WindowEvent::Focused(false) if !self.query.is_empty() => {
                self.query.clear();
                self.apply_geometry();
            }
            WindowEvent::Focused(false) if self.input.is_some() => {
                self.input = None;
                self.apply_geometry();
                if self.pointer.is_none() {
                    self.close_at = Some(Instant::now() + CLOSE_DELAY);
                }
            }
            WindowEvent::KeyboardInput { event, is_synthetic: false, .. } if event.state == ElementState::Pressed => {
                return self.key(&event.logical_key, event.text.as_deref());
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let (x, y) = self.pointer?;
                self.press = Some(((x, y), self.hit(x, y)));
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                let (_, hit) = self.press.take()?;
                // A press that never moved far is a click on whatever was under it.
                let (x, y) = self.pointer?;
                if hit.is_some() && hit == self.hit(x, y) {
                    return self.click(hit?);
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => {
                if self.want_open && self.input.is_none() {
                    self.toggle_menu();
                }
            }
            WindowEvent::MouseWheel { delta, .. } if crate::input::resize_modifier_held() => {
                let up = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y > 0.0,
                    MouseScrollDelta::PixelDelta(p) => p.y > 0.0,
                };
                return self.resize_by(if up { 1.0 } else { -1.0 });
            }
            WindowEvent::MouseWheel { delta, .. } if !self.icons() => {
                let s = self.scale();
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 3.0 * self.note_h() * s,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                let max = (Self::content_h(&self.rows()) - self.body_rect().3).max(0.0);
                let target = (self.scroll.value() - dy).clamp(0.0, max);
                self.scroll.to(target);
                self.needs_paint = true;
            }
            _ => {}
        }
        None
    }

    /// Advances timers and springs. Returns when the dock next needs to wake up.
    pub fn tick(&mut self, now: Instant) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        let mut wake = |t: Instant| next = Some(next.map_or(t, |n: Instant| n.min(t)));
        if let Some(at) = self.open_at {
            if now >= at {
                self.open_at = None;
                self.start(true);
            } else {
                wake(at);
            }
        }
        if let Some(at) = self.close_at {
            if now >= at {
                self.close_at = None;
                self.start(false);
            } else {
                wake(at);
            }
        }
        let mut animating = false;
        for m in [&mut self.open, &mut self.magnify, &mut self.follow, &mut self.scroll] {
            let before = m.value();
            if m.tick(now) {
                wake(now + FRAME);
            }
            if m.value() != before {
                self.needs_paint = true;
                animating = true;
            }
        }
        if let Some((at, i)) = self.peek_at {
            if now >= at {
                self.peek_at = None;
                // Trashed notes have no window to float.
                if self.hover == Some(Hit::Note(i)) && self.items.get(i).is_some_and(|it| !it.trashed) {
                    self.peeking = true;
                    self.pending = Some(DockCmd::Peek(self.items.get(i).map(|it| it.id)));
                }
            } else {
                wake(at);
            }
        }
        if let Some((sx, sy)) = self.snap.as_mut() {
            let moving = sx.tick(now) | sy.tick(now);
            let pos = PhysicalPosition::new(sx.value().round() as i32, sy.value().round() as i32);
            self.window.set_outer_position(pos);
            if moving {
                wake(now + FRAME);
            } else {
                self.snap = None;
            }
        }
        // Content slides and moves under a pointer that may be still: re-hit-test.
        if let Some((x, y)) = self.pointer.filter(|_| animating && self.want_open && self.drag.is_none()) {
            if let Some(cmd) = self.update_hover(x, y) {
                if self.pending.is_none() {
                    self.pending = Some(cmd);
                }
            }
        }
        if !self.want_open && self.expanded && !self.open.moving() {
            self.expanded = false;
            self.apply_geometry();
        }
        next
    }

    // ---- painting ---------------------------------------------------------------------------

    pub fn render(&mut self, shared: &mut Shared) {
        self.needs_paint = false;
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        if self.icons() {
            self.render_icons(shared);
        } else {
            self.render_list(shared);
        }
    }

    fn render_list(&mut self, shared: &mut Shared) {
        let size = self.window.inner_size();
        let (width, height) = (size.width, size.height);
        let (w, h) = (width as f32, height as f32);
        let s = self.scale();
        let st = self.style.clone();
        let pal = self.pal();
        let open = self.open.value().max(0.0);
        let expanded = self.expanded;
        let dark = self.dark;
        let bg = self.card_bg();
        let hover = self.hover;
        let fonts = &mut shared.fonts;

        let body = self.body_rect();
        let rows = self.rows();
        let scroll = self.scroll.value();
        let body_clip = Clip { x0: 0, y0: body.1 as i32, x1: width as i32, y1: (body.1 + body.3) as i32 };

        // Everything to draw: (shadowed card rect, alpha), vector shapes, text.
        let mut cards: Vec<(Rect, f32, f32, bool)> = Vec::new(); // rect, alpha, radius, hovered
        let mut shapes: Vec<(Shape, Rect, f32)> = Vec::new();
        let mut labels: Vec<Label> = Vec::new();
        let mut tiles: Vec<(Rect, Rgb, f32)> = Vec::new(); // icon tiles: rect, wash, alpha
        let mut glows: Vec<(Rect, f32)> = Vec::new(); // soft card-coloured backing for desktop text

        if expanded {
            if self.menu {
                // The right-click menu is a single panel card.
                let panel = (body.0, body.1 - MENU_PAD * s, body.2, body.3 + 2.0 * MENU_PAD * s);
                cards.push((panel, 1.0, st.radius * s, false));
                self.menu_contents(fonts, &rows, body, scroll, &mut shapes, &mut labels, body_clip);
            } else {
                for card in self.cards(&rows) {
                    let row = &rows[card.row];
                    let (x, y, cw, ch) = card.rect;
                    let a = card.alpha;
                    let hovered = row.hit.is_some() && row.hit == hover;
                    match &row.kind {
                        RowKind::Section { title, count, collapsed } => {
                            let caps = title.to_uppercase();
                            match st.list_headers {
                                ListHeaders::Pill | ListHeaders::None => {
                                    let t = small_label(fonts, &caps, 10.5 * s, Weight::SEMIBOLD);
                                    let c = small_label(fonts, &count.to_string(), 10.5 * s, Weight::MEDIUM);
                                    let (tw, nw) = (label_width(&t), label_width(&c));
                                    let pw = tw + nw + 40.0 * s;
                                    let px = if self.right() { x + cw - pw } else { x };
                                    cards.push(((px, y, pw, ch), a * 0.92, ch / 2.0, hovered));
                                    let ty = y + (ch - line_h(&t)) / 2.0;
                                    labels.push((
                                        t,
                                        pal.secondary.text((255.0 * a) as u8),
                                        (px + 10.0 * s, ty),
                                        body_clip,
                                    ));
                                    let cx = px + 14.0 * s + tw;
                                    labels.push((c, pal.secondary.text((170.0 * a) as u8), (cx, ty), body_clip));
                                    let chevron = Shape::Disclosure(*collapsed, pal.secondary);
                                    shapes.push((chevron, (cx + nw + 4.0 * s, y, 10.0 * s, ch), a));
                                }
                                ListHeaders::Card => {
                                    // Laid out like a note: a tile (holding the count), the name,
                                    // and a chevron where a note's extras go.
                                    let radius = (st.radius * s).min(ch / 2.0);
                                    cards.push((card.rect, a, radius, hovered));
                                    let base = (ch - 2.0 * 6.0 * s).clamp(18.0 * s, (st.icon_size + 14.0) * s);
                                    let tile = base * (1.0 + 0.14 * card.lift);
                                    let tx = x + 6.0 * s + (base - tile) / 2.0;
                                    let ty = y + (ch - tile) / 2.0;
                                    tiles.push(((tx, ty, tile, tile), pal.secondary, a * 0.16));
                                    let n =
                                        small_label(fonts, &count.to_string(), (base * 0.42).round(), Weight::SEMIBOLD);
                                    let at = (tx + (tile - label_width(&n)) / 2.0, ty + (tile - line_h(&n)) / 2.0);
                                    labels.push((n, pal.secondary.text((255.0 * a) as u8), at, body_clip));
                                    let text_x = x + 6.0 * s + base + 10.0 * s;
                                    let chevron_x = x + cw - 22.0 * s;
                                    let room = chevron_x - 8.0 * s - text_x;
                                    let t = ellipsized(fonts, title, st.font_size * s, Weight::SEMIBOLD, room);
                                    let at = (text_x, y + (ch - line_h(&t)) / 2.0);
                                    labels.push((t, pal.text.text((255.0 * a) as u8), at, body_clip));
                                    let chevron = Shape::Disclosure(*collapsed, pal.secondary);
                                    shapes.push((chevron, (chevron_x, y, 10.0 * s, ch), a));
                                }
                                ListHeaders::Caption | ListHeaders::Line => {
                                    // Text on the desktop itself, over a soft glow of the card colour
                                    // so it reads in either theme on any wallpaper.
                                    let line = st.list_headers == ListHeaders::Line;
                                    let t = small_label(fonts, &caps, 10.5 * s, Weight::SEMIBOLD);
                                    let c = small_label(fonts, &count.to_string(), 10.5 * s, Weight::MEDIUM);
                                    let (tw, nw, th) = (label_width(&t), label_width(&c), line_h(&t));
                                    let tx = x + 12.0 * s;
                                    let ty = y + (ch - th) / 2.0;
                                    let strong = if hovered { 255.0 } else { 225.0 };
                                    labels.push((t, pal.secondary.text((strong * a) as u8), (tx, ty), body_clip));
                                    let cx = tx + tw + 6.0 * s;
                                    labels.push((c, pal.secondary.text((strong * 0.7 * a) as u8), (cx, ty), body_clip));
                                    let mut end = cx + nw;
                                    let chevron = Shape::Disclosure(*collapsed, pal.secondary);
                                    if line {
                                        // The rule runs to the chevron at the column's far end.
                                        let chevron_x = x + cw - 22.0 * s;
                                        shapes.push((
                                            Shape::Rule,
                                            (end + 10.0 * s, y, chevron_x - end - 16.0 * s, ch),
                                            a,
                                        ));
                                        shapes.push((chevron, (chevron_x, y, 10.0 * s, ch), a));
                                        glows.push(((chevron_x - 4.0 * s, ty, 18.0 * s, th), a));
                                    } else if hovered || *collapsed {
                                        // Only when it says something: under the pointer, or folded away.
                                        shapes.push((chevron, (end + 4.0 * s, y, 10.0 * s, ch), a));
                                        end += 14.0 * s;
                                    }
                                    glows.push(((tx - 6.0 * s, ty, end - tx + 12.0 * s, th), a));
                                }
                            }
                        }
                        RowKind::Note(i) => {
                            let item = &self.items[*i];
                            let radius = (st.radius * s).min(ch / 2.0);
                            cards.push((card.rect, a, radius, hovered));
                            // Icon tile (the square in the sketch), swelling a little under the pointer.
                            let base = (ch - 2.0 * 6.0 * s).clamp(18.0 * s, (st.icon_size + 14.0) * s);
                            let tile = base * (1.0 + 0.14 * card.lift);
                            let tx = x + 6.0 * s + (base - tile) / 2.0;
                            let ty = y + (ch - tile) / 2.0;
                            let (accent, wash) = theme::tint(item.tint, dark);
                            tiles.push(((tx, ty, tile, tile), wash, if item.hidden { a * 0.5 } else { a }));
                            let glyph = if item.icon.is_empty() {
                                let initial: String = item.title.chars().next().unwrap_or('•').to_uppercase().collect();
                                (small_label(fonts, &initial, (base * 0.46).round(), Weight::SEMIBOLD), accent)
                            } else {
                                (small_label(fonts, &item.icon, (base * 0.56).round(), Weight::NORMAL), pal.text)
                            };
                            let at = (tx + (tile - label_width(&glyph.0)) / 2.0, ty + (tile - line_h(&glyph.0)) / 2.0);
                            labels.push((glyph.0, glyph.1.text((255.0 * a) as u8), at, body_clip));

                            let text_x = x + 6.0 * s + base + 10.0 * s;
                            let tag = item.hidden.then(|| small_label(fonts, "hidden", 10.5 * s, Weight::MEDIUM));
                            let tag_w = tag.as_ref().map_or(0.0, |t| label_width(t) + 10.0 * s);
                            let text_w =
                                x + cw - 12.0 * s - text_x - tag_w - if item.reminder { 12.0 * s } else { 0.0 };
                            if let Some(t) = tag {
                                let at = (x + cw - 12.0 * s - label_width(&t), y + (ch - line_h(&t)) / 2.0);
                                labels.push((t, pal.secondary.text((200.0 * a) as u8), at, body_clip));
                            }
                            let dim = if item.hidden {
                                0.5
                            } else if item.here {
                                1.0
                            } else {
                                0.65
                            };
                            let title = ellipsized(fonts, &item.title, st.font_size * s, Weight::MEDIUM, text_w);
                            let detailed = self.view == View::Detailed && !item.sub.is_empty();
                            let title_h = line_h(&title);
                            let sub_h = st.font_size * 0.86 * 1.3 * s;
                            let block = if detailed { title_h + sub_h } else { title_h };
                            let top = y + (ch - block) / 2.0;
                            labels.push((title, pal.text.text((255.0 * a * dim) as u8), (text_x, top), body_clip));
                            if detailed {
                                let sub = ellipsized(fonts, &item.sub, st.font_size * 0.86 * s, Weight::NORMAL, text_w);
                                labels.push((
                                    sub,
                                    pal.secondary.text((255.0 * a * dim) as u8),
                                    (text_x, top + title_h),
                                    body_clip,
                                ));
                            }
                            if item.reminder {
                                shapes.push((
                                    Shape::Badge(pal.alert),
                                    (x + cw - 16.0 * s, y + ch / 2.0 - 3.5 * s, 7.0 * s, 7.0 * s),
                                    a,
                                ));
                            }
                        }
                        RowKind::Empty(text) => {
                            cards.push((card.rect, a, (st.radius * s).min(ch / 2.0), false));
                            let t = small_label(fonts, text, st.font_size * s, Weight::NORMAL);
                            let at = (x + 14.0 * s, y + (ch - line_h(&t)) / 2.0);
                            labels.push((t, pal.secondary.text((255.0 * a) as u8), at, body_clip));
                        }
                        RowKind::NewNote => {
                            // Quieter than a note: translucent, with a "+" where the icon goes.
                            cards.push((card.rect, a * 0.82, (st.radius * s).min(ch / 2.0), hovered));
                            let base = (ch - 2.0 * 6.0 * s).clamp(18.0 * s, (st.icon_size + 14.0) * s);
                            let tile = (x + 6.0 * s, y + (ch - base) / 2.0, base, base);
                            shapes.push((Shape::PlusTile, tile, a));
                            let t = small_label(fonts, "New note", st.font_size * s, Weight::MEDIUM);
                            let at = (tile.0 + base + 10.0 * s, y + (ch - line_h(&t)) / 2.0);
                            labels.push((t, pal.secondary.text((255.0 * a) as u8), at, body_clip));
                        }
                        RowKind::Search => {
                            let t = small_label(fonts, &format!("Search “{}”", self.query), 11.5 * s, Weight::SEMIBOLD);
                            let pw = label_width(&t) + 24.0 * s;
                            let px = if self.right() { x + cw - pw } else { x };
                            cards.push(((px, y, pw, ch), a, ch / 2.0, false));
                            let at = (px + 12.0 * s, y + (ch - line_h(&t)) / 2.0);
                            labels.push((t, pal.selection.0.text((255.0 * a) as u8), at, body_clip));
                        }
                        _ => {}
                    }
                }
            }
        }

        let swash = &mut shared.swash;
        let tab = TabParams::of(self);
        self.surface.paint(width, height, 255, |data| {
            if expanded && surface::ALPHA && open > 0.0 {
                let base = pal.shadow as f32 / 255.0 * open.min(1.0) * 0.8;
                for &(r, a, radius, hovered) in &cards {
                    let k = if hovered { 1.4 } else { 1.0 };
                    let hole = (r.0 + 2.0 * s, r.1 + radius, r.2 - 4.0 * s, r.3 - 2.0 * radius);
                    let layers = [(9.0 * s, 4.0 * s, base * a * k), (1.5 * s, 1.0 * s, base * a * 0.6)];
                    gfx::shadow(data, width, height, r, &layers, Some(hole));
                }
            }
            {
                let Some(mut pm) = PixmapMut::from_bytes(data, width, height) else { return };
                if expanded && open > 0.0 {
                    for &((x, y, cw, ch), a, radius, hovered) in &cards {
                        if let Some(p) = gfx::squircle(x, y, cw, ch, radius) {
                            let alpha = if hovered { 255.0 } else { st.opacity as f32 };
                            gfx::fill_path(&mut pm, &p, bg, (alpha * a) as u8);
                            let (edge, ea) = if dark { (theme::WHITE, 26.0) } else { (pal.text, 20.0) };
                            gfx::stroke_path(&mut pm, &p, 1.0 * s, edge, (ea * a) as u8);
                        }
                    }
                    // A blur, faked with a few widening translucent layers.
                    for &((gx, gy, gw, gh), a) in &glows {
                        for k in (0..4).rev() {
                            let e = k as f32 * 2.5 * s;
                            let r = gh / 2.0 + e;
                            if let Some(p) = gfx::rounded_rect(gx - e, gy - e, gw + 2.0 * e, gh + 2.0 * e, r) {
                                gfx::fill_path(&mut pm, &p, bg, (st.opacity as f32 * 0.3 * a) as u8);
                            }
                        }
                    }
                    for &((x, y, tw, th), wash, a) in &tiles {
                        if let Some(p) = gfx::squircle(x, y, tw, th, tw * 0.26) {
                            gfx::fill_path(&mut pm, &p, wash, (255.0 * a) as u8);
                        }
                    }
                    for &(shape, r, a) in &shapes {
                        shape.draw(&mut pm, r, s, &pal, bg, a);
                    }
                }
                tab.draw(&mut pm, w, h);
            }
            for (mut b, color, (x, y), clip) in labels {
                gfx::draw_text(&mut b, fonts, swash, color, data, width, clip, (x.round() as i32, y.round() as i32));
            }
        });
    }

    /// The profile menu's rows (inside the panel card).
    #[allow(clippy::too_many_arguments)]
    fn menu_contents(
        &self,
        fonts: &mut FontSystem,
        rows: &[Row],
        body: (f32, f32, f32, f32),
        scroll: f32,
        shapes: &mut Vec<(Shape, Rect, f32)>,
        labels: &mut Vec<Label>,
        clip: Clip,
    ) {
        let s = self.scale();
        let st = &self.style;
        let pal = self.pal();
        let hover = self.hover;
        let text_x = body.0 + 14.0 * s;
        let right = body.0 + body.2 - 12.0 * s;
        for row in rows {
            let y = body.1 + row.y - scroll;
            if y + row.h < body.1 || y > body.1 + body.3 {
                continue;
            }
            let r = (body.0 + 6.0 * s, y, body.2 - 12.0 * s, row.h);
            let hovered = row.hit.is_some() && row.hit == hover;
            match &row.kind {
                RowKind::Caption(text) => {
                    let t = small_label(fonts, text, 10.5 * s, Weight::SEMIBOLD);
                    let ty = y + row.h - line_h(&t) - 4.0 * s;
                    labels.push((t, pal.secondary.text(255), (text_x, ty), clip));
                }
                RowKind::Choice { label, on, radio, delete } => {
                    if hovered || (delete.is_some() && *delete == hover) {
                        shapes.push((Shape::Hover, r, 1.0));
                    }
                    let bx = (text_x, y + (row.h - 14.0 * s) / 2.0, 14.0 * s, 14.0 * s);
                    shapes.push((if *radio { Shape::Radio(*on) } else { Shape::Check(*on) }, bx, 1.0));
                    let weight = if *on { Weight::MEDIUM } else { Weight::NORMAL };
                    let t = ellipsized(fonts, label, st.font_size * s, weight, right - text_x - 60.0 * s);
                    let ty = y + (row.h - line_h(&t)) / 2.0;
                    labels.push((t, pal.text.text(255), (text_x + 24.0 * s, ty), clip));
                    if let Some(del) = delete {
                        if hovered || Some(*del) == hover {
                            let dr = (right - 22.0 * s, y + (row.h - 20.0 * s) / 2.0, 20.0 * s, 20.0 * s);
                            shapes.push((Shape::Delete(Some(*del) == hover), dr, 1.0));
                        }
                    }
                }
                RowKind::Add(text) => {
                    if hovered {
                        shapes.push((Shape::Hover, r, 1.0));
                    }
                    shapes.push((Shape::SmallPlus, (text_x, y + (row.h - 14.0 * s) / 2.0, 14.0 * s, 14.0 * s), 1.0));
                    let t = small_label(fonts, text, st.font_size * s, Weight::NORMAL);
                    let ty = y + (row.h - line_h(&t)) / 2.0;
                    labels.push((t, pal.secondary.text(255), (text_x + 24.0 * s, ty), clip));
                }
                RowKind::TrashLink(count) => {
                    if hovered {
                        shapes.push((Shape::Hover, r, 1.0));
                    }
                    let icon = small_label(fonts, "🗑", 11.0 * s, Weight::NORMAL);
                    let iy = y + (row.h - line_h(&icon)) / 2.0;
                    labels.push((icon, pal.secondary.text(255), (text_x, iy), clip));
                    let t = small_label(fonts, "Trash", st.font_size * s, Weight::NORMAL);
                    let ty = y + (row.h - line_h(&t)) / 2.0;
                    labels.push((t, pal.secondary.text(255), (text_x + 24.0 * s, ty), clip));
                    if *count > 0 {
                        let c = small_label(fonts, &count.to_string(), 10.5 * s, Weight::MEDIUM);
                        let at = (right - 18.0 * s - label_width(&c), y + (row.h - line_h(&c)) / 2.0);
                        labels.push((c, pal.secondary.text(170), at, clip));
                    }
                    shapes.push((Shape::Disclosure(true, pal.secondary), (right - 12.0 * s, y, 10.0 * s, row.h), 1.0));
                }
                RowKind::Prompt(text) => {
                    let t = ellipsized(fonts, text, 11.5 * s, Weight::MEDIUM, right - text_x - 24.0 * s);
                    let ty = y + (row.h - line_h(&t)) / 2.0;
                    labels.push((t, pal.secondary.text(255), (text_x + 24.0 * s, ty), clip));
                }
                RowKind::Action { label, danger } => {
                    if hovered {
                        shapes.push((Shape::Hover, r, 1.0));
                    }
                    let color = if *danger {
                        pal.alert
                    } else if row.hit == Some(Hit::CancelDelete) {
                        pal.secondary
                    } else {
                        pal.text
                    };
                    let t = ellipsized(fonts, label, st.font_size * s, Weight::MEDIUM, right - text_x - 24.0 * s);
                    let ty = y + (row.h - line_h(&t)) / 2.0;
                    labels.push((t, color.text(if hovered { 255 } else { 220 }), (text_x + 24.0 * s, ty), clip));
                }
                RowKind::Back => {
                    if hovered {
                        shapes.push((Shape::Hover, r, 1.0));
                    }
                    let t = small_label(fonts, "‹  Trash", st.font_size * s, Weight::SEMIBOLD);
                    let ty = y + (row.h - line_h(&t)) / 2.0;
                    labels.push((t, pal.text.text(255), (text_x, ty), clip));
                }
                RowKind::Trashed(i) => {
                    let item = &self.items[*i];
                    let purge = hover == Some(Hit::Purge(*i));
                    if hovered || purge {
                        shapes.push((Shape::Hover, r, 1.0));
                    }
                    let glyph = if item.icon.is_empty() {
                        item.title.chars().next().unwrap_or('•').to_uppercase().collect()
                    } else {
                        item.icon.clone()
                    };
                    let g = small_label(fonts, &glyph, 12.0 * s, Weight::SEMIBOLD);
                    let (accent, _) = theme::tint(item.tint, self.dark);
                    let gx = text_x + (16.0 * s - label_width(&g)) / 2.0;
                    let gy = y + (row.h - line_h(&g)) / 2.0;
                    labels.push((g, accent.text(255), (gx, gy), clip));
                    let t = ellipsized(fonts, &item.title, st.font_size * s, Weight::NORMAL, right - text_x - 60.0 * s);
                    let ty = y + (row.h - line_h(&t)) / 2.0;
                    labels.push((t, pal.text.text(if hovered { 255 } else { 200 }), (text_x + 24.0 * s, ty), clip));
                    if hovered || purge {
                        let dr = (right - 22.0 * s, y + (row.h - 20.0 * s) / 2.0, 20.0 * s, 20.0 * s);
                        shapes.push((Shape::Delete(purge), dr, 1.0));
                    }
                }
                RowKind::EmptyTrash => {
                    if hovered {
                        shapes.push((Shape::Hover, r, 1.0));
                    }
                    let text = if self.confirm_empty { "Click again to delete them for good" } else { "Empty trash" };
                    let t = small_label(fonts, text, st.font_size * s, Weight::MEDIUM);
                    let ty = y + (row.h - line_h(&t)) / 2.0;
                    labels.push((
                        t,
                        pal.alert.text(if hovered || self.confirm_empty { 255 } else { 200 }),
                        (text_x, ty),
                        clip,
                    ));
                }
                RowKind::Input(placeholder) => {
                    let field = (r.0 + 4.0 * s, y + 3.0 * s, r.2 - 8.0 * s, row.h - 6.0 * s);
                    shapes.push((Shape::Field, field, 1.0));
                    let value = self.input.as_ref().map_or("", |(_, v)| v.as_str());
                    let (text, color) = if value.is_empty() {
                        (*placeholder, pal.secondary.text(200))
                    } else {
                        (value, pal.text.text(255))
                    };
                    let t = small_label(fonts, text, st.font_size * s, Weight::NORMAL);
                    let ty = field.1 + (field.3 - line_h(&t)) / 2.0;
                    let caret_x = field.0 + 10.0 * s + if value.is_empty() { 0.0 } else { label_width(&t) };
                    labels.push((t, color, (field.0 + 10.0 * s, ty), clip));
                    shapes.push((Shape::Caret, (caret_x, field.1 + 6.0 * s, 1.5 * s, field.3 - 12.0 * s), 1.0));
                }
                _ => {}
            }
        }
    }

    fn render_icons(&mut self, shared: &mut Shared) {
        let size = self.window.inner_size();
        let (width, height) = (size.width, size.height);
        let (w, h) = (width as f32, height as f32);
        let s = self.scale();
        let pal = self.pal();
        let open = self.open.value().max(0.0);
        let (strip, tiles, sep_y) = self.tiles();
        let fonts = &mut shared.fonts;
        let right = self.right();

        let mut labels: Vec<(Buffer, cosmic_text::Color, (i32, i32))> = Vec::new();
        let mut faces = Vec::new();
        let mut bubble = None;
        if self.expanded {
            for t in &tiles {
                let (x, y, size, _) = t.rect;
                let Hit::Note(i) = t.hit else { continue };
                let Some(item) = self.items.get(i) else { continue };
                let (accent, wash) = theme::tint(item.tint, self.dark);
                // Glyph sizes snap to 2px steps so magnification reuses cached shaping.
                let q = |v: f32| (v / 2.0).round() * 2.0;
                let b = if item.icon.is_empty() {
                    let initial: String = item.title.chars().next().unwrap_or('•').to_uppercase().collect();
                    small_label(fonts, &initial, q(size * 0.44), Weight::SEMIBOLD)
                } else {
                    small_label(fonts, &item.icon, q(size * 0.5), Weight::NORMAL)
                };
                let at = (
                    (x + (size - label_width(&b)) / 2.0).round() as i32,
                    (y + (size - line_h(&b)) / 2.0).round() as i32,
                );
                labels.push((b, accent.text(255), at));
                faces.push((t.rect, wash, item.reminder));
                if self.hover == Some(t.hit) {
                    let title = ellipsized(fonts, &item.title, 13.0 * s, Weight::SEMIBOLD, 180.0 * s);
                    let sub = (!item.sub.is_empty())
                        .then(|| ellipsized(fonts, &item.sub, 11.5 * s, Weight::NORMAL, 180.0 * s));
                    bubble = Some((t.rect, title, sub));
                }
            }
            if self.hover == Some(Hit::New) {
                if let Some(t) = tiles.iter().find(|t| t.hit == Hit::New) {
                    bubble = Some((t.rect, ellipsized(fonts, "New note", 13.0 * s, Weight::SEMIBOLD, 180.0 * s), None));
                }
            }
        }
        let bubble = bubble.map(|(tile, title, sub)| {
            let bw = label_width(&title).max(sub.as_ref().map_or(0.0, label_width)) + 24.0 * s;
            let bh = if sub.is_some() { 44.0 * s } else { 28.0 * s };
            let bx = if right { tile.0 - 12.0 * s - bw } else { tile.0 + tile.2 + 12.0 * s };
            let by = tile.1 + tile.3 / 2.0 - bh / 2.0;
            ((bx, by, bw, bh), tile.1 + tile.3 / 2.0, title, sub)
        });
        let swash = &mut shared.swash;
        let (dark, expanded) = (self.dark, self.expanded);
        let bg = self.card_bg();
        let opacity = self.style.opacity;
        let radius = self.style.radius;
        let tab = TabParams::of(self);

        self.surface.paint(width, height, 255, |data| {
            if expanded && surface::ALPHA && open > 0.0 {
                let a = pal.shadow as f32 / 255.0 * open.min(1.0);
                let r = (radius + 2.0) * s;
                let hole = (strip.0 + 2.0 * s, strip.1 + r, strip.2 - 4.0 * s, strip.3 - 2.0 * r);
                gfx::shadow(
                    data,
                    width,
                    height,
                    strip,
                    &[(14.0 * s, 6.0 * s, a), (2.0 * s, 1.0 * s, a * 0.5)],
                    Some(hole),
                );
            }
            {
                let Some(mut pm) = PixmapMut::from_bytes(data, width, height) else { return };
                if expanded && open > 0.0 {
                    let (sx, sy, sw, sh) = strip;
                    if let Some(p) = gfx::squircle(sx, sy, sw, sh, (radius + 2.0) * s) {
                        gfx::fill_path(&mut pm, &p, bg, opacity);
                        let (edge, a) = if dark { (theme::WHITE, 28) } else { (pal.text, 24) };
                        gfx::stroke_path(&mut pm, &p, 1.0 * s, edge, a);
                    }
                    if let Some(p) = gfx::rounded_rect(sx + 12.0 * s, sep_y, sw - 24.0 * s, s.max(1.0), 0.5 * s) {
                        gfx::fill_path(&mut pm, &p, pal.secondary, 90);
                    }
                    for &((x, y, size, _), wash, reminder) in &faces {
                        if let Some(p) = gfx::squircle(x, y, size, size, size * 0.24) {
                            gfx::fill_path(&mut pm, &p, wash, 255);
                            gfx::stroke_path(&mut pm, &p, 1.0 * s, pal.text, if dark { 40 } else { 22 });
                        }
                        if reminder {
                            let r = size * 0.12;
                            let cx = if right { x + size - r * 0.6 } else { x + r * 0.6 };
                            if let Some(p) = gfx::circle(cx, y + r * 0.6, r) {
                                gfx::fill_path(&mut pm, &p, pal.alert, 255);
                                gfx::stroke_path(&mut pm, &p, 1.5 * s, bg, 255);
                            }
                        }
                    }
                    if let Some(t) = tiles.iter().find(|t| t.hit == Hit::New) {
                        let (x, y, size, _) = t.rect;
                        if let Some(p) = gfx::squircle(x, y, size, size, size * 0.24) {
                            gfx::fill_path(&mut pm, &p, pal.text, if dark { 26 } else { 14 });
                        }
                        Shape::Plus.draw(&mut pm, t.rect, s, &pal, bg, 1.0);
                    }
                    if let Some(((bx, by, bw, bh), cy, _, _)) = &bubble {
                        let bubble_bg = Rgb(0x2B, 0x2B, 0x2E);
                        if let Some(p) = gfx::rounded_rect(*bx, *by, *bw, *bh, 8.0 * s) {
                            gfx::fill_path(&mut pm, &p, bubble_bg, 245);
                        }
                        let (edge, dir) = if right { (bx + bw - 0.5, 1.0) } else { (bx + 0.5, -1.0) };
                        let tip = [(edge, cy - 5.0 * s), (edge + dir * 6.0 * s, *cy), (edge, cy + 5.0 * s)];
                        if let Some(p) = gfx::polyline(&tip, true) {
                            gfx::fill_path(&mut pm, &p, bubble_bg, 245);
                        }
                    }
                }
                tab.draw(&mut pm, w, h);
            }
            let full = Clip { x0: 0, y0: 0, x1: width as i32, y1: height as i32 };
            for (mut b, color, at) in labels {
                gfx::draw_text(&mut b, fonts, swash, color, data, width, full, at);
            }
            if let Some(((bx, by, _, bh), _, mut title, sub)) = bubble {
                let tx = (bx + 12.0 * s) as i32;
                let ty = if sub.is_some() { by + 6.0 * s } else { by + (bh - 13.0 * s * 1.3) / 2.0 };
                gfx::draw_text(&mut title, fonts, swash, theme::WHITE.text(255), data, width, full, (tx, ty as i32));
                if let Some(mut sub) = sub {
                    gfx::draw_text(
                        &mut sub,
                        fonts,
                        swash,
                        theme::WHITE.text(165),
                        data,
                        width,
                        full,
                        (tx, (by + 24.0 * s) as i32),
                    );
                }
            }
        });
    }
}

/// What the tucked-away tab needs, copied out so it can be drawn inside the paint closure.
struct TabParams {
    open: f32,
    /// The tab's centre in the window (see `Dock::anchor`).
    anchor: f32,
    right: bool,
    dark: bool,
    s: f32,
    pal: Palette,
    style: DockStyle,
}

impl TabParams {
    fn of(d: &Dock) -> TabParams {
        TabParams {
            open: d.open.value().max(0.0),
            anchor: d.anchor,
            right: d.right(),
            dark: d.dark,
            s: d.scale(),
            pal: d.pal(),
            style: d.style.clone(),
        }
    }

    fn draw(&self, pm: &mut PixmapMut, w: f32, h: f32) {
        let s = self.s;
        let st = &self.style;
        let (hw, hh) = (st.trigger_width * s, st.trigger_height * s);
        let hx = if self.right { w - hw } else { 0.0 };
        // At the tab's own height, not the window's middle: near a screen edge the open dock's
        // window is clamped, and a target that moved would leave the pointer over nothing.
        let cy = tab_centre(self.anchor, hh, h);
        // Hover and grab target: alpha 1 is invisible but still receives the pointer (also while
        // the dock is open, so the tab can be dragged along the edge).
        gfx::fill_rect(pm, hx, cy - hh / 2.0, hw, hh, self.pal.text, 1);
        if self.open >= 1.0 {
            return;
        }
        let (tw, th) = (st.tab_width * s, st.tab_height * s);
        if tw <= 0.0 || th <= 0.0 {
            return;
        }
        let tx = if self.right { w - tw - 2.0 * s } else { 2.0 * s };
        if let Some(p) = gfx::rounded_rect(tx, tab_centre(self.anchor, th, h) - th / 2.0, tw, th, tw / 2.0) {
            let auto = if self.dark { Rgb(0xC8, 0xC8, 0xCC) } else { Rgb(0x70, 0x70, 0x76) };
            let alpha = st.tab_opacity as f32 * (1.0 - self.open).clamp(0.0, 1.0);
            gfx::fill_path(pm, &p, st.tab_color.unwrap_or(auto), alpha as u8);
        }
    }
}

/// The tab's centre: at `anchor`, kept `size` clear of the window's top and bottom.
fn tab_centre(anchor: f32, size: f32, height: f32) -> f32 {
    anchor.clamp(size / 2.0, (height - size / 2.0).max(size / 2.0))
}

/// Small vector shapes used by the dock.
#[derive(Clone, Copy)]
enum Shape {
    Hover,
    Plus,
    SmallPlus,
    Disclosure(bool, Rgb),
    /// A hairline across the middle of the rect.
    Rule,
    Badge(Rgb),
    Radio(bool),
    Check(bool),
    Delete(bool),
    Field,
    Caret,
    PlusTile,
}

impl Shape {
    fn draw(self, pm: &mut PixmapMut, (x, y, w, h): (f32, f32, f32, f32), s: f32, pal: &Palette, bg: Rgb, alpha: f32) {
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        let blue = pal.selection.0;
        let a = |v: u8| (v as f32 * alpha) as u8;
        let stroke = |pm: &mut PixmapMut, pts: &[(f32, f32)], width: f32, c: Rgb, al: u8| {
            if let Some(p) = gfx::polyline(pts, false) {
                gfx::stroke_path(pm, &p, width, c, al);
            }
        };
        match self {
            Shape::Hover => {
                if let Some(p) = gfx::rounded_rect(x, y, w, h, 7.0 * s) {
                    gfx::fill_path(pm, &p, pal.text, a(pal.soft));
                }
            }
            Shape::Plus | Shape::SmallPlus => {
                let r = if matches!(self, Shape::Plus) { (w.min(h) * 0.22).min(6.0 * s) } else { 4.5 * s };
                let c = if matches!(self, Shape::Plus) { pal.text } else { pal.secondary };
                stroke(pm, &[(cx - r, cy), (cx + r, cy)], 1.6 * s, c, a(220));
                stroke(pm, &[(cx, cy - r), (cx, cy + r)], 1.6 * s, c, a(220));
            }
            Shape::Disclosure(collapsed, ink) => {
                let pts = if collapsed {
                    [(cx - 1.5 * s, cy - 3.0 * s), (cx + 1.8 * s, cy), (cx - 1.5 * s, cy + 3.0 * s)]
                } else {
                    [(cx - 3.0 * s, cy - 1.5 * s), (cx, cy + 1.8 * s), (cx + 3.0 * s, cy - 1.5 * s)]
                };
                stroke(pm, &pts, 1.3 * s, ink, a(200));
            }
            Shape::Rule => {
                if w > 0.0 {
                    stroke(pm, &[(x, cy), (x + w, cy)], 1.0 * s, pal.secondary, a(110));
                }
            }
            Shape::Badge(c) => {
                if let Some(p) = gfx::circle(cx, cy, w / 2.0) {
                    gfx::fill_path(pm, &p, c, a(255));
                }
            }
            Shape::Radio(on) => {
                if let Some(p) = gfx::circle(cx, cy, w / 2.0) {
                    if on {
                        gfx::fill_path(pm, &p, blue, a(255));
                        if let Some(dot) = gfx::circle(cx, cy, w * 0.18) {
                            gfx::fill_path(pm, &dot, theme::WHITE, a(255));
                        }
                    } else {
                        gfx::stroke_path(pm, &p, 1.3 * s, pal.secondary, a(200));
                    }
                }
            }
            Shape::Check(on) => {
                if let Some(p) = gfx::rounded_rect(x, y, w, h, 3.0 * s) {
                    if on {
                        gfx::fill_path(pm, &p, blue, a(255));
                        let tick = [(x + w * 0.24, cy), (x + w * 0.43, y + h * 0.7), (x + w * 0.77, y + h * 0.3)];
                        stroke(pm, &tick, 1.6 * s, theme::WHITE, a(255));
                    } else {
                        gfx::stroke_path(pm, &p, 1.3 * s, pal.secondary, a(200));
                    }
                }
            }
            Shape::Delete(hot) => {
                if hot {
                    if let Some(p) = gfx::rounded_rect(x, y, w, h, 5.0 * s) {
                        gfx::fill_path(pm, &p, pal.alert, a(40));
                    }
                }
                let r = 3.5 * s;
                let c = if hot { pal.alert } else { pal.secondary };
                stroke(pm, &[(cx - r, cy - r), (cx + r, cy + r)], 1.4 * s, c, a(230));
                stroke(pm, &[(cx - r, cy + r), (cx + r, cy - r)], 1.4 * s, c, a(230));
            }
            Shape::Field => {
                if let Some(p) = gfx::rounded_rect(x, y, w, h, 6.0 * s) {
                    gfx::fill_path(pm, &p, bg, a(255));
                    gfx::stroke_path(pm, &p, 1.5 * s, blue, a(200));
                }
            }
            Shape::Caret => gfx::fill_rect(pm, x, y, w, h, pal.text, a(230)),
            Shape::PlusTile => {
                if let Some(p) = gfx::squircle(x, y, w, h, w * 0.26) {
                    gfx::fill_path(pm, &p, pal.text, a(pal.soft + 6));
                }
                let r = w * 0.2;
                stroke(pm, &[(cx - r, cy), (cx + r, cy)], 1.6 * s, pal.secondary, a(230));
                stroke(pm, &[(cx, cy - r), (cx, cy + r)], 1.6 * s, pal.secondary, a(230));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falloff_is_one_at_the_pointer_and_zero_beyond_reach() {
        assert_eq!(falloff(0.0, 100.0), 1.0);
        assert!(falloff(100.0, 100.0).abs() < 1e-6);
        assert!(falloff(250.0, 100.0).abs() < 1e-6);
        assert!((falloff(50.0, 100.0) - 0.5).abs() < 1e-6);
        assert_eq!(falloff(-30.0, 100.0), falloff(30.0, 100.0));
    }

    #[test]
    fn views_round_trip_by_name() {
        for v in [View::Compact, View::Detailed, View::Icons] {
            assert_eq!(View::parse(v.name()), v);
        }
    }
}
