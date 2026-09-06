//! Server-owned, local per-profile benchmark evidence and shared methodology.
mod storage;
pub mod suite;
use crate::{InferenceToolCall, InferenceUsage};
use norted_core::{ModelProfile, ModelProfileId, RuntimeProvenance};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Instant};
pub(crate) use storage::Store;
use tokio::sync::{Mutex, RwLock, watch};

pub const CONTROL_BENCHMARK_PATH: &str = "/control/v1/benchmarks";
tokio::task_local! { pub(crate) static EXECUTOR: (); }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BenchmarkRequest {
    Start { profile_id: ModelProfileId },
    Status,
    Cancel,
    History { profile_id: ModelProfileId },
    Result { run_id: String },
    Compare { left: String, right: String },
}

pub(crate) struct Service {
    pub owner: Mutex<Option<std::fs::File>>,
    pub gate: Arc<RwLock<()>>,
    pub active: Mutex<Option<Active>>,
    pub store: Store,
    pub load_handoff: Mutex<()>,
    pub load_task: std::sync::Mutex<Option<tokio::task::AbortHandle>>,
    pub quarantined: std::sync::atomic::AtomicBool,
}
pub(crate) struct Active {
    pub run_id: String,
    pub profile_id: ModelProfileId,
    pub started: Instant,
    pub phase: String,
    pub done: usize,
    pub cancel: watch::Sender<bool>,
}
impl Service {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self {
            owner: Mutex::new(None),
            load_handoff: Mutex::new(()),
            load_task: std::sync::Mutex::new(None),
            quarantined: std::sync::atomic::AtomicBool::new(false),
            gate: Arc::new(RwLock::new(())),
            active: Mutex::new(None),
            store: Store::new(path),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub id: String,
    pub category: String,
    pub status: String,
    pub score: Option<f64>,
    pub explanation: String,
    pub input: String,
    pub input_utf8_bytes: usize,
    pub input_unicode_characters: usize,
    pub expected: Value,
    pub request_overrides: Value,
    #[serde(default)]
    pub stream_event_observed: bool,
    pub seconds_limit: u64,
    pub max_output_tokens: Option<u32>,
    pub response: String,
    pub tools: Vec<Value>,
    pub timing: Timing,
    pub usage: Option<InferenceUsage>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Timing {
    pub request_start_unix_ms: u128,
    pub first_visible_ms: Option<f64>,
    pub first_text_ms: Option<f64>,
    pub last_text_ms: Option<f64>,
    pub completion_ms: Option<f64>,
    pub first_executable_tool_ms: Option<f64>,
    pub first_chunk_characters: usize,
    pub visible_characters: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub record_version: u32,
    pub run_id: String,
    pub profile: ModelProfile,
    pub profile_hash: String,
    pub started_unix_ms: u128,
    pub ended_unix_ms: Option<u128>,
    pub duration_seconds: f64,
    pub status: String,
    pub diagnostic: Option<String>,
    pub suite: String,
    pub pack_hash: String,
    pub methodology: String,
    pub policy: String,
    pub manifest: Value,
    pub server_version: String,
    pub source_revision: Option<String>,
    pub provenance: Option<RuntimeProvenance>,
    pub saved_configuration: Value,
    pub environment: Value,
    pub configuration_key: Option<String>,
    pub loaded_before: bool,
    pub load_seconds: Option<f64>,
    pub phases: BTreeMap<String, f64>,
    pub missing: Vec<String>,
    pub evidence: Vec<Evidence>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub run_id: String,
    pub profile_id: ModelProfileId,
    pub display_name: String,
    pub started_unix_ms: u128,
    pub ended_unix_ms: Option<u128>,
    pub status: String,
    pub diagnostic: Option<String>,
    pub configuration_key: Option<String>,
    pub profile_hash: String,
    pub saved_configuration: Value,
    pub suite: String,
    pub pack_hash: String,
    pub methodology: String,
    pub intelligence: Option<f64>,
    pub categories: BTreeMap<String, Value>,
    pub agentic: Option<f64>,
    pub single_pass: usize,
    pub multi_pass: usize,
    pub agentic_unavailable: Option<String>,
    pub saved_equals_served_requested_settings: Option<bool>,
    pub speed: Value,
    pub task_outcomes: BTreeMap<String, String>,
}

pub fn digest(value: impl Serialize) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&value).expect("serializable benchmark data"))
    )
}
pub fn manifest() -> Value {
    json!({"suite":suite::SUITE,"method":suite::METHOD,"policy":suite::POLICY,
        "intelligence":suite::questions(),"single_tools":suite::single_cases(),
        "agents":suite::AGENT_PROMPTS,"tools":suite::tools(),"fixture":(0..4).map(suite::Fixture::new).collect::<Vec<_>>(),
        "probes":suite::probes().into_iter().map(|(id,input)|json!({"id":id,"utf8_bytes":input.len(),"unicode_characters":input.chars().count(),"input":input,"seconds":15,"max_output_tokens":512})).collect::<Vec<_>>(),
        "warmup":{"input":"Reply with the word ready.","seconds":10,"max_output_tokens":32},
        "agent_limits":{"single_seconds":8,"single_turns":1,"multi_seconds":30,"multi_turns":6,"multi_calls":8,"max_output_tokens":384},
        "evidence_limit_bytes":65536,"speed_min_characters":400,"delivery_min_span_ms":50,"delivery_min_characters_after_first":128})
}
pub fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

