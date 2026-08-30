use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;

const ASCII_BORDER: border::Set<'static> = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

#[derive(Debug, Clone, Copy)]
pub struct Glyphs {
    pub brand: &'static str,
    pub running: &'static str,
    pub stopped: &'static str,
    pub transitional: &'static str,
    pub empty: &'static str,
    pub command: &'static str,
    pub dimensions: &'static str,
    pub up_down: &'static str,
    pub ellipsis: &'static str,
    pub border: border::Set<'static>,
}

impl Glyphs {
    pub fn current(unicode: bool) -> Self {
        if unicode {
            Self {
                brand: "◆",
                running: "●",
                stopped: "○",
                transitional: "◆",
                empty: "◇",
                command: "›",
                dimensions: "×",
                up_down: "↑↓",
                ellipsis: "…",
                border: border::PLAIN,
            }
        } else {
            Self {
                brand: "*",
                running: "*",
                stopped: "o",
                transitional: "*",
                empty: "-",
                command: ">",
                dimensions: "x",
                up_down: "Up/Dn",
                ellipsis: "...",
                border: ASCII_BORDER,
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub text: Style,
    pub muted: Style,
    pub accent: Style,
    pub success: Style,
    pub warning: Style,
    pub error: Style,
    pub border: Style,
    pub selected: Style,
    pub nav_active: Style,
    pub nav_inactive: Style,
    pub hovered: Style,
    pub focused: Style,
    pub command: Style,
    pub hint: Style,
    pub panel: Style,
}

impl Theme {
    pub fn current(configured_no_color: bool) -> Self {
        let no_color = configured_no_color || std::env::var_os("NO_COLOR").is_some();
        if no_color {
            return Self {
                text: Style::default(),
                muted: Style::default().add_modifier(Modifier::DIM),
                accent: Style::default().add_modifier(Modifier::BOLD),
                success: Style::default().add_modifier(Modifier::BOLD),
                warning: Style::default().add_modifier(Modifier::BOLD),
                error: Style::default().add_modifier(Modifier::BOLD),
                border: Style::default().add_modifier(Modifier::DIM),
                selected: Style::default().add_modifier(Modifier::REVERSED),
                nav_active: Style::default().add_modifier(Modifier::BOLD),
                nav_inactive: Style::default().add_modifier(Modifier::DIM),
                hovered: Style::default().add_modifier(Modifier::UNDERLINED),
                focused: Style::default().add_modifier(Modifier::REVERSED),
                command: Style::default().add_modifier(Modifier::BOLD),
                hint: Style::default().add_modifier(Modifier::DIM),
                panel: Style::default(),
            };
        }
        Self {
            text: Style::default().fg(Color::Rgb(225, 231, 239)),
            muted: Style::default().fg(Color::Rgb(126, 139, 157)),
            accent: Style::default()
                .fg(Color::Rgb(108, 205, 183))
                .add_modifier(Modifier::BOLD),
            success: Style::default().fg(Color::Rgb(111, 207, 151)),
            warning: Style::default().fg(Color::Rgb(238, 190, 92)),
            error: Style::default().fg(Color::Rgb(239, 119, 122)),
            border: Style::default().fg(Color::Rgb(68, 80, 98)),
            selected: Style::default()
                .fg(Color::Rgb(234, 241, 247))
                .bg(Color::Rgb(43, 78, 78))
                .add_modifier(Modifier::BOLD),
            nav_active: Style::default()
                .fg(Color::Rgb(108, 205, 183))
                .add_modifier(Modifier::BOLD),
            nav_inactive: Style::default().fg(Color::Rgb(126, 139, 157)),
            hovered: Style::default().bg(Color::Rgb(35, 49, 58)),
            focused: Style::default().add_modifier(Modifier::UNDERLINED),
            command: Style::default().fg(Color::Rgb(225, 231, 239)),
            hint: Style::default().fg(Color::Rgb(103, 116, 134)),
            panel: Style::default().bg(Color::Rgb(24, 29, 38)),
        }
    }
}
