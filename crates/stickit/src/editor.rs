//! Plain-text document model: text, caret/selection, undo history and IME composition.
//! Offsets are byte indices into `text` and always sit on char boundaries.

use crate::markdown::line_info;
use std::borrow::Cow;
use std::ops::Range;
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;

const UNDO_LIMIT: usize = 200;
/// History kept per note, in bytes of replaced text: the oldest steps go first beyond this.
const UNDO_BYTES: usize = 1 << 20;
const COALESCE_WINDOW: Duration = Duration::from_millis(1200);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    Typing,
    Deleting,
    Other,
}

#[derive(Clone)]
struct Snapshot {
    text: String,
    cursor: usize,
    anchor: Option<usize>,
}

/// One step of history, keeping only what changed: the text between `start` and `start + len`
/// was `old` before the step, and the caret and selection were at `cursor` and `anchor`.
struct Change {
    start: usize,
    len: usize,
    old: String,
    cursor: usize,
    anchor: Option<usize>,
}

/// The part of `after` that differs from `before`: (start, length in `after`, the text it
/// replaced in `before`), on char boundaries.
fn diff(before: &str, after: &str) -> (usize, usize, String) {
    let (a, b) = (before.as_bytes(), after.as_bytes());
    let mut start = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    while !before.is_char_boundary(start) {
        start -= 1;
    }
    let room = a.len().min(b.len()) - start;
    let mut tail = a.iter().rev().zip(b.iter().rev()).take(room).take_while(|(x, y)| x == y).count();
    while !before.is_char_boundary(a.len() - tail) || !after.is_char_boundary(b.len() - tail) {
        tail -= 1;
    }
    (start, b.len() - tail - start, before[start..a.len() - tail].to_string())
}

pub struct Preedit {
    pub text: String,
    /// Caret inside the preedit, as a byte offset into `text`.
    pub cursor: Option<usize>,
}

#[derive(Default)]
pub struct Doc {
    pub text: String,
    pub cursor: usize,
    pub anchor: Option<usize>,
    pub preedit: Option<Preedit>,
    undo: Vec<Change>,
    redo: Vec<Change>,
    /// The text as it was before the edits now being grouped, until `fold` turns it into a
    /// `Change` (one full copy at most, and none once the note is idle).
    pending: Option<Snapshot>,
    last_edit: Option<(EditKind, Instant)>,
}

/// Normalizes pasted/typed text: one kind of newline, no stray control characters.
pub fn sanitize(s: &str) -> String {
    let s = s.replace("\r\n", "\n");
    s.chars()
        .map(|c| match c {
            '\r' | '\u{85}' | '\u{2029}' | '\u{1C}'..='\u{1E}' => '\n',
            c => c,
        })
        .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
        .collect()
}

impl Doc {
    pub fn new(text: String) -> Self {
        Self { text: sanitize(&text), ..Self::default() }
    }

    pub fn selection(&self) -> Option<Range<usize>> {
        let a = self.anchor?;
        (a != self.cursor).then(|| a.min(self.cursor)..a.max(self.cursor))
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.selection().map(|r| &self.text[r])
    }

    /// Text as displayed: the IME composition is spliced in at the caret.
    /// Returns the display text and the display range occupied by the preedit.
    pub fn display(&self) -> (Cow<'_, str>, Option<Range<usize>>) {
        match &self.preedit {
            Some(p) if !p.text.is_empty() => {
                let mut s = String::with_capacity(self.text.len() + p.text.len());
                s.push_str(&self.text[..self.cursor]);
                s.push_str(&p.text);
                s.push_str(&self.text[self.cursor..]);
                (Cow::Owned(s), Some(self.cursor..self.cursor + p.text.len()))
            }
            _ => (Cow::Borrowed(&self.text), None),
        }
    }

    // ---- editing -------------------------------------------------------------------------

    fn snapshot(&self) -> Snapshot {
        Snapshot { text: self.text.clone(), cursor: self.cursor, anchor: self.anchor }
    }

