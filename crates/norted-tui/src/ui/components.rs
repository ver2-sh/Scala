use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Wrap};

use crate::theme::{Glyphs, Theme};

pub fn section_title<'a>(title: &'a str, subtitle: &'a str, theme: &Theme) -> Paragraph<'a> {
    Paragraph::new(vec![
        Line::from(Span::styled(title, theme.accent)),
        Line::from(Span::styled(subtitle, theme.muted)),
    ])
}

pub fn content_layout(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(3)])
        .split(area)
}

pub fn render_empty(frame: &mut Frame<'_>, area: Rect, title: &str, detail: &str, theme: &Theme) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(title, theme.text)),
            Line::default(),
            Line::from(Span::styled(detail, theme.muted)),
        ])
        .wrap(Wrap { trim: true }),
        area,
    );
}

pub fn key_value<'a>(key: &'a str, value: &'a str, theme: &Theme) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{key:<12}"), theme.hint),
        Span::styled(value, theme.text),
    ])
}

pub fn hint<'a>(key: &'a str, label: &'a str, theme: &Theme) -> Vec<Span<'a>> {
    vec![
        Span::styled(key, theme.accent),
        Span::styled(format!(" {label}   "), theme.hint),
    ]
}

pub fn popup_block<'a>(title: &'a str, theme: &Theme, glyphs: &Glyphs) -> Block<'a> {
    Block::default()
        .title(Span::styled(title, theme.accent))
        .borders(Borders::ALL)
        .border_set(glyphs.border)
        .border_style(theme.border)
        .style(theme.panel)
        .padding(Padding::horizontal(1))
}

pub fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

pub fn centered_message(frame: &mut Frame<'_>, area: Rect, lines: Vec<Line<'_>>) {
    let height = lines
        .len()
        .try_into()
        .unwrap_or(area.height)
        .min(area.height);
    let y = area.y + area.height.saturating_sub(height) / 2;
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        Rect::new(area.x, y, area.width, height),
    );
}

pub fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let bytes = bytes as f64;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes / GIB)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes / MIB)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes / KIB)
    } else {
        format!("{bytes:.0} B")
    }
}
