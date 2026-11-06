//! `stickit --shots DIR`: renders the README's pictures (docs/images) from a set of demo notes,
//! in light and dark. Each scene stages the notes or the dock, waits for the animations to
//! settle, then saves the window's next frame as a PNG cropped to what was drawn.

use super::App;
use crate::dock::View;
use crate::reminder::{Reminder, Repeat};
use crate::store::{NoteRecord, DEFAULT_LIST, DEFAULT_PROFILE};
use crate::surface::{self, Frame};
use crate::theme::preset_named;
use chrono::{Duration as Days, Local};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use winit::event_loop::ActiveEventLoop;
use winit::window::WindowId;

/// Long enough for every spring (notes popping in, the dock sliding out) to come to rest.
const SETTLE: Duration = Duration::from_millis(1600);

struct Demo {
    icon: &'static str,
    body: &'static str,
    color: &'static str,
    /// In the "Work" list rather than the default one.
    work: bool,
    compact: bool,
    size: (f64, f64),
}

const DEMOS: [Demo; 7] = [
    Demo {
        icon: "🧳",
        body: "# Weekend trip\n- [x] Book the train\n- [x] Find a place to stay\n\
               - [ ] Pack the **charger** and `adapter`\n- [ ] Check the weather\n> Leave by 8:30 on Saturday",
        color: "Lavender",
        work: false,
        compact: false,
        size: (340.0, 270.0),
    },
    Demo {
        icon: "🛒",
        body: "# Groceries\n- [ ] Oat milk\n- [ ] Coffee beans\n- [x] Bread\n- [ ] Lemons",
        color: "Yellow",
        work: false,
        compact: true,
        size: (260.0, 220.0),
    },
    Demo {
        icon: "💡",
        body: "# Ideas\n- A tiny habit tracker\n- Dark mode for the blog\n- Learn *async* Rust",
        color: "Aqua",
        work: false,
        compact: true,
        size: (260.0, 220.0),
    },
    Demo {
        icon: "📚",
        body: "# Reading list\n- *The Pragmatic Programmer*\n- *Designing Data-Intensive Applications*",
        color: "Mint",
        work: false,
        compact: true,
        size: (260.0, 220.0),
    },
    Demo {
        icon: "☕",
        body: "# Standup\n**Yesterday:** fixed the login bug\n**Today:** review pull requests\n**Blocked:** nothing",
        color: "Cobalt",
        work: true,
        compact: false,
        size: (280.0, 200.0),
    },
    Demo {
        icon: "🚀",
        body: "# Release checklist\n- [x] Bump the version\n- [ ] Write the release notes\n- [ ] Tag and push",
        color: "Grape",
        work: true,
        compact: false,
        size: (280.0, 200.0),
    },
    Demo {
        icon: "💼",
        body: "# Q3 pipeline\n> 💡 Review with **Dana** on {red: Friday}\n| Deal | Stage | Value |\n| --- | --- | ---: |\n\
               | Acme Corp | ==blue:Proposal== | $48k |\n| Brightline | ==green:Won== | $61k |\n\
               | Kestrel Labs | ==yellow:Discovery== | $15k |\n| Novatek | ==red:At risk== | $9.8k |",
        color: "Amber",
        work: true,
        compact: false,
        size: (400.0, 360.0),
    },
];