    fn record(&mut self, kind: EditKind) {
        let now = Instant::now();
        let coalesce =
            matches!(self.last_edit, Some((k, t)) if k == kind && kind != EditKind::Other && now - t < COALESCE_WINDOW);
        if !coalesce {
            self.fold();
            self.pending = Some(self.snapshot());
        }
        self.redo.clear();
        self.last_edit = Some((kind, now));
    }

    /// Replaces `range` with `s`, leaving the caret after the inserted text.
    pub fn replace(&mut self, range: Range<usize>, s: &str, kind: EditKind) {
        self.record(kind);
        self.text.replace_range(range.clone(), s);
        self.cursor = range.start + s.len();
        self.anchor = None;
    }

    pub fn insert(&mut self, s: &str) {
        let s = sanitize(s);
        if s.is_empty() {
            return;
        }
        let kind = if s.contains('\n') || s.len() > 4 { EditKind::Other } else { EditKind::Typing };
        let range = self.selection().unwrap_or(self.cursor..self.cursor);
        self.replace(range, &s, kind);
        // A space closes the typing group so undo steps back word by word.
        if s == " " {
            self.last_edit = None;
        }
    }

    pub fn backspace(&mut self, word: bool) {
        if let Some(r) = self.selection() {
            return self.replace(r, "", EditKind::Deleting);
        }
        if self.cursor == 0 {
            return;
        }
        // Backspace right after a list marker removes the whole marker.
        let (ls, le) = self.line_bounds(self.cursor);
        let line = &self.text[ls..le];
        let info = line_info(line);
        let content = ls + info.content_start(line);
        let from = if info.is_list() && self.cursor == content {
            ls + info.indent
        } else if word {
            self.prev_word(self.cursor)
        } else {
            self.prev_grapheme(self.cursor)
        };
        self.replace(from..self.cursor, "", EditKind::Deleting);
    }

    pub fn delete(&mut self, word: bool) {
        if let Some(r) = self.selection() {
            return self.replace(r, "", EditKind::Deleting);
        }
        if self.cursor >= self.text.len() {
            return;
        }
        let to = if word { self.next_word(self.cursor) } else { self.next_grapheme(self.cursor) };
        let at = self.cursor;
        self.replace(at..to, "", EditKind::Deleting);
        self.cursor = at;
    }

    /// Enter with list continuation: "- [ ] a⏎" starts "- [ ] ", an empty item ends the list.
    pub fn newline(&mut self) {
        let (ls, le) = self.line_bounds(self.cursor);
        let line = &self.text[ls..le];
        let info = line_info(line);
        if self.selection().is_none() && info.is_list() && self.cursor >= ls + info.marker_end {
            if line[info.content_start(line)..].trim().is_empty() {
                // Empty item: drop the marker instead of continuing the list.
                self.replace(ls..le, "", EditKind::Other);
                return;
            }
            let indent = &line[..info.indent];
            let marker = match info.ordered {
                Some(n) => format!("{}{}. ", indent, n + 1),
                None => line[..info.marker_end].to_string(),
            };
            let checkbox = if info.checkbox.is_some() { "[ ] " } else { "" };
            return self.insert_other(&format!("\n{marker}{checkbox}"));
        }
        self.insert_other("\n");
    }

    fn insert_other(&mut self, s: &str) {
        let range = self.selection().unwrap_or(self.cursor..self.cursor);
        self.replace(range, s, EditKind::Other);
    }

    /// Indents (or outdents) every line touched by the selection by two spaces.
    pub fn indent(&mut self, outdent: bool) {
        let sel = self.selection().unwrap_or(self.cursor..self.cursor);
        let first = self.line_bounds(sel.start).0;
        let mut starts = vec![first];
        starts.extend(
            self.text[first..sel.end].match_indices('\n').map(|(i, _)| first + i + 1).filter(|&i| i <= sel.end),
        );
        self.record(EditKind::Other);
        let (mut cursor, mut anchor) = (self.cursor, self.anchor);
        for &ls in starts.iter().rev() {
            let delta: isize = if outdent {
                let n = self.text[ls..].bytes().take(2).take_while(|&b| b == b' ').count();
                self.text.replace_range(ls..ls + n, "");
                -(n as isize)
            } else {
                self.text.insert_str(ls, "  ");
                2
            };
            cursor = shift_from(cursor, ls, delta);
            anchor = anchor.map(|p| shift_from(p, ls, delta));
        }
        self.cursor = cursor;
        self.anchor = anchor;
    }

