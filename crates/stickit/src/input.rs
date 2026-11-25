//! Mouse button and modifier state asked of the OS directly. Window events alone can't be
//! trusted for these: a window without keyboard focus doesn't see modifier changes, and on
//! macOS a focused window keeps getting pointer moves from anywhere on screen while the matching
//! button release can go to another window.

/// The primary (left) mouse button is held right now.
pub fn left_button_down() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_LBUTTON};
        unsafe { GetKeyState(VK_LBUTTON as i32) < 0 }
    }
    #[cfg(target_os = "macos")]
    {
        let buttons: usize = unsafe { objc2::msg_send![objc2::class!(NSEvent), pressedMouseButtons] };
        buttons & 1 != 0
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        true
    }
}

/// Ctrl (or ⌘ on macOS) is held right now.
pub fn resize_modifier_held() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL};
        unsafe { GetKeyState(VK_CONTROL as i32) < 0 }
    }
    #[cfg(target_os = "macos")]
    {
        // NSEventModifierFlagControl | NSEventModifierFlagCommand
        let flags: usize = unsafe { objc2::msg_send![objc2::class!(NSEvent), modifierFlags] };
        flags & ((1 << 18) | (1 << 20)) != 0
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        false
    }
}

/// `STICKIT_DEBUG=1`: print pointer and window events to stderr (for diagnosing platform quirks).
pub fn debug() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("STICKIT_DEBUG").is_some_and(|v| v != "0"))
}

/// Limits where `window` takes clicks to `area` (logical px from its top-left: x, y, w, h);
/// clicks elsewhere go to whatever is under it. macOS only: there the faint shadow a note draws
/// around its card catches clicks meant for the window below. Uses CoreGraphics' window event
/// shape, looked up at runtime; without it, the whole window keeps taking clicks. Returns
/// whether the area is in place (always, where there is nothing to do).
pub fn set_click_area(window: &winit::window::Window, area: (f64, f64, f64, f64)) -> bool {
    #[cfg(target_os = "macos")]
    return mac::set_click_area(window, area);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (window, area);
        true
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use std::ffi::{c_char, c_void, CStr};
    use std::sync::OnceLock;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    type MainConnection = unsafe extern "C" fn() -> i32;
    type NewRegion = unsafe extern "C" fn(*const CGRect, *mut *mut c_void) -> i32;
    type SetEventShape = unsafe extern "C" fn(i32, i32, *mut c_void) -> i32;
    type ReleaseRegion = unsafe extern "C" fn(*mut c_void) -> i32;

    extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }
    const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

    struct Fns {
        conn: i32,
        new_region: NewRegion,
        set_shape: SetEventShape,
        release: ReleaseRegion,
    }

    fn fns() -> Option<&'static Fns> {
        static FNS: OnceLock<Option<Fns>> = OnceLock::new();
        FNS.get_or_init(|| unsafe {
            unsafe fn lookup<T: Copy>(name: &CStr) -> Option<T> {
                let p = unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) };
                // SAFETY: T is the function pointer type of `name`.
                (!p.is_null()).then(|| unsafe { std::mem::transmute_copy::<*mut c_void, T>(&p) })
            }
            let conn: MainConnection = lookup(c"CGSMainConnectionID")?;
            Some(Fns {
                conn: conn(),
                new_region: lookup(c"CGSNewRegionWithRect")?,
                set_shape: lookup(c"CGSSetWindowEventShape")?,
                release: lookup(c"CGSReleaseRegion")?,
            })
        })
        .as_ref()
    }

    pub fn set_click_area(window: &winit::window::Window, (x, y, w, h): (f64, f64, f64, f64)) -> bool {
        // Nothing to call: the whole window takes clicks, as it always did.
        let Some(f) = fns() else { return true };
        let Ok(handle) = window.window_handle() else { return false };
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return false };
        // SAFETY: winit's handle holds a valid NSView; the event loop runs on the main thread.
        let view: &AnyObject = unsafe { handle.ns_view.cast().as_ref() };
        let Some(ns_window): Option<Retained<AnyObject>> = (unsafe { msg_send![view, window] }) else {
            return false;
        };
        let number: isize = unsafe { msg_send![&*ns_window, windowNumber] };
        if number <= 0 {
            return false;
        }
        let rect = CGRect::new(CGPoint::new(x, y), CGSize::new(w.max(0.0), h.max(0.0)));
        let mut region: *mut c_void = std::ptr::null_mut();
        // SAFETY: CoreGraphics' declarations; the region is released once applied.
        unsafe {
            if (f.new_region)(&rect, &mut region) != 0 || region.is_null() {
                return false;
            }
            let applied = (f.set_shape)(f.conn, number as i32, region) == 0;
            (f.release)(region);
            applied
        }
    }
}

