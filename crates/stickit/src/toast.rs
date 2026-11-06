//! "Note deleted · Undo": a small pill that rises from the bottom of the screen after a
//! delete and fades away by itself. Deleting more while it is up adds to it, and Undo brings
//! them all back. Resting the pointer on it keeps it up.

use crate::gfx::{self, Clip};
use crate::motion::{Feel, Motion};
use crate::note::{label_width, small_label, Shared};
use crate::surface::{self, Surface};
use crate::theme::{self, Rgb};
use cosmic_text::Weight;
use std::error::Error;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tiny_skia::PixmapMut;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::monitor::MonitorHandle;
use winit::window::{CursorIcon, Window, WindowLevel};

// Logical pixels.
const PILL_W: f32 = 236.0;
const PILL_H: f32 = 40.0;
/// Room around the pill for its shadow, and below it for the rise.
const MARGIN: f32 = 22.0;
const RISE: f32 = 14.0;
/// Distance from the bottom of the work area.
const BOTTOM: f32 = 72.0;
/// After the pointer leaves, it stays at least this long.
const LINGER: Duration = Duration::from_millis(2500);
const FRAME: Duration = Duration::from_millis(16);

pub enum ToastCmd {
    /// Restore these notes (database ids).
    Undo(Vec<i64>),
}

pub struct Toast {
    pub window: Rc<Window>,
    surface: Surface,
    notes: Vec<i64>,
    /// 0 = gone, 1 = up.
    shown: Motion,
    until: Instant,
    /// How long it stays up (from `[general] undo_seconds`).
    show_for: Duration,
    pointer: Option<(f64, f64)>,
    hover_undo: bool,
    dismissed: bool,
    dark: bool,
    scale: f32,
    pub needs_paint: bool,
}

impl Toast {
    /// Picks up a changed `[general] theme`.
    pub fn refresh_theme(&mut self) {
        let dark = crate::theme::is_dark(self.window.theme());
        if dark != self.dark {
            self.dark = dark;
            self.needs_paint = true;
        }
    }

    pub fn open(
        el: &ActiveEventLoop,
        ctx: &surface::Context,
        monitor: Option<MonitorHandle>,
        note: i64,
        show_for: Duration,
    ) -> Result<Toast, Box<dyn Error>> {
        let mon = monitor.or_else(|| el.primary_monitor()).ok_or("no monitor")?;
        let scale = mon.scale_factor() as f32;
        let (w, h) = Self::window_size(scale);
        let (mp, ms) = (mon.position(), mon.size());
        let x = mp.x + (ms.width as i32 - w as i32) / 2;
        let y = mp.y + ms.height as i32 - ((BOTTOM + PILL_H + MARGIN) * scale) as i32;
        let attrs = Window::default_attributes()
            // macOS: a see-through window (our pixels carry the alpha).
            .with_transparent(cfg!(target_os = "macos"))
            .with_title("Stick-itt")
            .with_decorations(false)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_active(false)
            .with_visible(false)
            .with_position(PhysicalPosition::new(x, y))
            .with_inner_size(PhysicalSize::new(w, h));
        #[cfg(windows)]
        let attrs = {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs.with_skip_taskbar(true)
        };
        let window = Rc::new(el.create_window(attrs)?);
        let surface = Surface::new(ctx, &window, true)?;
        let dark = crate::theme::is_dark(window.theme());
        let mut shown = Motion::new(0.0, Feel::Snappy);
        shown.to(1.0);
        let toast = Toast {
            window,
            surface,
            notes: vec![note],
            shown,
            until: Instant::now() + show_for,
            show_for,
            pointer: None,
            hover_undo: false,
            dismissed: false,
            dark,
            scale,
            needs_paint: true,
        };
        toast.window.set_visible(true);
        Ok(toast)
    }

    fn window_size(scale: f32) -> (u32, u32) {
        (((PILL_W + 2.0 * MARGIN) * scale) as u32, ((PILL_H + 2.0 * MARGIN + RISE) * scale) as u32)
    }

    /// Another note deleted while this is up: it joins the undo.
    pub fn add(&mut self, note: i64) {
        self.notes.push(note);
        self.until = Instant::now() + self.show_for;
        self.dismissed = false;
        self.shown.to(1.0);
        // A little nudge says "updated".
        self.shown.kick(-2.0);
        self.needs_paint = true;
    }

    pub fn dismiss(&mut self) {
        self.dismissed = true;
        self.shown.animate_to(0.0, Feel::Smooth);
        self.needs_paint = true;
    }

    /// Faded out after a dismissal: drop it.
    pub fn closed(&self) -> bool {
        self.dismissed && !self.shown.moving()
    }

    /// The pill, in window pixels, at the current rise.
    fn pill(&self) -> (f32, f32, f32, f32) {
        let s = self.scale;
        let rise = (1.0 - self.shown.value().clamp(0.0, 1.2)) * RISE * s;
        (MARGIN * s, MARGIN * s + rise, PILL_W * s, PILL_H * s)
    }

    /// The Undo button: the right part of the pill.
    fn undo_rect(&self) -> (f32, f32, f32, f32) {
        let (x, y, w, h) = self.pill();
        let bw = 76.0 * self.scale;
        (x + w - bw, y, bw, h)
    }

