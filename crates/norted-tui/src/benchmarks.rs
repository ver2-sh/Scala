//! Presentation and private-control actions only; selection/scoring live on server.
use crate::{
    app::App,
    theme::{Glyphs, Theme},
    ui::{
        components::{ActionState, action_style, inventory_columns, inventory_row, section_title},
        layout::{HoverTarget, UiLayout},
    },
};
use crossterm::event::{KeyCode, KeyEvent};
use norted_engine::benchmark::{BenchmarkRequest, performance_lines};
use ratatui::{
    Frame,
    layout::Rect,
    text::Line,
    widgets::{Paragraph, Wrap},
};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Run,
    History,
    Details,
    Mark,
    Compare,
    Back,
    Refresh,
    Cancel,
    Evidence,
}
impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Self::Run => "[b Run]",
            Self::History => "[h History]",
            Self::Details => "[d Details]",
            Self::Mark => "[Space Mark baseline]",
            Self::Compare => "[c Compare]",
            Self::Back => "[Backspace Back]",
            Self::Refresh => "[r Refresh]",
            Self::Cancel => "[x Cancel]",
            Self::Evidence => "[e Evidence]",
        }
    }
}
#[derive(Default)]
pub struct Benchmarks {
    pub overview: Value,
    pub history: Option<Value>,
    pub selected: usize,
    pub marked: Option<String>,
    pub pending: Option<BenchmarkRequest>,
    pub busy: bool,
    pub polling: bool,
    pub error: Option<String>,
    pub inspection: Option<Value>,
    pub evidence: bool,
    pub scroll: u16,
}
impl Benchmarks {
    pub fn rows(&self) -> &[Value] {
        let value = self
            .history
            .as_ref()
            .map(|h| &h["history"])
            .unwrap_or(&self.overview["rows"]);
        value.as_array().map(Vec::as_slice).unwrap_or(&[])
    }
    fn row(&self) -> &Value {
        self.rows().get(self.selected).unwrap_or(&Value::Null)
    }
    fn result(&self) -> &Value {
        if self.history.is_some() {
            self.row()
        } else {
            &self.row()["result"]
        }
    }
    pub fn actions(&self) -> Vec<Action> {
        use Action::*;
        if self.inspection.is_some() {
            vec![Back, Evidence, Cancel]
        } else if self.history.is_some() {
            vec![Back, Details, Mark, Compare, Refresh, Cancel]
        } else {
            vec![Run, History, Details, Refresh, Cancel]
        }
    }
    pub fn enabled(&self, action: Action) -> bool {
        use Action::*;
        let idle = (!self.busy || self.polling) && self.pending.is_none();
        match action {
            Back => self.history.is_some() || self.inspection.is_some(),
            Cancel => {
                self.overview["active"].is_object()
                    && self.overview["active"]["cancelling"] != true
                    && !matches!(self.pending, Some(BenchmarkRequest::Cancel))
            }
            Evidence => self.inspection.is_some() && !self.evidence,
            Mark => self.history.is_some() && self.row()["run_id"].is_string(),
            Run => {
                idle && self.history.is_none()
                    && self.row()["profile_id"].is_string()
                    && !self.overview["active"].is_object()
            }
            History => idle && self.history.is_none() && self.row()["profile_id"].is_string(),
            Details => idle && self.result()["run_id"].is_string(),
            Compare => {
                idle && self.history.is_some()
                    && self
                        .marked
                        .as_deref()
                        .is_some_and(|m| self.row()["run_id"].as_str().is_some_and(|r| r != m))
            }
            Refresh => idle,
        }
    }
    pub fn dispatch(&mut self, action: Action) {
        if !self.enabled(action) {
            return;
        }
        use Action::*;
        match action {
            Back => {
                if self.evidence {
                    self.evidence = false;
                } else if self.inspection.take().is_none() {
                    let profile = self
                        .history
                        .as_ref()
                        .and_then(|h| h["profile_id"].as_str())
                        .map(str::to_owned);
                    self.history = None;
                    self.selected = self
                        .rows()
                        .iter()
                        .position(|r| r["profile_id"].as_str() == profile.as_deref())
                        .unwrap_or(0);
                    self.marked = None;
                }
                self.scroll = 0;
            }
            Evidence => {
                self.evidence = true;
                self.scroll = 0;
            }
            Mark => self.marked = self.row()["run_id"].as_str().map(str::to_owned),
            Cancel => self.pending = Some(BenchmarkRequest::Cancel),
            Refresh => {
                self.pending = Some(
                    if let Some(id) = self
                        .history
                        .as_ref()
                        .and_then(|h| h["profile_id"].as_str())
                        .and_then(|s| s.parse().ok())
                    {
                        BenchmarkRequest::History { profile_id: id }
                    } else {
                        BenchmarkRequest::Status
                    },
                )
            }
            Run | History => {
                if let Some(id) = self.row()["profile_id"]
                    .as_str()
                    .and_then(|s| s.parse().ok())
                {
                    self.pending = Some(if action == Run {
                        BenchmarkRequest::Start { profile_id: id }
                    } else {
                        BenchmarkRequest::History { profile_id: id }
                    });
                }
            }
            Details => {
                self.pending = Some(BenchmarkRequest::Result {
                    run_id: self.result()["run_id"].as_str().unwrap().into(),
                })
            }
            Compare => {
                self.pending = Some(BenchmarkRequest::Compare {
                    left: self.marked.clone().unwrap(),
                    right: self.row()["run_id"].as_str().unwrap().into(),
                })
            }
        }
    }
    pub fn navigate(&mut self, direction: isize, detail: bool) {
        if detail || self.inspection.is_some() {
            self.scroll = self.scroll.saturating_add_signed(direction as i16);
        } else {
            self.selected = self
                .selected
                .saturating_add_signed(direction)
                .min(self.rows().len().saturating_sub(1));
            self.scroll = 0;
        }
    }
    pub fn key(&mut self, key: KeyEvent) {
        use Action::*;
        let action = match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.navigate(-1, false);
                return;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.navigate(1, false);
                return;
            }
            KeyCode::PageUp => {
                self.navigate(-5, true);
                return;
            }
            KeyCode::PageDown => {
                self.navigate(5, true);
                return;
            }
            KeyCode::Esc | KeyCode::Backspace => Back,
            KeyCode::Char('b') => Run,
            KeyCode::Char('h') => History,
            KeyCode::Char('d') => Details,
            KeyCode::Char(' ') => Mark,
            KeyCode::Char('c') => Compare,
            KeyCode::Char('r') => Refresh,
            KeyCode::Char('x') => Cancel,
            KeyCode::Char('e') => Evidence,
            KeyCode::Enter => {
                if self.history.is_some() {
                    Details
                } else {
                    History
                }
            }
            _ => return,
        };
        if self.actions().contains(&action) {
            self.dispatch(action);
        }
    }
    pub fn accept(&mut self, request: &BenchmarkRequest, result: Result<Value, String>) {
        self.busy = false;
        self.polling = false;
        match result {
            Err(error) => self.error = Some(error),
            Ok(value) => {
                self.error = None;
                match request {
                    BenchmarkRequest::Status => {
                        let id = self.row()["profile_id"].as_str().map(str::to_owned);
                        self.overview = value;
                        if self.history.is_none() {
                            self.selected = self
                                .rows()
                                .iter()
                                .position(|r| r["profile_id"].as_str() == id.as_deref())
                                .unwrap_or(0);
                        }
                    }
                    BenchmarkRequest::History { .. } => {
                        let id = self.row()["run_id"].as_str().map(str::to_owned);
                        self.history = Some(value);
                        self.selected = self
                            .rows()
                            .iter()
                            .position(|r| r["run_id"].as_str() == id.as_deref())
                            .unwrap_or(0);
                        self.scroll = 0;
                    }
                    BenchmarkRequest::Result { .. } | BenchmarkRequest::Compare { .. } => {
                        self.inspection = Some(value);
                        self.evidence = false;
                        self.scroll = 0;
                    }
                    _ => {
                        if self.pending.is_none() {
                            self.pending = Some(BenchmarkRequest::Status);
                        }
                    }
                }
            }
        }
    }
}