/// `(x, y)` (window-relative, physical px) lies inside a `size` window.
pub fn inside(x: f64, y: f64, size: winit::dpi::PhysicalSize<u32>) -> bool {
    x >= 0.0 && y >= 0.0 && x < size.width as f64 && y < size.height as f64
}

/// Shows `icon` at once, besides `Window::set_cursor`. macOS applies a window's cursor only when
/// the pointer next moves with no button held, so one set at the start of a drag would appear
/// after the release, and one set at the release only once the pointer moves again. Elsewhere
/// `Window::set_cursor` alone is enough.
pub fn show_cursor(icon: winit::window::CursorIcon) {
    #[cfg(not(target_os = "macos"))]
    let _ = icon;
    #[cfg(target_os = "macos")]
    {
        use objc2::rc::{Allocated, Retained};
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};
        use objc2_core_foundation::CGPoint;
        use std::ffi::CString;

        unsafe fn ns_string(s: &str) -> Option<Retained<AnyObject>> {
            let c = CString::new(s).ok()?;
            unsafe { msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()] }
        }
        /// WebKit's move cursor: the one winit shows for `CursorIcon::Move`.
        unsafe fn load() -> Option<Retained<AnyObject>> {
            let dir = "/System/Library/Frameworks/ApplicationServices.framework/Versions/A/Frameworks/\
                       HIServices.framework/Versions/A/Resources/cursors/move";
            unsafe {
                let pdf = ns_string(&format!("{dir}/cursor.pdf"))?;
                let image: Allocated<AnyObject> = msg_send![class!(NSImage), alloc];
                let image: Option<Retained<AnyObject>> = msg_send![image, initByReferencingFile: &*pdf];
                let image = image?;
                let plist = ns_string(&format!("{dir}/info.plist"))?;
                let info: Option<Retained<AnyObject>> =
                    msg_send![class!(NSDictionary), dictionaryWithContentsOfFile: &*plist];
                let hot = |key: &str| -> f64 {
                    let (Some(info), Some(key)) = (info.as_ref(), ns_string(key)) else { return 0.0 };
                    let n: Option<Retained<AnyObject>> = msg_send![&**info, objectForKey: &*key];
                    n.map_or(0.0, |n| msg_send![&*n, doubleValue])
                };
                let hotspot = CGPoint::new(hot("hotx"), hot("hoty"));
                let cursor: Allocated<AnyObject> = msg_send![class!(NSCursor), alloc];
                msg_send![cursor, initWithImage: &*image, hotSpot: hotspot]
            }
        }
        thread_local! {
            static MOVE: Option<Retained<AnyObject>> = unsafe { load() };
        }
        use winit::window::CursorIcon;
        let cursor: Option<Retained<AnyObject>> = match icon {
            CursorIcon::Move => MOVE.with(|c| c.clone()),
            CursorIcon::Pointer => unsafe { msg_send![class!(NSCursor), pointingHandCursor] },
            CursorIcon::Text => unsafe { msg_send![class!(NSCursor), IBeamCursor] },
            CursorIcon::Grab => unsafe { msg_send![class!(NSCursor), openHandCursor] },
            CursorIcon::Grabbing => unsafe { msg_send![class!(NSCursor), closedHandCursor] },
            _ => unsafe { msg_send![class!(NSCursor), arrowCursor] },
        };
        if let Some(cursor) = cursor {
            let _: () = unsafe { msg_send![&*cursor, set] };
        }
    }
}

/// Opens a web or mail link in the default browser or mail app (callers pass only `http(s)://`
/// and `mailto:` addresses).
pub fn open_url(url: &str) {
    #[cfg(windows)]
    let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}
