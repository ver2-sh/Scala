//! Presentation and private-control actions only; selection/scoring live on server.
use crossterm::event::{KeyCode, KeyEvent};
use norted_engine::benchmark::BenchmarkRequest;
use ratatui::{
    Frame,
    layout::Rect,
    text::Line,
    widgets::{Paragraph, Wrap},
};
use serde_json::Value;

#[derive(Default)]
pub struct Benchmarks {
    pub overview: Value,
    pub history: Option<Value>,
    pub selected: usize,
    pub marked: Option<String>,
    pub pending: Option<BenchmarkRequest>,
    pub busy: bool,
    pub error: Option<String>,
}
impl Benchmarks {
    pub fn rows(&self) -> &[Value] {
        if let Some(h) = &self.history {
            h["history"].as_array().map(Vec::as_slice).unwrap_or(&[])
        } else {
            self.overview["rows"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[])
        }
    }
    pub fn key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.rows().len().saturating_sub(1))
            }
            KeyCode::Esc | KeyCode::Backspace => {
                self.history = None;
                self.selected = 0;
            }
            KeyCode::Char('x') => self.pending = Some(BenchmarkRequest::Cancel),
            KeyCode::Char('r') => self.pending = Some(BenchmarkRequest::Status),
            KeyCode::Char('b') if self.history.is_none() => {
                if let Some(id) = self
                    .rows()
                    .get(self.selected)
                    .and_then(|r| r["profile_id"].as_str())
                    .and_then(|s| s.parse().ok())
                {
                    self.pending = Some(BenchmarkRequest::Start { profile_id: id });
                }
            }
            KeyCode::Char('h') | KeyCode::Enter if self.history.is_none() => {
                if let Some(id) = self
                    .rows()
                    .get(self.selected)
                    .and_then(|r| r["profile_id"].as_str())
                    .and_then(|s| s.parse().ok())
                {
                    self.pending = Some(BenchmarkRequest::History { profile_id: id });
                    self.selected = 0;
                }
            }
            KeyCode::Enter | KeyCode::Char('d') if self.history.is_some() => {
                if let Some(id) = self
                    .rows()
                    .get(self.selected)
                    .and_then(|r| r["run_id"].as_str())
                {
                    self.pending = Some(BenchmarkRequest::Result { run_id: id.into() });
                }
            }
            KeyCode::Char(' ') if self.history.is_some() => {
                self.marked = self
                    .rows()
                    .get(self.selected)
                    .and_then(|r| r["run_id"].as_str())
                    .map(str::to_owned);
            }
            KeyCode::Char('c') if self.history.is_some() => {
                if let Some(left) = &self.marked {
                    if let Some(right) = self
                        .rows()
                        .get(self.selected)
                        .and_then(|r| r["run_id"].as_str())
                    {
                        self.pending = Some(BenchmarkRequest::Compare {
                            left: left.clone(),
                            right: right.into(),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    pub fn accept(
        &mut self,
        request: &BenchmarkRequest,
        result: Result<Value, String>,
    ) -> Option<String> {
        self.busy = false;
        match result {
            Err(error) => self.error = Some(error),
            Ok(value) => {
                self.error = None;
                match request {
                    BenchmarkRequest::Status => self.overview = value,
                    BenchmarkRequest::History { .. } => {
                        self.history = Some(value);
                        self.selected = 0;
                    }
                    BenchmarkRequest::Result { .. } | BenchmarkRequest::Compare { .. } => {
                        return Some(norted_engine::benchmark::inspection_text(&value));
                    }
                    _ => self.pending = Some(BenchmarkRequest::Status),
                }
            }
        }
        None
    }
}
fn number(value: &Value) -> String {
    value
        .as_f64()
        .map(|n| format!("{n:.1}"))
        .unwrap_or_else(|| "—".into())
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
pub fn render(frame: &mut Frame<'_>, area: Rect, state: &Benchmarks) {
    let mut lines = vec![
        Line::from("Benchmarks · Norted Quick Bench v1"),
        Line::from("b Run selected profile  h History  x Cancel  r Refresh"),
        Line::from("History: Enter Details  Space Mark  c Compare  Backspace Profiles"),
    ];
    let active = &state.overview["active"];
    if !active.is_null() {
        lines.push(Line::from(format!(
            "RESERVED: {} · {} · {} / 40 · {}s elapsed · {}s left",
            active["profile_id"].as_str().unwrap_or(""),
            active["phase"].as_str().unwrap_or(""),
            active["completed_tasks"],
            number(&active["elapsed_seconds"]),
            number(&active["remaining_seconds"])
        )));
    } else {
        lines.push(Line::from(
            "Manual only. Running a benchmark temporarily reserves normal inference.",
        ));
    }
    if let Some(error) = &state.error {
        lines.push(Line::from(format!("Error: {error}")));
    }
    lines.push(Line::from(
        "Norted Quick Intelligence / Agentic: points; speed: visible chars/s; latency: first visible ms",
    ));
    lines.push(Line::from(""));
    let capacity = (area.height as usize).saturating_sub(lines.len() + 2) / 3;
    let start = state.selected.saturating_sub(capacity.saturating_sub(1));
    for (i, row) in state
        .rows()
        .iter()
        .enumerate()
        .skip(start)
        .take(capacity.max(1))
    {
        let (r, title, status, last) = if state.history.is_some() {
            (
                row,
                row["run_id"].as_str().unwrap_or(""),
                row["status"].as_str().unwrap_or(""),
                &row["ended_unix_ms"],
            )
        } else {
            (
                &row["result"],
                row["display_name"].as_str().unwrap_or(""),
                row["state"].as_str().unwrap_or(""),
                &row["last_benchmark_unix_ms"],
            )
        };
        let status = if status == "completed_unavailable" {
            "Finished — metrics unavailable"
        } else {
            status
        };
        let title: String = title.chars().take(36).collect();
        lines.push(Line::from(format!(
            "{} {} · {}",
            if i == state.selected { ">" } else { " " },
            title,
            status
        )));
        lines.push(Line::from(format!(
            "  I {}  A {}  Speed {}  Latency {}  {} {}",
            number(&r["intelligence"]),
            number(&r["agentic"]),
            number(&r["speed"]["combined"]["visible_delivery_characters_per_second"]["median"]),
            number(&r["speed"]["combined"]["first_visible_ms"]["median"]),
            if state.history.is_some() {
                "Ended"
            } else {
                "Last"
            },
            timestamp(last)
        )));
        let note = if state.history.is_some() {
            row["diagnostic"].as_str().unwrap_or("").to_owned()
        } else {
            format!(
                "Latest attempt: {}{}",
                row["latest_attempt"]["status"].as_str().unwrap_or("none"),
                if r["agentic_unavailable"].is_string() {
                    " · Agentic unsupported (see details)"
                } else {
                    ""
                }
            )
        };
        lines.push(Line::from(format!("  {note}")));
    }
    if state.rows().is_empty() {
        lines.push(Line::from(if state.busy {
            "Loading…"
        } else {
            "No records. Select a Model Profile and press b to run."
        }));
    }
    if let Some(mark) = &state.marked {
        lines.push(Line::from(format!("Comparison baseline: {mark}")));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}
