//! Central color theme. Every color in the TUI is drawn from a [`Theme`].
//!
//! The default theme is `blush`: a neutral grey canvas (#1b1b1f) with a light
//! pink accent (#f2b8cf).

use ratatui::style::{Color, Modifier, Style};

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub name: &'static str,
    pub is_light: bool,

    // Surfaces
    pub bg: Color,
    /// User-prompt band, code blocks, menus.
    pub bg_raised: Color,
    /// Selected rows / hover.
    pub bg_hover: Color,

    // Text
    pub text: Color,
    pub text_secondary: Color,
    pub gray_dim: Color,
    pub gray: Color,
    pub gray_bright: Color,

    // Accents
    pub accent: Color,
    pub accent_thinking: Color,
    pub success: Color,
    pub error: Color,
    pub warning: Color,
    pub info: Color,
    pub path: Color,
    pub command: Color,
    pub link: Color,
    pub model: Color,
    pub plan: Color,

    // Chrome
    pub border: Color,
    pub border_active: Color,
    pub scrollbar_bg: Color,
    pub scrollbar_fg: Color,

    // Diff
    pub diff_ins_bg: Color,
    pub diff_ins_fg: Color,
    pub diff_del_bg: Color,
    pub diff_del_fg: Color,

    // Markdown
    pub h1: Color,
    pub h2: Color,
    pub h3: Color,
    pub h4: Color,
    pub code_inline: Color,
    pub md_text: Color,

    pub syntax_theme: &'static str,
}

pub const THEME_NAMES: &[(&str, &str)] = &[
    ("blush", "Grey canvas, light pink accent (default)"),
    ("night", "Neutral near-black with a violet accent"),
    ("rosepine", "Muted Rosé-Pine-Moon inspired palette"),
    ("day", "Light theme for bright terminals"),
    ("terminal", "Your terminal's own colors, no backgrounds"),
];

impl Theme {
    pub fn by_name(name: &str) -> Option<Theme> {
        match name.trim().to_ascii_lowercase().as_str() {
            "blush" | "default" | "dark" | "pink" => Some(Self::blush()),
            "night" | "groknight" | "grok-night" => Some(Self::night()),
            "rosepine" | "rose-pine" | "rosepine-moon" => Some(Self::rosepine()),
            "day" | "light" => Some(Self::day()),
            "terminal" | "native" | "transparent" => Some(Self::terminal()),
            _ => None,
        }
    }

    pub fn blush() -> Theme {
        Theme {
            name: "blush",
            is_light: false,
            bg: rgb(0x1b, 0x1b, 0x1f),
            bg_raised: rgb(0x26, 0x26, 0x2b),
            bg_hover: rgb(0x30, 0x2f, 0x36),
            text: rgb(0xe8, 0xe6, 0xea),
            text_secondary: rgb(0xcc, 0xc9, 0xcf),
            gray_dim: rgb(0x5c, 0x5b, 0x63),
            gray: rgb(0x78, 0x76, 0x7f),
            gray_bright: rgb(0x93, 0x90, 0x99),
            accent: rgb(0xf2, 0xb8, 0xcf),
            accent_thinking: rgb(0xd9, 0xa5, 0xc0),
            success: rgb(0x9e, 0xce, 0x8a),
            error: rgb(0xf7, 0x76, 0x8e),
            warning: rgb(0xe8, 0xc0, 0x7d),
            info: rgb(0xa8, 0xc0, 0xf0),
            path: rgb(0xf5, 0xc6, 0xa5),
            command: rgb(0xe8, 0xcf, 0x95),
            link: rgb(0x9f, 0xbc, 0xe8),
            model: rgb(0xf2, 0xb8, 0xcf),
            plan: rgb(0xff, 0xdb, 0x8d),
            border: rgb(0x36, 0x35, 0x3c),
            border_active: rgb(0x52, 0x50, 0x5a),
            scrollbar_bg: rgb(0x1f, 0x1f, 0x24),
            scrollbar_fg: rgb(0x3a, 0x39, 0x41),
            diff_ins_bg: rgb(0x1e, 0x33, 0x26),
            diff_ins_fg: rgb(0x9e, 0xce, 0x8a),
            diff_del_bg: rgb(0x3e, 0x1e, 0x29),
            diff_del_fg: rgb(0xf7, 0x76, 0x8e),
            h1: rgb(0xf2, 0xb8, 0xcf),
            h2: rgb(0xd8, 0xb4, 0xf0),
            h3: rgb(0xa8, 0xc8, 0xf0),
            h4: rgb(0x93, 0x90, 0x99),
            code_inline: rgb(0x8f, 0xd6, 0xc9),
            md_text: rgb(0xd6, 0xd3, 0xd9),
            syntax_theme: "base16-ocean.dark",
        }
    }

