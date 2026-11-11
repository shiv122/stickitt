//! Desktop-owned global shortcuts on Wayland; never depend on XWayland key snooping.

use crate::{app::UserEvent, config::GlobalAction};
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use futures_util::StreamExt;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use tokio::sync::watch;
use winit::event_loop::EventLoopProxy;

pub const APP_ID: &str = "dev.stickitt.Stickitt";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub action: GlobalAction,
    pub hotkey: HotKey,
}

pub struct Shortcuts {
    enabled: bool,
    tx: Option<watch::Sender<Vec<Binding>>>,
    done: std::sync::mpsc::Receiver<()>,
}

impl Shortcuts {
    pub fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        let enabled = std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v == "wayland");
        let (tx, rx) = watch::channel(Vec::new());
        let (done_tx, done) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("stickit-portals".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(runtime) => runtime,
                    Err(e) => {
                        eprintln!("stickit: cannot start desktop portals: {e}");
                        let _ = proxy.send_event(UserEvent::PortalUnavailable);
                        let _ = done_tx.send(());
                        return;
                    }
                };
                runtime.block_on(async {
                    let theme = tokio::spawn(appearance(proxy.clone()));
                    if let Err(e) = shortcuts(rx, &proxy).await {
                        eprintln!(
                            "stickit: Wayland shortcut portal unavailable: {e}; using X11 shortcuts where permitted"
                        );
                        let _ = proxy.send_event(UserEvent::PortalUnavailable);
                    }
                    theme.abort();
                });
                let _ = done_tx.send(());
            })
            .expect("desktop portal thread");
        Self { enabled, tx: Some(tx), done }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn update(&self, bindings: Vec<Binding>) {
        self.tx.as_ref().expect("portal sender").send_if_modified(|old| {
            if *old == bindings {
                return false;
            }
            *old = bindings;
            true
        });
    }
}

impl Drop for Shortcuts {
    fn drop(&mut self) {
        self.tx.take();
        // Close the session on normal quit, without hanging on an unresponsive portal.
        let _ = self.done.recv_timeout(std::time::Duration::from_secs(1));
    }
}

async fn appearance(proxy: EventLoopProxy<UserEvent>) {
    use ashpd::desktop::settings::{ColorScheme, Settings};
    let Ok(settings) = Settings::new().await else { return };
    let Ok(mut changes) = settings.receive_color_scheme_changed().await else { return };
    if let Ok(scheme) = settings.color_scheme().await {
        let _ = proxy.send_event(UserEvent::SystemTheme(scheme == ColorScheme::PreferDark));
    }
    while let Some(scheme) = changes.next().await {
        if proxy.send_event(UserEvent::SystemTheme(scheme == ColorScheme::PreferDark)).is_err() {
            break;
        }
    }
}

