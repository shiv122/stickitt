//! Linux window-manager integration through EWMH, also used on XWayland.

use std::sync::OnceLock;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConnectionExt, EventMask, KeyButMask, MapState, PropMode, Window as XWindow,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

x11rb::atom_manager! {
    pub Atoms: AtomsCookie {
        UTF8_STRING,
        _NET_SUPPORTED,
        _NET_CURRENT_DESKTOP,
        _NET_NUMBER_OF_DESKTOPS,
        _NET_DESKTOP_NAMES,
        _NET_WM_DESKTOP,
        _NET_WM_STATE,
        _NET_WM_STATE_BELOW,
        _NET_WM_STATE_SKIP_TASKBAR,
        _NET_WM_STATE_SKIP_PAGER,
        _KDE_NET_WM_STATE_SKIP_SWITCHER,
        _NET_WM_WINDOW_OPACITY,
        _NET_WM_NAME,
        _NET_ACTIVE_WINDOW,
    }
}

pub struct X11 {
    pub conn: RustConnection,
    pub root: XWindow,
    pub atoms: Atoms,
}

pub fn x11() -> Option<&'static X11> {
    static DISPLAY: OnceLock<Option<X11>> = OnceLock::new();
    DISPLAY
        .get_or_init(|| {
            let (conn, screen) = x11rb::connect(None).map_err(|e| eprintln!("stickit: X11: {e}")).ok()?;
            let root = conn.setup().roots[screen].root;
            let atoms = Atoms::new(&conn).ok()?.reply().ok()?;
            Some(X11 { conn, root, atoms })
        })
        .as_ref()
}

pub fn xid(window: &Window) -> Option<XWindow> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Xlib(h) => h.window.try_into().ok(),
        RawWindowHandle::Xcb(h) => Some(h.window.get()),
        _ => None,
    }
}

impl X11 {
    pub fn values(&self, window: XWindow, property: Atom, kind: Atom) -> Option<Vec<u32>> {
        let reply = self.conn.get_property(false, window, property, kind, 0, 4096).ok()?.reply().ok()?;
        let values = reply.value32()?.collect();
        Some(values)
    }

    pub fn cardinal(&self, window: XWindow, property: Atom) -> Option<u32> {
        self.values(window, property, AtomEnum::CARDINAL.into())?.first().copied()
    }

    pub fn message(&self, window: XWindow, property: Atom, data: [u32; 5]) -> bool {
        let event = ClientMessageEvent::new(32, window, property, data);
        self.conn
            .send_event(false, self.root, EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY, event)
            .is_ok_and(|c| c.check().is_ok())
    }

    pub fn desktop(&self, window: XWindow) -> Option<u32> {
        self.cardinal(window, self.atoms._NET_WM_DESKTOP).filter(|&n| n != u32::MAX)
    }

    pub fn current_desktop(&self) -> Option<u32> {
        self.cardinal(self.root, self.atoms._NET_CURRENT_DESKTOP)
    }

    pub fn move_to(&self, window: XWindow, desktop: u32) -> bool {
        if desktop != u32::MAX
            && !self.cardinal(self.root, self.atoms._NET_NUMBER_OF_DESKTOPS).is_some_and(|n| desktop < n)
        {
            return false;
        }
        let Some(mapped) = self.conn.get_window_attributes(window).ok().and_then(|c| c.reply().ok()) else {
            return false;
        };
        if mapped.map_state == MapState::UNMAPPED {
            return self
                .conn
                .change_property32(
                    PropMode::REPLACE,
                    window,
                    self.atoms._NET_WM_DESKTOP,
                    AtomEnum::CARDINAL,
                    &[desktop],
                )
                .is_ok_and(|c| c.check().is_ok());
        }
        self.message(window, self.atoms._NET_WM_DESKTOP, [desktop, 1, 0, 0, 0])
    }

    pub fn state(&self, window: XWindow, state: Atom, on: bool) {
        let Some(attrs) = self.conn.get_window_attributes(window).ok().and_then(|c| c.reply().ok()) else {
            return;
        };
        if attrs.map_state == MapState::UNMAPPED {
            let mut states = self.values(window, self.atoms._NET_WM_STATE, AtomEnum::ATOM.into()).unwrap_or_default();
            states.retain(|&s| s != state);
            if on {
                states.push(state);
            }
            let _ = self.conn.change_property32(
                PropMode::REPLACE,
                window,
                self.atoms._NET_WM_STATE,
                AtomEnum::ATOM,
                &states,
            );
            let _ = self.conn.flush();
        } else {
            self.message(window, self.atoms._NET_WM_STATE, [u32::from(on), state, 0, 1, 0]);
        }
    }
}

pub fn pointer_mask() -> Option<KeyButMask> {
    let x = x11()?;
    Some(x.conn.query_pointer(x.root).ok()?.reply().ok()?.mask)
}

