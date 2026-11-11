//! KWin 6/7 Wayland stopped exporting real workspaces through X11's EWMH root.
//! A transient, PID-scoped script reports only our windows and applies their workspace rules.

use crate::app::UserEvent;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{mpsc, Arc, Mutex},
};
use tokio::sync::mpsc as async_mpsc;
use winit::{event_loop::EventLoopProxy, window::Window};

#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
struct Snapshot {
    current: String,
    desktops: Vec<(String, String)>,
    windows: Vec<OwnedWindow>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
struct OwnedWindow {
    name: String,
    desktops: Vec<String>,
}

enum Command {
    Move(String, Option<String>),
    Updated,
}

static STATE: Mutex<Option<Snapshot>> = Mutex::new(None);
static SENDER: Mutex<Option<async_mpsc::UnboundedSender<Command>>> = Mutex::new(None);

pub fn instance(role: &str) -> String {
    format!("stickitt-{}-{role}", std::process::id())
}

fn name(window: u32) -> Option<String> {
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};
    let x = crate::linux::x11()?;
    let p = x.conn.get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256).ok()?.reply().ok()?;
    String::from_utf8(p.value.split(|&b| b == 0).next()?.to_vec()).ok()
}

pub fn active() -> bool {
    STATE.lock().is_ok_and(|s| s.is_some())
}

pub fn current() -> Option<String> {
    STATE.lock().ok()?.as_ref().map(|s| s.current.clone())
}

pub fn list() -> Option<Vec<(String, Option<String>)>> {
    Some(STATE.lock().ok()?.as_ref()?.desktops.iter().map(|(id, name)| (id.clone(), Some(name.clone()))).collect())
}

pub fn of(window: u32) -> Option<Vec<String>> {
    let name = name(window)?;
    STATE.lock().ok()?.as_ref()?.windows.iter().find(|w| w.name == name).map(|w| w.desktops.clone())
}

pub fn move_to(window: &Window, desktop: Option<&str>) -> bool {
    let Some(id) = crate::linux::xid(window) else { return false };
    move_id(id, desktop)
}

pub fn move_id(window: u32, desktop: Option<&str>) -> bool {
    let Some(name) = name(window).filter(|n| n.starts_with(&instance(""))) else { return false };
    let Ok(state) = STATE.lock() else { return false };
    let Some(state) = state.as_ref() else { return false };
    if desktop.is_some_and(|id| !state.desktops.iter().any(|(d, _)| d == id)) {
        return false;
    }
    SENDER
        .lock()
        .ok()
        .and_then(|tx| tx.as_ref().map(|tx| tx.send(Command::Move(name, desktop.map(str::to_owned))).is_ok()))
        .unwrap_or(false)
}

struct Service {
    proxy: EventLoopProxy<UserEvent>,
    ready: Arc<Mutex<Option<mpsc::SyncSender<bool>>>>,
    tx: async_mpsc::UnboundedSender<Command>,
}

#[zbus::interface(name = "dev.stickitt.Windows")]
impl Service {
    fn snapshot(&self) -> String {
        STATE.lock().ok().and_then(|s| s.as_ref().and_then(|s| serde_json::to_string(s).ok())).unwrap_or_default()
    }
    fn update(&self, json: &str) {
        if json.len() > 1024 * 1024 {
            return;
        }
        let Ok(snapshot) = serde_json::from_str::<Snapshot>(json) else { return };
        if snapshot.desktops.len() > 256 || snapshot.windows.iter().any(|w| !w.name.starts_with(&instance(""))) {
            return;
        }
        if let Ok(mut state) = STATE.lock() {
            if state.as_ref() == Some(&snapshot) {
                return;
            }
            *state = Some(snapshot);
        }
        if let Ok(mut ready) = self.ready.lock() {
            if let Some(ready) = ready.take() {
                let _ = ready.send(true);
            }
        }
        let _ = self.tx.send(Command::Updated);
        let _ = self.proxy.send_event(UserEvent::DesktopsChanged);
    }
}

pub struct Bridge {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    done: mpsc::Receiver<()>,
}

impl Bridge {
    pub fn new(proxy: EventLoopProxy<UserEvent>) -> Option<Self> {
        if !std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.split(':').any(|v| v.eq_ignore_ascii_case("KDE")))
            || !std::env::var("XDG_SESSION_TYPE").is_ok_and(|s| s == "wayland")
        {
            return None;
        }
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let ready = Arc::new(Mutex::new(Some(ready_tx)));
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let (done_tx, done) = mpsc::channel();
        std::thread::Builder::new()
            .name("stickit-kwin".into())
            .spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| e.to_string())
                    .and_then(|runtime| {
                        runtime.block_on(serve(proxy, ready.clone(), stopped)).map_err(|e| e.to_string())
                    });
                if let Err(e) = result {
                    eprintln!("stickit: KDE workspace integration: {e}");
                }
                if let Ok(mut ready) = ready.lock() {
                    if let Some(ready) = ready.take() {
                        let _ = ready.send(false);
                    }
                }
                let _ = done_tx.send(());
            })
            .ok()?;
        let bridge = Self { stop: Some(stop), done };
        if ready_rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap_or(false) {
            Some(bridge)
        } else {
            eprintln!("stickit: KDE workspaces unavailable; using the X11 workspace view");
            None
        }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = self.done.recv_timeout(std::time::Duration::from_secs(1));
    }
}