    /// Toggles the checkbox on the caret's line; turns a plain line into a checklist item.
    pub fn toggle_check(&mut self) {
        let (ls, le) = self.line_bounds(self.cursor);
        self.toggle_check_at_line(ls, le);
    }

    pub fn toggle_check_at_line(&mut self, ls: usize, le: usize) {
        let line = &self.text[ls..le];
        let info = line_info(line);
        let caret = self.cursor;
        self.record(EditKind::Other);
        match info.checkbox {
            Some((at, checked)) => {
                let mark = if checked { " " } else { "x" };
                self.text.replace_range(ls + at + 1..ls + at + 2, mark);
                self.cursor = caret;
            }
            None => {
                let (at, insert) =
                    if info.is_list() { (ls + info.marker_end, "[ ] ") } else { (ls + info.indent, "- [ ] ") };
                self.text.insert_str(at, insert);
                self.cursor = if caret >= at { caret + insert.len() } else { caret };
            }
        }
        self.anchor = None;
    }

    /// Replaces the block syntax at the start of the caret's line (heading, quote, list,
    /// checkbox) with `prefix`, keeping the caret on the same content.
    pub fn set_line_prefix(&mut self, prefix: &str) {
        let (ls, le) = self.line_bounds(self.cursor);
        let line = &self.text[ls..le];
        let info = line_info(line);
        let rest = &line[info.indent..];
        let old = if info.is_list() {
            info.content_start(line) - info.indent
        } else if rest.starts_with('#') {
            let hashes = rest.bytes().take_while(|&b| b == b'#').count();
            hashes + usize::from(rest[hashes..].starts_with(' '))
        } else if let Some(r) = rest.strip_prefix('>') {
            1 + usize::from(r.starts_with(' '))
        } else {
            0
        };
        let start = ls + info.indent;
        let content_offset = self.cursor.saturating_sub(start + old);
        self.replace(start..start + old, prefix, EditKind::Other);
        self.cursor = self.clamp(start + prefix.len() + content_offset);
    }

    /// Deletes several ranges as one undo step, swallowing one adjacent space per range so
    /// "call @3pm now" becomes "call now".
    pub fn remove_ranges(&mut self, ranges: &[Range<usize>]) {
        self.record(EditKind::Other);
        let (mut cursor, mut anchor) = (self.cursor, self.anchor);
        for r in ranges.iter().rev() {
            let mut r = r.clone();
            if r.start > 0 && self.text.as_bytes()[r.start - 1] == b' ' {
                r.start -= 1;
            } else if self.text[r.end..].starts_with(' ') {
                r.end += 1;
            }
            self.text.replace_range(r.clone(), "");
            cursor = after_removal(cursor, &r);
            anchor = anchor.map(|p| after_removal(p, &r));
        }
        self.cursor = cursor;
        self.anchor = anchor;
        self.last_edit = None;
    }

    /// Wraps the selection in `marker` (e.g. "**"), or inserts an empty pair at the caret.
    pub fn wrap(&mut self, marker: &str) {
        let r = self.selection().unwrap_or(self.cursor..self.cursor);
        let inner = self.text[r.clone()].to_string();
        self.replace(r.clone(), &format!("{marker}{inner}{marker}"), EditKind::Other);
        self.anchor = Some(r.start + marker.len());
        self.cursor = r.start + marker.len() + inner.len();
        if inner.is_empty() {
            self.anchor = None;
        }
    }

