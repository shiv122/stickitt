//! Which virtual desktop a window lives on. Windows 10+: the documented
//! `IVirtualDesktopManager` COM interface, desktops identified by GUID strings. macOS: Spaces,
//! identified by their space id; whether a window is on the current Space is public API, which
//! Space it is on and moving it are CoreGraphics calls looked up at runtime (when a macOS
//! release drops them, notes simply stay where they are). Elsewhere every call is a no-op.

use winit::window::Window;

pub struct Desktops {
    inner: imp::Manager,
}

impl Desktops {
    pub fn new() -> Self {
        Self { inner: imp::Manager::new() }
    }

    /// The desktop `window` is on, if the OS tracks it.
    pub fn of(&self, window: &Window) -> Option<String> {
        self.inner.of(imp::hwnd(window)?)
    }

    /// Whether `window` is on the desktop the user is looking at (true when unknown).
    pub fn is_current(&self, window: &Window) -> bool {
        imp::hwnd(window).and_then(|h| self.inner.is_current(h)).unwrap_or(true)
    }

    pub fn move_to(&self, window: &Window, desktop: &str) -> bool {
        imp::hwnd(window).is_some_and(|h| self.inner.move_to(h, desktop))
    }

    /// The desktop of whatever app currently has focus: the one the user is on.
    pub fn of_foreground(&self) -> Option<String> {
        self.inner.of_foreground()
    }

    /// Every desktop, in order, as (id, name): the user's names where they gave one (Windows),
    /// else "Desktop 1", "Desktop 2"…. Empty where the OS doesn't say.
    pub fn list(&self) -> Vec<(String, String)> {
        self.inner
            .list()
            .into_iter()
            .enumerate()
            .map(|(i, (id, name))| (id, name.unwrap_or(format!("Desktop {}", i + 1))))
            .collect()
    }
}

