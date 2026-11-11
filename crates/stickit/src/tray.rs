//! The notification-area icon (menu bar on macOS): click it to open the dock; its menu has the
//! everyday actions and every hidden note, so nothing is ever out of reach.

use tray_icon::menu::accelerator::Accelerator;
use tray_icon::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// Hidden notes listed in the menu; the rest are in the dock.
const MAX_HIDDEN: usize = 25;
const NOTE_PREFIX: &str = "note:";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrayCmd {
    NewNote,
    PasteAsNote,
    OpenDock,
    ToggleNotes,
    /// Reveal one hidden note (by database id).
    Reveal(i64),
    RevealAll,
    Settings,
    Quit,
}

pub struct NativeTray {
    _icon: TrayIcon,
    toggle: MenuItem,
    hidden: Submenu,
    /// What the hidden-notes submenu shows now: (database id, label).
    listed: Vec<(i64, String)>,
}

/// Shortcut hints shown beside menu items (they are not registered here).
pub struct Hints<'a> {
    pub new_note: &'a str,
    pub paste_as_note: &'a str,
    pub open_dock: &'a str,
    pub toggle_notes: &'a str,
    pub settings: &'a str,
}

fn item(id: &str, text: &str, hint: &str) -> MenuItem {
    MenuItem::with_id(id, text, true, hint.parse::<Accelerator>().ok())
}

impl NativeTray {
    /// `rgba` is a square icon image.
    pub fn new(rgba: Vec<u8>, size: u32, hints: Hints) -> Option<Self> {
        let icon = Icon::from_rgba(rgba, size, size).ok()?;
        let toggle = item("toggle", "Hide all notes", hints.toggle_notes);
        let hidden = Submenu::new("Hidden notes", false);
        let menu = Menu::new();
        let items: [&dyn IsMenuItem; 10] = [
            &item("new", "New note", hints.new_note),
            &item("paste", "Paste as note", hints.paste_as_note),
            &PredefinedMenuItem::separator(),
            &item("dock", "Open dock", hints.open_dock),
            &toggle,
            &hidden,
            &PredefinedMenuItem::separator(),
            &item("settings", "Settings…", hints.settings),
            &PredefinedMenuItem::separator(),
            &item("quit", "Quit Stick-itt", ""),
        ];
        menu.append_items(&items).ok()?;
        let tray = TrayIconBuilder::new()
            .with_icon(icon)
            .with_tooltip("Stick-itt")
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .map_err(|e| eprintln!("stickit: no tray icon: {e}"))
            .ok()?;
        Some(Self { _icon: tray, toggle, hidden, listed: Vec::new() })
    }

    /// Lists the hidden notes; rebuilds the submenu only when they changed.
    pub fn set_hidden_notes(&mut self, mut notes: Vec<(i64, String)>) {
        notes.truncate(MAX_HIDDEN);
        if notes == self.listed {
            return;
        }
        for _ in 0..self.hidden.items().len() {
            let _ = self.hidden.remove_at(0);
        }
        for (id, label) in &notes {
            let _ = self.hidden.append(&MenuItem::with_id(format!("{NOTE_PREFIX}{id}"), label, true, None));
        }
        if notes.len() > 1 {
            let _ = self.hidden.append(&PredefinedMenuItem::separator());
            let _ = self.hidden.append(&MenuItem::with_id("reveal-all", "Show them all", true, None));
        }
        self.hidden.set_enabled(!notes.is_empty());
        self.hidden.set_text(if notes.is_empty() {
            "Hidden notes".to_string()
        } else {
            format!("Hidden notes ({})", notes.len())
        });
        self.listed = notes;
    }

    /// Follows "hide all notes" so the item reads as the action it will take.
    pub fn set_notes_hidden(&self, hidden: bool) {
        self.toggle.set_text(if hidden { "Show notes" } else { "Hide all notes" });
    }

    pub fn command(id: &str) -> Option<TrayCmd> {
        Some(match id {
            "new" => TrayCmd::NewNote,
            "paste" => TrayCmd::PasteAsNote,
            "dock" => TrayCmd::OpenDock,
            "toggle" => TrayCmd::ToggleNotes,
            "reveal-all" => TrayCmd::RevealAll,
            "settings" => TrayCmd::Settings,
            "quit" => TrayCmd::Quit,
            other => TrayCmd::Reveal(other.strip_prefix(NOTE_PREFIX)?.parse().ok()?),
        })
    }
}

