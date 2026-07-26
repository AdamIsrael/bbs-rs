//! Color themes. A [`Theme`] is the resolved set of concrete colors the UI
//! draws with; it's built from a [`crate::config::ThemeConfig`] by starting
//! from a named preset and applying any per-color overrides.

use ratatui::style::Color;

use crate::config::ThemeConfig;

/// The concrete colors the renderer uses. Semantic, not per-widget: each field
/// maps to a role (title bar, accent, etc.) applied consistently across screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Title-bar text and background.
    pub title_fg: Color,
    pub title_bg: Color,
    /// Headings, tags, author names.
    pub accent: Color,
    /// "New"/unread markers.
    pub highlight: Color,
    /// Status/warning line text and background.
    pub warning_fg: Color,
    pub warning_bg: Color,
    /// Secondary text: hints, labels, flags.
    pub dim: Color,
}

impl Theme {
    /// The default "classic" look — matches the pre-theme hardcoded colors.
    pub const fn classic() -> Self {
        Self {
            title_fg: Color::Black,
            title_bg: Color::Cyan,
            accent: Color::Cyan,
            highlight: Color::Green,
            warning_fg: Color::Black,
            warning_bg: Color::Yellow,
            dim: Color::DarkGray,
        }
    }

    /// Monochrome: grayscale, no hues.
    pub const fn mono() -> Self {
        Self {
            title_fg: Color::Black,
            title_bg: Color::Gray,
            accent: Color::White,
            highlight: Color::White,
            warning_fg: Color::Black,
            warning_bg: Color::Gray,
            dim: Color::DarkGray,
        }
    }

    /// Amber monochrome, like an old phosphor terminal.
    pub const fn amber() -> Self {
        let amber = Color::Rgb(255, 176, 0);
        Self {
            title_fg: Color::Black,
            title_bg: amber,
            accent: amber,
            highlight: Color::Rgb(255, 214, 90),
            warning_fg: Color::Black,
            warning_bg: amber,
            dim: Color::Rgb(150, 100, 0),
        }
    }

    /// Green-on-black "matrix" phosphor.
    pub const fn matrix() -> Self {
        Self {
            title_fg: Color::Black,
            title_bg: Color::Green,
            accent: Color::Green,
            highlight: Color::LightGreen,
            warning_fg: Color::Black,
            warning_bg: Color::Green,
            dim: Color::Rgb(0, 120, 0),
        }
    }

    /// Look up a preset by (case-insensitive) name; `None` for unknown names.
    pub fn preset(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "classic" => Some(Self::classic()),
            "mono" => Some(Self::mono()),
            "amber" => Some(Self::amber()),
            "matrix" => Some(Self::matrix()),
            _ => None,
        }
    }

    /// Resolve a [`ThemeConfig`] into concrete colors: start from the named
    /// preset (classic if unset/unknown) and override each color the operator
    /// set. An unparseable color string is ignored (keeps the preset's value).
    pub fn resolve(cfg: &ThemeConfig) -> Self {
        let mut t = cfg
            .preset
            .as_deref()
            .and_then(Self::preset)
            .unwrap_or_else(Self::classic);
        let set = |slot: &mut Color, s: &Option<String>| {
            if let Some(c) = s.as_deref().and_then(parse_color) {
                *slot = c;
            }
        };
        set(&mut t.title_fg, &cfg.title_fg);
        set(&mut t.title_bg, &cfg.title_bg);
        set(&mut t.accent, &cfg.accent);
        set(&mut t.highlight, &cfg.highlight);
        set(&mut t.warning_fg, &cfg.warning_fg);
        set(&mut t.warning_bg, &cfg.warning_bg);
        set(&mut t.dim, &cfg.dim);
        t
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::classic()
    }
}

/// Parse a color string: a named color, a 256-palette index (`"208"`), or a
/// hex triple (`"#ff8800"`). Returns `None` if unrecognized.
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some(Color::Rgb(r, g, b));
        }
        return None;
    }
    if let Ok(idx) = s.parse::<u8>() {
        return Some(Color::Indexed(idx));
    }
    let c = match s.to_ascii_lowercase().as_str() {
        "reset" | "default" => Color::Reset,
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        "white" => Color::White,
        _ => return None,
    };
    Some(c)
}

/// Render a [`Color`] as a CSS hex string, for the browser frontend (#203).
///
/// Named and 256-indexed colours are mapped through **xterm.js's default
/// palette**, deliberately: the same theme drives the TUI inside that terminal,
/// so if we invented our own hexes the page chrome and the terminal content
/// would disagree about what "cyan" is on the very same screen.
///
/// [`Color::Reset`] has no hex — it means "whatever the terminal's default is"
/// — so it yields `None` and the caller keeps its built-in value.
pub fn css_color(c: Color) -> Option<String> {
    /// xterm.js's default 16 ANSI colours, in palette order.
    const ANSI: [&str; 16] = [
        "#000000", "#cd3131", "#0dbc79", "#e5e510", "#2472c8", "#bc3fbc", "#11a8cd", "#e5e5e5",
        "#666666", "#f14c4c", "#23d18b", "#f5f543", "#3b8eea", "#d670d6", "#29b8db", "#ffffff",
    ];
    let idx = match c {
        Color::Rgb(r, g, b) => return Some(format!("#{r:02x}{g:02x}{b:02x}")),
        Color::Reset => return None,
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::Gray => 7,
        Color::DarkGray => 8,
        Color::LightRed => 9,
        Color::LightGreen => 10,
        Color::LightYellow => 11,
        Color::LightBlue => 12,
        Color::LightMagenta => 13,
        Color::LightCyan => 14,
        Color::White => 15,
        Color::Indexed(i) => return Some(indexed_hex(i)),
    };
    Some(ANSI[idx].to_string())
}

