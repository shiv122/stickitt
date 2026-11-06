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
