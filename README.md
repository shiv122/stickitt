# Stick-it

Tiny, native notes that float on your desktop, for Windows and macOS. Every note is a borderless
window that draws its own text in Rust. There is no web view, no Electron and no background web server.

| Notes open | Working set | Private memory | Idle CPU |
|-----------:|------------:|---------------:|---------:|
| 1          | 19 MB       | 4 MB           | 0%       |
| 10         | 26 MB       | 8 MB           | 0%       |
| 30         | 45 MB       | 19 MB          | 0%       |

Measured on Windows 11 with `scripts/measure.ps1`, including the edge dock.

## Build and run

```sh
cargo run --release              # notes stored in your data dir (e.g. %APPDATA%\Stick-it\stickit.db)
cargo run --release -- --db x.db # use a specific database
cargo test
```

## Using it

**Notes.** Each note is a page in the Notion style: an optional emoji icon, a soft colour cover at
the top, and markdown with live preview. Syntax such as `#`, `**`, backticks, `>` and link URLs is
hidden on every line except the one you're editing. To-dos get square checkboxes, inline code gets
Notion's red-on-grey pill, quotes get a bar and `---` becomes a divider. Light and dark follow your
system setting.

**Slash menu.** Type `/` at the start of a line (or after a space) to insert a block: text,
headings, to-do list, bulleted or numbered list, quote, code, divider, reminder, or icon. Keep
typing to filter (`/todo`, `/h2`), use ↑/↓ and Enter, or press Esc to cancel.

**Icons.** Hover a note and click **Add icon** (or Ctrl+Shift+I), or click an existing icon. The
picker has the full emoji set in category tabs, with search; just type. The icon sits top-left in
the note's bar, or large above the text with `icon_position = page`.

**Emoji shortcodes.** Type `:fire:` and it becomes 🔥. Typing `:fi` shows suggestions (↑/↓, Enter).

**Colours.** Click the colour dot in a note's hover controls (or Ctrl+Shift+K): 18 presets, plus a
custom strip for any hue. Every colour adapts to light and dark mode.

**Edge dock.** Rest the pointer on the screen edge where a slim tab peeks out (or press
Ctrl+Alt+D). Your notes slide out as a stack of cards, each showing its icon and title:
- The cards cascade in.
- Hovering pushes the card under the pointer out from the edge, its neighbours a little less,
  like the macOS Dock's magnification turned sideways.
- Hovering a card floats that note to the front; clicking focuses it. A note on another virtual
  desktop is brought to your current one.
- The last card makes a new note.
- **Drag any card** to move the dock to the left or right edge of any monitor, at any height.
- **Right-click** for the menu: switch profile, choose which lists the profile shows, create or
  delete profiles and lists, pick the view (compact list, detailed list, or Dock-style icons),
  and open the **Trash**.
- Opened with Ctrl+Alt+D, the dock takes the keyboard: type to search, ↑/↓ and Enter to open,
  Esc to close.

**Note menu.** The ⋯ button in a note's hover controls (or Ctrl+Shift+L) moves the note to
another list, switches compact view, and holds **Delete note**. Delete sits at the bottom, apart
from everything else, and needs a second click ("Click again to delete").

**Compact view.** Ctrl+Shift+M (or the note menu) shrinks a note to one line: its icon, its title
cut off with "…" at `max_width`, and on the right its to-do progress (2/5) or reminder. Drag it to
move it; click it (or press Enter) to open the full note, which folds back to one line when you
click elsewhere. Sizes, colour tint and behaviour are under `[notes.compact]`.

**Hiding and deleting.** × (Ctrl+W) hides a note; it stays in the dock, dimmed, and comes back
when you click it. Ctrl+D (or ⋯ → Delete note) deletes a note: a "Note deleted · Undo" toast appears at the bottom of
the screen for a few seconds. Deleted notes go to the Trash (dock right-click menu → Trash): click
one to restore it, × to delete it for good, or empty the whole trash. Notes left in the Trash for
30 days (`trash_days`) are deleted automatically.

**Tray icon.** Stick-it sits in the notification area (Windows 11 may put it under the ^ arrow
at first; drag it out to keep it visible). Click it to open the dock. Its menu has New note,
Paste as note, Open dock, Hide all notes, every hidden note, Settings and Quit. Turn it off with
`tray_icon = false` under `[general]`.

**One copy at a time.** Starting Stick-it again while it is running just opens the dock of the
running copy.

**Lists and profiles.** Every note belongs to a list. Move a note to another list from its hover
controls: ⋯ → Move to list (or Ctrl+Shift+L). A profile is a set of lists: switching profile (right-click the dock, or
Ctrl+Alt+P) changes both the dock and which notes are on screen.

**Virtual desktops.** Each note stays on the virtual desktop it was made on, and each desktop
remembers its own dock profile. Windows only keeps a window on one desktop if it has a taskbar
entry, so notes share one grouped Stick-it taskbar button. Turn this off with
`per_desktop = false` (no taskbar button; notes show on every desktop).

**Window mode.** The window button in a note's hover controls opens a small menu: **Normal** (like
any window), **Always on top** (floats above every app) or **On desktop** (behind your windows,
like a widget). An "on desktop" note comes to the front while you use it (click it, pick it on the
taskbar or in the dock) and goes back behind your windows when you switch away. A note that is
not Normal keeps its mode icon visible in the corner.

**Closing.** "Close all windows" on the taskbar hides every note, and Stick-it keeps running in the
tray. Click the tray icon, press Ctrl+Alt+H or start Stick-it again to bring them back.