pub fn distribution(mut values: Vec<f64>) -> Value {
    values.retain(|v| v.is_finite() && *v >= 0.0);
    values.sort_by(f64::total_cmp);
    if values.is_empty() {
        return json!({"count":0,"median":null,"min":null,"max":null});
    }
    let n = values.len();
    let median = if n % 2 == 0 {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    } else {
        values[n / 2]
    };
    json!({"count":n,"median":median,"min":values[0],"max":values[n-1]})
}

pub fn speed_sample(e: &Evidence) -> Value {
    let t = &e.timing;
    let reason = if e.status != "passed" {
        Some(e.explanation.as_str())
    } else if e.response.trim().to_lowercase().starts_with("i cannot")
        || e.response.trim().to_lowercase().starts_with("i can’t")
        || e.response.trim().to_lowercase().starts_with("i can't")
        || e.response.trim().to_lowercase().starts_with("i'm sorry")
    {
        Some("refusal, not a usable workload response")
    } else if t.visible_characters < 400 {
        Some("insufficient output: fewer than 400 Unicode characters")
    } else if t.completion_ms.is_none_or(|v| !v.is_finite() || v < 50.0)
        || t.first_text_ms
            .zip(t.last_text_ms)
            .zip(t.completion_ms)
            .is_none_or(|((first, last), end)| {
                !first.is_finite() || !last.is_finite() || first < 0.0 || first > last || last > end
            })
        || t.first_visible_ms
            .zip(t.first_text_ms)
            .zip(t.completion_ms)
            .is_none_or(|((visible, text), end)| {
                !visible.is_finite() || visible < text || visible > end
            })
        || t.first_chunk_characters > t.visible_characters
    {
        Some("unusable timing: require ordered finite timestamps and at least 50 ms")
    } else {
        None
    };
    if let Some(reason) = reason {
        return json!({"id":e.id,"valid":false,"reason":reason});
    }
    let delivery = t
        .first_text_ms
        .zip(t.last_text_ms)
        .and_then(|(first, last)| {
            let remaining = t
                .visible_characters
                .saturating_sub(t.first_chunk_characters);
            (last - first >= 50.0 && remaining >= 128)
                .then_some(remaining as f64 * 1000.0 / (last - first))
        });
    // Native counters include any hidden reasoning. Only the whole-request
    // interval covers that population; no visible-interval native decode rate.
    let native = e
        .usage
        .as_ref()
        .filter(|u| {
            u.input_tokens > 0
                && u.output_tokens > 0
                && Some(u.total_tokens) == u.input_tokens.checked_add(u.output_tokens)
                && u.reasoning_output_tokens
                    .is_none_or(|r| r <= u.output_tokens)
        })
        .map(|u| u.output_tokens as f64 * 1000.0 / t.completion_ms.unwrap_or(1.0));
    json!({"id":e.id,"valid":delivery.is_some(),"reason":if delivery.is_none(){Some("insufficient post-first-chunk delivery: need 128 characters and 50 ms")}else{None},"visible_delivery_characters_per_second":delivery,
        "native_end_to_end_output_tokens_per_second":native,"first_visible_ms":t.first_visible_ms,
        "first_text_ms":t.first_text_ms,"completion_ms":t.completion_ms,
        "native_decode_tokens_per_second":null,"first_answer_ms":null})
}