async fn shortcuts(mut rx: watch::Receiver<Vec<Binding>>, proxy: &EventLoopProxy<UserEvent>) -> ashpd::Result<()> {
    let mut changed = false;
    loop {
        if !changed && rx.changed().await.is_err() {
            return Ok(());
        }
        changed = false;
        let bindings = rx.borrow_and_update().clone();
        if bindings.is_empty() {
            continue;
        }
        let connection = ashpd::zbus::Connection::session().await?;
        // Stable identity makes desktop-granted shortcuts survive restarting a local build.
        if let Err(e) = ashpd::register_host_app_with_connection(connection.clone(), APP_ID.parse()?).await {
            eprintln!("stickit: portal application registration: {e}");
        }
        let portal = GlobalShortcuts::with_connection(connection).await?;
        let session = portal.create_session(Default::default()).await?;
        let mut activated = portal.receive_activated().await?;
        let requests: Vec<_> = bindings
            .iter()
            .filter_map(|b| {
                let trigger = trigger(b.hotkey)?;
                Some(NewShortcut::new(b.action.key(), b.action.doc()).preferred_trigger(trigger.as_str()))
            })
            .collect();
        let result = tokio::select! {
            result = portal.bind_shortcuts(&session, &requests, None, Default::default()) => result,
            change = rx.changed() => {
                let _ = session.close().await;
                if change.is_err() { return Ok(()); }
                changed = true;
                continue;
            }
        };
        match result.and_then(|r| r.response()) {
            Ok(response) => {
                for binding in &bindings {
                    if !response.shortcuts().iter().any(|s| s.id() == binding.action.key()) {
                        eprintln!("stickit: desktop did not grant shortcut {}", binding.action.key());
                    }
                }
            }
            Err(e) => {
                // A dismissed permission dialog must not silently bypass the user's choice.
                eprintln!("stickit: desktop shortcuts were not granted: {e}. Reopen the app to request them again.");
                let _ = session.close().await;
                continue;
            }
        }
        loop {
            tokio::select! {
                change = rx.changed() => {
                    let _ = session.close().await;
                    if change.is_err() { return Ok(()); }
                    changed = true;
                    break;
                }
                event = activated.next() => {
                    let Some(event) = event else {
                        let _ = session.close().await;
                        return Err(ashpd::Error::NoResponse);
                    };
                    // This connection owns exactly one session; the portal unicasts its signals.
                    if let Some(binding) = bindings.iter().find(|b| b.action.key() == event.shortcut_id()) {
                        let _ = proxy.send_event(UserEvent::PortalAction(binding.action));
                    }
                }
            }
        }
    }
}

/// XDG uses XKB keysyms, not winit's physical key names (KeyN, ArrowUp, Equal…).
fn trigger(hotkey: HotKey) -> Option<String> {
    let raw = hotkey.key.to_string();
    let key = match hotkey.key {
        Code::Enter => "Return",
        Code::Escape => "Escape",
        Code::Backspace => "BackSpace",
        Code::Delete => "Delete",
        Code::Tab => "Tab",
        Code::Space => "space",
        Code::ArrowUp => "Up",
        Code::ArrowDown => "Down",
        Code::ArrowLeft => "Left",
        Code::ArrowRight => "Right",
        Code::Equal => "equal",
        Code::Minus => "minus",
        Code::Comma => "comma",
        Code::Period => "period",
        Code::Slash => "slash",
        Code::Backquote => "grave",
        Code::BracketLeft => "bracketleft",
        Code::BracketRight => "bracketright",
        _ if raw.starts_with("Key") => &raw[3..],
        _ if raw.starts_with("Digit") => &raw[5..],
        _ if raw.strip_prefix('F').is_some_and(|s| s.parse::<u8>().is_ok_and(|n| (1..=24).contains(&n))) => &raw,
        _ => return None,
    };
    let mut parts = Vec::new();
    for (flag, name) in
        [(Modifiers::CONTROL, "CTRL"), (Modifiers::ALT, "ALT"), (Modifiers::SHIFT, "SHIFT"), (Modifiers::META, "LOGO")]
    {
        if hotkey.mods.contains(flag) {
            parts.push(name);
        }
    }
    parts.push(key);
    Some(parts.join("+"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_use_xdg_keysyms() {
        for (binding, expected) in [
            ("CmdOrCtrl+Alt+N", "CTRL+ALT+N"),
            ("Ctrl+Alt+Comma", "CTRL+ALT+comma"),
            ("Super+Shift+ArrowUp", "SHIFT+LOGO+Up"),
            ("Ctrl+F12", "CTRL+F12"),
            ("Ctrl+1", "CTRL+1"),
        ] {
            assert_eq!(trigger(binding.parse().unwrap()).as_deref(), Some(expected));
        }
        for action in GlobalAction::ALL {
            assert!(trigger(action.default_binding().parse().unwrap()).is_some());
        }
        assert!(trigger(HotKey::new(None, Code::Unidentified)).is_none());
    }
}
