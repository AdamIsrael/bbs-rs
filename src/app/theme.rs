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

#[cfg(test)]
mod tests {
    use super::*;

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
