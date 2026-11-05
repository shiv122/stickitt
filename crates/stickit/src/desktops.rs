//! Which virtual desktop a window lives on (Windows 10+), through the documented
//! `IVirtualDesktopManager` COM interface. Desktops are identified by GUID strings so they can
//! be stored. Elsewhere every call is a no-op.

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
    }

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

#[cfg(not(windows))]
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
    }
}
