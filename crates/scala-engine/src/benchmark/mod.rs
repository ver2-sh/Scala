//! Server-owned, local per-profile benchmark evidence and shared methodology.
pub(crate) mod coding;
pub mod plan;
pub(crate) mod retrieval;
mod scorecard;
mod storage;
pub use plan::BenchmarkPlan;
pub mod suite;
use crate::{InferenceToolCall, InferenceUsage};
use scala_core::{ModelProfile, ModelProfileId, RuntimeProvenance};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Instant};
pub(crate) use storage::Store;
use tokio::sync::{Mutex, RwLock, watch};

pub const CONTROL_BENCHMARK_PATH: &str = "/control/v1/benchmarks";
tokio::task_local! { pub(crate) static EXECUTOR: (); }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum BenchmarkRequest {
    Start { profile_id: ModelProfileId },
    Plan { profile_id: ModelProfileId },
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
    pub plan: BenchmarkPlan,
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
/// Versioned comparison identity. Full configuration matching remains a separate key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkSignature {
    pub version: u32,
    pub methodology: Value,
    pub quality_key: String,
    pub performance_conditions: Value,
    pub performance_key: Option<String>,
    pub performance_identity_reason: Option<String>,
    pub model: Value,
    pub model_metadata: Value,
    pub profile_id: ModelProfileId,
    pub profile_hash: String,
    pub file_observations: Value,
    pub accelerator_binding: Option<scala_core::AcceleratorBinding>,
    pub runtime: Value,
    pub effective_settings: Value,
    pub observed_settings: Value,
    pub server_version: String,
    pub source_revision: Option<String>,
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
    #[serde(default)]
    pub scorecard: Value,
    #[serde(default)]
    pub signature: Value,
    pub intelligence: Option<f64>,
    pub categories: BTreeMap<String, Value>,
    pub agentic: Option<f64>,
    #[serde(default)]
    pub coding: Option<f64>,
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
/// Shared bundled material; run manifests use `BenchmarkPlan::manifest`.
pub fn manifest() -> Value {
    json!({"suite":suite::SUITE,"method":suite::METHOD,"policy":suite::POLICY,
        "retrieval":retrieval::manifest(),"coding":coding::manifest(),"intelligence":suite::questions(),"single_tools":suite::single_cases(),
        "agents":suite::AGENT_PROMPTS,"tools":suite::tools(),"fixture":(0..4).map(suite::Fixture::new).collect::<Vec<_>>(),
        "probes":suite::probes().into_iter().map(|(id,input)|json!({"id":id,"utf8_bytes":input.len(),"unicode_characters":input.chars().count(),"input":input,"seconds":suite::PROBE_SECONDS,"max_output_tokens":suite::PROBE_TOKENS})).collect::<Vec<_>>(),
        "warmup":{"input":"Reply with the word ready.","seconds":10,"max_output_tokens":32},
        "agent_limits":{"single_seconds":7,"single_turns":1,"multi_seconds":21,"multi_turns":6,"multi_calls":8,"max_output_tokens":384},
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
    let timestamp = |v: Option<f64>| v.filter(|v| v.is_finite() && *v >= 0.0);
    let end = timestamp(t.completion_ms);
    let first = timestamp(t.first_text_ms).filter(|v| end.is_none_or(|end| *v <= end));
    let visible = timestamp(t.first_visible_ms)
        .filter(|v| first.is_none_or(|first| *v >= first) && end.is_none_or(|end| *v <= end));
    let last = timestamp(t.last_text_ms)
        .filter(|v| first.is_some_and(|first| *v >= first) && end.is_none_or(|end| *v <= end));
    let refusal = ["i cannot", "i can’t", "i can't", "i'm sorry"]
        .iter()
        .any(|prefix| e.response.trim().to_lowercase().starts_with(prefix));
    let delivery_reason = if e.status != "passed" {
        Some(e.explanation.as_str())
    } else if refusal {
        Some("refusal, not a usable workload response")
    } else if t.visible_characters < 400 {
        Some("insufficient output: fewer than 400 Unicode characters")
    } else if t.first_chunk_characters == t.visible_characters {
        Some("response delivered in one chunk")
    } else if t.first_chunk_characters > t.visible_characters
        || first
            .zip(last)
            .is_none_or(|(first, last)| last - first < 50.0)
        || end.is_none()
        || t.visible_characters
            .saturating_sub(t.first_chunk_characters)
            < 128
    {
        Some(
            "insufficient post-first-chunk delivery: need ordered timestamps, 128 characters and 50 ms",
        )
    } else {
        None
    };
    let delivery = delivery_reason.is_none().then(|| {
        (t.visible_characters - t.first_chunk_characters) as f64 * 1000.0
            / (last.unwrap() - first.unwrap())
    });
    // Completion counters cover the entire request, including hidden reasoning.
    // A terminal length limit still has an observed whole-request rate; its
    // unsuccessful outcome is retained and excluded from complete comparisons.
    let native_reason = if end.is_none_or(|end| end < 50.0) {
        Some("whole-request completion timing unavailable or below 50 ms")
    } else if e.usage.is_none() {
        Some("native token usage unavailable")
    } else if e.usage.as_ref().is_some_and(|u| {
        u.output_tokens == 0
            || Some(u.total_tokens) != u.input_tokens.checked_add(u.output_tokens)
            || u.reasoning_output_tokens
                .is_some_and(|r| r > u.output_tokens)
    }) {
        Some("native token usage inconsistent")
    } else {
        None
    };
    let native = native_reason
        .is_none()
        .then(|| e.usage.as_ref().unwrap().output_tokens as f64 * 1000.0 / end.unwrap());
    let text_reason = if end.is_none_or(|end| end < 50.0) {
        Some("whole-request completion timing unavailable or below 50 ms")
    } else if t.visible_characters == 0 || first.is_none() || last.is_none() {
        Some("no measured text output")
    } else {
        None
    };
    let text_rate = text_reason
        .is_none()
        .then(|| t.visible_characters as f64 * 1000.0 / end.unwrap());
    let latency_reason =
        visible
            .is_none()
            .then_some(if e.status == "timeout" && t.first_visible_ms.is_none() {
                "no output before deadline"
            } else {
                "no valid first-visible timestamp"
            });
    let prefill = e
        .usage
        .as_ref()
        .and_then(InferenceUsage::prefill_tokens_per_second);
    let prefill_reason = prefill.is_none().then_some(
        if e.usage.as_ref().is_some_and(|u| {
            u.prompt_processing_tokens.is_some() || u.prompt_processing_ms.is_some()
        }) {
            "native prefill timing or token accounting invalid or incomplete"
        } else {
            "native per-request prefill timing unavailable from this runtime"
        },
    );
    json!({"id":e.id,"outcome":e.status,"outcome_reason":e.explanation,
        "workload_complete":e.status == "passed" && !refusal,
        "visible_delivery_characters_per_second":delivery,
        "visible_end_to_end_characters_per_second":text_rate,
        "native_end_to_end_output_tokens_per_second":native,
        "native_prefill_tokens_per_second":prefill,
        "prompt_processing_tokens":e.usage.as_ref().and_then(|u|u.prompt_processing_tokens),
        "prompt_processing_ms":e.usage.as_ref().and_then(|u|u.prompt_processing_ms),
        "cached_input_tokens":e.usage.as_ref().and_then(|u|u.cached_input_tokens),
        "first_visible_ms":visible,"first_text_ms":first,"completion_ms":end,
        "reasons":{
            "visible_delivery_characters_per_second":delivery_reason,
            "visible_end_to_end_characters_per_second":text_reason,
            "native_end_to_end_output_tokens_per_second":native_reason,
            "native_prefill_tokens_per_second":prefill_reason,
            "first_visible_ms":latency_reason,
            "first_text_ms":first.is_none().then_some("no valid first-text timestamp"),
            "completion_ms":end.is_none().then_some("no valid completion timestamp")},
        "native_decode_tokens_per_second":null,"first_answer_ms":null})
}

impl Run {
    pub fn plan(&self) -> Option<BenchmarkPlan> {
        serde_json::from_value(self.manifest["plan"].clone()).ok()
    }
    pub fn summary(&self) -> Summary {
        let plan = self.plan();
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
            let required = plan.as_ref().map_or(6, |p| {
                p.questions
                    .iter()
                    .filter(|id| id.starts_with(&format!("{name}-")))
                    .count()
            });
            let complete = required > 0 && scored.len() == required;
            if complete {
                means.push(scored.iter().sum::<f64>() / required as f64);
            }
            categories.insert(name.into(),json!({"passed":passed,"attempted":items.len(),"scored":scored.len(),"required":required,"score":complete.then_some(passed as f64/required.max(1) as f64*100.0),"wilson_95":scorecard::wilson(passed,scored.len())}));
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
        let tool_complete = single.iter().filter(|e| e.score.is_some()).count()
            == plan.as_ref().map_or(8, |p| p.single.len())
            && multi.iter().filter(|e| e.score.is_some()).count()
                == plan.as_ref().map_or(4, |p| p.agents.len())
            && plan.as_ref().is_none_or(|p| !p.single.is_empty());
        let agentic_unavailable = self
            .missing
            .iter()
            .find(|s| s.starts_with("Agentic unavailable:"))
            .cloned();
        let samples = self
            .evidence
            .iter()
            .filter(|e| e.category == "speed")
            .map(|e| {
                let mut sample = speed_sample(e);
                if self.profile.engine_id.as_str() == "llama.cpp"
                    && self.provenance.as_ref().is_none_or(|p| {
                        !p.normalized_settings
                            .get("native_prefill_contract")
                            .is_some_and(Value::is_string)
                    })
                {
                    sample["reasons"]["native_prefill_tokens_per_second"] =
                        json!("native prefill timing semantics are unverified for this runtime");
                }
                sample
            })
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
                "native_prefill_tokens_per_second",
                "visible_end_to_end_characters_per_second",
                "first_visible_ms",
                "first_text_ms",
                "completion_ms",
            ] {
                let values = selected
                    .iter()
                    .filter(|s| s["workload_complete"] == true)
                    .filter_map(|s| s[field].as_f64())
                    .collect::<Vec<_>>();
                let probe_ids = plan.as_ref().map(|p| p.probes.clone()).unwrap_or_else(|| {
                    vec![
                        "short-1".into(),
                        "short-2".into(),
                        "medium-1".into(),
                        "medium-2".into(),
                    ]
                });
                let required = probe_ids.iter().filter(|id| id.starts_with(prefix)).count() as u64;
                let mut stats = distribution(values);
                stats["required"] = json!(required);
                stats["partial_median"] = stats["median"].clone();
                stats["complete"] = json!(stats["count"].as_u64() == Some(required));
                stats["missing"] = json!(
                    probe_ids
                        .iter()
                        .map(String::as_str)
                        .filter(|id| id.starts_with(prefix))
                        .filter_map(|id| {
                            let sample = selected.iter().find(|s| s["id"] == id);
                            match sample {
                                Some(s)
                                    if s["workload_complete"] == true && s[field].is_number() =>
                                {
                                    None
                                }
                                Some(s) => {
                                    Some(json!({"id":id,"reason": if s["outcome"] != "passed" {
                                s["outcome_reason"].clone()
                            } else if s["workload_complete"] != true {
                                json!("refusal, not a usable workload response")
                            } else { s["reasons"][field].clone() }}))
                                }
                                None => Some(json!({"id":id,"reason":"probe not attempted"})),
                            }
                        })
                        .collect::<Vec<_>>()
                );
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
        groups.insert(
            "summary_method".into(),
            json!(if self.methodology == suite::METHOD {
                "independent-native-prefill/4"
            } else {
                "independent-observed-metrics/3"
            }),
        );
        let scorecard = scorecard::build(self, &categories);
        Summary {
            signature: self.environment["benchmark_signature"].clone(),
            scorecard: scorecard.clone(),
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
            intelligence: if self.methodology == suite::METHOD {
                scorecard["intelligence"]["score"].as_f64()
            } else {
                (means.len() == 4).then(|| 100.0 * means.iter().sum::<f64>() / 4.0)
            },
            coding: scorecard["coding"]["score"].as_f64(),
            categories,
            agentic: (tool_complete
                && (self.methodology != suite::METHOD || finished(&self.status)))
            .then_some(
                100.0
                    * (0.5 * single_pass as f64
                        / plan.as_ref().map_or(8, |p| p.single.len()).max(1) as f64
                        + 0.5 * multi_pass as f64
                            / plan.as_ref().map_or(4, |p| p.agents.len()).max(1) as f64),
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
            } else if attempt.status == "cancelled" {
                "Cancelled"
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
    let quality_comparable = left.suite == right.suite
        && left.methodology == right.methodology
        && left.pack_hash == right.pack_hash
        && a.signature["quality_key"].is_string()
        && a.signature["quality_key"] == b.signature["quality_key"];
    let performance_changes = value_changes(
        &a.signature["performance_conditions"],
        &b.signature["performance_conditions"],
    );
    let performance_comparable = quality_comparable
        && a.signature["performance_key"].is_string()
        && a.signature["performance_key"] == b.signature["performance_key"];
    let quality_changes = value_changes(&a.signature["methodology"], &b.signature["methodology"]);
    let comparable = quality_comparable;
    let quality_deltas = ["intelligence", "coding", "agentic", "retrieval"]
        .into_iter()
        .map(|k| {
            (
                k,
                if quality_comparable && finished(&a.status) && finished(&b.status) {
                    a.scorecard[k]["score"]
                        .as_f64()
                        .zip(b.scorecard[k]["score"].as_f64())
                        .map(|(a, b)| b - a)
                } else {
                    None
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let changed=left.evidence.iter().filter_map(|e|right.evidence.iter().find(|r|r.id==e.id).filter(|r|r.score!=e.score||r.status!=e.status).map(|r|json!({"id":e.id,"left":e.status,"right":r.status,"left_score":e.score,"right_score":r.score}))).collect::<Vec<_>>();
    json!({"left":a,"right":b,"same_methods":comparable,"quality_comparable":quality_comparable,"performance_comparable":performance_comparable,"quality_differences":quality_changes,"performance_differences":performance_changes,"quality_deltas":quality_deltas,"comparison_note":if quality_comparable {"Matching frozen quality method"} else {"Plan or methodology differ, or legacy signature unavailable"},"performance_note":if performance_comparable {"Equivalent recorded deployment conditions; concurrent workloads/cache remain unverified"} else {"Deployment observations only; no strict performance delta"},"same_configuration":left.configuration_key.is_some()&&left.configuration_key==right.configuration_key,
        "changed_settings":value_changes(&json!(left.provenance.as_ref().map(|p|&p.settings)),&json!(right.provenance.as_ref().map(|p|&p.settings))),
        "changed_runtime":value_changes(&json!(left.provenance.as_ref().map(|p|&p.runtime)),&json!(right.provenance.as_ref().map(|p|&p.runtime))),
        "changed_hardware":value_changes(&left.environment["host"],&right.environment["host"]),
        "conditions_verified":false,"warning":"Small task pack: one changed answer is not a decisive winner. Cache and concurrent hardware conditions are unverified.",
        "changed_tasks":changed,"settings":{"left":left.provenance.as_ref().map(|p|&p.settings),"right":right.provenance.as_ref().map(|p|&p.settings)},
        "runtime":{"left":left.provenance.as_ref().map(|p|&p.runtime),"right":right.provenance.as_ref().map(|p|&p.runtime)},
        "hardware":{"left":left.environment,"right":right.environment},
        "intelligence_delta":a.intelligence.zip(b.intelligence).filter(|_|quality_comparable).map(|(a,b)|b-a),"agentic_delta":a.agentic.zip(b.agentic).filter(|_|quality_comparable).map(|(a,b)|b-a),
        "coding_delta":a.coding.zip(b.coding).filter(|_|quality_comparable).map(|(a,b)|b-a),
        "prefill_tps_delta":a.speed["combined"]["native_prefill_tokens_per_second"]["median"].as_f64().zip(b.speed["combined"]["native_prefill_tokens_per_second"]["median"].as_f64()).filter(|_|performance_comparable&&finished(&a.status)&&finished(&b.status)).map(|(a,b)|b-a),
        "output_tps_delta":a.speed["combined"]["native_end_to_end_output_tokens_per_second"]["median"].as_f64().zip(b.speed["combined"]["native_end_to_end_output_tokens_per_second"]["median"].as_f64()).filter(|_|performance_comparable&&finished(&a.status)&&finished(&b.status)).map(|(a,b)|b-a),
        "latency_delta_ms":a.speed["combined"]["first_visible_ms"]["median"].as_f64().zip(b.speed["combined"]["first_visible_ms"]["median"].as_f64()).filter(|_|performance_comparable&&finished(&a.status)&&finished(&b.status)).map(|(a,b)|b-a)})
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

/// Shared TUI/CLI presentation of one method; never substitutes another rate.
pub fn performance_metric(speed: &Value, field: &str, units: &str) -> String {
    let combined = &speed["combined"][field];
    let count = combined["count"].as_u64().unwrap_or(0);
    let required = combined["required"].as_u64().unwrap_or(4);
    let value = combined["median"]
        .as_f64()
        .or_else(|| combined["partial_median"].as_f64());
    let coverage = |group: &str| {
        let stats = &speed[group][field];
        let value = stats["median"]
            .as_f64()
            .or_else(|| stats["partial_median"].as_f64());
        format!(
            "{} {}/{}{}",
            group,
            stats["count"].as_u64().unwrap_or(0),
            stats["required"].as_u64().unwrap_or(2),
            value.map(|v| format!("={v:.2}")).unwrap_or_default()
        )
    };
    let mut text = format!(
        "{}{} {units} ({count}/{required}; {}; {})",
        if count > 0 && count < required {
            "partial "
        } else {
            ""
        },
        value
            .map(|v| format!("{v:.2}"))
            .unwrap_or_else(|| "unavailable".into()),
        coverage("short"),
        coverage("medium")
    );
    if let Some(missing) = combined["missing"]
        .as_array()
        .and_then(|items| items.first())
    {
        text.push_str(&format!(
            "; {}: {}",
            missing["id"].as_str().unwrap_or("probe"),
            missing["reason"]
                .as_str()
                .unwrap_or("measurement unavailable")
        ));
    }
    text
}

pub fn performance_lines(speed: &Value) -> Vec<String> {
    [
        (
            "Delivery speed",
            "visible_delivery_characters_per_second",
            "chars/s after first chunk",
        ),
        (
            "Text end-to-end",
            "visible_end_to_end_characters_per_second",
            "chars/s whole request",
        ),
        (
            "Native end-to-end",
            "native_end_to_end_output_tokens_per_second",
            "output tokens/s whole request",
        ),
        ("Native prefill", "native_prefill_tokens_per_second", "processed prompt tokens/s"),
        ("First visible latency", "first_visible_ms", "ms"),
    ]
    .into_iter()
    .map(|(label, field, units)| format!("{label}: {}", performance_metric(speed, field, units)))
    .chain([
        "TPS definition: native output tokens divided by whole-request duration; not decode-only speed.".into(),
        "Latency definition: time from managed request start to first nonempty visible output.".into(),
    ])
    .collect()
}

pub fn benchmark_timestamp(value: &Value) -> String {
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

/// Headline values require a finished run and a full aggregate; partial evidence
/// stays in Details. This is presentation only, not a change to summary data.
pub fn scorecard_value(summary: &Value, field: &str) -> Option<f64> {
    if !finished(summary["status"].as_str().unwrap_or("")) {
        return None;
    }
    if summary["scorecard"][field].is_object() {
        return summary["scorecard"][field]["score"].as_f64();
    }
    if matches!(field, "intelligence" | "agentic" | "coding" | "retrieval") {
        summary[field].as_f64()
    } else {
        let combined = &summary["speed"]["combined"][field];
        // Headline TPS/latency require a complete selected-probe aggregate; partial
        // medians remain available in Details/verbose evidence only.
        if combined["complete"] != true {
            return None;
        }
        combined["median"].as_f64()
    }
}

pub const SCORECARD_METRICS: [(&str, &str, &str); 7] = [
    ("Intelligence ↑", "intelligence", " / 100"),
    ("Agentic ↑", "agentic", " / 100"),
    ("Coding ↑", "coding", " / 100"),
    ("Retrieval ↑", "retrieval", " / 100"),
    (
        "Output TPS ↑",
        "native_end_to_end_output_tokens_per_second",
        "",
    ),
    ("Prefill TPS ↑", "native_prefill_tokens_per_second", ""),
    ("Latency ↓", "first_visible_ms", " ms"),
];

pub fn scorecard_metric(summary: &Value, field: &str, unit: &str) -> String {
    scorecard_value(summary, field)
        .map(|n| {
            if field == "first_visible_ms" {
                format!("{n:.0}{unit}")
            } else {
                format!("{n:.1}{unit}")
            }
        })
        .unwrap_or_else(|| "—".into())
}

pub fn status_label(summary: &Value) -> String {
    match summary["status"].as_str().unwrap_or("") {
        "completed" | "completed_unavailable" => "Completed".into(),
        "cancelled" => "Cancelled".into(),
        "failed" => "Failed".into(),
        "incomplete" => "Incomplete".into(),
        "" => "Never benchmarked".into(),
        other => other.replace('_', " "),
    }
}

pub fn scorecard_lines(summary: &Value) -> Vec<String> {
    if summary.is_null() {
        return vec!["Never benchmarked".into()];
    }
    let mut lines = Vec::new();
    lines.extend(SCORECARD_METRICS.into_iter().map(|(label, field, unit)| {
        format!("{label:<16} {}", scorecard_metric(summary, field, unit))
    }));
    lines.push(format!("Status           {}", status_label(summary)));
    if let (Some(start), Some(end)) = (
        summary["started_unix_ms"].as_u64(),
        summary["ended_unix_ms"].as_u64(),
    ) {
        lines.push(format!(
            "Duration         {:.1} s",
            end.saturating_sub(start) as f64 / 1000.0
        ));
    }
    lines.push(format!(
        "Last benchmark   {}",
        benchmark_timestamp(if finished(summary["status"].as_str().unwrap_or("")) {
            &summary["ended_unix_ms"]
        } else {
            &Value::Null
        })
    ));
    lines
}

pub fn comparison_lines(value: &Value) -> Vec<String> {
    let mut lines = vec!["Comparison: baseline (left) -> selected (right)".into()];
    for (label, field, unit) in SCORECARD_METRICS {
        let allowed = if matches!(field, "intelligence" | "agentic" | "coding" | "retrieval") {
            value["quality_comparable"] == true
        } else {
            value["performance_comparable"] == true
        };
        let delta = match (
            scorecard_value(&value["left"], field),
            scorecard_value(&value["right"], field),
        ) {
            (Some(a), Some(b)) if allowed => format!(
                "{:+.1}{}",
                b - a,
                if unit == " / 100" { " points" } else { unit }
            ),
            _ => "—".into(),
        };
        lines.push(format!(
            "{label}: {} -> {} (delta {delta})",
            scorecard_metric(&value["left"], field, unit),
            scorecard_metric(&value["right"], field, unit)
        ));
    }
    for key in ["quality_differences", "performance_differences"] {
        if let Some(diffs) = value[key].as_array() {
            for d in diffs {
                lines.push(format!(
                    "{key}: {}: {} -> {}",
                    d["field"], d["left"], d["right"]
                ));
            }
        }
    }
    lines.push(format!(
        "Quality comparable: {}",
        value["quality_comparable"]
    ));
    lines.push(format!(
        "Performance comparable: {}",
        value["performance_comparable"]
    ));
    lines.push(
        value["comparison_note"]
            .as_str()
            .unwrap_or("Legacy methodology")
            .into(),
    );
    lines.push(
        value["performance_note"]
            .as_str()
            .unwrap_or("Deployment observations")
            .into(),
    );
    lines
}

/// Concise human output; JSON inspection also retains the complete raw record.
pub fn scorecard_text(value: &Value) -> String {
    if value["plan"].is_object() {
        return format!(
            "Profile: {}\nFrozen plan: {}\nPhase work: {} s; headroom: {} s; hard maximum: {} s\nPack: {}",
            value["profile_id"],
            serde_json::to_string_pretty(&value["plan"]).unwrap_or_default(),
            value["manifest"]["phase_work_seconds"],
            value["manifest"]["headroom_seconds"],
            value["plan"]["hard_seconds"],
            value["pack_hash"]
        );
    }
    if value["summary"].is_object() {
        let s = &value["summary"];
        return format!(
            "{} | Run: {}\n{}",
            s["display_name"].as_str().unwrap_or("Profile"),
            s["run_id"].as_str().unwrap_or("—"),
            scorecard_lines(s).join("\n")
        );
    }
    if value["left"].is_object() && value["right"].is_object() {
        let mut lines = comparison_lines(value);
        for (label, side) in [("Baseline", "left"), ("Selected", "right")] {
            lines.push(format!(
                "{label}: {} | Run: {}",
                value[side]["display_name"].as_str().unwrap_or("—"),
                value[side]["run_id"].as_str().unwrap_or("—")
            ));
        }
        if let Some(warning) = value["warning"].as_str() {
            lines.push(warning.into());
        }
        if value["same_methods"] != true {
            lines
                .push("WARNING: suite/methods differ; results are not directly comparable.".into());
        }
        return lines.join("\n");
    }
    if let Some(rows) = value["rows"]
        .as_array()
        .or_else(|| value["history"].as_array())
    {
        let mut lines = Vec::new();
        for row in rows {
            let s = if row["result"].is_object() {
                &row["result"]
            } else {
                row
            };
            lines.push(format!(
                "{} | Run: {}",
                row["display_name"].as_str().unwrap_or("Profile"),
                s["run_id"].as_str().unwrap_or("—")
            ));
            lines.extend(scorecard_lines(s));
            if row["latest_attempt"].is_object() && row["latest_attempt"]["run_id"] != s["run_id"] {
                lines.push(format!(
                    "Latest attempt: {}",
                    status_label(&row["latest_attempt"])
                ));
            }
            lines.push(String::new());
        }
        if value["active"].is_object() {
            let active = &value["active"];
            lines.push(format!(
                "Active benchmark: {} | {} | {}/{} tasks{}",
                active["profile_id"].as_str().unwrap_or("—"),
                active["phase"].as_str().unwrap_or("Running"),
                active["completed_tasks"],
                active["total_tasks"],
                if active["cancelling"] == true {
                    " | Cancelling"
                } else {
                    ""
                }
            ));
        }
        return if lines.is_empty() {
            "No benchmark results.".into()
        } else {
            lines.join("\n")
        };
    }
    let mut lines = vec![format!("Status: {}", status_label(value))];
    if let Some(run) = value["run_id"].as_str() {
        lines.push(format!("Run: {run}"));
    }
    if let Some(message) = value["message"].as_str() {
        lines.push(message.into());
    }
    lines.join("\n")
}

/// Explicit verbose presentation retains all diagnostic and raw information.
pub fn inspection_text(value: &Value) -> String {
    let mut lines = vec![scorecard_text(value)];
    if value["summary"].is_object() {
        lines.extend(performance_lines(&value["summary"]["speed"]));
    }
    lines.push(serde_json::to_string_pretty(value).unwrap_or_default());
    lines.join("\n")
}