**Settings and shortcuts.** Open them with **Ctrl+Alt+,** (anywhere), **Ctrl+,** (in a note) or
**Settings…** in the dock's right-click menu. Any of these opens `%APPDATA%\Stick-it\stickit.ini` in
your editor, and changes apply about a second after you save. Every option is documented in the
file:
- `[general]`: the tray icon, whether Delete needs a second click, days kept in the trash, how
  long the Undo toast stays.
- `[notes]`: icon position and size, top bar height, gap above the text, padding, corner
  radius, font size, line height, shadow, colour cover strength.
- `[notes.compact]`: the one-line view: height, min and max width, font and icon size, padding,
  radius, colour tint, to-do progress, folding back when you click away.
- `[dock]`: side, position, open delay, edge gap.
- `[dock.list]`: card width, card gap, padding, font and icon size, radius, opacity, hover push.
- `[dock.icons]`: tile size and magnification.
- `[dock.tab]`: the edge tab's colour, size and opacity, and the trigger area.
- `[desktops]`: per-desktop notes, other desktops in the dock, a profile per desktop.
- `[shortcuts.global]` and `[shortcuts.note]`: every shortcut.

Files from older versions are converted automatically, keeping your values, and the original is
kept as `stickit.ini.bak`.

All motion is spring-driven, using the [`springs`](https://crates.io/crates/springs) crate.

### Reminders

Click the clock in a note's hover controls (or press Ctrl+R; once set, click the reminder tag):
- **Soon**: in 20 minutes, 1 hour or 3 hours.
- **Day**: a week strip. Pick a day, then a **time** (times already past today are greyed out).
- **Repeat**: once, daily, weekdays or weekly. It applies to the time you pick, and to a reminder
  that's already set.
- **Type anything** in the field at the top. A live preview shows what it understood
  ("→ Tomorrow, 9 AM · every Thursday"); press Enter to set it.
- **Remove reminder**.

The times and "soon" choices are set under `[reminders]` in `stickit.ini`. You can also type a time
after `@` right in the note, then press Enter or click away; the token disappears and a tag shows:

`@3pm` · `@15:30` · `@in 20m` · `@in 1h30m` · `@tomorrow 9am` · `@fri 2pm` · `@2026-12-01 10:00` ·
`@tonight` · `@daily 9am` · `@weekdays 8:30am` · `@every 45m` · `@every mon 10am`

When a reminder is due, the note jumps to the front and shakes. It nudges again every 45 s until you
press **10m**, **1h** or **Done** (Esc). Reminders that came due while the app was closed ring at the next launch.

### Default shortcuts (all remappable in `stickit.ini`; Ctrl is ⌘ on macOS)

| Anywhere | Action |
|---|---|
| Ctrl+Alt+N | New note |
| Ctrl+Alt+V | Clipboard → new note |
| Ctrl+Alt+H | Hide / show all notes |
| Ctrl+Alt+D | Open the dock (keyboard search) |
| Ctrl+Alt+P | Next profile |

| In a note | Action |
|---|---|
| Ctrl+N / Ctrl+W / Ctrl+D / Ctrl+Q | New note / close / delete / quit |
| Ctrl+T | Window mode: normal → always on top → on desktop |
| Ctrl+1 … Ctrl+9 / Ctrl+Shift+C | Colour / next colour |
| Ctrl+Enter | Tick the to-do on this line (or make one) |
| Ctrl+B / Ctrl+I / Ctrl+E / Ctrl+Shift+X | Bold / italic / inline code / strikethrough |
| Ctrl+R / Ctrl+Shift+R | Reminder menu / clear the reminder |
| Ctrl+Shift+I / Ctrl+Shift+L | Pick an icon / note menu (list, view, delete) |
| Ctrl+Shift+M | Compact view on / off |
| Tab / Shift+Tab | Indent / outdent |
| Ctrl+= / Ctrl+- / Ctrl+0 | Zoom text |

Hover over a note to show its controls. Drag the top strip to move the note, and drag its edges to resize it.

## Architecture

```
crates/stickit/src
├─ main.rs      args, store, event loop
├─ app.rs       owns windows, shared fonts, batched saves, reminder scheduling, dock wiring
├─ note.rs      one note window: layout, input, painting, title bar
├─ dock.rs      the edge dock: cards, magnification, peek, menu, trash
├─ tray.rs      notification-area icon and menu
├─ toast.rs     the "Note deleted · Undo" toast
├─ instance.rs  one running copy per database
├─ popover.rs   slash menu items and the icon set
├─ editor.rs    text model: selection, undo, list-aware Enter/Tab/Backspace, IME
├─ markdown.rs  per-byte style flags (source-preserving)
├─ reminder.rs  natural-language @tokens and recurrence
├─ motion.rs    spring presets (smooth / snappy / bouncy / shake)
├─ surface.rs   per-pixel-alpha layered windows (Windows), softbuffer elsewhere
├─ gfx.rs       tiny-skia drawing, squircles, analytic soft shadows, glyph blending
├─ store.rs     SQLite (WAL), schema migrations, soft delete
└─ theme.rs     Notion-style palettes, light and dark
```

On Windows every window is a layered window, so the rounded "squircle" cards and soft shadows are
real transparency, and the empty space around them is click-through. Rendering uses winit windows,
cosmic-text for shaping and layout, and tiny-skia, all on the CPU. Each window costs little more
than its own pixels. Fonts are memory-mapped and shared by every note. The event loop sleeps between
events and wakes only for a blinking caret (which stops after 20 s idle), an animation, a pending
save or the next reminder.
#   s t i c k i t t  
 