#[cfg(not(target_os = "linux"))]
pub type Tray = NativeTray;

#[cfg(target_os = "linux")]
pub use gtk_tray::Tray;

#[cfg(target_os = "linux")]
mod gtk_tray {
    use super::{Hints, NativeTray, TrayCmd};
    use gtk::glib::{self, ControlFlow, MainContext, Priority};
    use std::sync::{mpsc, OnceLock};

    enum Command {
        Create { rgba: Vec<u8>, size: u32, hints: [String; 5], ready: mpsc::Sender<bool> },
        Hidden(Vec<(i64, String)>),
        Toggle(bool),
        Clear,
    }

    // GTK3's matching glib release uses channels to wake its loop without idle polling.
    #[allow(deprecated)]
    fn runtime() -> Option<glib::Sender<Command>> {
        static RUNTIME: OnceLock<Option<glib::Sender<Command>>> = OnceLock::new();
        RUNTIME
            .get_or_init(|| {
                let (tx, rx) = MainContext::channel(Priority::DEFAULT);
                let (ready_tx, ready_rx) = mpsc::channel();
                std::thread::Builder::new()
                    .name("stickit-tray".into())
                    .spawn(move || {
                        if let Err(e) = gtk::init() {
                            eprintln!("stickit: cannot initialize GTK tray: {e}");
                            let _ = ready_tx.send(false);
                            return;
                        }
                        let mut tray: Option<NativeTray> = None;
                        rx.attach(None, move |command| {
                            match command {
                                Command::Create { rgba, size, hints, ready } => {
                                    tray = NativeTray::new(
                                        rgba,
                                        size,
                                        Hints {
                                            new_note: &hints[0],
                                            paste_as_note: &hints[1],
                                            open_dock: &hints[2],
                                            toggle_notes: &hints[3],
                                            settings: &hints[4],
                                        },
                                    );
                                    let _ = ready.send(tray.is_some());
                                }
                                Command::Hidden(notes) => {
                                    if let Some(tray) = tray.as_mut() {
                                        tray.set_hidden_notes(notes);
                                    }
                                }
                                Command::Toggle(hidden) => {
                                    if let Some(tray) = tray.as_ref() {
                                        tray.set_notes_hidden(hidden);
                                    }
                                }
                                Command::Clear => tray = None,
                            }
                            ControlFlow::Continue
                        });
                        let _ = ready_tx.send(true);
                        gtk::main();
                    })
                    .ok()?;
                ready_rx.recv().ok()?.then_some(tx)
            })
            .clone()
    }

    #[allow(deprecated)]
    pub struct Tray(glib::Sender<Command>);

    impl Tray {
        pub fn new(rgba: Vec<u8>, size: u32, hints: Hints) -> Option<Self> {
            let tx = runtime()?;
            let (ready, rx) = mpsc::channel();
            let hints = [hints.new_note, hints.paste_as_note, hints.open_dock, hints.toggle_notes, hints.settings]
                .map(str::to_owned);
            tx.send(Command::Create { rgba, size, hints, ready }).ok()?;
            rx.recv().ok()?.then_some(Self(tx))
        }
        pub fn set_hidden_notes(&mut self, notes: Vec<(i64, String)>) {
            let _ = self.0.send(Command::Hidden(notes));
        }
        pub fn set_notes_hidden(&self, hidden: bool) {
            let _ = self.0.send(Command::Toggle(hidden));
        }
        pub fn command(id: &str) -> Option<TrayCmd> {
            NativeTray::command(id)
        }
    }

    impl Drop for Tray {
        fn drop(&mut self) {
            let _ = self.0.send(Command::Clear);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_ids_map_to_commands() {
        assert_eq!(Tray::command("quit"), Some(TrayCmd::Quit));
        assert_eq!(Tray::command("note:42"), Some(TrayCmd::Reveal(42)));
        assert_eq!(Tray::command("note:x"), None);
        assert_eq!(Tray::command("other"), None);
    }

    #[test]
    fn default_shortcuts_show_as_hints() {
        for action in crate::config::GlobalAction::ALL {
            let binding = action.default_binding();
            assert!(binding.parse::<Accelerator>().is_ok(), "{binding}");
        }
    }
}
