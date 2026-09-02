use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Wrap};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::theme::{Glyphs, Theme};
use norted_engine::BackendLoadProgress;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ActionState {
    Normal,
    Primary,
    Disabled,
    Destructive,
    Confirm,
}

pub fn action_style(theme: &Theme, state: ActionState, hovered: bool) -> ratatui::style::Style {
    let style = match state {
        ActionState::Normal => theme.text,
        ActionState::Primary => theme.accent,
        ActionState::Disabled => theme.muted,
        ActionState::Destructive => theme.warning,
        ActionState::Confirm => theme.error,
    };
    if hovered && state != ActionState::Disabled {
        style.patch(theme.hovered)
    } else {
        style
    }
}

pub fn section_title<'a>(title: &'a str, subtitle: &'a str, theme: &Theme) -> Paragraph<'a> {
    Paragraph::new(vec![
        Line::from(Span::styled(title, theme.accent)),
        Line::from(Span::styled(subtitle, theme.muted)),
    ])
    .wrap(Wrap { trim: true })
}

pub fn content_layout(area: Rect, compact: bool) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if compact { 3 } else { 4 }),
            Constraint::Min(3),
        ])
        .split(area)
}

pub fn render_empty(frame: &mut Frame<'_>, area: Rect, title: &str, detail: &str, theme: &Theme) {
    let area = area.inner(ratatui::layout::Margin {
        horizontal: u16::from(area.width >= 72) * 2,
        vertical: u16::from(area.height >= 8),
    });
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

pub const KEY_COLUMN: usize = 17;

pub fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

pub fn remaining_width(total_width: u16, static_spans: &[&str]) -> usize {
    (total_width as usize).saturating_sub(
        static_spans
            .iter()
            .map(|span| display_width(span))
            .sum::<usize>(),
    )
}

pub fn key_value_width(total_width: u16) -> usize {
    (total_width as usize).saturating_sub(KEY_COLUMN + 1)
}

pub fn pad_display_width(text: &str, width: usize) -> String {
    let mut padded = text.to_owned();
    padded.push_str(&" ".repeat(width.saturating_sub(display_width(text))));
    padded
}

pub fn key_value<'a>(key: &'a str, value: &'a str, theme: &Theme) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("{} ", pad_display_width(key, KEY_COLUMN)),
            theme.hint,
        ),
        Span::styled(value, theme.text),
    ])
}

pub fn hint<'a>(key: &'a str, label: &'a str, theme: &Theme) -> Vec<Span<'a>> {
    vec![
        Span::styled(key, theme.accent),
        Span::styled(format!(" {label}   "), theme.hint),
    ]
}

pub fn popup_block<'a>(title: &'a str, theme: &Theme, glyphs: &Glyphs, compact: bool) -> Block<'a> {
    Block::default()
        .title(Span::styled(title, theme.accent))
        .borders(Borders::ALL)
        .border_set(glyphs.border)
        .border_style(theme.border)
        .style(theme.panel)
        .padding(if compact {
            Padding::horizontal(1)
        } else {
            Padding::new(2, 2, 1, 1)
        })
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
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_owned();
    }
    let ellipsis_width = UnicodeWidthStr::width(ellipsis);
    if max_width <= ellipsis_width {
        return display_prefix(ellipsis, max_width);
    }
    let keep = max_width - ellipsis_width;
    let front = keep / 2;
    let back = keep - front;
    format!(
        "{}{ellipsis}{}",
        display_prefix(text, front),
        display_suffix(text, back)
    )
}

/// Returns a single-line viewport which reveals an overflowing read-only value.
///
/// The first and last views are held for a few animation steps. Movement only
/// begins on grapheme boundaries, so combining marks and wide glyphs never get
/// split during scrolling.
pub fn marquee_text(text: &str, max_width: usize, animation_frame: u32) -> String {
    if max_width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(text) <= max_width {
        return pad_display_width(text, max_width);
    }

    let views = marquee_views(text, max_width);
    let Some(last_index) = views.len().checked_sub(1) else {
        return " ".repeat(max_width);
    };

    const START_HOLD: usize = 4;
    const END_HOLD: usize = 3;
    let cycle = START_HOLD + last_index + END_HOLD;
    let phase = (animation_frame as usize) % cycle.max(1);
    let index = phase.saturating_sub(START_HOLD).min(last_index);
    pad_display_width(&views[index], max_width)
}