/// These rectangles are shared verbatim by rendering and input.
#[derive(Debug, Clone, Default)]
pub struct BenchmarkLayout {
    pub title: Rect,
    pub progress: Rect,
    pub header: Rect,
    pub detail: Rect,
    pub rows: Vec<(usize, Rect)>,
    pub actions: Vec<(Action, Rect)>,
}
impl BenchmarkLayout {
    pub fn calculate(area: Rect, state: &Benchmarks) -> Self {
        let mut out = Self::default();
        if area.height == 0 || area.width == 0 {
            return out;
        }
        // Flow actions first so compact terminals reserve exactly the lines they need.
        let mut x = 0;
        let mut y = 0;
        for action in state.actions() {
            let width = (action.label().len() as u16).min(area.width);
            if x > 0 && x + width > area.width {
                y += 1;
                x = 0;
            }
            out.actions
                .push((action, Rect::new(area.x + x, y, width, 1)));
            x += width + 1;
        }
        let action_height = (y + 1).min(area.height);
        for (_, rect) in &mut out.actions {
            rect.y += area.bottom() - action_height;
        }
        out.actions.retain(|(_, r)| r.y < area.bottom());
        let mut remaining = area.height - action_height;
        let title_height = if remaining >= 8 { 2 } else { 0 };
        out.title = Rect::new(area.x, area.y, area.width, title_height);
        remaining -= title_height;
        let progress_height = u16::from(remaining >= 3);
        out.progress = Rect::new(area.x, out.title.bottom(), area.width, progress_height);
        remaining -= progress_height;
        let top = out.progress.bottom();
        if state.inspection.is_some() || state.rows().is_empty() {
            out.detail = Rect::new(area.x, top, area.width, remaining);
            return out;
        }
        let header_height = u16::from(remaining >= 2);
        out.header = Rect::new(area.x, top, area.width, header_height);
        remaining -= header_height;
        let capacity = if remaining >= 6 {
            remaining / 2
        } else {
            remaining.saturating_sub(1).min(3)
        };
        let capacity = capacity.min(state.rows().len().min(u16::MAX as usize) as u16);
        let start = state
            .selected
            .saturating_sub(capacity.saturating_sub(1) as usize);
        out.rows = state
            .rows()
            .iter()
            .enumerate()
            .skip(start)
            .take(capacity as usize)
            .enumerate()
            .map(|(offset, (index, _))| {
                (
                    index,
                    Rect::new(area.x, out.header.bottom() + offset as u16, area.width, 1),
                )
            })
            .collect();
        out.detail = Rect::new(
            area.x,
            out.header.bottom() + capacity,
            area.width,
            remaining - capacity,
        );
        out
    }
}
fn number(value: &Value) -> String {
    value
        .as_f64()
        .map(|n| format!("{n:.1}"))
        .unwrap_or_else(|| "unavailable".into())
}
fn text(value: &Value) -> String {
    value.as_str().map(str::to_owned).unwrap_or_else(|| {
        if value.is_null() {
            "unavailable".into()
        } else {
            value.to_string()
        }
    })
}
fn timestamp(value: &Value) -> String {
    let Some(ms) = value.as_u64() else {
        return "never".into();
    };
    // UTC civil date conversion, avoiding a date/time dependency for table cells.
    let seconds = ms / 1000;
    let z = (seconds / 86400) as i64 + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}Z",
        seconds % 86400 / 3600,
        seconds % 3600 / 60
    )
}

