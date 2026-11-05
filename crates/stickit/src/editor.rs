//! Plain-text document model: text, caret/selection, undo history and IME composition.
//! Offsets are byte indices into `text` and always sit on char boundaries.

use crate::markdown::line_info;
use std::borrow::Cow;
use std::ops::Range;
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;

const UNDO_LIMIT: usize = 200;
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
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
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
            self.undo.push(self.snapshot());
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
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

    pub fn undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            self.redo.push(self.snapshot());
            self.restore(s);
        }
    }

    pub fn redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            self.undo.push(self.snapshot());
            self.restore(s);
        }
    }

    fn restore(&mut self, s: Snapshot) {
        self.text = s.text;
        self.cursor = s.cursor.min(self.text.len());
        self.anchor = s.anchor.map(|a| a.min(self.text.len()));
        self.last_edit = None;
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
