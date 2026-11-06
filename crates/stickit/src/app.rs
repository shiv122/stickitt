//! Event-loop glue: owns every note window, the dock, the shared font system and the store;
//! applies profiles (which lists are visible), virtual desktops and global hotkeys.
//! The loop sleeps (`ControlFlow::Wait`) unless a caret is blinking, an animation is
//! running, a save is pending or a reminder is coming up.

use crate::config::{Config, GlobalAction};
use crate::desktops::Desktops;
use crate::dock::{Dock, DockCmd, DockItem, DockModel, View};
use crate::note::{title_of, Cmd, Note, Shared, DEFAULT_SIZE};
use crate::store::{NoteRecord, Profile, Store, DEFAULT_LIST, DEFAULT_PROFILE};
use crate::surface;
use crate::toast::{Toast, ToastCmd};
use crate::tray::{Hints, Tray, TrayCmd};
use chrono::Local;
use cosmic_text::{FontSystem, SwashCache};
use global_hotkey::hotkey::HotKey;
use global_hotkey::GlobalHotKeyManager;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::window::WindowId;

mod shots;

pub use shots::Shots;

const SAVE_DELAY: Duration = Duration::from_millis(400);
/// Paints are paced to at most one per display frame, however many input events arrive; a
/// high-rate mouse would otherwise trigger hundreds of repaints a second. This is the fallback
/// when the monitor's refresh rate is unknown.
const DEFAULT_FRAME: Duration = Duration::from_micros(16_667);
/// After "Settings…" opens stickit.ini, it is checked for saves this often (for a while), so
/// edits apply live.
const CONFIG_POLL: Duration = Duration::from_millis(700);
const CONFIG_WATCH: Duration = Duration::from_secs(30 * 60);
/// How often old entries are dropped from the text shaping cache.
const SHAPE_CACHE_TRIM: Duration = Duration::from_secs(5);
/// This long after the last paint the app counts as idle and gives memory back (see `trim_idle`).
const IDLE_TRIM: Duration = Duration::from_secs(2);
/// Rasterized glyphs kept between paints; beyond this the cache starts over (glyphs are drawn
/// again as needed, which is quick).
const GLYPH_CACHE: usize = 1024;
/// Trashed notes listed in the dock.
const TRASH_LISTED: i64 = 50;
/// System closes this close together are the taskbar's "Close all windows".
const CLOSE_BURST: Duration = Duration::from_millis(600);
/// Re-check reminders at least this often; monotonic timers can stall across system sleep.
const REMINDER_POLL: Duration = Duration::from_secs(30);

const WELCOME: &str = "# Welcome
- [x] Click a box to tick it
- [ ] Type `/` for blocks, `@in 1m` for a reminder
- [ ] Rest the pointer on the right screen edge

> Hover for controls · Ctrl+N new note";

/// `--bench N [--big]`: sample notes instead of the saved ones.
#[derive(Clone, Copy)]
pub struct Bench {
    pub notes: usize,
    /// Long, richly formatted notes (about 360 lines) in larger windows.
    pub big: bool,
}

/// A long note exercising every kind of formatting, for benchmarks.
fn big_note() -> String {
    let mut s = String::from("# Big note benchmark\n");
    for i in 0..60 {
        s.push_str(&format!(
            "## Section {i}\n\
             Some **bold** and *italic* text with `inline code`, a [link](https://example.com) and ~~struck~~ words; \
             lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore.\n\
             - [ ] open task {i}\n\
             - [x] done task {i}\n\
             - a bullet point that is long enough to wrap onto a second line here\n\
             > a quoted line in section {i}\n"
        ));
    }
    s
}

/// Events from outside the window system.
#[derive(Debug)]
pub enum UserEvent {
    Hotkey(u32),
    /// A tray menu item, by id.
    Menu(String),
    TrayClick,
    /// Stick-itt was launched again while running.
    Relaunched,
}

pub struct App {
    shared: Shared,
    store: Store,
    notes: HashMap<WindowId, Note>,
    dock: Option<Dock>,
    /// The note currently floated to the front by hovering it in the dock.
    peeked: Option<WindowId>,
    ctx: Option<surface::Context>,
    save_at: Option<Instant>,
    /// Open sample notes instead of the saved ones (benchmarking).
    bench: Option<Bench>,
    started: bool,
    desktops: Desktops,
    /// The desktop the user was on when the dock opened.
    current_desktop: Option<String>,
    profiles: Vec<Profile>,
    active_profile: i64,
    /// "Hide all notes" is on.
    hidden: bool,
    hotkeys: Option<GlobalHotKeyManager>,
    tray: Option<Tray>,
    /// Recent closes requested by the system (taskbar, Alt+F4).
    system_closes: Vec<(Instant, WindowId)>,
    /// "Note deleted · Undo".
    toast: Option<Toast>,
    /// The tray's hidden-notes list may be out of date.
    tray_stale: bool,
    bindings: Vec<(HotKey, GlobalAction)>,
    /// Paint timings, when `STICKIT_TRACE=<file>` is set.
    trace: Option<std::fs::File>,
    next_frame: Instant,
    /// One refresh of the primary monitor.
    frame: Duration,
    last_trim: Instant,
    /// When the app will count as idle (set after each paint).
    trim_at: Option<Instant>,
    /// Notes were added, removed or changed since the dock's list was built.
    dock_stale: bool,
    /// Watching stickit.ini for saves until then; next check at the second instant.
    config_watch: Option<(Instant, Instant)>,
    /// `--shots`: rendering the README's pictures.
    shots: Option<Shots>,
}