fn metric(result: &Value, field: &str) -> String {
    let v = &result["speed"]["combined"][field];
    if !v["median"].is_null() {
        number(&v["median"])
    } else if !v["partial_median"].is_null() {
        format!("partial {}", number(&v["partial_median"]))
    } else {
        "unavailable".into()
    }
}
fn fields(lines: &mut Vec<String>, label: &str, value: &Value) {
    match value {
        Value::Object(map) => {
            lines.push(label.to_owned());
            for (key, value) in map {
                fields(lines, &format!("  {}", key.replace('_', " ")), value);
            }
        }
        Value::Array(items) => {
            lines.push(label.to_owned());
            for item in items {
                fields(lines, "  -", item);
            }
        }
        _ => lines.push(format!("{label:<24} {}", text(value))),
    }
}

pub fn clamp_scroll(state: &mut Benchmarks, area: Rect) {
    let max = Paragraph::new(detail_lines(state).join("\n"))
        .wrap(Wrap { trim: false })
        .line_count(area.width.max(1))
        .saturating_sub(area.height as usize)
        .min(u16::MAX as usize) as u16;
    state.scroll = state.scroll.min(max);
}

fn summary_lines(s: &Value) -> Vec<String> {
    if s.is_null() {
        return vec![
            "No selected result. Measurements are unavailable until a run finishes.".into(),
        ];
    }
    let mut lines = vec![
        format!("Run: {} | {}", text(&s["run_id"]), text(&s["status"])),
        format!(
            "Intelligence: {} / 100 | Agentic: {} / 100",
            number(&s["intelligence"]),
            number(&s["agentic"])
        ),
    ];
    lines.extend(performance_lines(&s["speed"]).into_iter().map(|line| {
        line.split_once(": ")
            .map(|(label, value)| format!("{label:<24} {value}"))
            .unwrap_or(line)
    }));
    for key in [
        "display_name",
        "agentic_unavailable",
        "diagnostic",
        "suite",
        "methodology",
        "configuration_key",
        "saved_equals_served_requested_settings",
        "saved_configuration",
        "single_pass",
        "multi_pass",
        "categories",
        "task_outcomes",
    ] {
        if !s[key].is_null() {
            fields(&mut lines, &key.replace('_', " "), &s[key]);
        }
    }
    for field in [
        "visible_delivery_characters_per_second",
        "visible_end_to_end_characters_per_second",
        "native_end_to_end_output_tokens_per_second",
        "first_visible_ms",
    ] {
        for missing in s["speed"]["combined"][field]["missing"]
            .as_array()
            .into_iter()
            .flatten()
        {
            lines.push(format!(
                "{} / {}: {}",
                field.replace('_', " "),
                text(&missing["id"]),
                text(&missing["reason"])
            ));
        }
    }
    lines.push(format!("Finished: {}", timestamp(&s["ended_unix_ms"])));
    for sample in s["speed"]["samples"].as_array().into_iter().flatten() {
        lines.push(format!(
            "Probe {}: {} - {}",
            text(&sample["id"]),
            text(&sample["outcome"]),
            text(&sample["outcome_reason"])
        ));
    }
    lines
}
fn detail_lines(state: &Benchmarks) -> Vec<String> {
    let mut lines = content_lines(state);
    if let Some(error) = &state.error {
        lines.insert(0, format!("Error: {error}"));
    }
    lines
}
fn content_lines(state: &Benchmarks) -> Vec<String> {
    if let Some(value) = &state.inspection {
        if state.evidence {
            return serde_json::to_string_pretty(value)
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect();
        }
        if value["summary"].is_object() {
            return summary_lines(&value["summary"]);
        }
        let mut lines = vec!["Comparison: baseline (left) -> selected (right)".into()];
        for key in [
            "same_methods",
            "same_configuration",
            "conditions_verified",
            "warning",
            "intelligence_delta",
            "agentic_delta",
            "latency_delta_ms",
        ] {
            lines.push(format!(
                "{:<24} {}",
                key.replace('_', " "),
                text(&value[key])
            ));
        }
        if value["same_methods"] != true {
            lines
                .push("WARNING: suite/methods differ; results are not directly comparable.".into());
        }
        lines.push(format!(
            "{:<30} {:<18} {}",
            "Metric", "Baseline", "Selected"
        ));
        for (label, key) in [
            ("Intelligence (points)", "intelligence"),
            ("Agentic (points)", "agentic"),
        ] {
            lines.push(format!(
                "{label:<30} {:<18} {}",
                number(&value["left"][key]),
                number(&value["right"][key])
            ));
        }
        for (label, key) in [
            (
                "Delivery (chars/s)",
                "visible_delivery_characters_per_second",
            ),
            (
                "Text end-to-end (chars/s)",
                "visible_end_to_end_characters_per_second",
            ),
            (
                "Native end-to-end (tokens/s)",
                "native_end_to_end_output_tokens_per_second",
            ),
            ("First visible (ms)", "first_visible_ms"),
        ] {
            lines.push(format!(
                "{label:<30} {:<18} {}",
                metric(&value["left"], key),
                metric(&value["right"], key)
            ));
        }
        lines.push("Baseline".into());
        lines.extend(summary_lines(&value["left"]));
        lines.push("Selected run".into());
        lines.extend(summary_lines(&value["right"]));
        for key in [
            "changed_settings",
            "changed_runtime",
            "changed_hardware",
            "changed_tasks",
        ] {
            lines.push(key.replace('_', " "));
            for change in value[key].as_array().into_iter().flatten() {
                lines.push(format!(
                    "{}: {} -> {}",
                    text(change.get("field").unwrap_or(&change["id"])),
                    text(&change["left"]),
                    text(&change["right"])
                ));
                if change.get("left_score").is_some() {
                    lines.push(format!(
                        "Score: {} -> {}",
                        text(&change["left_score"]),
                        text(&change["right_score"])
                    ));
                }
            }
        }
        return lines;
    }
    if state.rows().is_empty() {
        return vec![if state.busy && state.overview.is_null() {
            "Loading benchmarks...".into()
        } else if state.history.is_some() {
            "No history for this profile. Back returns to profiles.".into()
        } else {
            "No benchmark records. Run a Model Profile to create a result.".into()
        }];
    }
    let row = state.row();
    let mut lines = vec![format!(
        "Selected: {}",
        text(if state.history.is_some() {
            &row["run_id"]
        } else {
            &row["display_name"]
        })
    )];
    if state.history.is_none() {
        lines.push(format!(
            "State: {} | Last finished: {}",
            text(&row["state"]),
            timestamp(&row["last_benchmark_unix_ms"])
        ));
        lines.push(format!(
            "Latest attempt (separate): {} | {}",
            text(&row["latest_attempt"]["run_id"]),
            text(&row["latest_attempt"]["status"])
        ));
        if let Some(diagnostic) = row["latest_attempt"]["diagnostic"].as_str() {
            lines.push(format!("Latest attempt diagnostic: {diagnostic}"));
        }
        for key in ["configuration_notes", "identity_note"] {
            if !row[key].is_null() {
                lines.push(format!("{}: {}", key.replace('_', " "), text(&row[key])));
            }
        }
    }
    if let Some(mark) = &state.marked {
        lines.insert(0, format!("* Baseline: {mark}"));
    }
    lines.extend(summary_lines(state.result()));
    lines
}
pub fn render(frame: &mut Frame<'_>, app: &App, theme: &Theme, glyphs: &Glyphs, layout: &UiLayout) {
    let state = &app.benchmarks;
    let l = &layout.benchmarks;
    let title = if state.evidence {
        "Benchmark evidence"
    } else if state.inspection.is_some() {
        "Benchmark inspection"
    } else if state.history.is_some() {
        "Benchmark history"
    } else {
        "Benchmarks"
    };
    frame.render_widget(
        section_title(
            title,
            "Manual runs | PgUp/PgDn or wheel: selected details",
            theme,
        ),
        l.title,
    );
    let active = &state.overview["active"];
    let status = if active.is_object() {
        format!(
            "{}: {} {}/{} | {}s elapsed | {}s left",
            if active["cancelling"] == true {
                "Cancelling"
            } else {
                "Running"
            },
            text(&active["phase"]),
            active["completed_tasks"],
            active["total_tasks"],
            number(&active["elapsed_seconds"]),
            number(&active["remaining_seconds"])
        )
    } else if let Some(error) = &state.error {
        format!("Error: {error}")
    } else if state.busy && state.overview.is_null() {
        "Loading...".into()
    } else {
        format!("Suite: {} | No active run", text(&state.overview["suite"]))
    };
    frame.render_widget(
        Paragraph::new(status).style(if active.is_object() {
            theme.warning
        } else if state.error.is_some() {
            theme.error
        } else {
            theme.muted
        }),
        l.progress,
    );
    let wide = l.header.width >= 120;
    let tails: &[u16] = if wide {
        &[12, 11, 16, 16, 22, 17]
    } else {
        &[22]
    };
    let columns = inventory_columns(l.header, tails, &[18]);
    let headings: Vec<String> = if wide {
        vec![
            "Profile / run",
            "Intelligence",
            "Agentic",
            "Delivery chars/s",
            "First visible ms",
            "State",
            "Last finished UTC",
        ]
    } else {
        vec!["Profile / run", "State"]
    }
    .into_iter()
    .map(str::to_owned)
    .collect();
    inventory_row(frame, l.header, &columns, &headings, theme.muted, glyphs);
    for (index, rect) in &l.rows {
        let row = &state.rows()[*index];
        let result = if state.history.is_some() {
            row
        } else {
            &row["result"]
        };
        let selected = *index == state.selected;
        let marked = state
            .marked
            .as_deref()
            .is_some_and(|m| row["run_id"].as_str() == Some(m));
        let name = format!(
            "{}{} {}",
            if selected { ">" } else { " " },
            if marked { "*" } else { " " },
            text(if state.history.is_some() {
                &row["run_id"]
            } else {
                &row["display_name"]
            })
        );
        let status = text(if state.history.is_some() {
            &row["status"]
        } else {
            &row["state"]
        });
        let values = if wide {
            vec![
                name,
                number(&result["intelligence"]),
                number(&result["agentic"]),
                metric(result, "visible_delivery_characters_per_second"),
                metric(result, "first_visible_ms"),
                status,
                timestamp(if state.history.is_some() {
                    &row["ended_unix_ms"]
                } else {
                    &row["last_benchmark_unix_ms"]
                }),
            ]
        } else {
            vec![name, status]
        };
        let style = if selected { theme.selected } else { theme.text };
        let style = if app.hover == Some(HoverTarget::BenchmarkRow(*index)) {
            style.patch(theme.hovered)
        } else {
            style
        };
        inventory_row(frame, *rect, &columns, &values, style, glyphs);
    }
    let lines = detail_lines(state);
    let paragraph = Paragraph::new(
        lines
            .into_iter()
            .map(|line| {
                let heading = line.starts_with("Selected:")
                    || line.starts_with("Comparison:")
                    || matches!(line.as_str(), "Baseline" | "Selected run");
                Line::styled(line, if heading { theme.accent } else { theme.text })
            })
            .collect::<Vec<_>>(),
    )
    .wrap(Wrap { trim: false });
    let max = paragraph
        .line_count(l.detail.width.max(1))
        .saturating_sub(l.detail.height as usize)
        .min(u16::MAX as usize) as u16;
    frame.render_widget(
        paragraph
            .scroll((state.scroll.min(max), 0))
            .style(theme.text),
        l.detail,
    );
    for (action, rect) in &l.actions {
        let enabled = state.enabled(*action);
        let style = action_style(
            theme,
            if !enabled {
                ActionState::Disabled
            } else if *action == Action::Run {
                ActionState::Primary
            } else if *action == Action::Cancel {
                ActionState::Destructive
            } else {
                ActionState::Normal
            },
            app.hover == Some(HoverTarget::BenchmarkAction(*action)),
        );
        let label = if enabled {
            action.label().to_owned()
        } else {
            action.label().replace('[', "(").replace(']', ")")
        };
        frame.render_widget(Paragraph::new(label).style(style), *rect);
    }
}
