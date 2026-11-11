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
    #[cfg(target_os = "linux")]
    _lock: std::fs::File,
}

/// FNV-1a: stable across builds, so a preview build and a release build agree on the name.
fn key_hash(key: &Path) -> u64 {
    #[cfg(not(target_os = "linux"))]
    let text = key.to_string_lossy().to_lowercase();
    #[cfg(not(target_os = "linux"))]
    let bytes = text.as_bytes();
    #[cfg(target_os = "linux")]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        key.as_os_str().as_bytes()
    };
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
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
#[cfg(all(unix, not(target_os = "linux")))]
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

#[cfg(target_os = "linux")]
mod imp {
    use super::Instance;
    use rustix::fs::{flock, FlockOperation};
    use std::fs::{self, File, OpenOptions};
    use std::os::unix::{
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        net::{UnixListener, UnixStream},
    };
    use std::path::Path;

    fn acquire_at(dir: &Path, hash: u64) -> std::io::Result<Option<Instance>> {
        match fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let meta = fs::symlink_metadata(dir)?;
        if !meta.is_dir() || meta.uid() != rustix::process::getuid().as_raw() || meta.mode() & 0o077 != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "instance directory must be owned by this user with mode 0700",
            ));
        }
        let name = format!("{hash:016x}");
        let lock: File = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(dir.join(format!("{name}.lock")))?;
        let socket = dir.join(format!("{name}.sock"));
        match flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(rustix::io::Errno::WOULDBLOCK) => {
                // The owner can be between locking and binding its socket.
                for _ in 0..20 {
                    if UnixStream::connect(&socket).is_ok() {
                        return Ok(None);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "running instance did not answer"));
            }
            Err(e) => return Err(e.into()),
        }
        match fs::remove_file(&socket) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let listener = UnixListener::bind(socket)?;
        Ok(Some(Instance { listener: Some(listener), _lock: lock }))
    }

    pub fn acquire(hash: u64) -> Option<Instance> {
        let dir = dirs::runtime_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join(format!("stickitt-{}", rustix::process::getuid().as_raw()));
        match acquire_at(&dir, hash) {
            Ok(instance) => instance,
            Err(e) => {
                eprintln!("stickit: cannot acquire instance lock: {e}");
                std::process::exit(1);
            }
        }
    }

    pub fn listen(instance: Instance, on_signal: impl Fn() + Send + 'static) {
        std::thread::Builder::new()
            .name("stickit-instance".into())
            .spawn(move || {
                let _lock = instance._lock;
                let Some(listener) = instance.listener else { return };
                for stream in listener.incoming() {
                    if stream.is_ok() {
                        on_signal();
                    }
                }
            })
            .expect("instance listener thread");
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn lock_serializes_startup_and_recovers_stale_socket() {
            let dir = std::env::temp_dir().join(format!("stickitt-lock-test-{}", std::process::id()));
            let first = acquire_at(&dir, 1).unwrap().unwrap();
            assert!(acquire_at(&dir, 1).unwrap().is_none());
            assert!(first.listener.as_ref().unwrap().accept().is_ok());
            drop(first);
            let recovered = acquire_at(&dir, 1).unwrap().unwrap();
            assert!(acquire_at(&dir, 2).unwrap().is_some());
            drop(recovered);
            fs::remove_dir_all(dir).unwrap();
        }

        #[test]
        fn shared_directory_is_rejected() {
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().join(format!("stickitt-lock-mode-test-{}", std::process::id()));
            fs::create_dir(&dir).unwrap();
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(acquire_at(&dir, 1).err().unwrap().kind(), std::io::ErrorKind::PermissionDenied);
            fs::remove_dir(dir).unwrap();
        }
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
    #[cfg(not(target_os = "linux"))]
    fn key_ignores_case() {
        assert_eq!(key_hash(Path::new("C:\\A\\stickit.db")), key_hash(Path::new("c:\\a\\STICKIT.db")));
        assert_ne!(key_hash(Path::new("a.db")), key_hash(Path::new("b.db")));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_database_names_are_case_sensitive() {
        assert_ne!(key_hash(Path::new("a.db")), key_hash(Path::new("A.db")));
        assert_eq!(key_hash(Path::new("a.db")), key_hash(Path::new("a.db")));
    }
}