/// Shows `window` on every desktop (macOS: every Space, full-screen apps included), or only on
/// its own. Windows does this through the taskbar entry instead (see `Note::set_taskbar`).
pub fn show_on_all(window: &Window, on: bool) {
    #[cfg(target_os = "macos")]
    imp::show_on_all(window, on);
    #[cfg(not(target_os = "macos"))]
    let _ = (window, on);
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::ptr::null_mut;
    use windows_sys::core::{GUID, HRESULT};
    use windows_sys::Win32::Foundation::{BOOL, HWND};
    use windows_sys::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED};
    use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;

    const CLSID: GUID = GUID::from_u128(0xaa509086_5ca9_4c25_8f95_589d3c07b48a);
    const IID: GUID = GUID::from_u128(0xa5cd92ff_29be_454c_8d04_d82879fb3f1b);

    /// `IVirtualDesktopManager`'s vtable (after IUnknown's three entries).
    #[repr(C)]
    struct Vtbl {
        _query_interface: usize,
        _add_ref: usize,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        is_window_on_current: unsafe extern "system" fn(*mut c_void, HWND, *mut BOOL) -> HRESULT,
        get_window_desktop_id: unsafe extern "system" fn(*mut c_void, HWND, *mut GUID) -> HRESULT,
        move_window_to_desktop: unsafe extern "system" fn(*mut c_void, HWND, *const GUID) -> HRESULT,
    }

    pub struct Manager(*mut c_void);

    pub fn hwnd(window: &Window) -> Option<HWND> {
        match window.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
            _ => None,
        }
    }

    impl Manager {
        pub fn new() -> Self {
            let mut ptr = null_mut();
            unsafe {
                // Already initialised by winit on this thread; this just balances the call.
                CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
                if CoCreateInstance(&CLSID, null_mut(), CLSCTX_ALL, &IID, &mut ptr) < 0 {
                    ptr = null_mut();
                }
            }
            Self(ptr)
        }

        fn vtbl(&self) -> Option<&Vtbl> {
            (!self.0.is_null()).then(|| unsafe { &**(self.0 as *const *const Vtbl) })
        }

        pub fn of(&self, hwnd: HWND) -> Option<String> {
            let v = self.vtbl()?;
            let mut id: GUID = unsafe { std::mem::zeroed() };
            let ok = unsafe { (v.get_window_desktop_id)(self.0, hwnd, &mut id) } >= 0;
            let empty = id.data1 == 0 && id.data2 == 0 && id.data3 == 0 && id.data4 == [0; 8];
            (ok && !empty).then(|| format_guid(&id))
        }

        pub fn is_current(&self, hwnd: HWND) -> Option<bool> {
            let v = self.vtbl()?;
            let mut on: BOOL = 1;
            (unsafe { (v.is_window_on_current)(self.0, hwnd, &mut on) } >= 0).then_some(on != 0)
        }

        pub fn move_to(&self, hwnd: HWND, desktop: &str) -> bool {
            let (Some(v), Some(id)) = (self.vtbl(), parse_guid(desktop)) else { return false };
            unsafe { (v.move_window_to_desktop)(self.0, hwnd, &id) >= 0 }
        }

        pub fn of_foreground(&self) -> Option<String> {
            let fg = unsafe { GetForegroundWindow() };
            (fg != 0).then(|| self.of(fg)).flatten()
        }

        /// The desktops Explorer lists (`VirtualDesktopIDs`, 16-byte GUIDs in order) with the
        /// names the user gave them.
        pub fn list(&self) -> Vec<(String, Option<String>)> {
            const KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\VirtualDesktops";
            let Some(ids) = registry(KEY, "VirtualDesktopIDs", RRF_RT_REG_BINARY) else { return Vec::new() };
            ids.chunks_exact(16)
                .map(|b| {
                    let g = GUID {
                        data1: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                        data2: u16::from_le_bytes([b[4], b[5]]),
                        data3: u16::from_le_bytes([b[6], b[7]]),
                        data4: [b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]],
                    };
                    let id = format_guid(&g);
                    let name = registry(&format!("{KEY}\\Desktops\\{id}"), "Name", RRF_RT_REG_SZ).and_then(|raw| {
                        let wide: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                        let name = String::from_utf16_lossy(&wide).trim_end_matches('\0').trim().to_string();
                        (!name.is_empty()).then_some(name)
                    });
                    (id, name)
                })
                .collect()
        }
    }

    /// A value under HKEY_CURRENT_USER, as raw bytes.
    fn registry(key: &str, value: &str, kind: u32) -> Option<Vec<u8>> {
        use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER};
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        let (key, value) = (wide(key), wide(value));
        let mut len = 0u32;
        // SAFETY: valid NUL-terminated strings; a size query first, then the read into a buffer of that size.
        unsafe {
            if RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr(), kind, null_mut(), null_mut(), &mut len)
                != 0
            {
                return None;
            }
            let mut buf = vec![0u8; len as usize];
            let status = RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                kind,
                null_mut(),
                buf.as_mut_ptr().cast(),
                &mut len,
            );
            (status == 0).then(|| {
                buf.truncate(len as usize);
                buf
            })
        }
    }
    use windows_sys::Win32::System::Registry::{RRF_RT_REG_BINARY, RRF_RT_REG_SZ};

    impl Drop for Manager {
        fn drop(&mut self) {
            if let Some(v) = self.vtbl() {
                unsafe { (v.release)(self.0) };
            }
        }
    }

    fn format_guid(g: &GUID) -> String {
        let d = g.data4;
        format!(
            "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
            g.data1, g.data2, g.data3, d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
        )
    }

    fn parse_guid(s: &str) -> Option<GUID> {
        let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        (hex.len() == 32).then(|| u128::from_str_radix(&hex, 16).ok().map(GUID::from_u128)).flatten()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn guid_roundtrip() {
            let s = "{AA509086-5CA9-4C25-8F95-589D3C07B48A}";
            assert_eq!(format_guid(&parse_guid(s).unwrap()), s);
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool};
    use objc2::{class, msg_send};
    use std::ffi::{c_char, c_void, CStr};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;

    type MainConnection = unsafe extern "C" fn() -> i32;
    type ActiveSpace = unsafe extern "C" fn(i32) -> u64;
    type SpacesForWindows = unsafe extern "C" fn(i32, i32, *const AnyObject) -> *mut AnyObject;
    type AddRemove = unsafe extern "C" fn(i32, *const AnyObject, *const AnyObject);

    extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }
    const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

    /// `NSWindowCollectionBehavior`: on every Space, kept still by Mission Control, allowed over
    /// full-screen apps, left out of window cycling.
    const ALL_SPACES: usize = (1 << 0) | (1 << 4) | (1 << 6) | (1 << 8);
    /// `kCGSAllSpacesMask`: every kind of Space a window can be on.
    const ALL_SPACE_KINDS: i32 = 7;

    struct Fns {
        conn: i32,
        active: ActiveSpace,
        spaces_for: SpacesForWindows,
        add: Option<AddRemove>,
        remove: Option<AddRemove>,
    }

    pub struct Manager(Option<Fns>);

    /// The `NSWindow` behind a winit window.
    pub fn hwnd(window: &Window) -> Option<Retained<AnyObject>> {
        let RawWindowHandle::AppKit(h) = window.window_handle().ok()?.as_raw() else { return None };
        // SAFETY: winit's handle holds a valid NSView; the event loop runs on the main thread.
        let view: &AnyObject = unsafe { h.ns_view.cast().as_ref() };
        unsafe { msg_send![view, window] }
    }

    pub fn show_on_all(window: &Window, on: bool) {
        let Some(w) = hwnd(window) else { return };
        unsafe {
            let behavior: usize = msg_send![&*w, collectionBehavior];
            let behavior = if on { behavior | ALL_SPACES } else { behavior & !ALL_SPACES };
            let _: () = msg_send![&*w, setCollectionBehavior: behavior];
        }
    }

    unsafe fn lookup<T: Copy>(name: &CStr) -> Option<T> {
        let p = unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) };
        // SAFETY: T is the function pointer type of `name`.
        (!p.is_null()).then(|| unsafe { std::mem::transmute_copy::<*mut c_void, T>(&p) })
    }

    /// An `NSArray` of one number (toll-free bridged to the `CFArray` CoreGraphics takes).
    fn one(n: u64) -> Option<Retained<AnyObject>> {
        unsafe {
            let n: Retained<AnyObject> = msg_send![class!(NSNumber), numberWithUnsignedLongLong: n];
            msg_send![class!(NSArray), arrayWithObject: &*n]
        }
    }

    impl Manager {
        pub fn new() -> Self {
            // SAFETY: the signatures match CoreGraphics' (SkyLight's) declarations.
            let fns = unsafe {
                let conn: Option<MainConnection> = lookup(c"CGSMainConnectionID");
                match (conn, lookup(c"CGSGetActiveSpace"), lookup(c"CGSCopySpacesForWindows")) {
                    (Some(conn), Some(active), Some(spaces_for)) => Some(Fns {
                        conn: conn(),
                        active,
                        spaces_for,
                        add: lookup(c"CGSAddWindowsToSpaces"),
                        remove: lookup(c"CGSRemoveWindowsFromSpaces"),
                    }),
                    _ => None,
                }
            };
            Self(fns)
        }

        /// The Spaces `w` is on.
        fn spaces(&self, w: &AnyObject) -> Vec<u64> {
            let Some(f) = &self.0 else { return Vec::new() };
            let number: isize = unsafe { msg_send![w, windowNumber] };
            let Some(ids) = one(number as u64) else { return Vec::new() };
            // SAFETY: returns a +1 CFArray of CFNumbers (or null), owned here.
            let list = unsafe { (f.spaces_for)(f.conn, ALL_SPACE_KINDS, &*ids) };
            let Some(list) = (unsafe { Retained::from_raw(list) }) else { return Vec::new() };
            let count: usize = unsafe { msg_send![&*list, count] };
            (0..count)
                .map(|i| unsafe {
                    let n: *mut AnyObject = msg_send![&*list, objectAtIndex: i];
                    msg_send![n, unsignedLongLongValue]
                })
                .collect()
        }

        pub fn of(&self, w: Retained<AnyObject>) -> Option<String> {
            // A window on every Space (or none yet) is on no one desktop.
            match self.spaces(&w)[..] {
                [one] => Some(one.to_string()),
                _ => None,
            }
        }

        pub fn is_current(&self, w: Retained<AnyObject>) -> Option<bool> {
            // A hidden window is on no Space as far as macOS is concerned.
            let visible: Bool = unsafe { msg_send![&*w, isVisible] };
            visible.as_bool().then(|| {
                let here: Bool = unsafe { msg_send![&*w, isOnActiveSpace] };
                here.as_bool()
            })
        }

        pub fn move_to(&self, w: Retained<AnyObject>, desktop: &str) -> bool {
            let (Some(f), Ok(to)) = (&self.0, desktop.parse::<u64>()) else { return false };
            let (Some(add), Some(remove)) = (f.add, f.remove) else { return false };
            let from = self.spaces(&w);
            if from == [to] {
                return true;
            }
            let number: isize = unsafe { msg_send![&*w, windowNumber] };
            let (Some(ids), Some(target)) = (one(number as u64), one(to)) else { return false };
            unsafe {
                add(f.conn, &*ids, &*target);
                for space in from.into_iter().filter(|&s| s != to) {
                    if let Some(old) = one(space) {
                        remove(f.conn, &*ids, &*old);
                    }
                }
            }
            true
        }

        pub fn of_foreground(&self) -> Option<String> {
            let f = self.0.as_ref()?;
            let space = unsafe { (f.active)(f.conn) };
            (space != 0).then(|| space.to_string())
        }

        /// Every desktop Space, display by display (full-screen app Spaces left out).
        pub fn list(&self) -> Vec<(String, Option<String>)> {
            type Managed = unsafe extern "C" fn(i32) -> *mut AnyObject;
            let Some(f) = &self.0 else { return Vec::new() };
            // SAFETY: the signature matches CoreGraphics' declaration; it returns a +1 CFArray.
            let Some(copy) = (unsafe { lookup::<Managed>(c"CGSCopyManagedDisplaySpaces") }) else { return Vec::new() };
            let Some(displays) = (unsafe { Retained::from_raw(copy(f.conn)) }) else { return Vec::new() };
            let key = |k: &CStr| -> Option<Retained<AnyObject>> {
                unsafe { msg_send![class!(NSString), stringWithUTF8String: k.as_ptr()] }
            };
            let (Some(spaces_key), Some(type_key), Some(id_key)) = (key(c"Spaces"), key(c"type"), key(c"id64")) else {
                return Vec::new();
            };
            let mut out = Vec::new();
            unsafe {
                let n: usize = msg_send![&*displays, count];
                for i in 0..n {
                    let display: *mut AnyObject = msg_send![&*displays, objectAtIndex: i];
                    let spaces: *mut AnyObject = msg_send![display, objectForKey: &*spaces_key];
                    if spaces.is_null() {
                        continue;
                    }
                    let m: usize = msg_send![spaces, count];
                    for j in 0..m {
                        let space: *mut AnyObject = msg_send![spaces, objectAtIndex: j];
                        let kind: *mut AnyObject = msg_send![space, objectForKey: &*type_key];
                        let id: *mut AnyObject = msg_send![space, objectForKey: &*id_key];
                        if kind.is_null() || id.is_null() {
                            continue;
                        }
                        let kind: i32 = msg_send![kind, intValue];
                        if kind == 0 {
                            let id: u64 = msg_send![id, unsignedLongLongValue];
                            out.push((id.to_string(), None));
                        }
                    }
                }
            }
            out
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    use winit::window::Window;

    pub struct Manager;

    pub fn hwnd(_: &Window) -> Option<()> {
        None
    }

    impl Manager {
        pub fn new() -> Self {
            Self
        }
        pub fn of(&self, _: ()) -> Option<String> {
            None
        }
        pub fn is_current(&self, _: ()) -> Option<bool> {
            None
        }
        pub fn move_to(&self, _: (), _: &str) -> bool {
            false
        }
        pub fn of_foreground(&self) -> Option<String> {
            None
        }
        pub fn list(&self) -> Vec<(String, Option<String>)> {
            Vec::new()
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn reads_the_desktop_list() {
        // Explorer's list: whatever this machine has, each a braced GUID.
        let list = super::Desktops::new().list();
        for (id, _) in &list {
            assert!(id.starts_with('{') && id.ends_with('}') && id.len() == 38, "{id}");
        }
        eprintln!("desktops: {list:?}");
    }
}