async fn run_script(connection: &zbus::Connection, path: &std::path::Path, name: &str) -> zbus::Result<()> {
    let scripting = zbus::Proxy::new(connection, "org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting").await?;
    let id: i32 = scripting.call("loadScript", &(path.to_string_lossy().as_ref(), name)).await?;
    if id < 0 {
        return Err(zbus::Error::Failure("KWin rejected workspace script".into()));
    }
    let path = format!("/Scripting/Script{id}");
    let script = zbus::Proxy::new(connection, "org.kde.KWin", path, "org.kde.kwin.Script").await?;
    script.call::<_, _, ()>("run", &()).await
}

async fn unload(connection: &zbus::Connection, name: &str) {
    if let Ok(scripting) = zbus::Proxy::new(connection, "org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting").await {
        let _ = scripting.call::<_, _, bool>("unloadScript", &(name,)).await;
    }
}

async fn serve(
    proxy: EventLoopProxy<UserEvent>,
    ready: Arc<Mutex<Option<mpsc::SyncSender<bool>>>>,
    mut stopped: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), Box<dyn std::error::Error>> {
    let bus = format!("dev.stickitt.Windows.p{}", std::process::id());
    let (tx, mut rx) = async_mpsc::unbounded_channel();
    let connection = zbus::connection::Builder::session()?
        .name(bus.as_str())?
        .serve_at("/dev/stickitt/Windows", Service { proxy, ready, tx: tx.clone() })?
        .build()
        .await?;
    let dir = tempfile::Builder::new().prefix("stickitt-kwin-").tempdir()?;
    let watcher = dir.path().join("watch.js");
    let script = include_str!("kwin.js")
        .replace("@PID@", &std::process::id().to_string())
        .replace("@BUS@", &serde_json::to_string(&bus)?);
    std::fs::write(&watcher, script)?;
    let watcher_name = format!("stickitt-{}-watch", std::process::id());
    let command_name = format!("stickitt-{}-command", std::process::id());
    *SENDER.lock().map_err(|_| "workspace sender poisoned")? = Some(tx);
    if let Err(e) = run_script(&connection, &watcher, &watcher_name).await {
        unload(&connection, &watcher_name).await;
        *SENDER.lock().map_err(|_| "workspace sender poisoned")? = None;
        *STATE.lock().map_err(|_| "workspace state poisoned")? = None;
        return Err(e.into());
    }
    let mut pending: HashMap<String, Option<String>> = HashMap::new();
    loop {
        let command = tokio::select! { _ = &mut stopped => break, command = rx.recv() => command };
        match command {
            Some(Command::Move(name, desktop)) => {
                pending.insert(name, desktop);
            }
            Some(Command::Updated) => {}
            None => break,
        }
        let known = STATE
            .lock()
            .ok()
            .and_then(|s| s.as_ref().map(|s| s.windows.iter().map(|w| w.name.clone()).collect::<Vec<_>>()))
            .unwrap_or_default();
        let commands: Vec<_> =
            known.into_iter().filter_map(|name| pending.remove(&name).map(|desktop| (name, desktop))).collect();
        if commands.is_empty() {
            continue;
        }
        let script = format!("const rules={}; for (const w of workspace.windowList()) {{ if(w.pid!=={}) continue; for(const [name,id] of rules) {{ if(w.resourceName!==name) continue; if(id===null) w.onAllDesktops=true; else {{ const d=workspace.desktops.find(d=>d.id===id); if(d) w.desktops=[d]; }} }} }}", serde_json::to_string(&commands)?, std::process::id());
        let path = dir.path().join("command.js");
        std::fs::write(&path, script)?;
        if let Err(e) = run_script(&connection, &path, &command_name).await {
            eprintln!("stickit: cannot move note to KDE workspace: {e}");
        }
        unload(&connection, &command_name).await;
    }
    unload(&connection, &command_name).await;
    unload(&connection, &watcher_name).await;
    *SENDER.lock().map_err(|_| "workspace sender poisoned")? = None;
    *STATE.lock().map_err(|_| "workspace state poisoned")? = None;
    Ok(())
}