impl Run {
    pub fn summary(&self) -> Summary {
        let mut categories = BTreeMap::new();
        let mut means = Vec::new();
        for name in ["logic", "context", "code", "instruction"] {
            let items = self
                .evidence
                .iter()
                .filter(|e| e.category == name)
                .collect::<Vec<_>>();
            let scored = items.iter().filter_map(|e| e.score).collect::<Vec<_>>();
            let passed = scored.iter().filter(|s| **s == 1.0).count();
            let complete = scored.len() == 6;
            if complete {
                means.push(scored.iter().sum::<f64>() / 6.0);
            }
            categories.insert(name.into(),json!({"passed":passed,"attempted":items.len(),"scored":scored.len(),"required":6,"score":complete.then_some(passed as f64/6.0*100.0)}));
        }
        let single = self
            .evidence
            .iter()
            .filter(|e| e.category == "tool")
            .collect::<Vec<_>>();
        let multi = self
            .evidence
            .iter()
            .filter(|e| e.category == "agent")
            .collect::<Vec<_>>();
        let single_pass = single.iter().filter(|e| e.score == Some(1.0)).count();
        let multi_pass = multi.iter().filter(|e| e.score == Some(1.0)).count();
        let tool_complete = single.iter().filter(|e| e.score.is_some()).count() == 8
            && multi.iter().filter(|e| e.score.is_some()).count() == 4;
        let agentic_unavailable = self
            .missing
            .iter()
            .find(|s| s.starts_with("Agentic unavailable:"))
            .cloned();
        let samples = self
            .evidence
            .iter()
            .filter(|e| e.category == "speed")
            .map(speed_sample)
            .collect::<Vec<_>>();
        let mut groups = serde_json::Map::new();
        for prefix in ["short", "medium", ""] {
            let selected = samples
                .iter()
                .filter(|s| s["id"].as_str().unwrap_or("").starts_with(prefix))
                .collect::<Vec<_>>();
            let mut group = serde_json::Map::new();
            for field in [
                "visible_delivery_characters_per_second",
                "native_end_to_end_output_tokens_per_second",
                "first_visible_ms",
                "first_text_ms",
                "completion_ms",
            ] {
                let values = selected
                    .iter()
                    .filter_map(|s| s[field].as_f64())
                    .collect::<Vec<_>>();
                let required = if prefix.is_empty() { 4 } else { 2 };
                let mut stats = distribution(values);
                if stats["count"].as_u64() != Some(required) {
                    stats["median"] = Value::Null;
                }
                group.insert(field.into(), stats);
            }
            groups.insert(
                if prefix.is_empty() {
                    "combined"
                } else {
                    prefix
                }
                .into(),
                Value::Object(group),
            );
        }
        groups.insert("samples".into(), json!(samples));
        Summary {
            run_id: self.run_id.clone(),
            profile_id: self.profile.id.clone(),
            display_name: self.profile.display_name.clone(),
            started_unix_ms: self.started_unix_ms,
            ended_unix_ms: self.ended_unix_ms,
            status: self.status.clone(),
            diagnostic: self.diagnostic.clone(),
            configuration_key: self.configuration_key.clone(),
            profile_hash: self.profile_hash.clone(),
            saved_configuration: self.saved_configuration.clone(),
            suite: self.suite.clone(),
            pack_hash: self.pack_hash.clone(),
            methodology: self.methodology.clone(),
            intelligence: (means.len() == 4).then(|| 100.0 * means.iter().sum::<f64>() / 4.0),
            categories,
            agentic: tool_complete.then_some(
                100.0 * (0.5 * single_pass as f64 / 8.0 + 0.5 * multi_pass as f64 / 4.0),
            ),
            single_pass,
            multi_pass,
            agentic_unavailable,
            saved_equals_served_requested_settings:
                self.environment["saved_equals_served_requested_settings"].as_bool(),
            speed: Value::Object(groups),
            task_outcomes: self
                .evidence
                .iter()
                .map(|e| (e.id.clone(), e.status.clone()))
                .collect(),
        }
    }
}

/// Latest complete applicable result, never best score. The fallback is a
/// coherent historical run; latest attempt is displayed separately.
pub fn finished(status: &str) -> bool {
    matches!(status, "completed" | "completed_unavailable")
}

pub fn select<'a>(
    history: &'a [Summary],
    key: Option<&str>,
    pack: &str,
) -> (Option<&'a Summary>, &'static str) {
    if let Some(current) = history.iter().find(|s| {
        finished(&s.status)
            && key.is_some()
            && s.configuration_key.as_deref() == key
            && s.pack_hash == pack
    }) {
        return (
            Some(current),
            if current.status == "completed_unavailable" {
                "Current — metrics unavailable"
            } else {
                "Current"
            },
        );
    }
    if let Some(old) = history.iter().find(|s| finished(&s.status)) {
        return (
            Some(old),
            if key.is_some() {
                "Configuration changed"
            } else {
                "Historical — identity unverified"
            },
        );
    }
    // With no completed result, show one attempt's evidence explicitly as
    // incomplete/failed; it is never Current and has no Last benchmark date.
    match history.first() {
        Some(attempt) => (
            Some(attempt),
            if attempt.status == "failed" {
                "Failed"
            } else {
                "Incomplete"
            },
        ),
        None => (None, "Never benchmarked"),
    }
}

