use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Wrap};

use crate::theme::{Glyphs, Theme};
use norted_engine::BackendLoadProgress;

pub fn section_title<'a>(title: &'a str, subtitle: &'a str, theme: &Theme) -> Paragraph<'a> {
    Paragraph::new(vec![
        Line::from(Span::styled(title, theme.accent)),
        Line::from(Span::styled(subtitle, theme.muted)),
    ])
    .wrap(Wrap { trim: true })
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

pub const KEY_COLUMN: usize = 15;

pub fn key_value<'a>(key: &'a str, value: &'a str, theme: &Theme) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{key:<KEY_COLUMN$} "), theme.hint),
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

pub fn truncate_middle(text: &str, max_width: usize, ellipsis: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_width {
        return text.to_owned();
    }
    let ellipsis_width = ellipsis.chars().count();
    if max_width <= ellipsis_width {
        return chars[..max_width].iter().collect();
    }
    let keep = max_width - ellipsis_width;
    let front = keep / 2;
    let back = keep - front;
    format!(
        "{}{ellipsis}{}",
        chars[..front].iter().collect::<String>(),
        chars[chars.len() - back..].iter().collect::<String>()
    )
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

/// Renders a polished model-load progress bar. Determinate when the runtime
/// provides a trustworthy fraction; indeterminate (animated marquee) when it
/// does not. Respects Unicode/ASCII mode and narrow widths.
pub fn render_load_progress(
    frame: &mut Frame<'_>,
    area: Rect,
    progress: &BackendLoadProgress,
    animation_frame: u32,
    theme: &Theme,
    glyphs: &Glyphs,
) {
    if area.height < 2 || area.width < 4 {
        return;
    }
    let bar_area = Rect::new(area.x, area.y, area.width, 1);
    let detail_area = Rect::new(area.x, area.y + 1, area.width, area.height - 1);

    let bar = if let Some(fraction) = progress.fraction {
        determinate_bar(fraction, bar_area.width, glyphs, theme)
    } else {
        indeterminate_bar(animation_frame, bar_area.width, glyphs, theme)
    };
    frame.render_widget(Paragraph::new(bar), bar_area);

    let detail = progress_detail(progress, bar_area.width);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(detail, theme.muted))),
        detail_area,
    );
}

fn determinate_bar(fraction: f32, width: u16, glyphs: &Glyphs, theme: &Theme) -> Line<'static> {
    let inner_width = (width as usize).saturating_sub(8);
    let filled = if inner_width > 0 {
        (fraction.clamp(0.0, 1.0) * inner_width as f32).round() as usize
    } else {
        0
    };
    let empty = inner_width.saturating_sub(filled);
    let (full, empty_glyph) = if glyphs.unicode {
        ("\u{2588}", "\u{2591}")
    } else {
        ("#", "-")
    };
    let bar: String = full.repeat(filled) + empty_glyph.repeat(empty).as_str();
    let percent = (fraction.clamp(0.0, 1.0) * 100.0).round() as u32;
    Line::from(vec![
        Span::styled(bar, theme.accent),
        Span::raw(" "),
        Span::styled(format!("{percent:>3}%"), theme.text),
    ])
}

fn indeterminate_bar(frame: u32, width: u16, glyphs: &Glyphs, theme: &Theme) -> Line<'static> {
    let width = width as usize;
    if width == 0 {
        return Line::default();
    }
    let (track, head, segment) = if glyphs.unicode {
        ("\u{2500}", "\u{2578}", "\u{2501}")
    } else {
        ("-", ">", "=")
    };
    let segment_len = (if glyphs.unicode { 6 } else { 5 }).min(width);
    let head_position = (frame as usize) % width;
    Line::from(
        (0..width)
            .map(|position| {
                let distance_behind = (head_position + width - position) % width;
                if distance_behind == 0 {
                    Span::styled(head, theme.accent)
                } else if distance_behind < segment_len {
                    Span::styled(segment, theme.accent)
                } else {
                    Span::styled(track, theme.muted)
                }
            })
            .collect::<Vec<_>>(),
    )
}

fn progress_detail(progress: &BackendLoadProgress, width: u16) -> String {
    let phase = progress.phase.label();
    let mut detail = if let Some(message) = &progress.message {
        if message.is_empty() {
            phase.to_owned()
        } else {
            format!("{phase}: {message}")
        }
    } else {
        phase.to_owned()
    };
    if let (Some(current), Some(total)) = (progress.current, progress.total) {
        detail.push_str(&format!(
            " {} / {}",
            format_bytes(current),
            format_bytes(total)
        ));
    }
    if detail.len() > width as usize {
        truncate_middle(&detail, width as usize, "\u{2026}")
    } else {
        detail
    }
}

