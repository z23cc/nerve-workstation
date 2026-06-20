//! Accent color themes for the UI chrome (header logo, status spinner, input
//! prompt, palette selection). Cycled live with `/theme`. Transcript colors are
//! fixed (see [`crate::ui::palette`]).
//!
//! A theme is just a named ratatui [`Color`]; callers build the accent [`Style`].

use ratatui::style::{Color, Style};

/// A named accent color for the chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub name: &'static str,
    pub color: Color,
}

/// The cycle of accent themes, in `/theme` order (matches the TS `THEMES`).
pub const THEMES: [Theme; 4] = [
    Theme {
        name: "cyan",
        color: Color::Cyan,
    },
    Theme {
        name: "green",
        color: Color::Green,
    },
    Theme {
        name: "magenta",
        color: Color::Magenta,
    },
    Theme {
        name: "amber",
        color: Color::Yellow,
    },
];

/// Index of a theme by name, defaulting to 0 (cyan). Ports `themeIndexByName`.
#[must_use]
pub fn theme_index_by_name(name: Option<&str>) -> usize {
    name.and_then(|name| THEMES.iter().position(|theme| theme.name == name))
        .unwrap_or(0)
}

/// The accent [`Style`] for a theme index (wraps around the cycle).
#[must_use]
pub fn accent_style(index: usize) -> Style {
    Style::default().fg(THEMES[index % THEMES.len()].color)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_has_four_named_accents() {
        assert_eq!(THEMES.len(), 4);
        assert_eq!(THEMES[0].name, "cyan");
        assert_eq!(THEMES[3].name, "amber");
    }

    #[test]
    fn index_by_name_defaults_to_zero() {
        assert_eq!(theme_index_by_name(Some("magenta")), 2);
        assert_eq!(theme_index_by_name(Some("nope")), 0);
        assert_eq!(theme_index_by_name(None), 0);
    }

    #[test]
    fn accent_style_wraps_around() {
        assert_eq!(accent_style(0), accent_style(THEMES.len()));
    }
}