pub fn needs_marquee(text: &str, max_width: usize) -> bool {
    max_width > 0
        && UnicodeWidthStr::width(text) > max_width
        && marquee_views(text, max_width).len() > 1
}

fn marquee_views(text: &str, max_width: usize) -> Vec<String> {
    let graphemes = UnicodeSegmentation::graphemes(text, true).collect::<Vec<_>>();
    let mut suffix_widths = vec![0usize; graphemes.len() + 1];
    for index in (0..graphemes.len()).rev() {
        suffix_widths[index] =
            suffix_widths[index + 1].saturating_add(UnicodeWidthStr::width(graphemes[index]));
    }
    let mut views = Vec::with_capacity(graphemes.len());
    for start in 0..graphemes.len() {
        let mut width = 0usize;
        let mut view = String::new();
        for grapheme in &graphemes[start..] {
            let grapheme_width = UnicodeWidthStr::width(*grapheme);
            if width.saturating_add(grapheme_width) > max_width {
                break;
            }
            view.push_str(grapheme);
            width = width.saturating_add(grapheme_width);
        }
        if views.last() != Some(&view) {
            views.push(view);
        }
        if suffix_widths[start] <= max_width {
            break;
        }
    }
    views
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct InputWindow {
    pub text: String,
    pub cursor_column: u16,
}

/// Windows editable text around its cursor without allowing animation to move it.
pub fn input_window(text: &str, cursor: usize, max_width: usize) -> InputWindow {
    if max_width == 0 {
        return InputWindow {
            text: String::new(),
            cursor_column: 0,
        };
    }
    let cursor_byte = text
        .char_indices()
        .nth(cursor.min(text.chars().count()))
        .map_or(text.len(), |(index, _)| index);
    let prefix = &text[..cursor_byte];
    let suffix = &text[cursor_byte..];
    let full_width = UnicodeWidthStr::width(text);
    let prefix_width = UnicodeWidthStr::width(prefix);
    if full_width < max_width || (full_width == max_width && prefix_width < max_width) {
        return InputWindow {
            text: text.to_owned(),
            cursor_column: prefix_width as u16,
        };
    }

    let before_budget = prefix_width.min(max_width.saturating_sub(1));
    let before = display_suffix(prefix, before_budget);
    let before_width = UnicodeWidthStr::width(before.as_str());
    let after = display_prefix(suffix, max_width.saturating_sub(before_width));
    InputWindow {
        text: before + &after,
        cursor_column: before_width as u16,
    }
}

pub fn marked_input_window(text: &str, cursor: usize, max_width: usize, marker: &str) -> String {
    if max_width == 0 {
        return String::new();
    }
    let cursor = cursor.min(text.chars().count());
    let byte = text
        .char_indices()
        .nth(cursor)
        .map_or(text.len(), |(index, _)| index);
    let marked = format!("{}{marker}{}", &text[..byte], &text[byte..]);
    input_window(&marked, cursor, max_width).text
}

fn display_prefix(text: &str, max_width: usize) -> String {
    let mut width: usize = 0;
    UnicodeSegmentation::graphemes(text, true)
        .take_while(|grapheme| {
            let next = width.saturating_add(UnicodeWidthStr::width(*grapheme));
            if next > max_width {
                false
            } else {
                width = next;
                true
            }
        })
        .collect()
}

fn display_suffix(text: &str, max_width: usize) -> String {
    let graphemes = UnicodeSegmentation::graphemes(text, true).collect::<Vec<_>>();
    let mut width: usize = 0;
    let start = graphemes
        .iter()
        .enumerate()
        .rev()
        .take_while(|(_, grapheme)| {
            let next = width.saturating_add(UnicodeWidthStr::width(**grapheme));
            if next > max_width {
                false
            } else {
                width = next;
                true
            }
        })
        .last()
        .map_or(graphemes.len(), |(index, _)| index);
    graphemes[start..].concat()
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
