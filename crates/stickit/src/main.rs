#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod chime;
mod config;
mod desktops;
mod dock;
mod editor;
mod gfx;
mod input;
mod instance;
mod markdown;
mod motion;
mod note;
mod popover;
mod reminder;
mod store;
mod surface;
mod table;
mod theme;
mod toast;
mod tray;

use global_hotkey::{GlobalHotKeyEvent, HotKeyState};
use std::path::PathBuf;
use std::sync::Mutex;
use tray_icon::menu::MenuEvent;
use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};
use winit::event_loop::EventLoop;

/// Where notes and settings live. Builds before the rename used a "Stick-it" folder: it is
/// moved on first launch, or kept in use if it can't be moved (e.g. the old copy is running).
fn data_dir() -> PathBuf {
    let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    let (dir, old) = (base.join("Stick-itt"), base.join("Stick-it"));
    if !dir.exists() && old.is_dir() && std::fs::rename(&old, &dir).is_err() {
        return old;
    }
    dir
}

/// Appends every panic (message and source location) to `path`, besides the usual stderr report.
fn log_panics(path: PathBuf) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default(info);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            use std::io::Write;
            let _ = writeln!(f, "{} {info}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
        }
    }));
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut bench = None;
    let mut big = false;
    let mut db_path: Option<PathBuf> = None;
    let mut config_path: Option<PathBuf> = None;
    let mut shots: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bench" => bench = args.next().and_then(|n| n.parse().ok()),
            // With --bench: long, richly formatted notes in larger windows.
            "--big" => big = true,
            "--db" => db_path = args.next().map(PathBuf::from),
            "--config" => config_path = args.next().map(PathBuf::from),
            // Render the README's pictures into a folder (demo notes, light and dark), then quit.
            "--shots" => shots = args.next().map(PathBuf::from),
            other => eprintln!("stickit: ignoring unknown argument {other}"),
        }
    }

    if shots.is_some() {
        bench = Some(0);
    }
    let data_dir = data_dir();
    // A crash leaves its message in the data folder: a bundled app has no terminal to show it.
    log_panics(data_dir.join("stickit.log"));
    let db_path = db_path.unwrap_or_else(|| data_dir.join("stickit.db"));
    // One copy per database; launching again just brings up the running one.
    let instance = if bench.is_some() {
        None
    } else {
        match instance::acquire(&std::path::absolute(&db_path).unwrap_or_else(|_| db_path.clone())) {
            Some(i) => Some(i),
            None => return,
        }
    };
    let store = if bench.is_some() { store::Store::in_memory() } else { store::Store::open(&db_path) };
    let store = match store {
        Ok(s) => s,
        Err(e) => {
            eprintln!("stickit: cannot open database: {e}");
            std::process::exit(1);
        }
    };
    let config_path =
        if bench.is_some() { config_path } else { Some(config_path.unwrap_or_else(|| data_dir.join("stickit.ini"))) };
    let config = config::Config::load(config_path);

    let event_loop = EventLoop::<app::UserEvent>::with_user_event().build().expect("event loop");
    // Hotkeys, tray clicks, tray menu picks and relaunches arrive outside the window system;
    // forward them into the event loop.
    let send = |proxy: &Mutex<winit::event_loop::EventLoopProxy<app::UserEvent>>, e| {
        if let Ok(p) = proxy.lock() {
            let _ = p.send_event(e);
        }
    };
    let proxy = Mutex::new(event_loop.create_proxy());
    GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
        if e.state == HotKeyState::Pressed {
            send(&proxy, app::UserEvent::Hotkey(e.id));
        }
    }));
    let proxy = Mutex::new(event_loop.create_proxy());
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| send(&proxy, app::UserEvent::Menu(e.id.0))));
    let proxy = Mutex::new(event_loop.create_proxy());
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
            send(&proxy, app::UserEvent::TrayClick);
        }
    }));
    if let Some(instance) = instance {
        let proxy = Mutex::new(event_loop.create_proxy());
        instance.listen(move || send(&proxy, app::UserEvent::Relaunched));
    }
    let bench = bench.map(|notes| app::Bench { notes, big });
    let mut app = app::App::new(store, bench, shots.map(app::Shots::new), config);
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("stickit: {e}");
    }
}