    pub fn night() -> Theme {
        Theme {
            name: "night",
            bg: rgb(20, 20, 20),
            bg_raised: rgb(36, 36, 36),
            bg_hover: rgb(44, 44, 44),
            text: rgb(225, 225, 225),
            text_secondary: rgb(200, 200, 200),
            gray_dim: rgb(88, 88, 88),
            gray: rgb(108, 108, 108),
            gray_bright: rgb(120, 120, 120),
            accent: rgb(187, 154, 247),
            accent_thinking: rgb(187, 154, 247),
            border: rgb(50, 50, 55),
            border_active: rgb(80, 80, 88),
            scrollbar_bg: rgb(17, 17, 17),
            scrollbar_fg: rgb(36, 36, 36),
            diff_ins_bg: rgb(6, 56, 6),
            diff_del_bg: rgb(66, 14, 20),
            h1: rgb(26, 188, 156),
            h2: rgb(122, 162, 247),
            h3: rgb(157, 124, 216),
            h4: rgb(120, 120, 120),
            code_inline: rgb(58, 149, 171),
            md_text: rgb(200, 200, 200),
            model: rgb(26, 188, 156),
            ..Self::blush()
        }
    }

    pub fn rosepine() -> Theme {
        Theme {
            name: "rosepine",
            bg: rgb(0x23, 0x21, 0x36),
            bg_raised: rgb(0x2a, 0x27, 0x3f),
            bg_hover: rgb(0x39, 0x35, 0x52),
            text: rgb(0xe0, 0xde, 0xf4),
            text_secondary: rgb(0xc4, 0xc0, 0xdc),
            gray_dim: rgb(0x56, 0x52, 0x6e),
            gray: rgb(0x6e, 0x6a, 0x86),
            gray_bright: rgb(0x90, 0x8c, 0xaa),
            accent: rgb(0xc4, 0xa7, 0xe7),
            accent_thinking: rgb(0xc4, 0xa7, 0xe7),
            success: rgb(0x9c, 0xcf, 0xd8),
            error: rgb(0xeb, 0x6f, 0x92),
            warning: rgb(0xf6, 0xc1, 0x77),
            path: rgb(0xea, 0x9a, 0x97),
            border: rgb(0x39, 0x35, 0x52),
            border_active: rgb(0x56, 0x52, 0x6e),
            scrollbar_bg: rgb(0x1f, 0x1d, 0x2e),
            scrollbar_fg: rgb(0x39, 0x35, 0x52),
            h1: rgb(0xea, 0x9a, 0x97),
            h2: rgb(0xc4, 0xa7, 0xe7),
            h3: rgb(0x9c, 0xcf, 0xd8),
            code_inline: rgb(0x9c, 0xcf, 0xd8),
            md_text: rgb(0xe0, 0xde, 0xf4),
            model: rgb(0xea, 0x9a, 0x97),
            ..Self::blush()
        }
    }

    pub fn day() -> Theme {
        Theme {
            name: "day",
            is_light: true,
            bg: rgb(0xfa, 0xf8, 0xf9),
            bg_raised: rgb(0xee, 0xea, 0xec),
            bg_hover: rgb(0xe4, 0xdf, 0xe2),
            text: rgb(0x1f, 0x1d, 0x22),
            text_secondary: rgb(0x3a, 0x37, 0x3f),
            gray_dim: rgb(0xb4, 0xb0, 0xb7),
            gray: rgb(0x8a, 0x86, 0x8e),
            gray_bright: rgb(0x6a, 0x66, 0x6e),
            accent: rgb(0xc2, 0x4f, 0x86),
            accent_thinking: rgb(0xb0, 0x5a, 0x88),
            success: rgb(0x3d, 0x8b, 0x3d),
            error: rgb(0xc7, 0x2c, 0x48),
            warning: rgb(0xa8, 0x6b, 0x00),
            info: rgb(0x2f, 0x5f, 0xb3),
            path: rgb(0xb3, 0x5a, 0x1f),
            command: rgb(0x8a, 0x62, 0x00),
            link: rgb(0x2f, 0x5f, 0xb3),
            model: rgb(0xc2, 0x4f, 0x86),
            plan: rgb(0xa8, 0x6b, 0x00),
            border: rgb(0xdc, 0xd6, 0xd9),
            border_active: rgb(0xb4, 0xab, 0xb0),
            scrollbar_bg: rgb(0xf0, 0xec, 0xee),
            scrollbar_fg: rgb(0xd0, 0xc8, 0xcc),
            diff_ins_bg: rgb(0xd9, 0xf2, 0xd9),
            diff_ins_fg: rgb(0x2d, 0x7a, 0x2d),
            diff_del_bg: rgb(0xf8, 0xd9, 0xde),
            diff_del_fg: rgb(0xb3, 0x25, 0x3e),
            h1: rgb(0xc2, 0x4f, 0x86),
            h2: rgb(0x7a, 0x4f, 0xb0),
            h3: rgb(0x2f, 0x5f, 0xb3),
            h4: rgb(0x6a, 0x66, 0x6e),
            code_inline: rgb(0x0f, 0x7a, 0x6e),
            md_text: rgb(0x2a, 0x27, 0x2e),
            syntax_theme: "InspiredGitHub",
        }
    }