pub fn compare(left: &Run, right: &Run) -> Value {
    let a = left.summary();
    let b = right.summary();
    let comparable = left.pack_hash == right.pack_hash && left.methodology == right.methodology;
    let changed=left.evidence.iter().filter_map(|e|right.evidence.iter().find(|r|r.id==e.id).filter(|r|r.score!=e.score||r.status!=e.status).map(|r|json!({"id":e.id,"left":e.status,"right":r.status,"left_score":e.score,"right_score":r.score}))).collect::<Vec<_>>();
    json!({"left":a,"right":b,"same_methods":comparable,"same_configuration":left.configuration_key.is_some()&&left.configuration_key==right.configuration_key,
        "changed_settings":value_changes(&json!(left.provenance.as_ref().map(|p|&p.settings)),&json!(right.provenance.as_ref().map(|p|&p.settings))),
        "changed_runtime":value_changes(&json!(left.provenance.as_ref().map(|p|&p.runtime)),&json!(right.provenance.as_ref().map(|p|&p.runtime))),
        "changed_hardware":value_changes(&left.environment["host"],&right.environment["host"]),
        "conditions_verified":false,"warning":"Small task pack: one changed answer is not a decisive winner. Cache and concurrent hardware conditions are unverified.",
        "changed_tasks":changed,"settings":{"left":left.provenance.as_ref().map(|p|&p.settings),"right":right.provenance.as_ref().map(|p|&p.settings)},
        "runtime":{"left":left.provenance.as_ref().map(|p|&p.runtime),"right":right.provenance.as_ref().map(|p|&p.runtime)},
        "hardware":{"left":left.environment,"right":right.environment},
        "intelligence_delta":a.intelligence.zip(b.intelligence).map(|(a,b)|b-a),"agentic_delta":a.agentic.zip(b.agentic).map(|(a,b)|b-a),
        "latency_delta_ms":a.speed["combined"]["first_visible_ms"]["median"].as_f64().zip(b.speed["combined"]["first_visible_ms"]["median"].as_f64()).map(|(a,b)|b-a)})
}

pub(crate) struct Response {
    pub text: String,
    pub calls: Vec<InferenceToolCall>,
    pub timing: Timing,
    pub usage: Option<InferenceUsage>,
    pub finish: String,
}

fn value_changes(left: &Value, right: &Value) -> Vec<Value> {
    fn visit(path: &str, a: &Value, b: &Value, out: &mut Vec<Value>) {
        if a == b || out.len() >= 128 {
            return;
        }
        if let (Some(a), Some(b)) = (a.as_object(), b.as_object()) {
            let keys = a
                .keys()
                .chain(b.keys())
                .collect::<std::collections::BTreeSet<_>>();
            for k in keys {
                if matches!(
                    k.as_str(),
                    "display_name"
                        | "observed_at_unix"
                        | "installed_at_unix"
                        | "detail"
                        | "observations"
                ) {
                    continue;
                }
                visit(
                    &format!("{path}/{k}"),
                    a.get(k).unwrap_or(&Value::Null),
                    b.get(k).unwrap_or(&Value::Null),
                    out,
                );
            }
        } else {
            out.push(json!({"field":path,"left":a,"right":b}));
        }
    }
    let mut changes = Vec::new();
    visit("", left, right, &mut changes);
    changes
}

/// Shared result presentation; scoring stays in Run::summary for every client.
pub fn inspection_text(value: &Value) -> String {
    let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap_or_default();
    let number = |v: &Value| {
        v.as_f64()
            .map(|n| format!("{n:.2}"))
            .unwrap_or_else(|| "unavailable".into())
    };
    if !value["summary"].is_object() {
        return pretty(value);
    }
    let s = &value["summary"];
    format!(
        "Norted Quick Intelligence: {} / 100\nAgentic: {} / 100 (single {} / 8; multi {} / 4)\nVisible delivery: {} Unicode chars/s after first chunk\nFirst visible latency: {} ms\nStatus: {} · Suite: {}\n\nSummary and category counts:\n{}\n\nImmutable evidence record:\n{}",
        number(&s["intelligence"]),
        number(&s["agentic"]),
        s["single_pass"],
        s["multi_pass"],
        number(&s["speed"]["combined"]["visible_delivery_characters_per_second"]["median"]),
        number(&s["speed"]["combined"]["first_visible_ms"]["median"]),
        s["status"].as_str().unwrap_or("unknown"),
        s["suite"].as_str().unwrap_or("unknown"),
        pretty(s),
        pretty(&value["record"])
    )
}
