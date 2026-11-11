//! A short system sound when a reminder rings.

/// Plays the system's notification sound (Windows) or "Glass" (macOS), without waiting for it.
pub fn play() {
    #[cfg(target_os = "linux")]
    {
        use std::io::Write;
        use std::process::{Command, Stdio};
        std::thread::Builder::new()
            .name("stickit-chime".into())
            .spawn(|| {
                // Respect the desktop's sound theme first; use the bundled chime without canberra.
                if Command::new("canberra-gtk-play")
                    .args(["--id", "message", "--description", "Stick-itt reminder"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success())
                {
                    return;
                }
                for player in ["pw-play", "paplay", "aplay"] {
                    if let Ok(mut child) = Command::new(player)
                        .arg("-")
                        .stdin(Stdio::piped())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                    {
                        if let Some(mut stdin) = child.stdin.take() {
                            let _ = stdin.write_all(include_bytes!("../../../assets/reminder.wav"));
                        }
                        if child.wait().is_ok_and(|s| s.success()) {
                            return;
                        }
                    }
                }
                eprintln!(
                    "stickit: reminder sound unavailable; install libcanberra, PipeWire, PulseAudio or ALSA utilities"
                );
            })
            .ok();
    }
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
