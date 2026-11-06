//! A short system sound when a reminder rings.

/// Plays the system's notification sound (Windows) or "Glass" (macOS), without waiting for it.
pub fn play() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
        use windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONASTERISK;
        unsafe {
            MessageBeep(MB_ICONASTERISK);
        }
    }
    #[cfg(target_os = "macos")]
    {
        use objc2::rc::Retained;
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};
        let Ok(name) = std::ffi::CString::new("Glass") else { return };
        unsafe {
            let name: Option<Retained<AnyObject>> = msg_send![class!(NSString), stringWithUTF8String: name.as_ptr()];
            let Some(name) = name else { return };
            let sound: Option<Retained<AnyObject>> = msg_send![class!(NSSound), soundNamed: &*name];
            if let Some(sound) = sound {
                let _: bool = msg_send![&*sound, play];
            }
        }
    }
}