    /// Whether the selection sits right inside a pair of `marker`s (e.g. it is bold already).
    pub fn is_wrapped(&self, marker: &str) -> bool {
        let Some(r) = self.selection() else { return false };
        let m = marker.len();
        r.start >= m && self.text.get(r.start - m..r.start) == Some(marker) && self.text[r.end..].starts_with(marker)
    }

    /// Wraps the selection in `marker`, or unwraps it if it is wrapped already (the toolbar's
    /// Bold, Italic, Strikethrough and Code buttons). The text stays selected.
    pub fn toggle_wrap(&mut self, marker: &str) {
        let Some(r) = self.selection() else { return self.wrap(marker) };
        if !self.is_wrapped(marker) {
            return self.wrap(marker);
        }
        let m = marker.len();
        let inner = self.text[r.clone()].to_string();
        self.replace(r.start - m..r.end + m, &inner, EditKind::Other);
        self.anchor = Some(r.start - m);
        self.cursor = r.end - m;
    }

    /// The `==highlight==` the selection is in: its whole span, its text, and its colour name
    /// ("" for plain `==text==`).
    pub fn highlight_around(&self) -> Option<(Range<usize>, Range<usize>, String)> {
        let r = self.selection()?;
        let (ls, le) = self.line_bounds(r.start);
        let line = &self.text[ls..le];
        crate::markdown::highlights(line).into_iter().find_map(|h| {
            let text = ls + h.text.start..ls + h.text.end;
            (text.start <= r.start && r.end <= text.end).then(|| {
                let open = &line[h.open.clone()];
                let color = open.trim_start_matches('=').trim_end_matches(':').trim().to_string();
                (ls + h.open.start..ls + h.close.end, text, color)
            })
        })
    }

    /// Gives the selection a colour badge (`==green:text==`, or `==text==` for yellow), changes
    /// the colour of the badge it is in, or with `None` removes that badge. The text stays selected.
    pub fn set_highlight(&mut self, color: Option<&str>) {
        let Some(r) = self.selection() else { return };
        let open = |c: &str| if c == "yellow" { "==".to_string() } else { format!("=={c}:") };
        match (self.highlight_around(), color) {
            (Some((span, text, _)), new) => {
                let inner = self.text[text.clone()].to_string();
                let (head, tail) = match new {
                    Some(c) => (open(c), "==".to_string()),
                    None => (String::new(), String::new()),
                };
                self.replace(span.clone(), &format!("{head}{inner}{tail}"), EditKind::Other);
                let start = span.start + head.len() + (r.start - text.start);
                self.anchor = Some(start);
                self.cursor = start + (r.end - r.start);
            }
            (None, Some(c)) => {
                let head = open(c);
                let inner = self.text[r.clone()].to_string();
                self.replace(r.clone(), &format!("{head}{inner}=="), EditKind::Other);
                self.anchor = Some(r.start + head.len());
                self.cursor = r.start + head.len() + inner.len();
            }
            (None, None) => {}
        }
    }

    /// The `{red: text}` colour the selection is in: its whole span, its text, its colour name.
    pub fn text_color_around(&self) -> Option<(Range<usize>, Range<usize>, String)> {
        let r = self.selection()?;
        let (ls, le) = self.line_bounds(r.start);
        let line = &self.text[ls..le];
        crate::markdown::text_colors(line).into_iter().find_map(|h| {
            let text = ls + h.text.start..ls + h.text.end;
            (text.start <= r.start && r.end <= text.end)
                .then(|| (ls + h.open.start..ls + h.close.end, text, crate::markdown::HIGHLIGHTS[h.color].to_string()))
        })
    }