    pub fn terminal() -> Theme {
        Theme {
            name: "terminal",
            is_light: false,
            bg: Color::Reset,
            bg_raised: Color::Reset,
            bg_hover: Color::Reset,
            text: Color::Reset,
            text_secondary: Color::Reset,
            gray_dim: Color::DarkGray,
            gray: Color::DarkGray,
            gray_bright: Color::Gray,
            accent: Color::Magenta,
            accent_thinking: Color::Magenta,
            success: Color::Green,
            error: Color::Red,
            warning: Color::Yellow,
            info: Color::Blue,
            path: Color::Yellow,
            command: Color::Yellow,
            link: Color::Blue,
            model: Color::Magenta,
            plan: Color::Yellow,
            border: Color::DarkGray,
            border_active: Color::Reset,
            scrollbar_bg: Color::Reset,
            scrollbar_fg: Color::DarkGray,
            diff_ins_bg: Color::Reset,
            diff_ins_fg: Color::Green,
            diff_del_bg: Color::Reset,
            diff_del_fg: Color::Red,
            h1: Color::Magenta,
            h2: Color::Blue,
            h3: Color::Cyan,
            h4: Color::Gray,
            code_inline: Color::Cyan,
            md_text: Color::Reset,
            syntax_theme: "base16-ocean.dark",
        }
    }

    /// Apply user overrides from config (hex strings).
    pub fn with_overrides(mut self, accent: Option<&str>, background: Option<&str>) -> Theme {
        if let Some(c) = accent.and_then(parse_hex) {
            self.accent = c;
            self.model = c;
            self.h1 = c;
        }
        if let Some(c) = background.and_then(parse_hex) {
            self.bg = c;
            if let Color::Rgb(r, g, b) = c {
                let lift = |v: u8, d: i16| (v as i16 + d).clamp(0, 255) as u8;
                let d = if self.is_light { -12 } else { 11 };
                self.bg_raised = rgb(lift(r, d), lift(g, d), lift(b, d + 1));
                self.bg_hover = rgb(lift(r, d * 2), lift(g, d * 2), lift(b, d * 2 + 2));
                self.scrollbar_bg = rgb(lift(r, d / 3), lift(g, d / 3), lift(b, d / 3));
            }
        }
        self
    }

    // ---- style helpers ----
    pub fn base(&self) -> Style {
        Style::default().fg(self.text).bg(self.bg)
    }
    pub fn fg(&self, c: Color) -> Style {
        Style::default().fg(c)
    }
    pub fn muted(&self) -> Style {
        Style::default().fg(self.gray)
    }
    pub fn dim(&self) -> Style {
        Style::default().fg(self.gray_dim)
    }
    pub fn secondary(&self) -> Style {
        Style::default().fg(self.text_secondary)
    }
    pub fn bold(&self) -> Style {
        Style::default().fg(self.text).add_modifier(Modifier::BOLD)
    }
    pub fn accent(&self) -> Style {
        Style::default().fg(self.accent)
    }
}

pub fn parse_hex(s: &str) -> Option<Color> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// Linear blend between two colors (t = 0 → a, t = 1 → b). Non-RGB colors snap.
pub fn blend(a: Color, b: Color, t: f32) -> Color {
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
            rgb(m(r1, r2), m(g1, g2), m(b1, b2))
        }
        _ => {
            if t < 0.5 {
                a
            } else {
                b
            }
        }
    }
}

/// Brightness of a travelling wave used for running rails / bullets.
pub fn wave(tick: u64, row: u16) -> f32 {
    let phase = (tick as f32) * 0.35 - (row as f32) * 0.55;
    0.45 + 0.55 * (0.5 + 0.5 * phase.sin())
}