/// Compact one-line progress for Overview or narrow layouts.
pub fn load_progress_compact(
    progress: &BackendLoadProgress,
    animation_frame: u32,
    width: u16,
    glyphs: &Glyphs,
) -> String {
    let phase = progress.phase.label();
    if let Some(fraction) = progress.fraction {
        let inner_width = (width as usize).saturating_sub(phase.len() + 6);
        let filled = if inner_width > 0 {
            (fraction.clamp(0.0, 1.0) * inner_width as f32).round() as usize
        } else {
            0
        };
        let (full, empty) = if glyphs.unicode {
            ("\u{2588}", "\u{2591}")
        } else {
            ("#", "-")
        };
        let bar: String = full.repeat(filled) + &empty.repeat(inner_width.saturating_sub(filled));
        let percent = (fraction.clamp(0.0, 1.0) * 100.0).round() as u32;
        format!("{phase} {bar} {percent}%")
    } else {
        let inner_width = (width as usize).saturating_sub(phase.len() + 2);
        let position = (animation_frame as usize) % (inner_width.max(1) + 3);
        let (track, head) = if glyphs.unicode {
            ("\u{2500}", "\u{2578}")
        } else {
            ("-", ">")
        };
        let bar: String = track.repeat(position.min(inner_width)) + head;
        let remaining = inner_width.saturating_sub(bar.chars().count());
        let full_bar = bar + &track.repeat(remaining);
        format!("{phase} {full_bar}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norted_engine::BackendLoadPhase;
    use unicode_width::UnicodeWidthStr;

    fn theme() -> Theme {
        Theme::current(true)
    }

    fn glyphs() -> Glyphs {
        Glyphs::current(true)
    }

    fn ascii_glyphs() -> Glyphs {
        Glyphs::current(false)
    }

    #[test]
    fn determinate_bar_renders_filled_and_empty_segments() {
        let line = determinate_bar(0.5, 20, &glyphs(), &theme());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains('\u{2588}'));
        assert!(text.contains('\u{2591}'));
        assert!(text.contains("50%"));
    }

    #[test]
    fn determinate_bar_ascii_mode_uses_hash_and_dash() {
        let line = determinate_bar(0.25, 20, &ascii_glyphs(), &theme());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains('#'));
        assert!(text.contains('-'));
        assert!(text.contains("25%"));
    }

    #[test]
    fn determinate_bar_clamps_above_100_percent() {
        let line = determinate_bar(1.5, 20, &glyphs(), &theme());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("100%"));
    }

    #[test]
    fn determinate_bar_clamps_below_zero() {
        let line = determinate_bar(-0.5, 20, &glyphs(), &theme());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("0%"));
    }

    #[test]
    fn indeterminate_bar_moves_with_animation_frame() {
        let frame0 = indeterminate_bar(0, 30, &glyphs(), &theme());
        let frame5 = indeterminate_bar(5, 30, &glyphs(), &theme());
        let text0: String = frame0.spans.iter().map(|s| s.content.as_ref()).collect();
        let text5: String = frame5.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_ne!(text0, text5);
    }

    #[test]
    fn indeterminate_bar_has_exact_width_and_one_moving_head() {
        for frame in 0..40 {
            let line = indeterminate_bar(frame, 30, &glyphs(), &theme());
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(UnicodeWidthStr::width(text.as_str()), 30);
            assert_eq!(text.matches('\u{2578}').count(), 1);
            if frame % 30 != 29 {
                assert!(!text.ends_with('\u{2578}'));
            }
        }
    }

    #[test]
    fn indeterminate_bar_ascii_mode_uses_ascii_characters() {
        let line = indeterminate_bar(3, 30, &ascii_glyphs(), &theme());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(UnicodeWidthStr::width(text.as_str()), 30);
        assert_eq!(text.matches('>').count(), 1);
        assert!(text.is_ascii());
    }

    #[test]
    fn indeterminate_bar_handles_narrow_width() {
        for width in 0..=4 {
            for frame in 0..10 {
                let line = indeterminate_bar(frame, width, &glyphs(), &theme());
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                assert_eq!(UnicodeWidthStr::width(text.as_str()), width as usize);
                assert_eq!(text.matches('\u{2578}').count(), usize::from(width > 0));
            }
        }
    }

    #[test]
    fn progress_detail_includes_phase_and_message() {
        let progress = BackendLoadProgress::with_message(
            BackendLoadPhase::LoadingModel,
            "Loading tensors 148 / 200",
        );
        let detail = progress_detail(&progress, 80);
        assert!(detail.contains("Loading model"));
        assert!(detail.contains("Loading tensors 148 / 200"));
    }

    #[test]
    fn progress_detail_truncates_on_narrow_width() {
        let progress = BackendLoadProgress::with_message(
            BackendLoadPhase::LoadingModel,
            "a very long message that exceeds the available width",
        );
        let detail = progress_detail(&progress, 20);
        assert!(detail.contains('\u{2026}'));
        assert!(detail.chars().count() <= 20);
    }

    #[test]
    fn progress_detail_appends_count_when_present() {
        let progress = BackendLoadProgress {
            phase: BackendLoadPhase::LoadingModel,
            fraction: Some(0.74),
            current: Some(148),
            total: Some(200),
            message: Some("Loading tensors".to_owned()),
        };
        let detail = progress_detail(&progress, 80);
        assert!(detail.contains("148 B / 200 B"));
    }

    #[test]
    fn compact_progress_determinate_shows_percentage() {
        let progress = BackendLoadProgress {
            phase: BackendLoadPhase::LoadingModel,
            fraction: Some(0.67),
            current: None,
            total: None,
            message: None,
        };
        let compact = load_progress_compact(&progress, 0, 40, &glyphs());
        assert!(compact.contains("67%"));
        assert!(compact.contains("Loading model"));
    }

    #[test]
    fn compact_progress_indeterminate_shows_phase_and_bar() {
        let progress = BackendLoadProgress::indeterminate(BackendLoadPhase::LoadingModel);
        let compact = load_progress_compact(&progress, 3, 40, &glyphs());
        assert!(compact.contains("Loading model"));
    }

    #[test]
    fn compact_progress_ascii_mode_uses_ascii() {
        let progress = BackendLoadProgress::indeterminate(BackendLoadPhase::LoadingModel);
        let compact = load_progress_compact(&progress, 3, 40, &ascii_glyphs());
        assert!(!compact.contains('\u{2500}'));
    }
}