#[derive(Clone, Copy)]
enum Subject {
    /// A demo note, hovered, with one of its menus open.
    Note(usize, Option<&'static str>),
    /// The compact demo notes, stacked.
    Compact,
    /// The open dock in a view, with or without its right-click menu.
    Dock(View, bool),
}

const SCENES: [(&str, Subject); 15] = [
    ("note", Subject::Note(0, None)),
    ("note-menu", Subject::Note(0, Some("more"))),
    ("note-reminder", Subject::Note(0, Some("remind"))),
    ("note-colors", Subject::Note(0, Some("colors"))),
    ("note-window", Subject::Note(0, Some("window"))),
    ("note-table", Subject::Note(6, None)),
    ("note-ringing", Subject::Note(0, Some("ring"))),
    ("note-format", Subject::Note(6, Some("format"))),
    ("note-table-edit", Subject::Note(6, Some("edit-table"))),
    ("note-row-menu", Subject::Note(6, Some("row-menu"))),
    ("note-col-menu", Subject::Note(6, Some("col-menu"))),
    ("compact", Subject::Compact),
    ("dock", Subject::Dock(View::Detailed, false)),
    ("dock-icons", Subject::Dock(View::Icons, false)),
    ("dock-menu", Subject::Dock(View::Detailed, true)),
];

pub struct Shots {
    dir: PathBuf,
    /// The demo notes' windows, in `DEMOS` order.
    notes: Vec<WindowId>,
    /// The scene staged now (all scenes in light, then in dark), captured at `at`.
    step: Option<usize>,
    at: Instant,
}

impl Shots {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, notes: Vec::new(), step: None, at: Instant::now() }
    }
}

/// Opens the demo notes.
pub(super) fn start(app: &mut App, el: &ActiveEventLoop) {
    surface::capture_frames();
    let work = app.store.create_list("Work", DEFAULT_PROFILE).unwrap_or(DEFAULT_LIST);
    app.reload_model();
    let tomorrow = (Local::now() + Days::days(1)).date_naive().and_hms_opt(9, 0, 0);
    let due = tomorrow.and_then(|t| t.and_local_timezone(Local).single()).map_or(0, |t| t.timestamp());
    let mut notes = Vec::new();
    for (i, d) in DEMOS.iter().enumerate() {
        let mut rec = NoteRecord {
            id: 0,
            body: d.body.to_string(),
            pos: Some((60 + i as i32 * 40, 60 + i as i32 * 30)),
            size: d.size,
            color: preset_named(d.color).unwrap_or(0),
            mode: 0,
            zoom: 100,
            reminder: (i == 0).then_some(Reminder { due, anchor: due, repeat: Repeat::Once }),
            icon: d.icon.to_string(),
            list: if d.work { work } else { DEFAULT_LIST },
            desktop: None,
            hidden: false,
            view: u8::from(d.compact),
        };
        let Ok(id) = app.store.insert(&rec) else { continue };
        rec.id = id;
        if let Some(win) = app.open(el, &rec) {
            notes.push(win);
        }
    }
    app.dock_stale = true;
    if let Some(shots) = app.shots.as_mut() {
        shots.notes = notes;
        shots.at = Instant::now() + SETTLE;
    }
}

/// Captures the staged scene once it has settled and stages the next; quits after the last.
/// Returns when to be called again.
pub(super) fn step(app: &mut App, el: &ActiveEventLoop) -> Option<Instant> {
    let shots = app.shots.as_ref()?;
    let now = Instant::now();
    if now < shots.at {
        return Some(shots.at);
    }
    let next = match shots.step {
        Some(step) => {
            capture(app, step);
            step + 1
        }
        None => 0,
    };
    if next == 2 * SCENES.len() {
        if let Some(shots) = app.shots.take() {
            eprintln!("stickit: pictures saved in {}", shots.dir.display());
        }
        el.exit();
        return None;
    }
    stage(app, next);
    let shots = app.shots.as_mut()?;
    shots.step = Some(next);
    shots.at = now + SETTLE;
    Some(shots.at)
}

fn stage(app: &mut App, step: usize) {
    let dark = step >= SCENES.len();
    let (_, subject) = SCENES[step % SCENES.len()];
    let ids = app.shots.as_ref().map(|s| s.notes.clone()).unwrap_or_default();
    for (i, id) in ids.iter().enumerate() {
        if let Some(note) = app.notes.get_mut(id) {
            let (hovered, menu) = match subject {
                Subject::Note(n, menu) if n == i => (true, menu),
                _ => (false, None),
            };
            note.stage(dark, hovered, menu, &mut app.shared);
        }
    }
    let (view, open, menu) = match subject {
        Subject::Dock(view, menu) => (view, true, menu),
        _ => (View::Detailed, false, false),
    };
    if let Some(dock) = app.dock.as_mut() {
        dock.stage(dark, open, view, menu);
    }
}