pub fn show_on_all(window: &Window, on: bool) {
    if crate::kwin::active() {
        let desktop = if on {
            None
        } else {
            xid(window).and_then(crate::kwin::of).and_then(|d| d.first().cloned()).or_else(crate::kwin::current)
        };
        if crate::kwin::move_to(window, desktop.as_deref()) {
            return;
        }
    }
    let (Some(x), Some(id)) = (x11(), xid(window)) else { return };
    if let Some(desktop) = if on { Some(u32::MAX) } else { x.desktop(id).or_else(|| x.current_desktop()) } {
        x.move_to(id, desktop);
    }
}

pub fn utility(window: &Window) {
    let (Some(x), Some(id)) = (x11(), xid(window)) else { return };
    for state in
        [x.atoms._NET_WM_STATE_SKIP_TASKBAR, x.atoms._NET_WM_STATE_SKIP_PAGER, x.atoms._KDE_NET_WM_STATE_SKIP_SWITCHER]
    {
        x.state(id, state, true);
    }
}

pub fn pin_to_bottom(window: &Window, on: bool) {
    if let (Some(x), Some(id)) = (x11(), xid(window)) {
        x.state(id, x.atoms._NET_WM_STATE_BELOW, on);
    }
}

/// X Shape leaves fully transparent pixels click-through, including rounded corners.
pub fn input_region(data: &[u8], width: u32) -> Vec<x11rb::protocol::xproto::Rectangle> {
    use x11rb::protocol::xproto::Rectangle;
    if width == 0 || width > i16::MAX as u32 {
        return Vec::new();
    }
    let mut rects: Vec<Rectangle> = Vec::new();
    for (y, row) in data.chunks_exact(width as usize * 4).take(i16::MAX as usize).enumerate() {
        let mut x = 0;
        while x < width as usize {
            if row[x * 4 + 3] == 0 {
                x += 1;
                continue;
            }
            let start = x;
            while x < width as usize && row[x * 4 + 3] != 0 {
                x += 1;
            }
            let span = (x - start) as u16;
            if let Some(last) = rects
                .last_mut()
                .filter(|r| r.x == start as i16 && r.width == span && r.y as usize + r.height as usize == y)
            {
                last.height += 1;
            } else {
                rects.push(Rectangle { x: start as i16, y: y as i16, width: span, height: 1 });
            }
        }
    }
    rects
}

pub fn set_input_region(window: &Window, rectangles: &[x11rb::protocol::xproto::Rectangle]) {
    use x11rb::protocol::shape::{ConnectionExt, SK, SO};
    let (Some(x), Some(id)) = (x11(), xid(window)) else { return };
    let _ = x.conn.shape_rectangles(
        SO::SET,
        SK::INPUT,
        x11rb::protocol::xproto::ClipOrdering::UNSORTED,
        id,
        0,
        0,
        rectangles,
    );
    let _ = x.conn.flush();
}

/// ARGB32 visuals keep softbuffer's high byte; scale every premultiplied channel together.
pub fn opacity(data: &mut [u8], opacity: u8) {
    if opacity != 255 {
        for channel in data {
            *channel = ((*channel as u16 * opacity as u16 + 127) / 255) as u8;
        }
    }
}

pub fn names(bytes: &[u8], count: u32) -> Vec<(String, Option<String>)> {
    let names: Vec<_> = bytes.split(|&b| b == 0).collect();
    (0..count.min(256))
        .map(|i| {
            let name = names.get(i as usize).map(|s| String::from_utf8_lossy(s).into_owned()).filter(|s| !s.is_empty());
            (i.to_string(), name)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_names_keep_indices_and_handle_missing_names() {
        assert_eq!(
            names(b"Work\0Home\0", 3),
            vec![("0".into(), Some("Work".into())), ("1".into(), Some("Home".into())), ("2".into(), None)]
        );
        assert!(names(b"", 0).is_empty());
        assert_eq!(names(b"", u32::MAX).len(), 256);
        assert_eq!(names(b"\xff\0", 1)[0].1.as_deref(), Some("�"));
    }

    #[test]
    fn input_shape_excludes_transparency_and_preserves_holes() {
        let mut data = [0; 4 * 4 * 3];
        for (x, y) in [(1, 0), (2, 0), (1, 1), (2, 1), (0, 2), (3, 2)] {
            data[(y * 4 + x) * 4 + 3] = 255;
        }
        let r = input_region(&data, 4);
        assert_eq!(r.len(), 3);
        assert_eq!((r[0].x, r[0].y, r[0].width, r[0].height), (1, 0, 2, 2));
        assert_eq!((r[1].x, r[1].y, r[1].width, r[1].height), (0, 2, 1, 1));
        assert_eq!(r[2].x, 3);
        assert!(input_region(&data, 0).is_empty());
        let mut pixel = [20, 40, 60, 80];
        opacity(&mut pixel, 128);
        assert_eq!(pixel, [10, 20, 30, 40]);
        opacity(&mut pixel, 0);
        assert_eq!(pixel, [0; 4]);
    }
}