    /// Colours the selected text (`{red: text}`), changes the colour it has, or with `None`
    /// takes it off. The text stays selected.
    pub fn set_text_color(&mut self, color: Option<&str>) {
        let Some(r) = self.selection() else { return };
        match (self.text_color_around(), color) {
            (Some((span, text, _)), new) => {
                let inner = self.text[text.clone()].to_string();
                let head = new.map_or(String::new(), |c| format!("{{{c}: "));
                let tail = if new.is_some() { "}" } else { "" };
                self.replace(span.clone(), &format!("{head}{inner}{tail}"), EditKind::Other);
                let start = span.start + head.len() + (r.start - text.start);
                self.anchor = Some(start);
                self.cursor = start + (r.end - r.start);
            }
            (None, Some(c)) => {
                let head = format!("{{{c}: ");
                let inner = self.text[r.clone()].to_string();
                self.replace(r.clone(), &format!("{head}{inner}}}"), EditKind::Other);
                self.anchor = Some(r.start + head.len());
                self.cursor = r.start + head.len() + inner.len();
            }
            (None, None) => {}
        }
    }

    /// Turns the selection into a link, `[text](https://)`, with the address selected to type
    /// or paste over.
    pub fn link(&mut self) {
        let Some(r) = self.selection() else { return };
        let inner = self.text[r.clone()].to_string();
        self.replace(r.clone(), &format!("[{inner}](https://)"), EditKind::Other);
        let url = r.start + inner.len() + 3;
        self.anchor = Some(url);
        self.cursor = url + "https://".len();
    }

    /// Turns the edits grouped since the last step into a compact history step, dropping the
    /// full copy of the text kept meanwhile. Called as a new group starts, before undo and redo,
    /// and when the note goes idle.
    pub fn fold(&mut self) {
        let Some(before) = self.pending.take() else { return };
        if before.text == self.text {
            return;
        }
        let (start, len, old) = diff(&before.text, &self.text);
        self.undo.push(Change { start, len, old, cursor: before.cursor, anchor: before.anchor });
        let mut bytes: usize = self.undo.iter().map(|c| c.old.len()).sum();
        while self.undo.len() > UNDO_LIMIT || (bytes > UNDO_BYTES && self.undo.len() > 1) {
            bytes -= self.undo.remove(0).old.len();
        }
    }

    /// Applies `c` and returns the step that reverses it.
    fn apply(&mut self, c: Change) -> Change {
        let end = (c.start + c.len).min(self.text.len());
        let now = self.text[c.start..end].to_string();
        self.text.replace_range(c.start..end, &c.old);
        let back = Change { start: c.start, len: c.old.len(), old: now, cursor: self.cursor, anchor: self.anchor };
        self.cursor = c.cursor.min(self.text.len());
        self.anchor = c.anchor.map(|a| a.min(self.text.len()));
        self.last_edit = None;
        back
    }

    pub fn undo(&mut self) {
        self.fold();
        if let Some(c) = self.undo.pop() {
            let back = self.apply(c);
            self.redo.push(back);
        }
    }

    pub fn redo(&mut self) {
        self.fold();
        if let Some(c) = self.redo.pop() {
            let back = self.apply(c);
            self.undo.push(back);
        }
    }

    // ---- motion ---------------------------------------------------------------------------

