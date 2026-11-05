<p align="center">
  <img src="assets/icon.png" alt="Stick-itt" width="128" height="128">
</p>

<h1 align="center">Stick-itt</h1>

[![CI](https://github.com/shiv122/stickitt/actions/workflows/ci.yml/badge.svg)](https://github.com/shiv122/stickitt/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/shiv122/stickitt)](https://github.com/shiv122/stickitt/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Tiny, native sticky notes for macOS and Windows. Notes are native windows drawn in Rust, with
no web view, so the app starts instantly, idles at 0% CPU and uses a few megabytes of memory.

## Features

- **Markdown notes** with live preview: headings, to-dos, lists, quotes, code and links. Type
  `/` for blocks and `:fire:` for emoji.
- **Edge dock**: a slim tab on the screen edge that slides out every note, with search, lists
  and profiles. Drag the tab to move it; Ctrl+scroll (⌘+scroll on macOS) over the dock to resize it.
- **Reminders**: pick a day, time and repeat from the clock menu, or type `@3pm`,
  `@fri 9am` or `@daily 8am` in a note.
- **Window modes**: normal, always on top, or on the desktop behind your windows.
- **Compact view**: shrink a note to a single line with its icon and title.
- **Trash with undo**, hide and restore, and a tray icon to get everything back.
- **Virtual desktops** (Windows): each note stays on the desktop it was made on.
- **Configurable**: sizes, colours, the dock and every shortcut live in one commented settings
  file that applies as you save.

## Install

**macOS** (11 Big Sur or later, Apple Silicon and Intel), with [Homebrew](https://brew.sh):

```sh
brew install --cask shiv122/tap/stick-itt
```

Or download `Stick-itt-<version>-macos.dmg` from
[Releases](https://github.com/shiv122/stickitt/releases) and drag Stick-itt to Applications.

**Windows** (10 or 11, x64): from [Releases](https://github.com/shiv122/stickitt/releases),
download either

- `Stick-itt-<version>-windows-setup.exe`: installs for your user (no admin prompt), with a
  Start menu entry, an uninstaller and an option to start Stick-itt when you sign in, or
- `Stick-itt-<version>-windows-portable.exe`: runs as is, nothing to install.

Release builds are not notarized or code-signed with a paid certificate. The Homebrew cask
clears macOS's download quarantine for you. From the `.dmg`, macOS blocks the first launch: open
**System Settings → Privacy & Security** and click **Open Anyway** (or run
`xattr -dr com.apple.quarantine /Applications/Stick-itt.app`). On Windows, SmartScreen may ask
you to confirm the first launch.

## Getting started

Stick-itt opens with a welcome note. From there:

- **New note**: <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>N</kbd> from anywhere, or the tray menu.
- **Find a note**: rest the pointer on the screen edge (or press
  <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>D</kbd> and type to search).
- **Note controls**: hover a note for colour, window mode, reminder and the ⋯ menu (lists,
  compact view, delete).
- **Dock menu**: right-click the dock for profiles, lists, views, Trash and Settings.

On macOS, <kbd>Ctrl</kbd> in the shortcuts below is <kbd>⌘</kbd>.

| Anywhere | | In a note | |
|---|---|---|---|
| <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>N</kbd> | New note | <kbd>Ctrl</kbd>+<kbd>R</kbd> | Reminder |
| <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>V</kbd> | Clipboard to note | <kbd>Ctrl</kbd>+<kbd>T</kbd> | Window mode |
| <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>D</kbd> | Open the dock | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>M</kbd> | Compact view |
| <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>H</kbd> | Hide / show notes | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>L</kbd> | Note menu |
| <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>P</kbd> | Next profile | <kbd>Ctrl</kbd>+<kbd>W</kbd> / <kbd>Ctrl</kbd>+<kbd>D</kbd> | Hide / delete |
| <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>,</kbd> | Settings | <kbd>Ctrl</kbd>+<kbd>Enter</kbd> | Tick a to-do |

## Configuration

Settings live in `stickit.ini`, created on first launch next to your notes:

| | Location |
|---|---|
| Windows | `%APPDATA%\Stick-itt\` |
| macOS | `~/Library/Application Support/Stick-itt/` |

Open it with **Settings…** in the dock or tray menu. Every option is documented in the file and
applies within a second of saving. Sections cover the app (`[general]`), notes
(`[notes]`, `[notes.compact]`), reminders, the dock (`[dock]`, `[dock.list]`, `[dock.icons]`,
`[dock.tab]`), virtual desktops, and all shortcuts (`[shortcuts.global]`, `[shortcuts.note]`).

Notes are stored in `stickit.db` (SQLite) in the same folder.

## Building from source

Requires [Rust](https://rustup.rs) 1.85 or later.

```sh
cargo run --release          # build and start
cargo test                   # unit tests
cargo clippy --all-targets   # lints (CI treats warnings as errors)
```

Packages, as built by the release workflow:

```sh
bash scripts/package-macos.sh       # dist/Stick-itt-<version>-macos.dmg and .zip (universal)
pwsh scripts/package-windows.ps1    # dist/Stick-itt-<version>-windows-setup.exe and -portable.exe
```

The Windows installer needs [Inno Setup 6](https://jrsoftware.org/isinfo.php). To test the
packaging without publishing, run the **Release** workflow by hand from the Actions tab: it
builds everything and keeps the files as workflow artifacts.

Icons for every platform are generated from `assets/icon.png` (1024×1024) with
`python scripts/make-icons.py`.

Set `STICKIT_DEBUG=1` to print the dock's and notes' pointer and window events to the terminal.

`stickit --bench 30 [--big]` opens sample notes in memory for performance testing;
`scripts/measure.ps1` (Windows) and `scripts/measure.sh` (macOS) report startup time, memory and
idle CPU.

## Releasing

1. Update `version` in `Cargo.toml` and commit.
2. Tag and push: `git tag v0.2.0 && git push origin v0.2.0`.

The release workflow runs CI, builds the macOS universal app and the Windows zip, publishes a
GitHub release with checksums, and updates the cask in
[shiv122/homebrew-tap](https://github.com/shiv122/homebrew-tap). The cask update needs a
`HOMEBREW_TAP_TOKEN` repository secret with write access to the tap.

## License

[MIT](LICENSE)
