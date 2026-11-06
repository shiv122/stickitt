//! In-note popovers, after Notion: the page-icon picker, the `/` block menu and `:emoji:`
//! shortcodes.

use emojis::{Group, SkinTone, UnicodeVersion};

/// Curated page icons.
pub const EMOJI: [&str; 40] = [
    "📌", "⭐", "🔥", "💡", "✅", "📚", "🎯", "🚀", "❤️", "🛒", "🏠", "💼", "📅", "⏰", "📝", "🎨", "🎵", "✈️", "🍎",
    "☕", "💪", "🧠", "🐛", "🔑", "💰", "📈", "🎁", "🌱", "🌙", "☀️", "⚡", "🧩", "📦", "🔔", "💬", "🔗", "🧪", "🏃",
    "🎉", "✏️",
];

/// Icon picker tabs: a glyph, a name and the emoji group shown (None = the curated picks).
pub const ICON_TABS: [(&str, &str, Option<Group>); 9] = [
    ("⭐", "favourites", None),
    ("😀", "smileys", Some(Group::SmileysAndEmotion)),
    ("👋", "people", Some(Group::PeopleAndBody)),
    ("🐶", "nature", Some(Group::AnimalsAndNature)),
    ("🍔", "food", Some(Group::FoodAndDrink)),
    ("✈️", "travel", Some(Group::TravelAndPlaces)),
    ("⚽", "activities", Some(Group::Activities)),
    ("💡", "objects", Some(Group::Objects)),
    ("🔣", "symbols", Some(Group::Symbols)),
];

/// Emoji the system font can draw (Windows 11's Segoe UI Emoji covers Emoji 14), default
/// skin tone only.
fn drawable(e: &emojis::Emoji) -> bool {
    e.unicode_version() <= UnicodeVersion::new(14, 0) && e.skin_tone().is_none_or(|t| t == SkinTone::Default)
}

/// Icons for a picker tab, or search results (by name or shortcode) when `query` is set.
pub fn icons(tab: usize, query: &str) -> Vec<&'static str> {
    let q = query.trim().to_lowercase();
    if !q.is_empty() {
        return emojis::iter()
            .filter(|e| drawable(e) && (e.name().contains(&q) || e.shortcodes().any(|s| s.contains(&q))))
            .map(|e| e.as_str())
            .take(240)
            .collect();
    }
    match ICON_TABS.get(tab).and_then(|t| t.2) {
        Some(group) => group.emojis().filter(|e| drawable(e)).map(|e| e.as_str()).collect(),
        None => EMOJI.to_vec(),
    }
}

/// `:shortcode` completions as (shortcode, emoji): shortest matching shortcodes first.
pub fn shortcode_matches(query: &str, limit: usize) -> Vec<(&'static str, &'static str)> {
    let q = query.to_lowercase();
    let mut out: Vec<(&'static str, &'static str)> = emojis::iter()
        .filter(|e| drawable(e))
        .filter_map(|e| e.shortcodes().find(|s| s.starts_with(&q)).map(|s| (s, e.as_str())))
        .collect();
    out.sort_by_key(|(s, _)| (s.len(), *s));
    out.truncate(limit);
    out
}

/// The emoji for an exact shortcode, e.g. "fire" → 🔥.
pub fn shortcode(name: &str) -> Option<&'static str> {
    emojis::get_by_shortcode(name).map(|e| e.as_str())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SlashKind {
    /// Plain text: strips the line's block syntax.
    Text,
    /// Replaces the line's block syntax with this prefix.
    Prefix(&'static str),
    CodeBlock,
    /// A two-column table to fill in.
    Table,
    Divider,
    /// Starts an `@` reminder token.
    Reminder,
    /// Opens the icon picker.
    Icon,
}

pub struct SlashItem {
    pub label: &'static str,
    /// Short glyph shown in the item's tile.
    pub tile: &'static str,
    /// Extra words the filter matches.
    pub aliases: &'static str,
    pub kind: SlashKind,
}

pub const SLASH: [SlashItem; 14] = [
    SlashItem { label: "Text", tile: "T", aliases: "plain paragraph", kind: SlashKind::Text },
    SlashItem {
        label: "To-do list",
        tile: "☐",
        aliases: "todo check task checkbox",
        kind: SlashKind::Prefix("- [ ] "),
    },
    SlashItem { label: "Heading 1", tile: "H1", aliases: "h1 title", kind: SlashKind::Prefix("# ") },
    SlashItem { label: "Heading 2", tile: "H2", aliases: "h2 subtitle", kind: SlashKind::Prefix("## ") },
    SlashItem { label: "Heading 3", tile: "H3", aliases: "h3", kind: SlashKind::Prefix("### ") },
    SlashItem { label: "Bulleted list", tile: "•", aliases: "bullet unordered ul", kind: SlashKind::Prefix("- ") },
    SlashItem { label: "Numbered list", tile: "1.", aliases: "number ordered ol", kind: SlashKind::Prefix("1. ") },
    SlashItem { label: "Quote", tile: "❝", aliases: "blockquote callout", kind: SlashKind::Prefix("> ") },
    SlashItem { label: "Table", tile: "▦", aliases: "table grid columns rows", kind: SlashKind::Table },
    SlashItem {
        label: "Callout", tile: "💡", aliases: "callout tip note info box", kind: SlashKind::Prefix("> 💡 ")
    },
    SlashItem { label: "Code", tile: "</>", aliases: "code block snippet", kind: SlashKind::CodeBlock },
    SlashItem { label: "Divider", tile: "—", aliases: "hr line separator rule", kind: SlashKind::Divider },
    SlashItem { label: "Reminder", tile: "⏰", aliases: "remind alarm time date due", kind: SlashKind::Reminder },
    SlashItem { label: "Icon", tile: "☺", aliases: "emoji page icon", kind: SlashKind::Icon },
];

/// Items matching `query` (case-insensitive prefix of any word in the label or aliases).
pub fn filter(query: &str) -> Vec<usize> {
    let q = query.to_lowercase();
    (0..SLASH.len())
        .filter(|&i| {
            let item = &SLASH[i];
            q.is_empty()
                || item.label.to_lowercase().starts_with(&q)
                || item.label.to_lowercase().split([' ', '-']).chain(item.aliases.split(' ')).any(|w| w.starts_with(&q))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emoji_lookups() {
        assert_eq!(shortcode("fire"), Some("🔥"));
        assert_eq!(shortcode("nope_not_real"), None);
        let thumbs = shortcode_matches("thumbs", 5);
        assert!(thumbs.iter().any(|(s, _)| *s == "thumbsup"), "{thumbs:?}");
        assert!(icons(1, "").len() > 50, "smileys tab");
        assert!(icons(0, "rocket").contains(&"🚀"));
    }

    #[test]
    fn filters_by_words_and_aliases() {
        let labels = |q: &str| filter(q).into_iter().map(|i| SLASH[i].label).collect::<Vec<_>>();
        assert_eq!(labels("").len(), SLASH.len());
        assert_eq!(labels("todo"), ["To-do list"]);
        assert_eq!(labels("h2"), ["Heading 2"]);
        assert_eq!(labels("head"), ["Heading 1", "Heading 2", "Heading 3"]);
        assert_eq!(labels("list"), ["To-do list", "Bulleted list", "Numbered list"]);
        assert!(labels("zzz").is_empty());
    }
}
