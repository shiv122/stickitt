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
