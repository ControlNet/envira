//! Palettes: Tokyo Night (default), Catppuccin Latte on light terminals, ANSI-16 without truecolor.

use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub fg: Color,
    pub dim: Color,
    pub border: Color,
    pub focus: Color,
    pub green: Color,
    pub yellow: Color,
    pub red: Color,
    pub cyan: Color,
    pub blue: Color,
    pub magenta: Color,
    /// Background of the cursor row; `None` uses reverse video.
    pub cursor_bg: Option<Color>,
}

impl Theme {
    pub fn tokyo_night() -> Theme {
        Theme {
            fg: Color::Rgb(0xc0, 0xca, 0xf5),
            dim: Color::Rgb(0x9a, 0xa5, 0xce),
            border: Color::Rgb(0x3b, 0x42, 0x61),
            focus: Color::Rgb(0x9e, 0xce, 0x6a),
            green: Color::Rgb(0x9e, 0xce, 0x6a),
            yellow: Color::Rgb(0xe0, 0xaf, 0x68),
            red: Color::Rgb(0xf7, 0x76, 0x8e),
            cyan: Color::Rgb(0x7d, 0xcf, 0xff),
            blue: Color::Rgb(0x7a, 0xa2, 0xf7),
            magenta: Color::Rgb(0xbb, 0x9a, 0xf7),
            cursor_bg: Some(Color::Rgb(0x28, 0x34, 0x57)),
        }
    }

    pub fn latte() -> Theme {
        Theme {
            fg: Color::Rgb(0x4c, 0x4f, 0x69),
            dim: Color::Rgb(0x6c, 0x6f, 0x85),
            border: Color::Rgb(0xbc, 0xc0, 0xcc),
            focus: Color::Rgb(0x40, 0xa0, 0x2b),
            green: Color::Rgb(0x40, 0xa0, 0x2b),
            yellow: Color::Rgb(0xdf, 0x8e, 0x1d),
            red: Color::Rgb(0xd2, 0x0f, 0x39),
            cyan: Color::Rgb(0x04, 0xa5, 0xe5),
            blue: Color::Rgb(0x1e, 0x66, 0xf5),
            magenta: Color::Rgb(0x88, 0x39, 0xef),
            cursor_bg: Some(Color::Rgb(0xcc, 0xd0, 0xda)),
        }
    }

    pub fn ansi() -> Theme {
        Theme {
            fg: Color::Reset,
            dim: Color::DarkGray,
            border: Color::DarkGray,
            focus: Color::Green,
            green: Color::Green,
            yellow: Color::Yellow,
            red: Color::Red,
            cyan: Color::Cyan,
            blue: Color::Blue,
            magenta: Color::Magenta,
            cursor_bg: None,
        }
    }

    /// `ENVIRA_THEME=dark|light|ansi`, else detected from `COLORTERM` / `COLORFGBG`.
    pub fn detect() -> Theme {
        let var = |k: &str| std::env::var(k).unwrap_or_default();
        match var("ENVIRA_THEME").as_str() {
            "dark" => return Theme::tokyo_night(),
            "light" => return Theme::latte(),
            "ansi" => return Theme::ansi(),
            _ => {}
        }
        if !matches!(var("COLORTERM").as_str(), "truecolor" | "24bit") {
            return Theme::ansi();
        }
        let light_bg = var("COLORFGBG")
            .rsplit(';')
            .next()
            .and_then(|b| b.parse::<u8>().ok())
            .is_some_and(|b| b == 7 || b == 15);
        if light_bg {
            Theme::latte()
        } else {
            Theme::tokyo_night()
        }
    }

    pub fn s(&self, c: Color) -> Style {
        Style::new().fg(c)
    }

    pub fn cursor(&self) -> Style {
        match self.cursor_bg {
            Some(bg) => Style::new().bg(bg),
            None => Style::new().add_modifier(Modifier::REVERSED),
        }
    }
}