    pub fn handle(&mut self, event: &WindowEvent) -> Option<ToastCmd> {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Some((position.x, position.y));
                let (x, y, w, h) = self.undo_rect();
                let (px, py) = (position.x as f32, position.y as f32);
                let over = px >= x && px < x + w && py >= y && py < y + h;
                if over != self.hover_undo {
                    self.hover_undo = over;
                    self.window.set_cursor(if over { CursorIcon::Pointer } else { CursorIcon::Default });
                    self.needs_paint = true;
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.pointer = None;
                self.hover_undo = false;
                self.until = self.until.max(Instant::now() + LINGER);
                self.needs_paint = true;
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                if self.dismissed {
                    return None;
                }
                let undo = self.hover_undo;
                self.dismiss();
                if undo {
                    return Some(ToastCmd::Undo(std::mem::take(&mut self.notes)));
                }
            }
            WindowEvent::ThemeChanged(theme) => {
                self.dark = crate::theme::is_dark(Some(*theme));
                self.needs_paint = true;
            }
            WindowEvent::RedrawRequested => self.needs_paint = true,
            _ => {}
        }
        None
    }

    pub fn tick(&mut self, now: Instant) -> Option<Instant> {
        let mut next = None;
        if !self.dismissed {
            if self.pointer.is_some() {
                // Kept up while the pointer is on it.
                self.until = self.until.max(now + LINGER);
            }
            if now >= self.until {
                self.dismiss();
            } else {
                next = Some(self.until);
            }
        }
        let before = self.shown.value();
        if self.shown.tick(now) {
            next = Some(now + FRAME);
        }
        if self.shown.value() != before {
            self.needs_paint = true;
        }
        next
    }

    pub fn render(&mut self, shared: &mut Shared) {
        self.needs_paint = false;
        let size = self.window.inner_size();
        let (width, height) = (size.width, size.height);
        if width == 0 || height == 0 {
            return;
        }
        let s = self.scale;
        let a = self.shown.value().clamp(0.0, 1.0);
        let (x, y, w, h) = self.pill();
        // Inverted, like a system toast: dark in light mode, light grey in dark mode.
        let (bg, fg, accent) = if self.dark {
            (Rgb(0x3A, 0x3A, 0x3E), theme::WHITE, Rgb(0x8A, 0xB4, 0xFF))
        } else {
            (Rgb(0x25, 0x25, 0x28), theme::WHITE, Rgb(0x7F, 0xB0, 0xFF))
        };
        let text = match self.notes.len() {
            0 | 1 => "Note deleted".to_string(),
            n => format!("{n} notes deleted"),
        };
        let fonts = &mut shared.fonts;
        let label = small_label(fonts, &text, 13.0 * s, Weight::MEDIUM);
        let undo = small_label(fonts, "Undo", 13.0 * s, Weight::SEMIBOLD);
        let ur = self.undo_rect();
        let label_at = (x + 18.0 * s, y + (h - label_h(&label)) / 2.0);
        let undo_at = (ur.0 + (ur.2 - label_width(&undo)) / 2.0, y + (h - label_h(&undo)) / 2.0);
        let hover = self.hover_undo;
        let swash = &mut shared.swash;
        let clip = Clip { x0: 0, y0: 0, x1: width as i32, y1: height as i32 };
        let opacity = (a * 255.0) as u8;
        self.surface.paint(width, height, opacity, |data| {
            if surface::ALPHA {
                let layers = [(10.0 * s, 4.0 * s, 0.28), (1.5 * s, 1.0 * s, 0.18)];
                gfx::shadow(
                    data,
                    width,
                    height,
                    (x, y, w, h),
                    &layers,
                    Some((x + 2.0 * s, y + h / 2.0, w - 4.0 * s, 1.0)),
                );
            }
            if let Some(mut pm) = PixmapMut::from_bytes(data, width, height) {
                if let Some(p) = gfx::squircle(x, y, w, h, h / 2.0) {
                    gfx::fill_path(&mut pm, &p, bg, 255);
                }
                // A hairline divider before Undo, and a soft highlight under it on hover.
                gfx::fill_rect(&mut pm, ur.0, y + 10.0 * s, 1.0 * s, h - 20.0 * s, fg, 40);
                if hover {
                    if let Some(p) = gfx::rounded_rect(
                        ur.0 + 5.0 * s,
                        y + 5.0 * s,
                        ur.2 - 10.0 * s,
                        h - 10.0 * s,
                        (h - 10.0 * s) / 2.0,
                    ) {
                        gfx::fill_path(&mut pm, &p, fg, 28);
                    }
                }
            }
            let mut label = label;
            let mut undo = undo;
            gfx::draw_text(
                &mut label,
                fonts,
                swash,
                fg.text(235),
                data,
                width,
                clip,
                (label_at.0.round() as i32, label_at.1.round() as i32),
            );
            gfx::draw_text(
                &mut undo,
                fonts,
                swash,
                accent.text(255),
                data,
                width,
                clip,
                (undo_at.0.round() as i32, undo_at.1.round() as i32),
            );
        });
    }
}

fn label_h(b: &cosmic_text::Buffer) -> f32 {
    b.layout_runs().next().map_or(0.0, |r| r.line_height)
}