impl App {
    pub fn new(store: Store, bench: Option<Bench>, shots: Option<Shots>, config: Config) -> Self {
        crate::theme::set_appearance(config.general.theme);
        let active_profile = store.setting("profile").and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_PROFILE);
        let mut app = Self {
            shared: Shared {
                fonts: font_system(),
                swash: SwashCache::new(),
                clipboard: arboard::Clipboard::new().ok(),
                config,
                lists: Vec::new(),
                icon: app_icon(),
            },
            store,
            notes: HashMap::new(),
            dock: None,
            peeked: None,
            ctx: None,
            save_at: None,
            bench,
            started: false,
            desktops: Desktops::new(),
            current_desktop: None,
            profiles: Vec::new(),
            active_profile,
            hidden: false,
            hotkeys: None,
            tray: None,
            system_closes: Vec::new(),
            toast: None,
            tray_stale: true,
            bindings: Vec::new(),
            trace: std::env::var_os("STICKIT_TRACE").and_then(|p| std::fs::File::create(p).ok()),
            next_frame: Instant::now(),
            frame: DEFAULT_FRAME,
            last_trim: Instant::now(),
            trim_at: None,
            dock_stale: true,
            config_watch: None,
            shots,
        };
        app.reload_model();
        app
    }

    // ---- lists & profiles -------------------------------------------------------------------

    fn reload_model(&mut self) {
        self.shared.lists = self.store.lists().unwrap_or_default();
        self.profiles = self.store.profiles().unwrap_or_default();
        if !self.profiles.iter().any(|p| p.id == self.active_profile) {
            self.active_profile = DEFAULT_PROFILE;
        }
        if let Some(dock) = self.dock.as_mut() {
            dock.set_model(DockModel {
                lists: self.shared.lists.clone(),
                profiles: self.profiles.clone(),
                active: self.active_profile,
                show_others: self.shared.config.desktops.dock_shows_others,
            });
        }
    }

    /// Shows the "Moved to trash · Undo" toast for a trashed note, or adds the note to the one
    /// already showing.
    fn offer_undo(&mut self, el: &ActiveEventLoop, id: i64, monitor: Option<winit::monitor::MonitorHandle>) {
        match self.toast.as_mut().filter(|t| !t.closed()) {
            Some(t) => t.add(id),
            None => {
                let Some(ctx) = self.ctx.as_ref() else { return };
                let show_for = Duration::from_secs_f32(self.shared.config.general.undo_seconds);
                self.toast =
                    Toast::open(el, ctx, monitor, id, show_for).map_err(|e| eprintln!("stickit: no toast: {e}")).ok();
            }
        }
    }

    fn active_lists(&self) -> Vec<i64> {
        self.profiles.iter().find(|p| p.id == self.active_profile).map(|p| p.lists.clone()).unwrap_or_default()
    }

    /// Where the active profile is remembered: per virtual desktop, or once for all.
    fn profile_key(&self, desktop: Option<&str>) -> String {
        match desktop.filter(|_| self.shared.config.desktops.profile_per_desktop) {
            Some(d) => format!("profile:{d}"),
            None => "profile".to_string(),
        }
    }

    fn profile_lists(&self, id: i64) -> Vec<i64> {
        self.profiles.iter().find(|p| p.id == id).map(|p| p.lists.clone()).unwrap_or_default()
    }

    /// Shows notes in their profile's lists and hides the rest (or all, when hidden). With
    /// per-desktop profiles, each note follows the profile of the desktop it is on.
    fn apply_visibility(&mut self) {
        self.dock_stale = true;
        self.tray_stale = true;
        let current = self.active_lists();
        let per_desktop = self.shared.config.desktops.profile_per_desktop;
        let mut by_desktop: HashMap<String, Vec<i64>> = HashMap::new();
        let mut shown: Vec<(WindowId, bool)> = Vec::new();
        for (id, n) in self.notes.iter().filter(|(_, n)| !n.closing) {
            let elsewhere = self.desktops.of(&n.window).filter(|_| per_desktop && !self.desktops.is_current(&n.window));
            let lists = match elsewhere {
                Some(d) => by_desktop
                    .entry(d.clone())
                    .or_insert_with(|| {
                        let profile = self.store.setting(&self.profile_key(Some(&d))).and_then(|v| v.parse().ok());
                        self.profile_lists(profile.unwrap_or(DEFAULT_PROFILE))
                    })
                    .clone(),
                None => current.clone(),
            };
            shown.push((*id, !self.hidden && !n.user_hidden && lists.contains(&n.list)));
        }
        for (id, on) in shown {
            if let Some(n) = self.notes.get_mut(&id) {
                n.set_shown(on);
            }
        }
        if let Some(tray) = &self.tray {
            tray.set_notes_hidden(self.hidden);
        }
    }

    /// Which virtual desktop the user is looking at: the foreground window's, or failing that,
    /// any note's that is visible right now.
    fn update_current_desktop(&mut self) {
        self.current_desktop = self.desktops.of_foreground().or_else(|| {
            self.notes.values().find(|n| self.desktops.is_current(&n.window)).and_then(|n| self.desktops.of(&n.window))
        });
    }

    fn select_profile(&mut self, id: i64) {
        self.active_profile = id;
        let key = self.profile_key(self.current_desktop.as_deref());
        self.store.set_setting(&key, &id.to_string());
        self.reload_model();
        self.apply_visibility();
    }

    // ---- notes ------------------------------------------------------------------------------

    fn open(&mut self, el: &ActiveEventLoop, rec: &NoteRecord) -> Option<WindowId> {
        let ctx = self.ctx.as_ref()?;
        match Note::open(el, ctx, rec, &mut self.shared) {
            Ok(note) => {
                // Back to the virtual desktop it was left on.
                if let Some(desktop) = &rec.desktop {
                    self.desktops.move_to(&note.window, desktop);
                }
                let id = note.window.id();
                self.notes.insert(id, note);
                Some(id)
            }
            Err(e) => {
                eprintln!("stickit: failed to open note {}: {e}", rec.id);
                None
            }
        }
    }

    /// Brings notes back from the trash, onto this desktop, focusing the last one.
    fn restore(&mut self, el: &ActiveEventLoop, ids: &[i64]) {
        self.update_current_desktop();
        let mut last = None;
        for &id in ids {
            // Undo can race the fade-out: drop a closing window for the same note first.
            self.notes.retain(|_, n| !(n.closing && n.id == id));
            let Ok(Some(rec)) = self.store.restore(id) else { continue };
            last = self.open(el, &rec).or(last);
        }
        self.dock_stale = true;
        self.tray_stale = true;
        if let Some(win) = last {
            self.show_notes();
            self.run_dock(el, DockCmd::Focus(win));
        }
    }

    /// The colour for a new note (`[notes] default_color`). The rotation position is kept in the
    /// store, so it carries on across restarts.
    fn next_color(&mut self) -> u16 {
        use crate::config::NewNoteColor;
        use crate::theme::ROTATION;
        let last: usize = self.store.setting("color_turn").and_then(|v| v.parse().ok()).unwrap_or(ROTATION.len() - 1);
        let turn = match self.shared.config.notes.default_color {
            NewNoteColor::Fixed(color) => return color,
            NewNoteColor::Cycle => (last + 1) % ROTATION.len(),
            NewNoteColor::Random => {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.subsec_nanos() as usize);
                // Any colour but the last one.
                (last + 1 + nanos % (ROTATION.len() - 1)) % ROTATION.len()
            }
        };
        self.store.set_setting("color_turn", &turn.to_string());
        ROTATION[turn]
    }

    fn create(
        &mut self,
        el: &ActiveEventLoop,
        body: &str,
        color: u16,
        pos: Option<(i32, i32)>,
        focus: bool,
    ) -> Option<WindowId> {
        // New notes go to the default list, unless this profile doesn't show it.
        let shown = self.active_lists();
        let list =
            if shown.contains(&DEFAULT_LIST) { DEFAULT_LIST } else { shown.first().copied().unwrap_or(DEFAULT_LIST) };
        let mut rec = NoteRecord {
            id: 0,
            body: body.to_string(),
            pos,
            size: DEFAULT_SIZE,
            color,
            mode: 0,
            zoom: 100,
            reminder: None,
            icon: String::new(),
            list,
            desktop: None,
            hidden: false,
            view: 0,
        };
        rec.id = self.store.insert(&rec).map_err(|e| eprintln!("stickit: insert failed: {e}")).ok()?;
        let id = self.open(el, &rec)?;
        self.dock_stale = true;
        self.tray_stale = true;
        if let Some(n) = self.notes.get_mut(&id) {
            n.dirty = true; // persist the OS-chosen position
            if focus {
                n.window.focus_window();
            }
        }
        Some(id)
    }

    fn flush(&mut self) {
        let desktops = &self.desktops;
        let recs: Vec<NoteRecord> = self
            .notes
            .values_mut()
            .filter(|n| n.dirty)
            .map(|n| {
                n.dirty = false;
                let mut rec = n.record();
                // Only a visible window reports its desktop; otherwise the stored one is kept.
                rec.desktop = desktops.of(&n.window);
                if rec.desktop.is_some() {
                    n.desktop = rec.desktop.clone();
                }
                rec
            })
            .collect();
        if !recs.is_empty() {
            if let Err(e) = self.store.save_all(&recs) {
                eprintln!("stickit: save failed: {e}");
            }
        }
        self.save_at = None;
    }

    fn run(&mut self, el: &ActiveEventLoop, from: WindowId, cmd: Cmd) {
        match cmd {
            Cmd::MoveToDesktop(desktop) => {
                if let Some(n) = self.notes.get_mut(&from) {
                    if self.desktops.move_to(&n.window, &desktop) {
                        n.desktop = Some(desktop);
                        n.dirty = true;
                        self.dock_stale = true;
                    }
                }
            }
            Cmd::PassClick(x, y) => {
                // The note whose card is under the point; one-line notes first, as they sit close.
                let inside = |n: &Note| {
                    n.card_on_screen().is_some_and(|(cx, cy, w, h)| x >= cx && x < cx + w && y >= cy && y < cy + h)
                };
                let target = self
                    .notes
                    .iter()
                    .filter(|(id, n)| **id != from && n.shown && !n.closing && inside(n))
                    .max_by_key(|(_, n)| n.is_collapsed())
                    .map(|(id, _)| *id);
                if let Some(n) = target.and_then(|id| self.notes.get_mut(&id)) {
                    n.take_click();
                }
            }
            Cmd::NewNote => {
                // Opens just below and right of the note it was made from.
                let pos = self.notes.get(&from).and_then(|n| {
                    let off = (28.0 * n.window.scale_factor()) as i32;
                    n.window.outer_position().ok().map(|p| (p.x + off, p.y + off))
                });
                let color = self.next_color();
                self.create(el, "", color, pos, true);
            }
            Cmd::SystemClose => {
                let now = Instant::now();
                self.system_closes.retain(|(t, _)| now - *t < CLOSE_BURST);
                self.system_closes.push((now, from));
                if self.system_closes.len() < 2 {
                    return self.run(el, from, Cmd::Close);
                }
                // Several at once: "Close all windows" on the taskbar. Hide every note instead,
                // and keep running in the tray; one click there (or Ctrl+Alt+H) brings them back.
                for (_, id) in &self.system_closes {
                    if let Some(n) = self.notes.get_mut(id) {
                        n.unhide();
                    }
                }
                self.hidden = true;
                self.apply_visibility();
            }
            Cmd::Close => {
                if let Some(n) = self.notes.get_mut(&from) {
                    if let Some(d) = self.desktops.of(&n.window) {
                        n.desktop = Some(d);
                    }
                    n.hide();
                }
                self.dock_stale = true;
                self.tray_stale = true;
            }
            Cmd::Delete => {
                self.flush();
                let Some(n) = self.notes.get_mut(&from) else { return };
                let _ = self.store.trash(n.id);
                n.dirty = false;
                n.begin_close();
                let (id, monitor) = (n.id, n.window.current_monitor());
                self.dock_stale = true;
                self.tray_stale = true;
                self.offer_undo(el, id, monitor);
            }
            Cmd::Quit => {
                self.flush();
                el.exit();
            }
            Cmd::OpenSettings => self.open_settings(),
        }
    }

    fn open_settings(&mut self) {
        self.shared.config.open_in_editor();
        let now = Instant::now();
        self.config_watch = Some((now + CONFIG_WATCH, now + CONFIG_POLL));
    }

    /// Re-reads stickit.ini if it changed and applies it everywhere.
    fn reload_config(&mut self) {
        if !self.shared.config.reload_if_changed() {
            return;
        }
        crate::theme::set_appearance(self.shared.config.general.theme);
        let style = self.dock_style();
        if let Some(dock) = self.dock.as_mut() {
            dock.set_style(style);
            dock.refresh_theme();
        }
        if let Some(toast) = self.toast.as_mut() {
            toast.refresh_theme();
        }
        let notes = self.shared.config.notes.clone();
        let taskbar = self.shared.config.desktops.per_desktop;
        for n in self.notes.values_mut() {
            n.set_style(&notes);
            n.set_taskbar(taskbar);
            n.refresh_theme();
        }
        self.reload_model();
        self.register_hotkeys();
        self.sync_tray();
    }

    // ---- dock -------------------------------------------------------------------------------

    fn run_dock(&mut self, el: &ActiveEventLoop, cmd: DockCmd) {
        match cmd {
            DockCmd::Peek(id) => {
                if let Some(prev) = self.peeked.take() {
                    if let Some(n) = self.notes.get_mut(&prev) {
                        n.peek(false);
                    }
                }
                if let Some(n) = id.and_then(|id| self.notes.get_mut(&id)) {
                    n.peek(true);
                    self.peeked = id;
                }
            }
            DockCmd::Focus(id) => {
                self.run_dock(el, DockCmd::Peek(None));
                if let Some(n) = self.notes.get_mut(&id) {
                    // A note on another desktop comes to this one.
                    if !self.desktops.is_current(&n.window) {
                        if let Some(here) = &self.current_desktop {
                            self.desktops.move_to(&n.window, here);
                            n.dirty = true;
                        }
                    }
                    n.reveal(true);
                }
            }
            DockCmd::NewNote => {
                let color = self.next_color();
                self.create(el, "", color, None, true);
            }
            DockCmd::Opening => {
                self.update_current_desktop();
                // Pick up edits to stickit.ini.
                self.reload_config();
                // This desktop's own profile.
                if self.shared.config.desktops.profile_per_desktop {
                    let key = self.profile_key(self.current_desktop.as_deref());
                    let id = self.store.setting(&key).and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_PROFILE);
                    if id != self.active_profile {
                        self.active_profile = id;
                        self.reload_model();
                        self.apply_visibility();
                    }
                }
                self.dock_stale = true;
                self.tray_stale = true;
            }
            DockCmd::OpenSettings => self.open_settings(),
            DockCmd::Restore(id) => self.restore(el, &[id]),
            DockCmd::Purge(id) => {
                let _ = self.store.purge(id);
                self.dock_stale = true;
            }
            DockCmd::EmptyTrash => {
                let _ = self.store.empty_trash(None);
                self.dock_stale = true;
            }
            DockCmd::Quit => {
                self.flush();
                el.exit();
            }
            DockCmd::SetView(view) => self.store.set_setting("dock_view", view.name()),
            DockCmd::SelectProfile(id) => self.select_profile(id),
            DockCmd::NewProfile(name) => {
                let lists = self.active_lists();
                if let Ok(id) = self.store.create_profile(&name, &lists) {
                    self.select_profile(id);
                }
            }
            DockCmd::DeleteProfile(id) => {
                let _ = self.store.delete_profile(id);
                let next = if self.active_profile == id { DEFAULT_PROFILE } else { self.active_profile };
                self.select_profile(next);
            }
            DockCmd::ToggleList(id, on) => {
                let _ = self.store.set_profile_list(self.active_profile, id, on);
                self.reload_model();
                self.apply_visibility();
            }
            DockCmd::NewList(name) => {
                let _ = self.store.create_list(&name, self.active_profile);
                self.reload_model();
            }
            DockCmd::DeleteList { id, trash_notes } => {
                self.flush();
                let _ = self.store.delete_list(id, trash_notes);
                let mut trashed = Vec::new();
                for n in self.notes.values_mut().filter(|n| n.list == id) {
                    n.list = DEFAULT_LIST;
                    if trash_notes && !n.closing {
                        n.dirty = false;
                        n.begin_close();
                        trashed.push((n.id, n.window.current_monitor()));
                    }
                }
                // One toast offers to bring them all back.
                for (note, monitor) in trashed {
                    self.offer_undo(el, note, monitor);
                }
                self.reload_model();
                self.apply_visibility();
            }
            DockCmd::Resized { tile_size, width } => {
                let dock = &mut self.shared.config.dock;
                dock.tile_size = tile_size;
                dock.width = width;
                self.shared.config.save();
            }
            DockCmd::Moved { side, position } => {
                self.store.set_setting("dock_side", if side == crate::config::Side::Left { "left" } else { "right" });
                self.store.set_setting("dock_position", &format!("{position:.3}"));
            }
        }
    }

    /// Dock style from stickit.ini, with the edge and height the user last dragged it to.
    fn dock_style(&self) -> crate::config::DockStyle {
        let mut style = self.shared.config.dock.clone();
        match self.store.setting("dock_side").as_deref() {
            Some("left") => style.side = crate::config::Side::Left,
            Some("right") => style.side = crate::config::Side::Right,
            _ => {}
        }
        if let Some(p) = self.store.setting("dock_position").and_then(|v| v.parse::<f32>().ok()) {
            style.position = p.clamp(0.0, 1.0);
        }
        style
    }

    /// Notes for the dock, most recently used first.
    fn dock_items(&self) -> Vec<DockItem> {
        let mut notes: Vec<&Note> = self.notes.values().filter(|n| !n.closing).collect();
        notes.sort_by(|a, b| b.last_active.cmp(&a.last_active).then(b.id.cmp(&a.id)));
        notes
            .into_iter()
            .map(|n| {
                let (title, sub) = n.summary();
                DockItem {
                    id: n.window.id(),
                    title,
                    sub,
                    tint: n.color,
                    icon: n.icon.clone(),
                    reminder: n.reminder.is_some(),
                    list: n.list,
                    here: if n.shown && !n.user_hidden {
                        self.desktops.is_current(&n.window)
                    } else {
                        // Hidden windows have no desktop as far as Windows is concerned.
                        match (&n.desktop, &self.current_desktop) {
                            (Some(note), Some(here)) => note == here,
                            _ => true,
                        }
                    },
                    hidden: n.user_hidden,
                    note: n.id,
                    trashed: false,
                }
            })
            .chain(self.store.trashed(TRASH_LISTED).unwrap_or_default().into_iter().map(|rec| DockItem {
                id: WindowId::dummy(),
                title: title_of(&rec.body),
                sub: String::new(),
                tint: rec.color,
                icon: rec.icon,
                reminder: false,
                list: rec.list,
                here: true,
                hidden: false,
                note: rec.id,
                trashed: true,
            }))
            .collect()
    }

    // ---- tray -------------------------------------------------------------------------------

    /// Creates or removes the tray icon to match the settings.
    fn sync_tray(&mut self) {
        if !self.shared.config.general.tray_icon {
            self.tray = None;
            return;
        }
        if self.tray.is_some() {
            return;
        }
        let hint =
            |a: GlobalAction| self.shared.config.global.iter().find(|(g, _)| *g == a).map_or("", |(_, b)| b.as_str());
        let hints = Hints {
            new_note: hint(GlobalAction::NewNote),
            paste_as_note: hint(GlobalAction::PasteAsNote),
            open_dock: hint(GlobalAction::OpenDock),
            toggle_notes: hint(GlobalAction::ToggleNotes),
            settings: hint(GlobalAction::OpenSettings),
        };
        self.tray = app_icon_rgba().and_then(|(rgba, size)| Tray::new(rgba, size, hints));
        if let Some(tray) = &self.tray {
            tray.set_notes_hidden(self.hidden);
        }
        self.tray_stale = true;
    }

    fn hidden_notes(&self) -> Vec<(i64, String)> {
        let mut notes: Vec<&Note> = self.notes.values().filter(|n| n.user_hidden && !n.closing).collect();
        notes.sort_by(|a, b| b.last_active.cmp(&a.last_active).then(b.id.cmp(&a.id)));
        notes
            .into_iter()
            .map(|n| {
                let (title, _) = n.summary();
                let title = if title.is_empty() { "Untitled".to_string() } else { title };
                // "&" marks a mnemonic in Windows menus.
                let title = title.replace('&', "&&");
                (n.id, if n.icon.is_empty() { title } else { format!("{}  {title}", n.icon) })
            })
            .collect()
    }

    fn run_tray(&mut self, el: &ActiveEventLoop, cmd: TrayCmd) {
        match cmd {
            TrayCmd::NewNote => self.run_global(el, GlobalAction::NewNote),
            TrayCmd::PasteAsNote => self.run_global(el, GlobalAction::PasteAsNote),
            TrayCmd::ToggleNotes => self.run_global(el, GlobalAction::ToggleNotes),
            TrayCmd::Settings => self.open_settings(),
            TrayCmd::OpenDock => self.open_dock(),
            TrayCmd::Reveal(id) => {
                self.show_notes();
                self.update_current_desktop();
                if let Some(win) = self.notes.values().find(|n| n.id == id).map(|n| n.window.id()) {
                    // Like picking it in the dock: it comes to this desktop.
                    self.run_dock(el, DockCmd::Focus(win));
                }
            }
            TrayCmd::RevealAll => {
                self.show_notes();
                for n in self.notes.values_mut().filter(|n| n.user_hidden && !n.closing) {
                    n.reveal(false);
                    // Each goes back to its own desktop (it is still transparent here).
                    if let Some(d) = &n.desktop {
                        self.desktops.move_to(&n.window, d);
                    }
                }
                self.dock_stale = true;
                self.tray_stale = true;
            }
            TrayCmd::Quit => {
                self.flush();
                el.exit();
            }
        }
    }

    /// Turns "hide all notes" off.
    fn show_notes(&mut self) {
        if self.hidden {
            self.hidden = false;
            self.apply_visibility();
        }
    }

    /// Slides the dock out (if it isn't already) and focuses it.
    fn open_dock(&mut self) {
        if let Some(dock) = self.dock.as_mut() {
            if !dock.is_open() {
                dock.toggle();
            }
        }
    }

    // ---- global hotkeys ---------------------------------------------------------------------

    fn register_hotkeys(&mut self) {
        let Some(manager) = self.hotkeys.as_ref() else { return };
        for (hk, _) in self.bindings.drain(..) {
            let _ = manager.unregister(hk);
        }
        for (action, binding) in &self.shared.config.global {
            if binding.trim().is_empty() {
                continue;
            }
            match binding.parse::<HotKey>() {
                Ok(hk) => match manager.register(hk) {
                    Ok(()) => self.bindings.push((hk, *action)),
                    Err(e) => eprintln!("stickit: {} ({binding}) unavailable: {e}", action.key()),
                },
                Err(e) => eprintln!("stickit: bad shortcut for {}: {e}", action.key()),
            }
        }
    }

    fn run_global(&mut self, el: &ActiveEventLoop, action: GlobalAction) {
        match action {
            GlobalAction::NewNote => {
                let color = self.next_color();
                self.create(el, "", color, None, true);
            }
            GlobalAction::PasteAsNote => {
                let text = self.shared.clipboard.as_mut().and_then(|c| c.get_text().ok()).unwrap_or_default();
                if !text.trim().is_empty() {
                    let color = self.next_color();
                    self.create(el, text.trim(), color, None, true);
                }
            }
            GlobalAction::ToggleNotes => {
                self.hidden = !self.hidden;
                self.apply_visibility();
            }
            GlobalAction::OpenDock => {
                if let Some(dock) = self.dock.as_mut() {
                    dock.toggle();
                }
            }
            GlobalAction::OpenSettings => self.open_settings(),
            GlobalAction::NextProfile => {
                if let Some(i) = self.profiles.iter().position(|p| p.id == self.active_profile) {
                    let next = self.profiles[(i + 1) % self.profiles.len()].id;
                    self.select_profile(next);
                }
            }
        }
    }
}