fn capture(app: &mut App, step: usize) {
    let Some(shots) = app.shots.as_ref() else { return };
    let (name, subject) = SCENES[step % SCENES.len()];
    let theme = if step >= SCENES.len() { "dark" } else { "light" };
    let path = shots.dir.join(format!("{name}-{theme}.png"));
    let ids = shots.notes.clone();
    let (notes, shared) = (&mut app.notes, &mut app.shared);
    let mut render_note = |i: usize| {
        let note = notes.get_mut(ids.get(i)?)?;
        note.settle();
        note.render(shared);
        surface::take_frame().map(crop)
    };
    let frame = match subject {
        Subject::Note(i, _) => render_note(i),
        Subject::Compact => {
            let frames: Vec<Frame> =
                DEMOS.iter().enumerate().filter(|(_, d)| d.compact).filter_map(|(i, _)| render_note(i)).collect();
            (!frames.is_empty()).then(|| stack(&frames))
        }
        Subject::Dock(..) => app.dock.as_mut().and_then(|dock| {
            dock.settle();
            dock.render(&mut app.shared);
            surface::take_frame().map(crop)
        }),
    };
    match frame.map(|f| save(&f, &path)) {
        Some(Ok(())) => eprintln!("stickit: saved {}", path.display()),
        Some(Err(e)) => eprintln!("stickit: cannot save {}: {e}", path.display()),
        None => eprintln!("stickit: nothing painted for {name}"),
    }
}

/// Cuts a frame down to its painted (not fully transparent) pixels.
fn crop((w, h, px): Frame) -> Frame {
    let alpha = |x: u32, y: u32| px[((y * w + x) * 4 + 3) as usize];
    let rows: Vec<u32> = (0..h).filter(|&y| (0..w).any(|x| alpha(x, y) > 0)).collect();
    let cols: Vec<u32> = (0..w).filter(|&x| (0..h).any(|y| alpha(x, y) > 0)).collect();
    let (Some(&y0), Some(&y1), Some(&x0), Some(&x1)) = (rows.first(), rows.last(), cols.first(), cols.last()) else {
        return (w, h, px);
    };
    let (cw, ch) = (x1 - x0 + 1, y1 - y0 + 1);
    let mut out = Vec::with_capacity((cw * ch * 4) as usize);
    for y in y0..=y1 {
        let start = ((y * w + x0) * 4) as usize;
        out.extend_from_slice(&px[start..start + (cw * 4) as usize]);
    }
    (cw, ch, out)
}

/// Frames one above another, left-aligned.
fn stack(frames: &[Frame]) -> Frame {
    let w = frames.iter().map(|f| f.0).max().unwrap_or(0);
    let h = frames.iter().map(|f| f.1).sum();
    let mut out = vec![0u8; (w * h * 4) as usize];
    let mut top = 0;
    for (fw, fh, px) in frames {
        let row = (fw * 4) as usize;
        for y in 0..*fh {
            let src = (y * fw * 4) as usize;
            let dst = ((top + y) * w * 4) as usize;
            out[dst..dst + row].copy_from_slice(&px[src..src + row]);
        }
        top += fh;
    }
    (w, h, out)
}

/// Writes premultiplied B,G,R,A pixels as a PNG.
fn save((w, h, px): &Frame, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut rgba = px.clone();
    for p in rgba.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    let size = tiny_skia::IntSize::from_wh(*w, *h).ok_or("empty picture")?;
    let pixmap = tiny_skia::Pixmap::from_vec(rgba, size).ok_or("bad picture size")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    pixmap.save_png(path)?;
    Ok(())
}