/// The xterm 256-colour cube: 0–15 are the ANSI set, 16–231 a 6×6×6 RGB cube,
/// 232–255 a 24-step grey ramp.
fn indexed_hex(i: u8) -> String {
    match i {
        0..=15 => css_color(match i {
            0 => Color::Black,
            1 => Color::Red,
            2 => Color::Green,
            3 => Color::Yellow,
            4 => Color::Blue,
            5 => Color::Magenta,
            6 => Color::Cyan,
            7 => Color::Gray,
            8 => Color::DarkGray,
            9 => Color::LightRed,
            10 => Color::LightGreen,
            11 => Color::LightYellow,
            12 => Color::LightBlue,
            13 => Color::LightMagenta,
            14 => Color::LightCyan,
            _ => Color::White,
        })
        .unwrap_or_else(|| "#000000".into()),
        16..=231 => {
            // Each axis steps 0, 95, 135, 175, 215, 255 — not evenly spaced.
            const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
            let n = i - 16;
            let (r, g, b) = (n / 36, (n % 36) / 6, n % 6);
            format!(
                "#{:02x}{:02x}{:02x}",
                STEPS[r as usize], STEPS[g as usize], STEPS[b as usize]
            )
        }
        232..=255 => {
            let v = 8 + (i - 232) * 10;
            format!("#{v:02x}{v:02x}{v:02x}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The browser chrome and the terminal content share one theme on one
    /// screen, so a colour must mean the same thing in both (#203).
    #[test]
    fn colors_map_to_the_xterm_palette() {
        assert_eq!(css_color(Color::Cyan).as_deref(), Some("#11a8cd"));
        assert_eq!(css_color(Color::Black).as_deref(), Some("#000000"));
        assert_eq!(css_color(Color::LightGreen).as_deref(), Some("#23d18b"));
        // An operator's own hex passes through exactly.
        assert_eq!(
            css_color(Color::Rgb(255, 176, 0)).as_deref(),
            Some("#ffb000")
        );
        // Reset means "the terminal's default" — there is no hex for that, so
        // the caller keeps its built-in.
        assert_eq!(css_color(Color::Reset), None);
    }

    #[test]
    fn indexed_colors_follow_the_256_cube() {
        // 0–15 alias the ANSI set.
        assert_eq!(css_color(Color::Indexed(6)).as_deref(), Some("#11a8cd"));
        // The cube's first entry is black, its last white.
        assert_eq!(css_color(Color::Indexed(16)).as_deref(), Some("#000000"));
        assert_eq!(css_color(Color::Indexed(231)).as_deref(), Some("#ffffff"));
        // A mid-cube value uses the uneven step table, not a linear ramp.
        assert_eq!(css_color(Color::Indexed(46)).as_deref(), Some("#00ff00"));
        // The grey ramp.
        assert_eq!(css_color(Color::Indexed(232)).as_deref(), Some("#080808"));
        assert_eq!(css_color(Color::Indexed(255)).as_deref(), Some("#eeeeee"));
    }

    #[test]
    fn default_is_classic() {
        assert_eq!(Theme::resolve(&ThemeConfig::default()), Theme::classic());
    }

    #[test]
    fn preset_selects_base() {
        let cfg = ThemeConfig {
            preset: Some("amber".into()),
            ..Default::default()
        };
        assert_eq!(Theme::resolve(&cfg), Theme::amber());
    }

    #[test]
    fn overrides_apply_on_top_of_preset() {
        let cfg = ThemeConfig {
            preset: Some("classic".into()),
            accent: Some("#ff8800".into()),
            title_bg: Some("200".into()),
            ..Default::default()
        };
        let t = Theme::resolve(&cfg);
        assert_eq!(t.accent, Color::Rgb(255, 136, 0));
        assert_eq!(t.title_bg, Color::Indexed(200));
        // Unset fields keep the classic values.
        assert_eq!(t.dim, Theme::classic().dim);
    }

    #[test]
    fn unknown_preset_and_bad_color_fall_back() {
        let cfg = ThemeConfig {
            preset: Some("nope".into()),
            accent: Some("notacolor".into()),
            ..Default::default()
        };
        // Unknown preset → classic; unparseable override ignored.
        assert_eq!(Theme::resolve(&cfg), Theme::classic());
    }

    #[test]
    fn parses_color_forms() {
        assert_eq!(parse_color("cyan"), Some(Color::Cyan));
        assert_eq!(parse_color("  Yellow "), Some(Color::Yellow));
        assert_eq!(parse_color("#00ff00"), Some(Color::Rgb(0, 255, 0)));
        assert_eq!(parse_color("255"), Some(Color::Indexed(255)));
        assert_eq!(parse_color("256"), None); // out of u8 range
        assert_eq!(parse_color("#fff"), None); // wrong length
        assert_eq!(parse_color("chartreuse"), None);
    }
}
