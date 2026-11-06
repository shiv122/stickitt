//! Lightweight, source-preserving markdown styling.
//!
//! Notes are edited as plain markdown: we never hide characters, we only style them
//! (markers dimmed, headings larger, etc.). Styling is computed per byte as bit flags and
//! then merged into runs for the text shaper. Lines are styled independently except for
//! fenced code blocks, which is all a sticky note needs and keeps re-styling trivially cheap.

use std::ops::Range;

pub type Flags = u16;

pub const BOLD: Flags = 1;
pub const ITALIC: Flags = 1 << 1;
pub const CODE: Flags = 1 << 2;
pub const STRIKE: Flags = 1 << 3;
pub const LINK: Flags = 1 << 4;
pub const MARKER: Flags = 1 << 5;
pub const QUOTE: Flags = 1 << 6;
pub const DONE: Flags = 1 << 7;
pub const BULLET: Flags = 1 << 8;
pub const REMINDER: Flags = 1 << 9;
pub const PREEDIT: Flags = 1 << 10;
/// Laid out but not painted (list syntax replaced by drawn checkboxes and bullets).
pub const HIDDEN: Flags = 1 << 14;
/// Shaped at near-zero width: markdown syntax hidden in live preview.
pub const COLLAPSED: Flags = 1 << 15;
const HEADING_SHIFT: u16 = 11;
const HEADING_MASK: Flags = 0b111 << HEADING_SHIFT;

pub fn heading_level(f: Flags) -> u8 {
    ((f & HEADING_MASK) >> HEADING_SHIFT) as u8
}

/// Structure of a single line, shared by styling and editing commands.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LineInfo {
    pub indent: usize,
    /// Length of the list marker including indent and trailing space ("  - " → 4).
    pub marker_end: usize,
    /// Byte offset of `[` within the line, and whether the box is checked.
    pub checkbox: Option<(usize, bool)>,
    /// Number of an ordered list item.
    pub ordered: Option<u64>,
}

impl LineInfo {
    pub fn is_list(&self) -> bool {
        self.marker_end > 0
    }

    /// Byte offset where the item's content starts (after marker and checkbox).
    pub fn content_start(&self, line: &str) -> usize {
        match self.checkbox {
            Some((at, _)) => (at + 4).min(line.len()),
            None => self.marker_end,
        }
    }
}

/// A table row (`| a | b |`): the byte offsets of its column dividers. A `|` inside inline
/// code or escaped as `\|` is text, not a divider.
pub fn table_pipes(line: &str) -> Option<Vec<usize>> {
    if !line.trim_start().starts_with('|') {
        return None;
    }
    let b = line.as_bytes();
    let mut pipes = Vec::new();
    let mut code = false;
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'`' => code = !code,
            b'|' if !code && (i == 0 || b[i - 1] != b'\\') => pipes.push(i),
            _ => {}
        }
    }
    (pipes.len() >= 2).then_some(pipes)
}

