//! One running copy per database: launching Stick-itt again signals the copy that is already
//! running (which opens its dock) and exits, instead of starting a second set of notes,
//! hotkeys and tray icons.

use std::path::Path;

/// Held by the running copy for its lifetime.
pub struct Instance {
    #[cfg(windows)]
    event: isize,
    #[cfg(unix)]
    listener: Option<std::os::unix::net::UnixListener>,
}

/// FNV-1a: stable across builds, so a preview build and a release build agree on the name.
fn key_hash(key: &Path) -> u64 {
    let text = key.to_string_lossy().to_lowercase();
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

#[cfg(windows)]
mod imp {
    use super::Instance;
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{CreateEventW, CreateMutexW, SetEvent, WaitForSingleObject, INFINITE};
    use windows_sys::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, ASFW_ANY};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    pub fn acquire(hash: u64) -> Option<Instance> {
        let base = format!("Local\\Stick-itt-{hash:016x}");
        // Both copies open the same auto-reset event first, so the signal can never be lost to
        // a copy that has taken the mutex but not yet made the event.
        let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, wide(&format!("{base}-show")).as_ptr()) };
        let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, wide(&base).as_ptr()) };
        let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if mutex == 0 || !exists {
            // The mutex handle is kept open (leaked) until the process ends.
            return Some(Instance { event });
        }
        if event != 0 {
            unsafe {
                // We were just launched by the user, so we may hand them the foreground.
                AllowSetForegroundWindow(ASFW_ANY);
                SetEvent(event);
            }
        }
        None
    }

    pub fn listen(instance: Instance, on_signal: impl Fn() + Send + 'static) {
        let event = instance.event;
        if event == 0 {
            return;
        }
        std::thread::Builder::new()
            .name("stickit-instance".into())
            .spawn(move || loop {
                if unsafe { WaitForSingleObject(event, INFINITE) } != WAIT_OBJECT_0 {
                    return;
                }
                on_signal();
            })
            .ok();
    }
}

/// macOS and other Unixes: the running copy listens on a per-user socket; a second launch
/// connects to it, which is the signal.
#[cfg(unix)]
mod imp {
    use super::Instance;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    fn socket(hash: u64) -> PathBuf {
        // $TMPDIR is per user on macOS.
        std::env::temp_dir().join(format!("stickit-{hash:016x}.sock"))
    }

    pub fn acquire(hash: u64) -> Option<Instance> {
        let path = socket(hash);
        if UnixStream::connect(&path).is_ok() {
            return None;
        }
        // Nobody answered: a stale socket from a copy that crashed.
        let _ = std::fs::remove_file(&path);
        Some(Instance { listener: UnixListener::bind(&path).ok() })
    }

    pub fn listen(instance: Instance, on_signal: impl Fn() + Send + 'static) {
        let Some(listener) = instance.listener else { return };
        std::thread::Builder::new()
            .name("stickit-instance".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if stream.is_ok() {
                        on_signal();
                    }
                }
            })
            .ok();
    }
}

/// Becomes the running copy for `key` (the database path), or, when one already runs, signals
/// it and returns `None`.
pub fn acquire(key: &Path) -> Option<Instance> {
    #[cfg(any(windows, unix))]
    return imp::acquire(key_hash(key));
    #[cfg(not(any(windows, unix)))]
    {
        let _ = key;
        Some(Instance {})
    }
}

impl Instance {
    /// Calls `on_signal` (on a background thread) whenever another launch is redirected here.
    pub fn listen(self, on_signal: impl Fn() + Send + 'static) {
        #[cfg(any(windows, unix))]
        imp::listen(self, on_signal);
        #[cfg(not(any(windows, unix)))]
        let _ = on_signal;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_ignores_case() {
        assert_eq!(key_hash(Path::new("C:\\A\\stickit.db")), key_hash(Path::new("c:\\a\\STICKIT.db")));
        assert_ne!(key_hash(Path::new("a.db")), key_hash(Path::new("b.db")));
    }
}
