//! Window pixel surfaces.
//!
//! On Windows every window is a *layered* window presented with `UpdateLayeredWindow`: the
//! alpha channel is real, so we draw our own continuous-corner card and soft shadow, and
//! fully transparent pixels are click-through. On macOS the window is non-opaque and the
//! pixels go to a Core Animation layer as a premultiplied-alpha image, to the same effect.
//! Elsewhere we fall back to softbuffer (opaque).
//!
//! Pixels are premultiplied B,G,R,A bytes in both cases.

use std::error::Error;
use std::rc::Rc;
use winit::event_loop::OwnedDisplayHandle;
use winit::window::Window;

pub type Context = softbuffer::Context<OwnedDisplayHandle>;

/// Whether the platform composites our alpha channel (and we must draw our own shadows).
pub const ALPHA: bool = cfg!(any(windows, target_os = "macos"));

pub struct Surface {
    inner: imp::Surface,
}

impl Surface {
    /// `tool` marks a utility window (the dock): on Windows it then shows on every virtual
    /// desktop instead of belonging to one.
    pub fn new(ctx: &Context, window: &Rc<Window>, tool: bool) -> Result<Self, Box<dyn Error>> {
        Ok(Self { inner: imp::Surface::new(ctx, window, tool)? })
    }

    /// Keeps the window at the bottom of the stack (behind other apps) even when clicked.
    pub fn pin_to_bottom(&mut self, on: bool) {
        self.inner.pin_to_bottom(on);
    }

    /// Frees the pixel buffer until the next paint. The system keeps its own copy of what is on
    /// screen, so an idle window needs none of its own; the next paint makes a new one.
    pub fn trim(&mut self) {
        self.inner.trim();
    }

    /// Hands `draw` a cleared `width`×`height` buffer, then puts it on screen at `opacity`
    /// (whole-window fade; only honoured where the platform composites our alpha).
    pub fn paint(&mut self, width: u32, height: u32, opacity: u8, draw: impl FnOnce(&mut [u8])) {
        if width > 0 && height > 0 {
            self.inner.paint(width, height, opacity, |buf| {
                draw(buf);
                if CAPTURE.get() {
                    LAST.replace(Some((width, height, buf.to_vec())));
                }
            });
        }
    }
}

thread_local! {
    static CAPTURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static LAST: std::cell::RefCell<Option<Frame>> = const { std::cell::RefCell::new(None) };
}

/// A painted frame: width, height and premultiplied B,G,R,A bytes.
pub type Frame = (u32, u32, Vec<u8>);

/// Keep a copy of every frame painted from now on (`--shots`); see [`take_frame`].
pub fn capture_frames() {
    CAPTURE.set(true);
}

/// The last frame painted on this thread since [`capture_frames`].
pub fn take_frame() -> Option<Frame> {
    LAST.take()
}