/// The line under a table's header: `| --- | :---: |`.
pub fn is_table_separator(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.contains('-') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// A row's cells: the text between dividers (and after the last one, if any), trimmed.
pub fn table_cells(line: &str, pipes: &[usize]) -> Vec<Range<usize>> {
    let mut cells = Vec::new();
    let trim = |r: Range<usize>| {
        let s = &line[r.clone()];
        let start = r.start + (s.len() - s.trim_start().len());
        let end = r.end - (s.len() - s.trim_end().len());
        start..end.max(start)
    };
    for w in pipes.windows(2) {
        cells.push(trim(w[0] + 1..w[1]));
    }
    let last = *pipes.last().unwrap_or(&0) + 1;
    if last < line.len() && !line[last..].trim().is_empty() {
        cells.push(trim(last..line.len()));
    }
    cells
}

pub fn line_info(line: &str) -> LineInfo {
    let trimmed = line.trim_start_matches([' ', '\t']);
    let indent = line.len() - trimmed.len();
    let b = trimmed.as_bytes();
    let mut info = LineInfo { indent, ..LineInfo::default() };

    let marker_len = if b.len() >= 2 && matches!(b[0], b'-' | b'*' | b'+') && b[1] == b' ' {
        2
    } else {
        let digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
        if (1..=9).contains(&digits)
            && b.len() > digits + 1
            && matches!(b[digits], b'.' | b')')
            && b[digits + 1] == b' '
        {
            info.ordered = trimmed[..digits].parse().ok();
            digits + 2
        } else {
            0
        }
    };
    if marker_len == 0 {
        return info;
    }
    info.marker_end = indent + marker_len;

    let rest = &line.as_bytes()[info.marker_end..];
    if rest.len() >= 3
        && rest[0] == b'['
        && rest[2] == b']'
        && matches!(rest[1], b' ' | b'x' | b'X')
        && (rest.len() == 3 || rest[3] == b' ')
    {
        info.checkbox = Some((info.marker_end, rest[1] != b' '));
    }
    info
}

/// Colours a highlight can name, as in `==red:text==` (plain `==text==` is the first).
pub const HIGHLIGHTS: [&str; 9] = ["yellow", "gray", "brown", "orange", "green", "blue", "purple", "pink", "red"];

/// A `==text==` or `==red:text==` highlight: where its text and its syntax are (byte ranges of
/// the whole text), and its colour (an index into `HIGHLIGHTS`).
#[derive(Debug, PartialEq, Eq)]
pub struct Highlight {
    pub text: Range<usize>,
    pub open: Range<usize>,
    pub close: Range<usize>,
    pub color: usize,
}

/// Every highlight in `text`; each stays within one line.
pub fn highlights(text: &str) -> Vec<Highlight> {
    let mut out = Vec::new();
    let mut base = 0;
    for line in text.split('\n') {
        let mut i = 0;
        while let Some(at) = line[i..].find("==").map(|k| i + k) {
            let from = at + 2;
            let Some(len) = line[from..].find("==") else { break };
            let to = from + len;
            let inner = &line[from..to];
            if inner.trim().is_empty() || inner.starts_with(' ') || inner.ends_with(' ') {
                i = from;
                continue;
            }
            // An optional colour: `red:`, then the text.
            let (color, skip) = match inner.split_once(':') {
                Some((name, rest)) if !rest.trim().is_empty() => {
                    match HIGHLIGHTS.iter().position(|h| h.eq_ignore_ascii_case(name)) {
                        Some(c) => (c, name.len() + 1 + (rest.len() - rest.trim_start().len())),
                        None => (0, 0),
                    }
                }
                _ => (0, 0),
            };
            out.push(Highlight {
                text: base + from + skip..base + to,
                open: base + at..base + from + skip,
                close: base + to..base + to + 2,
                color,
            });
            i = to + 2;
        }
        base += line.len() + 1;
    }
    out
}

/// Every `{red: text}` text colour in `text` (colours as in `HIGHLIGHTS`); each stays in a line.
pub fn text_colors(text: &str) -> Vec<Highlight> {
    let mut out = Vec::new();
    let mut base = 0;
    for line in text.split('\n') {
        let mut i = 0;
        while let Some(at) = line[i..].find('{').map(|k| i + k) {
            let Some(colon) = line[at..].find(':').map(|k| at + k) else { break };
            let name = &line[at + 1..colon];
            let Some(color) = HIGHLIGHTS.iter().position(|h| h.eq_ignore_ascii_case(name)) else {
                i = at + 1;
                continue;
            };
            let body = colon + 1 + (line[colon + 1..].len() - line[colon + 1..].trim_start().len());
            let Some(close) = line[body..].find('}').map(|k| body + k) else { break };
            if close > body {
                out.push(Highlight {
                    text: base + body..base + close,
                    open: base + at..base + body,
                    close: base + close..base + close + 1,
                    color,
                });
            }
            i = close + 1;
        }
        base += line.len() + 1;
    }
    out
}

/// A callout: a quote whose text starts with an emoji (`> 💡 Remember the demo`).
pub fn is_callout(line: &str) -> bool {
    let rest = line.trim_start().strip_prefix('>').map(str::trim_start);
    rest.and_then(|r| r.chars().next()).is_some_and(|c| {
        let c = c as u32;
        (0x1F300..=0x1FAFF).contains(&c) || (0x2600..=0x27BF).contains(&c) || (0x2B00..=0x2BFF).contains(&c)
    })
}

/// The web or mail address of the link at byte `col` of `line`: a `[text](url)` or a bare
/// `https://…`. Anything else (files, scripts) is not a link to open.
pub fn link_at(line: &str, col: usize) -> Option<&str> {
    let safe = |u: &str| ["https://", "http://", "mailto:"].iter().any(|p| u.starts_with(p));
    let mut i = 0;
    while let Some(open) = line[i..].find('[').map(|k| i + k) {
        let Some(close) = line[open..].find("](").map(|k| open + k) else { break };
        let Some(end) = line[close..].find(')').map(|k| close + k) else { break };
        if (open..=end).contains(&col) {
            let url = line[close + 2..end].trim();
            return safe(url).then_some(url);
        }
        i = end + 1;
    }
    let start = line[..col.min(line.len())].rfind(char::is_whitespace).map_or(0, |k| k + 1);
    let end = line[start..].find(char::is_whitespace).map_or(line.len(), |k| start + k);
    let word = line[start..end].trim_end_matches(['.', ',', ')', ';']);
    safe(word).then_some(word)
}

/// `line` without badge and text colour syntax: `==green:Won==` reads `Won` (for summaries).
pub fn strip_marks(line: &str) -> String {
    let mut cut: Vec<std::ops::Range<usize>> =
        highlights(line).into_iter().chain(text_colors(line)).flat_map(|h| [h.open, h.close]).collect();
    cut.sort_by_key(|r| r.start);
    let mut out = String::with_capacity(line.len());
    let mut at = 0;
    for r in cut {
        if r.start >= at {
            out.push_str(&line[at..r.start]);
            at = r.end;
        }
    }
    out.push_str(&line[at..]);
    out
}

/// Per-byte style flags for `text`.
pub fn style(text: &str) -> Vec<Flags> {
    let mut st = vec![0; text.len()];
    let mut in_fence = false;
    let mut start = 0;
    for line in text.split('\n') {
        let end = start + line.len();
        style_line(text, start, end, &mut st, &mut in_fence);
        start = end + 1;
    }
    st
}

/// Merges per-byte flags into contiguous runs.
pub fn runs(st: &[Flags]) -> Vec<(Range<usize>, Flags)> {
    let mut out: Vec<(Range<usize>, Flags)> = Vec::new();
    for (i, &f) in st.iter().enumerate() {
        match out.last_mut() {
            Some((r, last)) if *last == f => r.end = i + 1,
            _ => out.push((i..i + 1, f)),
        }
    }
    out
}

pub fn mark(st: &mut [Flags], r: Range<usize>, f: Flags) {
    for s in &mut st[r] {
        *s |= f;
    }
}

fn style_line(text: &str, s: usize, e: usize, st: &mut [Flags], in_fence: &mut bool) {
    let line = &text[s..e];
    let trimmed = line.trim_start();
    if trimmed.starts_with("```") {
        mark(st, s..e, CODE | MARKER);
        *in_fence = !*in_fence;
        return;
    }
    if *in_fence {
        mark(st, s..e, CODE);
        return;
    }
    let compact: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.len() >= 3
        && (compact.bytes().all(|c| c == b'-')
            || compact.bytes().all(|c| c == b'*')
            || compact.bytes().all(|c| c == b'_'))
    {
        mark(st, s..e, MARKER);
        return;
    }

    let mut body = s;
    let hashes = line.bytes().take_while(|&c| c == b'#').count();
    if (1..=6).contains(&hashes) && (line.len() == hashes || line.as_bytes()[hashes] == b' ') {
        let m = (hashes + 1).min(line.len());
        mark(st, s..e, (hashes as Flags) << HEADING_SHIFT);
        mark(st, s..s + m, MARKER);
        body = s + m;
    } else if let Some(rest) = trimmed.strip_prefix('>') {
        let m = line.len() - rest.len() + usize::from(rest.starts_with(' '));
        mark(st, s..s + m, MARKER);
        mark(st, s + m..e, QUOTE);
        body = s + m;
    } else {
        let info = line_info(line);
        if info.is_list() {
            mark(st, s + info.indent..s + info.marker_end, BULLET);
            body = s + info.marker_end;
            if let Some((at, checked)) = info.checkbox {
                mark(st, s + at..s + at + 3, BULLET);
                body = s + info.content_start(line);
                if checked {
                    mark(st, body..e, DONE);
                }
            }
        }
    }
    inline(text, body, e, st);
}

fn inline(text: &str, s: usize, e: usize, st: &mut [Flags]) {
    let b = text.as_bytes();
    let mut i = s;
    while i < e {
        match b[i] {
            b'`' => {
                if let Some(j) = find_byte(b, i + 1, e, b'`') {
                    mark(st, i..j + 1, CODE);
                    mark(st, i..i + 1, MARKER);
                    mark(st, j..j + 1, MARKER);
                    i = j + 1;
                    continue;
                }
            }
            c @ (b'*' | b'_' | b'~') => {
                let double = i + 1 < e && b[i + 1] == c;
                let dl = if double { 2 } else { 1 };
                let opens = i + dl < e
                    && !b[i + dl].is_ascii_whitespace()
                    && (c != b'~' || double)
                    && !(c == b'_' && i > s && b[i - 1].is_ascii_alphanumeric());
                if opens {
                    if let Some(j) = find_closer(b, i + dl, e, c, dl) {
                        let flag = match (c, dl) {
                            (b'~', _) => STRIKE,
                            (_, 2) => BOLD,
                            _ => ITALIC,
                        };
                        mark(st, i + dl..j, flag);
                        mark(st, i..i + dl, MARKER);
                        mark(st, j..j + dl, MARKER);
                        inline(text, i + dl, j, st);
                        i = j + dl;
                        continue;
                    }
                }
                i += dl;
                continue;
            }
            b'[' => {
                if let Some(close) = find_byte(b, i + 1, e, b']') {
                    if close + 1 < e && b[close + 1] == b'(' {
                        if let Some(paren) = find_byte(b, close + 2, e, b')') {
                            mark(st, i + 1..close, LINK);
                            mark(st, i..i + 1, MARKER);
                            mark(st, close..paren + 1, MARKER);
                            i = paren + 1;
                            continue;
                        }
                    }
                }
            }
            b'h' if (i == s || b[i - 1].is_ascii_whitespace() || b[i - 1] == b'(')
                && (text[i..e].starts_with("https://") || text[i..e].starts_with("http://")) =>
            {
                let end = (i..e).find(|&j| b[j].is_ascii_whitespace()).unwrap_or(e);
                mark(st, i..end, LINK);
                i = end;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
}

fn find_byte(b: &[u8], from: usize, to: usize, needle: u8) -> Option<usize> {
    (from..to).find(|&j| b[j] == needle)
}

fn find_closer(b: &[u8], from: usize, to: usize, c: u8, dl: usize) -> Option<usize> {
    let mut j = from + 1;
    while j + dl <= to {
        let matches = b[j..j + dl].iter().all(|&x| x == c);
        let single_ok = dl == 2 || j + 1 >= to || b[j + 1] != c;
        if matches && single_ok && !b[j - 1].is_ascii_whitespace() {
            return Some(j);
        }
        j += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_colours_callouts_links() {
        let t = "a {red: late} and {blue:ok} but {nope: x} {red: }";
        let found: Vec<(&str, &str)> =
            text_colors(t).iter().map(|h| (&t[h.text.clone()], &t[h.open.clone()])).collect();
        assert_eq!(found, [("late", "{red: "), ("ok", "{blue:")]);
        assert!(is_callout("> 💡 Tip"));
        assert!(!is_callout("> just a quote"));
        let line = "see [docs](https://x.dev/a) or https://y.dev, not [f](file:///etc)";
        assert_eq!(link_at(line, 6), Some("https://x.dev/a"));
        assert_eq!(link_at(line, line.find("y.dev").unwrap()), Some("https://y.dev"));
        assert_eq!(link_at(line, line.find("[f]").unwrap() + 1), None, "only web and mail links");
        assert_eq!(link_at(line, 1), None);
        assert_eq!(strip_marks("==green:Won== and {red: late}"), "Won and late");
    }

    #[test]
    fn highlights_with_and_without_colour() {
        let text = "a ==big== and ==red:urgent== but == not == or ==blue:==\nx ==sky:y==";
        let found: Vec<(&str, &str, usize)> =
            highlights(text).iter().map(|h| (&text[h.text.clone()], &text[h.open.clone()], h.color)).collect();
        assert_eq!(found, [("big", "==", 0), ("urgent", "==red:", 8), ("blue:", "==", 0), ("sky:y", "==", 0)]);
    }

    #[test]
    fn tables() {
        let row = "| Deal | Stage | `a|b` |";
        let pipes = table_pipes(row).unwrap();
        assert_eq!(pipes.len(), 4, "the pipe inside code is text");
        let cells: Vec<&str> = table_cells(row, &pipes).into_iter().map(|r| &row[r]).collect();
        assert_eq!(cells, ["Deal", "Stage", "`a|b`"]);
        // No trailing divider: the last cell runs to the end of the line.
        let row = "| a |  b";
        let cells: Vec<&str> = table_cells(row, &table_pipes(row).unwrap()).into_iter().map(|r| &row[r]).collect();
        assert_eq!(cells, ["a", "b"]);
        assert!(table_pipes("no | table").is_none());
        assert!(table_pipes("| lonely").is_none());
        assert!(is_table_separator("| --- | :--: |"));
        assert!(!is_table_separator("| a | b |"));
    }

    #[test]
    fn checklist_line() {
        let info = line_info("  - [x] done");
        assert_eq!(info.marker_end, 4);
        assert_eq!(info.checkbox, Some((4, true)));
        assert_eq!(info.content_start("  - [x] done"), 8);
    }

    #[test]
    fn ordered_line() {
        let info = line_info("12. item");
        assert_eq!(info.ordered, Some(12));
        assert_eq!(info.marker_end, 4);
    }

    #[test]
    fn inline_styles() {
        let t = "a **b** *c* `d` ~~e~~";
        let st = style(t);
        assert!(st[4] & BOLD != 0 && st[2] & MARKER != 0);
        assert!(st[9] & ITALIC != 0);
        assert!(st[13] & CODE != 0);
        assert!(st[18] & STRIKE != 0);
    }

    #[test]
    fn snake_case_is_not_italic() {
        let st = style("my_var_name");
        assert!(st.iter().all(|f| f & ITALIC == 0));
    }

    #[test]
    fn fence_spans_lines() {
        let st = style("```\n*x*\n```\n*y*");
        assert!(st[4] & CODE != 0 && st[5] & ITALIC == 0);
        assert!(st[13] & ITALIC != 0);
    }

    #[test]
    fn runs_tile_the_text() {
        let text = "# Title\nplain **b**";
        let flags = style(text);
        assert_eq!(flags.len(), text.len());
        let runs = runs(&flags);
        assert_eq!(runs.first().map(|r| r.0.start), Some(0));
        assert_eq!(runs.last().map(|r| r.0.end), Some(text.len()));
        assert!(runs.windows(2).all(|w| w[0].0.end == w[1].0.start && w[0].1 != w[1].1));
        assert_eq!(heading_level(flags[2]), 1);
        assert_eq!(heading_level(flags[9]), 0);
    }
}