    pub fn move_to(&mut self, pos: usize, extend: bool) {
        let pos = self.clamp(pos);
        if extend {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = pos;
        self.last_edit = None;
    }

    pub fn clamp(&self, mut pos: usize) -> usize {
        pos = pos.min(self.text.len());
        while !self.text.is_char_boundary(pos) {
            pos -= 1;
        }
        pos
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.text.len();
    }

    pub fn select_word_at(&mut self, pos: usize) {
        let mut start = pos;
        let mut end = pos;
        for (i, w) in self.text.split_word_bound_indices() {
            if i <= pos && pos <= i + w.len() && w.chars().any(|c| c.is_alphanumeric()) {
                start = i;
                end = i + w.len();
                break;
            }
            if i > pos {
                break;
            }
        }
        self.anchor = Some(start);
        self.cursor = end;
    }

    pub fn select_line_at(&mut self, pos: usize) {
        let (ls, le) = self.line_bounds(pos);
        self.anchor = Some(ls);
        self.cursor = (le + 1).min(self.text.len());
    }

    pub fn line_bounds(&self, pos: usize) -> (usize, usize) {
        let start = self.text[..pos].rfind('\n').map_or(0, |i| i + 1);
        let end = self.text[pos..].find('\n').map_or(self.text.len(), |i| pos + i);
        (start, end)
    }

    pub fn prev_grapheme(&self, pos: usize) -> usize {
        self.text[..pos].grapheme_indices(true).next_back().map_or(0, |(i, _)| i)
    }

    pub fn next_grapheme(&self, pos: usize) -> usize {
        self.text[pos..].graphemes(true).next().map_or(pos, |g| pos + g.len())
    }

    pub fn prev_word(&self, pos: usize) -> usize {
        self.text[..pos]
            .split_word_bound_indices()
            .rev()
            .find(|(_, w)| w.chars().any(|c| c.is_alphanumeric()))
            .map_or(0, |(i, _)| i)
    }

    pub fn next_word(&self, pos: usize) -> usize {
        self.text[pos..]
            .split_word_bound_indices()
            .find(|(_, w)| w.chars().any(|c| c.is_alphanumeric()))
            .map_or(self.text.len(), |(i, w)| pos + i + w.len())
    }
}

/// An offset after `delta` bytes were inserted (or removed) at `at`; never moves before `at`.
fn shift_from(p: usize, at: usize, delta: isize) -> usize {
    if p >= at {
        (p as isize + delta).max(at as isize) as usize
    } else {
        p
    }
}

/// An offset after the bytes in `r` were removed.
fn after_removal(p: usize, r: &Range<usize>) -> usize {
    if p >= r.end {
        p - r.len()
    } else {
        p.min(r.start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected(text: &str, word: &str) -> Doc {
        let mut d = Doc::new(text.to_string());
        let at = text.find(word).unwrap();
        d.anchor = Some(at);
        d.cursor = at + word.len();
        d
    }

    #[test]
    fn toolbar_text_colour_and_link() {
        let mut d = selected("call is late now", "late");
        d.set_text_color(Some("red"));
        assert_eq!(d.text, "call is {red: late} now");
        assert_eq!(d.selected_text(), Some("late"));
        d.set_text_color(Some("blue"));
        assert_eq!(d.text, "call is {blue: late} now");
        d.set_text_color(None);
        assert_eq!(d.text, "call is late now");
        let mut d = selected("read the docs", "docs");
        d.link();
        assert_eq!(d.text, "read the [docs](https://)");
        assert_eq!(d.selected_text(), Some("https://"));
    }

    #[test]
    fn toolbar_toggles_formatting() {
        let mut d = selected("ship it now", "it");
        d.toggle_wrap("**");
        assert_eq!(d.text, "ship **it** now");
        assert_eq!(d.selected_text(), Some("it"));
        assert!(d.is_wrapped("**"));
        d.toggle_wrap("**");
        assert_eq!(d.text, "ship it now");
        assert_eq!(d.selected_text(), Some("it"));
        d.undo();
        assert_eq!(d.text, "ship **it** now", "one undo per toggle");
    }

    #[test]
    fn toolbar_sets_changes_and_removes_colours() {
        let mut d = selected("deal is won today", "won");
        d.set_highlight(Some("green"));
        assert_eq!(d.text, "deal is ==green:won== today");
        assert_eq!(d.selected_text(), Some("won"));
        d.set_highlight(Some("red"));
        assert_eq!(d.text, "deal is ==red:won== today");
        d.set_highlight(Some("yellow"));
        assert_eq!(d.text, "deal is ==won== today");
        assert_eq!(d.highlight_around().map(|h| h.2), Some(String::new()));
        d.set_highlight(None);
        assert_eq!(d.text, "deal is won today");
        assert_eq!(d.selected_text(), Some("won"));
    }

    fn doc(text: &str) -> Doc {
        let mut d = Doc::new(text.to_string());
        d.cursor = d.text.len();
        d
    }

    #[test]
    fn continues_checklist() {
        let mut d = doc("- [x] milk");
        d.newline();
        assert_eq!(d.text, "- [x] milk\n- [ ] ");
        d.newline();
        assert_eq!(d.text, "- [x] milk\n");
    }

    #[test]
    fn continues_ordered_list() {
        let mut d = doc("1. a");
        d.newline();
        assert_eq!(d.text, "1. a\n2. ");
    }

    #[test]
    fn toggles_and_creates_checkbox() {
        let mut d = doc("buy eggs");
        d.toggle_check();
        assert_eq!(d.text, "- [ ] buy eggs");
        assert_eq!(d.cursor, d.text.len());
        d.toggle_check();
        assert_eq!(d.text, "- [x] buy eggs");
    }

    #[test]
    fn backspace_removes_marker() {
        let mut d = doc("- [ ] ");
        d.backspace(false);
        assert_eq!(d.text, "");
    }

    #[test]
    fn history_keeps_only_what_changed() {
        let big = "x".repeat(100_000);
        let mut d = doc(&big);
        for i in 0..150 {
            d.cursor = 50_000 + 2 * i;
            d.anchor = None;
            d.replace(d.cursor..d.cursor, "é", EditKind::Other);
        }
        d.fold();
        assert!(d.pending.is_none());
        let kept: usize = d.undo.iter().map(|c| c.old.len() + std::mem::size_of::<Change>()).sum();
        assert!(kept < 16 * 1024, "history holds {kept} bytes, not copies of the note");
        for _ in 0..150 {
            d.undo();
        }
        assert_eq!(d.text, big);
        d.redo();
        assert_eq!(d.text.len(), big.len() + "é".len());
    }

    #[test]
    fn diff_finds_the_changed_part() {
        assert_eq!(diff("hello world", "hello brave world"), (6, 6, String::new()));
        assert_eq!(diff("aaa", "aa"), (2, 0, "a".to_string()));
        assert_eq!(diff("café", "cafe"), (3, 1, "é".to_string()));
        assert_eq!(diff("ab", "ab"), (2, 0, String::new()));
    }

    #[test]
    fn undo_groups_typing() {
        let mut d = doc("");
        for c in ["a", "b", "c"] {
            d.insert(c);
        }
        d.insert(" ");
        d.insert("d");
        d.undo();
        assert_eq!(d.text, "abc ");
        d.undo();
        assert_eq!(d.text, "");
        d.redo();
        assert_eq!(d.text, "abc ");
    }

    #[test]
    fn indent_and_outdent() {
        let mut d = doc("a\nb");
        d.select_all();
        d.indent(false);
        assert_eq!(d.text, "  a\n  b");
        d.indent(true);
        assert_eq!(d.text, "a\nb");
    }

    #[test]
    fn word_motion() {
        let d = doc("hello  wide world");
        assert_eq!(d.next_word(0), 5);
        assert_eq!(d.prev_word(17), 12);
        assert_eq!(d.prev_word(12), 7);
    }

    #[test]
    fn line_prefix_replaces_block_syntax() {
        let mut d = doc("- [ ] buy milk");
        d.set_line_prefix("## ");
        assert_eq!(d.text, "## buy milk");
        assert_eq!(d.cursor, d.text.len());
        d.set_line_prefix("> ");
        assert_eq!(d.text, "> buy milk");
        d.set_line_prefix("");
        assert_eq!(d.text, "buy milk");
        d.set_line_prefix("- [ ] ");
        assert_eq!(d.text, "- [ ] buy milk");
    }

    #[test]
    fn sanitizes_newlines() {
        assert_eq!(sanitize("a\r\nb\rc\u{0}"), "a\nb\nc");
    }

    #[test]
    fn offset_helpers() {
        assert_eq!(shift_from(5, 3, 2), 7);
        assert_eq!(shift_from(2, 3, 2), 2);
        assert_eq!(shift_from(4, 3, -2), 3);
        assert_eq!(after_removal(10, &(2..5)), 7);
        assert_eq!(after_removal(3, &(2..5)), 2);
        assert_eq!(after_removal(1, &(2..5)), 1);
    }
}
