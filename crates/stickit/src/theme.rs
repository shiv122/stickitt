//! Colours, after Notion's palette: a neutral page, and per-note colours that are muted
//! rather than neon. A note's colour tints its "cover" (the top of the card), its icon tile
//! in the dock and its accents; light/dark follows the system appearance unless the settings
//! choose one.

use std::sync::atomic::{AtomicU8, Ordering};
use winit::window::Theme;

/// Light or dark: following the system, or chosen in the settings (`[general] theme`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Appearance {
    #[default]
    Auto,
    Light,
    Dark,
}

static APPEARANCE: AtomicU8 = AtomicU8::new(Appearance::Auto as u8);

/// Applies the settings' choice to every window drawn from now on.
pub fn set_appearance(a: Appearance) {
    APPEARANCE.store(a as u8, Ordering::Relaxed);
}

/// Whether to draw dark, given the system's theme for the window.
pub fn is_dark(system: Option<Theme>) -> bool {
    match APPEARANCE.load(Ordering::Relaxed) {
        x if x == Appearance::Light as u8 => false,
        x if x == Appearance::Dark as u8 => true,
        _ => system == Some(Theme::Dark),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const fn text(self, alpha: u8) -> cosmic_text::Color {
        cosmic_text::Color::rgba(self.0, self.1, self.2, alpha)
    }
}

/// (accent, background) pairs for light and dark mode.
struct Tint {
    light: (Rgb, Rgb),
    dark: (Rgb, Rgb),
}

/// Stored per note by index, so new colours are only ever appended.
/// Yellow, pink, blue, green, purple, gray, orange, red, brown.
const TINTS: [Tint; 9] = [
    Tint {
        light: (Rgb(0xCB, 0x91, 0x2F), Rgb(0xFB, 0xF3, 0xDB)),
        dark: (Rgb(0xCA, 0x98, 0x49), Rgb(0x56, 0x43, 0x28)),
    },
    Tint {
        light: (Rgb(0xC1, 0x4C, 0x8A), Rgb(0xFA, 0xF1, 0xF5)),
        dark: (Rgb(0xD1, 0x57, 0x96), Rgb(0x4E, 0x2C, 0x3C)),
    },
    Tint {
        light: (Rgb(0x33, 0x7E, 0xA9), Rgb(0xE7, 0xF3, 0xF8)),
        dark: (Rgb(0x37, 0x9A, 0xD3), Rgb(0x14, 0x3A, 0x4E)),
    },
    Tint {
        light: (Rgb(0x44, 0x83, 0x61), Rgb(0xED, 0xF3, 0xEC)),
        dark: (Rgb(0x52, 0x9E, 0x72), Rgb(0x24, 0x3D, 0x30)),
    },
    Tint {
        light: (Rgb(0x90, 0x65, 0xB0), Rgb(0xF6, 0xF3, 0xF9)),
        dark: (Rgb(0x9D, 0x68, 0xD3), Rgb(0x3C, 0x2D, 0x49)),
    },
    Tint {
        light: (Rgb(0x78, 0x77, 0x74), Rgb(0xF1, 0xF1, 0xEF)),
        dark: (Rgb(0x9B, 0x9B, 0x9B), Rgb(0x2F, 0x2F, 0x2F)),
    },
    Tint {
        light: (Rgb(0xD9, 0x73, 0x0D), Rgb(0xFB, 0xEC, 0xDD)),
        dark: (Rgb(0xC7, 0x7D, 0x48), Rgb(0x5C, 0x3B, 0x23)),
    },
    Tint {
        light: (Rgb(0xD4, 0x4C, 0x47), Rgb(0xFD, 0xEB, 0xEC)),
        dark: (Rgb(0xDF, 0x54, 0x52), Rgb(0x52, 0x2E, 0x2A)),
    },
    Tint {
        light: (Rgb(0x9F, 0x6B, 0x53), Rgb(0xF4, 0xEE, 0xEE)),
        dark: (Rgb(0xBA, 0x85, 0x6F), Rgb(0x4A, 0x32, 0x28)),
    },
];

/// Colours after the nine Notion ones, derived from (hue, saturation).
const EXTRA_HUES: [(f32, f32); 9] = [
    (174.0, 0.50), // teal
    (199.0, 0.70), // sky
    (234.0, 0.50), // indigo
    (268.0, 0.55), // violet
    (345.0, 0.60), // rose
    (38.0, 0.85),  // amber
    (84.0, 0.55),  // lime
    (152.0, 0.45), // mint
    (215.0, 0.15), // slate
];

/// Colours from the app icon, after the hue presets: Cobalt (the page curl and pin), Grape and
/// Aqua (the tabs) and Lavender (the note). Light-mode accents are deepened for readable text.
const BRAND: [Tint; 4] = [
    Tint {
        light: (Rgb(0x45, 0x5F, 0xF3), Rgb(0xEC, 0xEF, 0xFE)),
        dark: (Rgb(0x6F, 0x86, 0xFF), Rgb(0x1E, 0x25, 0x50)),
    },
    Tint {
        light: (Rgb(0x7A, 0x2E, 0xE6), Rgb(0xF3, 0xEC, 0xFF)),
        dark: (Rgb(0xA6, 0x6B, 0xFF), Rgb(0x2E, 0x1F, 0x4D)),
    },
    Tint {
        light: (Rgb(0x0A, 0x9F, 0xC2), Rgb(0xE3, 0xF7, 0xFC)),
        dark: (Rgb(0x2C, 0xC8, 0xEE), Rgb(0x12, 0x38, 0x4A)),
    },
    Tint {
        light: (Rgb(0x6B, 0x78, 0xE8), Rgb(0xE6, 0xE9, 0xFE)),
        dark: (Rgb(0xAE, 0xB8, 0xFF), Rgb(0x2A, 0x2D, 0x52)),
    },
];

/// Number of preset colours; a note's colour is a preset index or `CUSTOM + hue°`.
pub const PRESETS: u16 = (TINTS.len() + EXTRA_HUES.len() + BRAND.len()) as u16;
/// The colours new notes take in turn (`[notes] default_color = cycle`): the app icon's first,
/// then well-spread presets. Lavender, Cobalt, Aqua, Grape, Yellow, Pink, Green, Orange, Blue,
/// Teal, Rose, Mint.
pub const ROTATION: [u16; 12] = [21, 18, 20, 19, 0, 1, 3, 6, 2, 9, 13, 16];
pub const CUSTOM: u16 = 1000;
/// Saturation used for custom hues.
const CUSTOM_SATURATION: f32 = 0.75;

pub const PRESET_NAMES: [&str; PRESETS as usize] = [
    "Yellow", "Pink", "Blue", "Green", "Purple", "Gray", "Orange", "Red", "Brown", "Teal", "Sky", "Indigo", "Violet",
    "Rose", "Amber", "Lime", "Mint", "Slate", "Cobalt", "Grape", "Aqua", "Lavender",
];

/// A preset by its name (any case), e.g. "lavender".
pub fn preset_named(name: &str) -> Option<u16> {
    PRESET_NAMES.iter().position(|n| n.eq_ignore_ascii_case(name.trim())).map(|i| i as u16)
}

pub fn color_name(color: u16) -> String {
    match PRESET_NAMES.get(color as usize) {
        Some(name) => name.to_string(),
        None => format!("Custom {}°", color.saturating_sub(CUSTOM) % 360),
    }
}

/// The hue (degrees) of a custom colour.
pub fn custom_hue(color: u16) -> Option<f32> {
    (color >= CUSTOM).then(|| ((color - CUSTOM) % 360) as f32)
}

fn hsl(h: f32, s: f32, l: f32) -> Rgb {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    Rgb(to(r), to(g), to(b))
}

/// (accent, wash) for any hue, tuned to sit alongside the Notion colours.
fn from_hue(h: f32, s: f32, dark: bool) -> (Rgb, Rgb) {
    if dark {
        (hsl(h, 0.25 + 0.55 * s, 0.62), hsl(h, 0.15 + 0.3 * s, 0.21))
    } else {
        (hsl(h, 0.2 + 0.55 * s, 0.42), hsl(h, 0.25 + 0.55 * s, 0.94))
    }
}

/// The hue strip of the colour picker, sampled.
pub fn hue_sample(h: f32, dark: bool) -> Rgb {
    from_hue(h, CUSTOM_SATURATION, dark).0
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    /// Card (page) background.
    pub base: Rgb,
    pub text: Rgb,
    pub secondary: Rgb,
    /// The note's colour: checkboxes, caret, badges.
    pub accent: Rgb,
    /// The note's soft background colour: cover gradient, tags, dock tile.
    pub wash: Rgb,
    pub selection: (Rgb, u8),
    pub alert: Rgb,
    /// Inline code text colour (Notion's signature red).
    pub code: Rgb,
    /// Alpha for neutral fills (code backgrounds, hover states) in `text` colour.
    pub soft: u8,
    pub shadow: u8,
}

/// (accent, wash) for a note colour.
pub fn tint(color: u16, dark: bool) -> (Rgb, Rgb) {
    let i = color as usize;
    if let Some(t) = TINTS.get(i) {
        return if dark { t.dark } else { t.light };
    }
    if let Some(&(h, s)) = EXTRA_HUES.get(i.wrapping_sub(TINTS.len())) {
        return from_hue(h, s, dark);
    }
    if let Some(t) = BRAND.get(i.wrapping_sub(TINTS.len() + EXTRA_HUES.len())) {
        return if dark { t.dark } else { t.light };
    }
    from_hue(custom_hue(color).unwrap_or(0.0), CUSTOM_SATURATION, dark)
}

pub fn palette(index: u16, dark: bool) -> Palette {
    let (accent, wash) = tint(index, dark);
    if dark {
        Palette {
            dark,
            base: Rgb(0x19, 0x19, 0x19),
            text: Rgb(0xD4, 0xD4, 0xD4),
            secondary: Rgb(0x9B, 0x9B, 0x9B),
            accent,
            wash,
            selection: (Rgb(0x23, 0x83, 0xE2), 90),
            alert: Rgb(0xDF, 0x54, 0x52),
            code: Rgb(0xEB, 0x57, 0x57),
            soft: 22,
            shadow: 130,
        }
    } else {
        Palette {
            dark,
            base: Rgb(0xFF, 0xFF, 0xFF),
            text: Rgb(0x37, 0x35, 0x2F),
            secondary: Rgb(0x78, 0x77, 0x74),
            accent,
            wash,
            selection: (Rgb(0x23, 0x83, 0xE2), 64),
            alert: Rgb(0xD4, 0x4C, 0x47),
            code: Rgb(0xEB, 0x57, 0x57),
            soft: 20,
            shadow: 48,
        }
    }
}

pub const WHITE: Rgb = Rgb(0xFF, 0xFF, 0xFF);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours() {
        assert_eq!(hsl(0.0, 1.0, 0.5), Rgb(255, 0, 0));
        assert_eq!(hsl(120.0, 1.0, 0.5), Rgb(0, 255, 0));
        assert_eq!(tint(2, false), (Rgb(0x33, 0x7E, 0xA9), Rgb(0xE7, 0xF3, 0xF8)));
        assert_eq!(color_name(9), "Teal");
        // The rotation starts with the app icon's colours and repeats none.
        let names: Vec<String> = ROTATION.iter().map(|&c| color_name(c)).collect();
        assert_eq!(names[..4], ["Lavender", "Cobalt", "Aqua", "Grape"]);
        assert!(ROTATION.iter().all(|&c| c < PRESETS));
        assert!((1..ROTATION.len()).all(|i| !ROTATION[..i].contains(&ROTATION[i])));
        assert_eq!(preset_named("cobalt"), Some(18));
        assert_eq!(tint(18, false).0, Rgb(0x45, 0x5F, 0xF3));
        assert_eq!(color_name(CUSTOM + 200), "Custom 200°");
        assert_eq!(custom_hue(CUSTOM + 200), Some(200.0));
        assert_eq!(custom_hue(3), None);
        // Every preset and custom hue gives a light wash and a mid accent in light mode.
        for c in (0..PRESETS).chain([CUSTOM, CUSTOM + 359]) {
            let (accent, wash) = tint(c, false);
            assert!(wash.0 as u32 + wash.1 as u32 + wash.2 as u32 > 600, "{c}");
            assert!((accent.0 as u32 + accent.1 as u32 + accent.2 as u32) < 600, "{c}");
        }
    }

    #[test]
    fn custom_colours_and_names() {
        assert_eq!(custom_hue(CUSTOM + 200), Some(200.0));
        assert_eq!(custom_hue(3), None);
        assert!(!color_name(CUSTOM + 10).is_empty());
        assert_eq!(PRESET_NAMES.len(), PRESETS as usize);
        // Every preset has distinct light and dark looks.
        for c in 0..PRESETS {
            assert_ne!(tint(c, false), tint(c, true), "preset {c}");
        }
    }
}