impl App {
    /// The app is idle (nothing painted for `IDLE_TRIM`): give memory back without changing
    /// anything on screen. Windows' pixel buffers go (the system keeps what is shown), undo
    /// histories fold into compact steps, and the text caches are cut down.
    fn trim_idle(&mut self) {
        self.trim_at = None;
        for note in self.notes.values_mut() {
            note.trim();
        }
        if let Some(dock) = self.dock.as_mut() {
            dock.trim();
        }
        self.shared.fonts.shape_run_cache.trim(4);
        let swash = &mut self.shared.swash;
        if swash.image_cache.len() > GLYPH_CACHE {
            swash.image_cache.clear();
        }
        if swash.outline_command_cache.len() > GLYPH_CACHE {
            swash.outline_command_cache.clear();
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.started {
            return;
        }
        self.started = true;
        if let Some(mhz) = el.primary_monitor().and_then(|m| m.refresh_rate_millihertz()).filter(|&m| m > 0) {
            // Every k-th refresh, targeting 60-90 fps: smooth, and on 144 Hz+ screens half the
            // CPU of painting every refresh, while staying in step with the display.
            let k = (mhz as u64).div_ceil(90_000).max(1);
            self.frame = Duration::from_micros(k * 1_000_000_000 / mhz as u64);
        }
        self.ctx = softbuffer::Context::new(el.owned_display_handle()).ok();
        if let Some(ctx) = &self.ctx {
            let view = View::parse(&self.store.setting("dock_view").unwrap_or_default());
            self.dock =
                Dock::open(el, ctx, self.dock_style(), view).map_err(|e| eprintln!("stickit: no dock: {e}")).ok();
        }
        self.reload_model();
        self.hotkeys = GlobalHotKeyManager::new().map_err(|e| eprintln!("stickit: no global hotkeys: {e}")).ok();
        self.register_hotkeys();
        if self.bench.is_none() {
            self.sync_tray();
        }

        if self.shots.is_some() {
            return shots::start(self, el);
        }
        if let Some(b) = self.bench {
            let body = if b.big { big_note() } else { WELCOME.to_string() };
            for i in 0..b.notes {
                let pos = Some((40 + (i % 8) as i32 * 230, 40 + (i / 8) as i32 * 200));
                let id = self.create(el, &body, (i % 6) as u16, pos, false);
                if let Some(n) = id.filter(|_| b.big).and_then(|id| self.notes.get(&id)) {
                    n.resize(winit::dpi::LogicalSize::new(460.0, 720.0));
                }
            }
            return;
        }
        let keep = (self.shared.config.general.trash_days * 86_400.0) as i64;
        let _ = self.store.empty_trash(Some(keep));
        let recs = self.store.load().unwrap_or_else(|e| {
            eprintln!("stickit: load failed: {e}");
            Vec::new()
        });
        if recs.is_empty() {
            let color = self.next_color();
            if let Some(n) = self.create(el, WELCOME, color, None, false).and_then(|id| self.notes.get_mut(&id)) {
                n.icon = "👋".to_string();
                n.needs_paint = true;
            }
        }
        for rec in &recs {
            self.open(el, rec);
        }
        self.apply_visibility();
    }

    fn user_event(&mut self, el: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Hotkey(id) => {
                if let Some(&(_, action)) = self.bindings.iter().find(|(hk, _)| hk.id() == id) {
                    self.run_global(el, action);
                }
            }
            UserEvent::Menu(id) => {
                if let Some(cmd) = Tray::command(&id) {
                    self.run_tray(el, cmd);
                }
            }
            UserEvent::TrayClick => {
                self.show_notes();
                self.open_dock();
            }
            UserEvent::Relaunched => {
                self.show_notes();
                self.open_dock();
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if let Some(dock) = self.dock.as_mut().filter(|d| d.window.id() == id) {
            if let Some(cmd) = dock.handle(&event) {
                self.run_dock(el, cmd);
            }
            return;
        }
        if let Some(toast) = self.toast.as_mut().filter(|t| t.window.id() == id) {
            if let Some(ToastCmd::Undo(ids)) = toast.handle(&event) {
                self.restore(el, &ids);
            }
            return;
        }
        let Some(note) = self.notes.get_mut(&id).filter(|n| !n.closing) else { return };
        if matches!(event, WindowEvent::Focused(true)) {
            // What the note menu offers under "Move to desktop" (nothing when notes show on
            // every desktop), and where the note is now.
            note.desktop_list = if self.shared.config.desktops.per_desktop { self.desktops.list() } else { Vec::new() };
            if let Some(d) = self.desktops.of(&note.window) {
                note.desktop = Some(d);
            }
        }
        if let Some(cmd) = note.handle(&event, &mut self.shared) {
            self.run(el, id, cmd);
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let now = Instant::now();
        let ts = Local::now().timestamp();
        let mut next: Option<Instant> = None;
        let mut wake = |t: Instant| next = Some(next.map_or(t, |n: Instant| n.min(t)));

        let mut dirty = false;
        for note in self.notes.values_mut() {
            if note.due(ts) {
                note.ring(self.shared.config.reminders.sound);
            }
            if let Some(t) = note.tick(now) {
                wake(t);
            }
            if let Some(r) = note.reminder.as_ref().filter(|_| !note.ringing) {
                let secs = (r.due - ts).max(0) as u64;
                wake(now + Duration::from_secs(secs).min(REMINDER_POLL));
            }
            dirty |= note.dirty;
        }

        // Deleted notes are dropped once faded out. The app keeps running without any (the dock
        // and hotkeys stay available); quitting is explicit.
        let closed: Vec<WindowId> = self.notes.iter().filter(|(_, n)| n.closed()).map(|(id, _)| *id).collect();
        for id in closed {
            self.dock_stale = true;
            self.tray_stale = true;
            self.notes.remove(&id);
            if self.peeked == Some(id) {
                self.peeked = None;
            }
        }

        if (self.dock_stale || dirty) && self.dock.as_ref().is_some_and(|d| d.is_open()) {
            self.dock_stale = false;
            let items = self.dock_items();
            if let Some(dock) = self.dock.as_mut() {
                dock.set_items(items);
            }
        }
        if self.tray_stale || dirty {
            self.tray_stale = false;
            let hidden = self.hidden_notes();
            if let Some(tray) = self.tray.as_mut() {
                tray.set_hidden_notes(hidden);
            }
        }
        let pending = self.dock.as_mut().and_then(|dock| {
            if let Some(t) = dock.tick(now) {
                wake(t);
            }
            dock.pending.take()
        });
        if let Some(cmd) = pending {
            self.run_dock(el, cmd);
        }
        if let Some(toast) = self.toast.as_mut() {
            if let Some(t) = toast.tick(now) {
                wake(t);
            }
            if toast.closed() {
                self.toast = None;
            }
        }
        let wants_paint = self.toast.as_ref().is_some_and(|t| t.needs_paint)
            || self.dock.as_ref().is_some_and(|d| d.needs_paint)
            || self.notes.values().any(|n| n.needs_paint && n.shown);
        if wants_paint && now < self.next_frame {
            wake(self.next_frame);
        } else if wants_paint {
            self.next_frame = now + self.frame;
            if let Some(toast) = self.toast.as_mut().filter(|t| t.needs_paint) {
                toast.render(&mut self.shared);
            }
            if let Some(dock) = self.dock.as_mut().filter(|d| d.needs_paint) {
                let t = Instant::now();
                dock.render(&mut self.shared);
                trace(&mut self.trace, "dock", t);
            }
            for note in self.notes.values_mut().filter(|n| n.needs_paint && n.shown) {
                let t = Instant::now();
                note.render(&mut self.shared);
                trace(&mut self.trace, "note", t);
            }
            self.trim_at = Some(now + IDLE_TRIM);
        }
        if let Some(at) = self.trim_at {
            if now >= at {
                self.trim_idle();
            } else {
                wake(at);
            }
        }
        if let Some((until, at)) = self.config_watch {
            if now >= until {
                self.config_watch = None;
            } else if now >= at {
                self.reload_config();
                self.config_watch = Some((until, now + CONFIG_POLL));
                wake(now + CONFIG_POLL);
            } else {
                wake(at);
            }
        }
        if now - self.last_trim > SHAPE_CACHE_TRIM {
            self.last_trim = now;
            self.shared.fonts.shape_run_cache.trim(4);
        }

        if dirty && self.save_at.is_none() {
            self.save_at = Some(now + SAVE_DELAY);
        }
        if let Some(at) = self.save_at {
            if at <= now {
                self.flush();
            } else {
                wake(at);
            }
        }
        if let Some(t) = shots::step(self, el) {
            wake(t);
        }
        el.set_control_flow(next.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }

    fn exiting(&mut self, _el: &ActiveEventLoop) {
        if self.bench.is_none() {
            // Save everything once more so each note's desktop is recorded.
            for n in self.notes.values_mut() {
                n.dirty = true;
            }
            self.flush();
        }
    }
}

/// The app icon for windows (taskbar, Alt-Tab).
fn app_icon() -> Option<winit::window::Icon> {
    let (rgba, size) = app_icon_rgba()?;
    winit::window::Icon::from_rgba(rgba, size, size).ok()
}

/// The app icon as straight RGBA and its size: assets/icon-64.png, cropped to the artwork
/// (see scripts/make-icons.py).
fn app_icon_rgba() -> Option<(Vec<u8>, u32)> {
    let pixmap = tiny_skia::Pixmap::decode_png(include_bytes!("../../../assets/icon-64.png")).ok()?;
    let rgba = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    Some((rgba, pixmap.width()))
}

fn trace(file: &mut Option<std::fs::File>, what: &str, since: Instant) {
    if let Some(f) = file {
        use std::io::Write;
        let _ = writeln!(f, "{what} {:.3}", since.elapsed().as_secs_f64() * 1000.0);
    }
}

/// System fonts with platform UI families as the generic sans/mono defaults.
fn font_system() -> FontSystem {
    let mut fonts = FontSystem::new();
    let db = fonts.db_mut();
    let has =
        |db: &cosmic_text::fontdb::Database, name: &str| db.faces().any(|f| f.families.iter().any(|(n, _)| n == name));
    let (sans, mono): (&[&str], &[&str]) = if cfg!(windows) {
        (&["Segoe UI Variable", "Segoe UI"], &["Cascadia Mono", "Consolas"])
    } else if cfg!(target_os = "macos") {
        (&["Helvetica Neue"], &["SF Mono", "Menlo"])
    } else {
        (&["Noto Sans", "DejaVu Sans"], &["Noto Sans Mono", "DejaVu Sans Mono"])
    };
    if let Some(f) = sans.iter().find(|f| has(db, f)) {
        db.set_sans_serif_family(*f);
    }
    if let Some(f) = mono.iter().find(|f| has(db, f)) {
        db.set_monospace_family(*f);
    }
    fonts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_icon_decodes() {
        let (rgba, size) = app_icon_rgba().expect("assets/icon-64.png decodes");
        assert_eq!(size, 64);
        assert_eq!(rgba.len(), 64 * 64 * 4);
        // Transparent corner, opaque artwork in the middle.
        assert_eq!(rgba[3], 0);
        assert_eq!(rgba[(32 * 64 + 32) * 4 + 3], 255);
    }

    #[test]
    fn big_benchmark_note_is_well_formed() {
        let text = big_note();
        assert_eq!(text.lines().filter(|l| l.starts_with("## Section")).count(), 60);
        assert!(text.lines().all(|l| !l.starts_with(' ')), "no stray indentation");
    }
}