#[cfg(windows)]
mod imp {
    use super::Context;
    use std::error::Error;
    use std::ptr::{null, null_mut};
    use std::rc::Rc;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, SelectObject, AC_SRC_ALPHA,
        AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
    };
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, UpdateLayeredWindow, GWL_EXSTYLE, HWND_BOTTOM, HWND_TOP,
        STYLESTRUCT, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, ULW_ALPHA, WA_INACTIVE, WINDOWPOS,
        WM_ACTIVATE, WM_STYLECHANGING, WM_WINDOWPOSCHANGING, WS_EX_LAYERED, WS_EX_TOOLWINDOW,
    };
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;

    /// Subclass data flag (above the extended-style bits): hold the window at the bottom.
    const PIN_BOTTOM: usize = 1 << 40;
    /// A pinned window that is active right now: it may come to the front until deactivated.
    const RAISED: usize = 1 << 41;

    pub struct Surface {
        /// Extended-style bits kept set, plus `PIN_BOTTOM`.
        keep: usize,
        hwnd: HWND,
        dc: HDC,
        bitmap: HBITMAP,
        old: HGDIOBJ,
        bits: *mut u8,
        size: (u32, u32),
    }

    /// winit rewrites the extended style whenever window flags change (level, visibility…),
    /// which would drop our bits; this subclass keeps the ones passed in `keep` set. With
    /// `PIN_BOTTOM` it also turns every z-order change into "go to the bottom", so a note
    /// pinned to the desktop stays behind other windows, except while it is the active window
    /// (clicked, or picked on the taskbar): then it comes to the front, and sinks back once
    /// another window is activated.
    unsafe extern "system" fn keep_layered(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        keep: usize,
    ) -> LRESULT {
        if msg == WM_STYLECHANGING && wparam as i32 == GWL_EXSTYLE {
            let style = &mut *(lparam as *mut STYLESTRUCT);
            style.styleNew |= (keep & 0xFFFF_FFFF) as u32;
        }
        if msg == WM_ACTIVATE && keep & PIN_BOTTOM != 0 {
            let active = (wparam & 0xFFFF) as u32 != WA_INACTIVE;
            let keep = if active { keep | RAISED } else { keep & !RAISED };
            // Updating the subclass data from inside the subclass procedure is allowed.
            SetWindowSubclass(hwnd, Some(keep_layered), 1, keep);
            let to = if active { HWND_TOP } else { HWND_BOTTOM };
            SetWindowPos(hwnd, to, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
        if msg == WM_WINDOWPOSCHANGING && keep & PIN_BOTTOM != 0 && keep & RAISED == 0 {
            let pos = &mut *(lparam as *mut WINDOWPOS);
            if pos.flags & SWP_NOZORDER == 0 {
                pos.hwndInsertAfter = HWND_BOTTOM;
            }
        }
        DefSubclassProc(hwnd, msg, wparam, lparam)
    }

    impl Surface {
        pub fn new(_ctx: &Context, window: &Rc<Window>, tool: bool) -> Result<Self, Box<dyn Error>> {
            let RawWindowHandle::Win32(handle) = window.window_handle()?.as_raw() else {
                return Err("not a Win32 window".into());
            };
            let hwnd = handle.hwnd.get();
            unsafe {
                let keep = WS_EX_LAYERED | if tool { WS_EX_TOOLWINDOW } else { 0 };
                SetWindowSubclass(hwnd, Some(keep_layered), 1, keep as usize);
                let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | keep as isize);
                let dc = CreateCompatibleDC(0);
                if dc == 0 {
                    return Err("CreateCompatibleDC failed".into());
                }
                Ok(Self { keep: keep as usize, hwnd, dc, bitmap: 0, old: 0, bits: null_mut(), size: (0, 0) })
            }
        }

        pub fn pin_to_bottom(&mut self, on: bool) {
            let keep = if on { self.keep | PIN_BOTTOM } else { self.keep & !PIN_BOTTOM };
            if keep != self.keep {
                self.keep = keep;
                // Re-registering the same subclass id just updates its data.
                unsafe { SetWindowSubclass(self.hwnd, Some(keep_layered), 1, keep) };
            }
        }

        fn resize(&mut self, w: u32, h: u32) -> bool {
            if self.size == (w, h) && !self.bits.is_null() {
                return true;
            }
            unsafe {
                self.release_bitmap();
                let mut info: BITMAPINFO = std::mem::zeroed();
                info.bmiHeader = BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    biHeight: -(h as i32), // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..std::mem::zeroed()
                };
                let mut bits = null_mut();
                self.bitmap = CreateDIBSection(self.dc, &info, DIB_RGB_COLORS, &mut bits, 0, 0);
                if self.bitmap == 0 || bits.is_null() {
                    self.bitmap = 0;
                    return false;
                }
                self.old = SelectObject(self.dc, self.bitmap);
                self.bits = bits.cast();
                self.size = (w, h);
            }
            true
        }

        pub fn trim(&mut self) {
            unsafe { self.release_bitmap() };
            self.size = (0, 0);
        }

        unsafe fn release_bitmap(&mut self) {
            if self.bitmap != 0 {
                SelectObject(self.dc, self.old);
                DeleteObject(self.bitmap);
                self.bitmap = 0;
                self.bits = null_mut();
            }
        }

        pub fn paint(&mut self, w: u32, h: u32, opacity: u8, draw: impl FnOnce(&mut [u8])) {
            if !self.resize(w, h) {
                return;
            }
            let data = unsafe { std::slice::from_raw_parts_mut(self.bits, (w * h * 4) as usize) };
            data.fill(0);
            draw(data);
            unsafe {
                GdiFlush();
                let size = SIZE { cx: w as i32, cy: h as i32 };
                let origin = POINT { x: 0, y: 0 };
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: opacity,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                UpdateLayeredWindow(self.hwnd, 0, null(), &size, self.dc, &origin, 0, &blend, ULW_ALPHA);
            }
        }
    }

    impl Drop for Surface {
        fn drop(&mut self) {
            unsafe {
                self.release_bitmap();
                DeleteDC(self.dc);
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Context;
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{Bool, NSObject};
    use objc2_core_foundation::{CFRetained, CGPoint};
    use objc2_core_graphics::{
        CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage, CGImageAlphaInfo,
        CGImageByteOrderInfo, CGImageComponentInfo, CGImagePixelFormatInfo,
    };
    use objc2_quartz_core::{kCAGravityTopLeft, CALayer, CATransaction};
    use std::error::Error;
    use std::ffi::c_void;
    use std::ptr::{self, NonNull};
    use std::rc::Rc;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;

    pub struct Surface {
        window: Rc<Window>,
        /// Our layer, a sublayer of the view's own.
        layer: Retained<CALayer>,
        root: Retained<CALayer>,
        color_space: CFRetained<CGColorSpace>,
    }

    impl Surface {
        pub fn new(_ctx: &Context, window: &Rc<Window>, _tool: bool) -> Result<Self, Box<dyn Error>> {
            let RawWindowHandle::AppKit(handle) = window.window_handle()?.as_raw() else {
                return Err("not an AppKit window".into());
            };
            // SAFETY: winit's handle holds a valid NSView; we are on the main thread (the event loop's).
            let view: &NSObject = unsafe { handle.ns_view.cast().as_ref() };
            let root: Retained<CALayer> = unsafe {
                let _: () = msg_send![view, setWantsLayer: Bool::YES];
                let layer: Option<Retained<CALayer>> = msg_send![view, layer];
                layer.ok_or("no layer")?
            };
            unsafe {
                // We draw our own shadow; the window itself must be see-through.
                let ns_window: Option<Retained<NSObject>> = msg_send![view, window];
                if let Some(w) = ns_window {
                    let _: () = msg_send![&*w, setHasShadow: Bool::NO];
                    let _: () = msg_send![&*w, setOpaque: Bool::NO];
                }
            }
            let layer = CALayer::new();
            root.addSublayer(&layer);
            layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
            layer.setGeometryFlipped(true);
            layer.setOpaque(false);
            layer.setContentsGravity(unsafe { kCAGravityTopLeft });
            let color_space = CGColorSpace::new_device_rgb().ok_or("no colour space")?;
            Ok(Self { window: window.clone(), layer, root, color_space })
        }

        pub fn pin_to_bottom(&mut self, _on: bool) {}

        pub fn trim(&mut self) {}

        pub fn paint(&mut self, w: u32, h: u32, opacity: u8, draw: impl FnOnce(&mut [u8])) {
            unsafe extern "C-unwind" fn release(_info: *mut c_void, data: NonNull<c_void>, size: usize) {
                // SAFETY: the same boxed slice handed over below.
                drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(data.as_ptr().cast::<u8>(), size)) });
            }
            let mut pixels = vec![0u8; (w * h * 4) as usize].into_boxed_slice();
            draw(&mut pixels);
            let len = pixels.len();
            let data = Box::into_raw(pixels).cast::<c_void>();
            // SAFETY: valid pointer and length; ownership passes to the provider (freed in `release`).
            let Some(provider) = (unsafe { CGDataProvider::with_data(ptr::null_mut(), data, len, Some(release)) })
            else {
                return;
            };
            // Our buffers are premultiplied B,G,R,A: 32-bit little-endian ARGB, alpha first.
            let info = CGBitmapInfo(
                CGImageAlphaInfo::PremultipliedFirst.0
                    | CGImageComponentInfo::Integer.0
                    | CGImageByteOrderInfo::Order32Little.0
                    | CGImagePixelFormatInfo::Packed.0,
            );
            let image = unsafe {
                CGImage::new(
                    w as usize,
                    h as usize,
                    8,
                    32,
                    w as usize * 4,
                    Some(&self.color_space),
                    info,
                    Some(&provider),
                    ptr::null(),
                    false,
                    CGColorRenderingIntent::RenderingIntentDefault,
                )
            };
            let Some(image) = image else { return };
            CATransaction::begin();
            // No implicit fade between frames.
            CATransaction::setDisableActions(true);
            self.layer.setContentsScale(self.window.scale_factor());
            self.layer.setFrame(self.root.bounds());
            self.layer.setOpacity(opacity as f32 / 255.0);
            // SAFETY: a CGImage is valid layer contents.
            unsafe { self.layer.setContents(Some(image.as_ref())) };
            CATransaction::commit();
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    use super::Context;
    use std::error::Error;
    use std::num::NonZeroU32;
    use std::rc::Rc;
    use winit::event_loop::OwnedDisplayHandle;
    use winit::window::Window;

    pub struct Surface(softbuffer::Surface<OwnedDisplayHandle, Rc<Window>>);

    impl Surface {
        pub fn new(ctx: &Context, window: &Rc<Window>, _tool: bool) -> Result<Self, Box<dyn Error>> {
            Ok(Self(softbuffer::Surface::new(ctx, window.clone())?))
        }

        pub fn pin_to_bottom(&mut self, _on: bool) {}

        pub fn trim(&mut self) {}

        pub fn paint(&mut self, w: u32, h: u32, _opacity: u8, draw: impl FnOnce(&mut [u8])) {
            let (Some(nw), Some(nh)) = (NonZeroU32::new(w), NonZeroU32::new(h)) else { return };
            if self.0.resize(nw, nh).is_err() {
                return;
            }
            let Ok(mut buf) = self.0.buffer_mut() else { return };
            let data: &mut [u8] = bytemuck::cast_slice_mut(&mut buf[..]);
            data.fill(0);
            draw(data);
            let _ = buf.present();
        }
    }
}
