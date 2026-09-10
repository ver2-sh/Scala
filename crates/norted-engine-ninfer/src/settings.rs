use std::ffi::OsString;

use norted_core::{
    ArtifactNativeIdentity, ModelArtifact, ResolvedSettings, SettingCategory,
    SettingDefaultPreview, SettingDefaultSource, SettingDefinition, SettingId, SettingKind,
    SettingScope, SettingValue, UnsignedIntegerOrChoiceValue,
};
use norted_engine::{EngineError, common_setting_definitions_for};

const MAX_NINFER_CLI_INTEGER: u64 = i32::MAX as u64;
const NINFER_COMMON_SETTINGS: &[&str] = &[
    "ninfer.context_length",
    "ninfer.parallel_requests",
    "ninfer.temperature",
    "ninfer.top_p",
    "ninfer.top_k",
    "ninfer.min_p",
    "ninfer.seed",
    "ninfer.presence_penalty",
    "ninfer.frequency_penalty",
    "ninfer.max_output_tokens",
    "ninfer.stop_strings",
    "ninfer.system_prompt",
    "ninfer.reasoning",
    "ninfer.reasoning_effort",
    "ninfer.reasoning_budget",
];

pub(crate) fn definitions() -> Vec<SettingDefinition> {
    let mut definitions = common_setting_definitions_for(crate::ENGINE_ID, NINFER_COMMON_SETTINGS);
    definitions.extend([
        definition(
            "ninfer.kv_dtype",
            "KV dtype",
            "NInfer KV cache storage type",
            SettingKind::Choice {
                choices: choices(&["bf16", "int8", "fp8", "nvfp4", "k8v4"]),
            },
            Some("exact runtime default"),
        ),
        definition(
            "ninfer.kv_capacity",
            "KV capacity",
            "Shared KV token capacity or NInfer automatic sizing",
            SettingKind::UnsignedIntegerOrChoice {
                minimum: Some(1),
                maximum: Some(MAX_NINFER_CLI_INTEGER),
                choices: choices(&["auto"]),
            },
            Some("derived by exact runtime"),
        ),
        definition(
            "ninfer.prefill_chunk",
            "Prefill chunk",
            "Prefill chunk size; must be a positive multiple of 128",
            SettingKind::UnsignedInteger {
                minimum: Some(128),
                maximum: Some(MAX_NINFER_CLI_INTEGER),
            },
            Some("exact runtime default"),
        ),
        definition(
            "ninfer.speculation",
            "Speculation",
            "Enable or disable speculative decoding",
            SettingKind::Toggle,
            Some("exact runtime default"),
        ),
        definition(
            "ninfer.speculative_backend",
            "Speculative backend",
            "Explicit NInfer speculative backend; unset preserves exact runtime behavior",
            SettingKind::Choice {
                choices: choices(&["mtp", "dflash"]),
            },
            Some("exact runtime default"),
        ),
        definition(
            "ninfer.draft_tokens",
            "Draft tokens",
            "Speculative draft-token window",
            SettingKind::UnsignedInteger {
                minimum: Some(1),
                maximum: Some(15),
            },
            Some("exact runtime default"),
        ),
        toggle(
            "ninfer.lm_head_draft",
            "LM-head draft",
            "Use NInfer's optimized proposal head with an explicit speculative backend",
        ),
        toggle(
            "ninfer.vision",
            "Vision residency",
            "Load NInfer's fixed Vision GPU allocations and enable supported image/video inputs",
        ),
        toggle(
            "ninfer.greedy",
            "Greedy sampling",
            "Force exact argmax sampling for every request",
        ),
        toggle(
            "ninfer.cuda_graph",
            "CUDA Graph",
            "Enable or disable NInfer CUDA Graph execution",
        ),
        toggle(
            "ninfer.prefix_reuse",
            "Prefix reuse",
            "Enable or disable NInfer prefix and continuation caching",
        ),
        toggle(
            "ninfer.thinking",
            "Thinking",
            "Enable or disable NInfer thinking",
        ),
        toggle(
            "ninfer.preserve_thinking",
            "Preserve thinking",
            "Retain closed-turn assistant reasoning in NInfer's private prompt history",
        ),
        unsigned(
            "ninfer.device_state_slots",
            "Device state slots",
            "Extra device checkpoint slots beyond active request lanes",
            0,
            Some(MAX_NINFER_CLI_INTEGER),
        ),
        unsigned(
            "ninfer.host_state_slots",
            "Host state slots",
            "Host checkpoint-state slots",
            0,
            Some(MAX_NINFER_CLI_INTEGER),
        ),
        unsigned(
            "ninfer.host_kv_mib",
            "Host KV budget",
            "Host KV checkpoint capacity in MiB",
            0,
            Some(u64::MAX >> 20),
        ),
        unsigned(
            "ninfer.max_private_continuations",
            "Private continuations",
            "Maximum private cached continuations",
            0,
            Some(MAX_NINFER_CLI_INTEGER),
        ),
        unsigned(
            "ninfer.max_shared_prefixes",
            "Shared prefixes",
            "Maximum shared cached prefixes",
            0,
            Some(MAX_NINFER_CLI_INTEGER),
        ),
        unsigned(
            "ninfer.max_long_anchors_per_continuation",
            "Long anchors",
            "Maximum long anchors per continuation",
            0,
            Some(MAX_NINFER_CLI_INTEGER),
        ),
        unsigned(
            "ninfer.max_pending_requests",
            "Pending requests",
            "Maximum queued requests beyond active concurrency",
            1,
            Some(MAX_NINFER_CLI_INTEGER),
        ),
        unsigned(
            "ninfer.pending_timeout_ms",
            "Pending timeout",
            "Maximum queue wait before a request is rejected, in milliseconds",
            1,
            Some(MAX_NINFER_CLI_INTEGER),
        ),
        unsigned(
            "ninfer.log_stats_interval_ms",
            "Stats interval",
            "Periodic throughput logging interval in milliseconds; zero disables it",
            0,
            Some(MAX_NINFER_CLI_INTEGER),
        ),
        unsigned(
            "ninfer.max_request_mib",
            "Maximum request size",
            "Private NInfer request-body limit in MiB; Norted's public limit remains authoritative",
            1,
            Some(u64::MAX >> 20),
        ),
        unsigned(
            "ninfer.media_cache_mib",
            "Media cache budget",
            "Retained preprocessed-media cache budget in MiB; zero disables retained reuse",
            0,
            Some(u64::MAX >> 20),
        ),
        unsigned(
            "ninfer.media_live_mib",
            "Live media budget",
            "Aggregate live BF16 media-payload budget in MiB",
            1,
            Some(u64::MAX >> 20),
        ),
        unsigned(
            "ninfer.media_preprocess_threads",
            "Media preprocessing threads",
            "Media preprocessing workers; zero selects NInfer automatic sizing",
            0,
            Some(64),
        ),
        unsigned(
            "ninfer.response_store_max_records",
            "Response-store records",
            "Maximum process-local Responses records retained by NInfer's private API",
            1,
            Some(MAX_NINFER_CLI_INTEGER),
        ),
        unsigned(
            "ninfer.response_store_max_mib",
            "Response-store budget",
            "Process-local Responses store size budget in MiB",
            1,
            Some(u64::MAX >> 20),
        ),
        toggle(
            "ninfer.cors",
            "CORS",
            "Emit NInfer's permissive CORS headers on its private loopback backend",
        ),
        definition(
            "ninfer.log_level",
            "Log level",
            "NInfer process log verbosity",
            SettingKind::Choice {
                choices: choices(&[
                    "trace", "debug", "info", "warning", "error", "critical", "off",
                ]),
            },
            Some("exact runtime default"),
        ),
        definition(
            "ninfer.context_cost_presets",
            "Context-cost presets",
            "Optional NInfer context-cost preset file",
            SettingKind::Path,
            Some("exact runtime default"),
        ),
    ]);
    definitions
}

pub(crate) fn apply_reviewed_runtime_defaults(
    definitions: &mut [SettingDefinition],
    settings: Option<&ResolvedSettings>,
) {
    for (id, value) in [
        ("ninfer.context_length", "8192"),
        ("ninfer.parallel_requests", "1"),
        ("ninfer.temperature", "auto"),
        ("ninfer.top_p", "auto"),
        ("ninfer.top_k", "auto"),
        ("ninfer.min_p", "auto"),
        ("ninfer.presence_penalty", "auto"),
        ("ninfer.frequency_penalty", "auto"),
        ("ninfer.max_output_tokens", "8192"),
        ("ninfer.reasoning_effort", "auto"),
        ("ninfer.reasoning_budget", "unlimited"),
        ("ninfer.kv_dtype", "BF16"),
        ("ninfer.prefill_chunk", "1024"),
        ("ninfer.speculation", "disabled"),
        ("ninfer.speculative_backend", "off"),
        ("ninfer.draft_tokens", "0"),
        ("ninfer.lm_head_draft", "disabled"),
        ("ninfer.vision", "disabled"),
        ("ninfer.greedy", "disabled"),
        ("ninfer.cuda_graph", "enabled"),
        ("ninfer.prefix_reuse", "enabled"),
        ("ninfer.thinking", "enabled"),
        ("ninfer.preserve_thinking", "disabled"),
        ("ninfer.max_pending_requests", "16"),
        ("ninfer.pending_timeout_ms", "30000 ms"),
        ("ninfer.log_stats_interval_ms", "5000 ms"),
        ("ninfer.max_request_mib", "384 MiB"),
        ("ninfer.media_cache_mib", "1024 MiB"),
        ("ninfer.media_live_mib", "2048 MiB"),
        ("ninfer.response_store_max_records", "1024"),
        ("ninfer.response_store_max_mib", "256 MiB"),
        ("ninfer.cors", "disabled"),
        ("ninfer.log_level", "info"),
        ("ninfer.context_cost_presets", "None"),
    ] {
        set_default(
            definitions,
            id,
            SettingDefaultPreview::new(value, SettingDefaultSource::Runtime),
        );
    }
    set_default(
        definitions,
        "ninfer.seed",
        SettingDefaultPreview::new("random", SettingDefaultSource::StartupDynamic)
            .with_detail("NInfer creates a fresh random seed for each request when none is set"),
    );

    let thinking = reviewed_effective_thinking(settings);
    let thinking_is_derived = settings.is_some_and(|settings| {
        settings.value("ninfer.thinking").is_some()
            || settings.value("ninfer.reasoning_effort").is_some()
    });
    set_default(
        definitions,
        "ninfer.reasoning",
        SettingDefaultPreview::new(
            if thinking { "on" } else { "off" },
            if thinking_is_derived {
                SettingDefaultSource::Derived
            } else {
                SettingDefaultSource::Runtime
            },
        )
        .with_detail(if thinking_is_derived {
            "Inherited from the effective NInfer process thinking mode or reasoning-effort request default"
        } else {
            "The reviewed NInfer server enables thinking by default"
        }),
    );

    let concurrency = settings
        .and_then(|settings| settings.value("ninfer.parallel_requests"))
        .and_then(|value| match value {
            SettingValue::UnsignedInteger(value) => Some(*value),
            _ => None,
        })
        .unwrap_or(1);
    let context = settings
        .and_then(|settings| settings.value("ninfer.context_length"))
        .and_then(|value| match value {
            SettingValue::UnsignedInteger(value) => Some(*value),
            _ => None,
        })
        .unwrap_or(8192);
    let prefix_reuse = settings
        .and_then(|settings| settings.value("ninfer.prefix_reuse"))
        .and_then(|value| match value {
            SettingValue::Toggle(value) => Some(*value),
            _ => None,
        })
        .unwrap_or(true);
    set_default(
        definitions,
        "ninfer.kv_capacity",
        SettingDefaultPreview::new(context.to_string(), SettingDefaultSource::Derived)
            .with_detail(
                "When omitted, the reviewed runtime sets explicit KV capacity to effective context length",
            ),
    );
    let cache_defaults = if prefix_reuse {
        [
            (
                "ninfer.device_state_slots",
                concurrency,
                "With prefix reuse enabled, the reviewed runtime defaults extra device checkpoint slots to effective concurrency",
            ),
            (
                "ninfer.host_state_slots",
                8,
                "With prefix reuse enabled, the reviewed runtime defaults Host state capacity to 8 slots",
            ),
            (
                "ninfer.host_kv_mib",
                8192,
                "With prefix reuse enabled, the reviewed runtime defaults Host KV capacity to 8192 MiB",
            ),
            (
                "ninfer.max_private_continuations",
                concurrency.saturating_mul(2),
                "With prefix reuse enabled, the reviewed runtime defaults private continuations to twice effective concurrency",
            ),
            (
                "ninfer.max_shared_prefixes",
                concurrency.max(4),
                "With prefix reuse enabled, the reviewed runtime defaults shared prefixes to max(effective concurrency, 4)",
            ),
            (
                "ninfer.max_long_anchors_per_continuation",
                2,
                "With prefix reuse enabled, the reviewed runtime defaults long anchors per continuation to 2",
            ),
        ]
    } else {
        [
            (
                "ninfer.device_state_slots",
                0,
                "Disabling prefix reuse makes the reviewed runtime use no extra device checkpoint slots",
            ),
            (
                "ninfer.host_state_slots",
                0,
                "Disabling prefix reuse makes the reviewed runtime use no Host state slots",
            ),
            (
                "ninfer.host_kv_mib",
                0,
                "Disabling prefix reuse makes the reviewed runtime use no Host KV capacity",
            ),
            (
                "ninfer.max_private_continuations",
                concurrency,
                "Disabling prefix reuse retains only the reviewed runtime's active root continuations",
            ),
            (
                "ninfer.max_shared_prefixes",
                0,
                "Disabling prefix reuse makes the reviewed runtime retain no shared prefixes",
            ),
            (
                "ninfer.max_long_anchors_per_continuation",
                0,
                "Disabling prefix reuse makes the reviewed runtime retain no long anchors",
            ),
        ]
    };
    for (id, value, formula) in cache_defaults {
        set_default(
            definitions,
            id,
            SettingDefaultPreview::new(
                if id == "ninfer.host_kv_mib" {
                    format!("{value} MiB")
                } else {
                    value.to_string()
                },
                SettingDefaultSource::Derived,
            )
            .with_detail(formula),
        );
    }
    set_default(
        definitions,
        "ninfer.media_preprocess_threads",
        SettingDefaultPreview::new("auto", SettingDefaultSource::Runtime).with_detail(
            "The reviewed runtime selects up to 16 media preprocessing workers from host concurrency",
        ),
    );
}

pub(crate) fn reviewed_request_thinking_override(
    settings: Option<&ResolvedSettings>,
) -> Option<bool> {
    settings
        .and_then(|settings| settings.value("ninfer.reasoning"))
        .and_then(|value| match value {
            SettingValue::Choice(value) if value == "on" => Some(true),
            SettingValue::Choice(value) if value == "off" => Some(false),
            _ => None,
        })
        .or_else(|| {
            settings
                .and_then(|settings| settings.value("ninfer.reasoning_effort"))
                .and_then(|value| match value {
                    SettingValue::Choice(value) if value == "none" => Some(false),
                    SettingValue::Choice(value)
                        if matches!(value.as_str(), "low" | "medium" | "xhigh") =>
                    {
                        Some(true)
                    }
                    _ => None,
                })
        })
}

fn reviewed_effective_thinking(settings: Option<&ResolvedSettings>) -> bool {
    reviewed_request_thinking_override(settings)
        .or_else(|| {
            settings
                .and_then(|settings| settings.value("ninfer.thinking"))
                .and_then(|value| match value {
                    SettingValue::Toggle(value) => Some(*value),
                    _ => None,
                })
        })
        .unwrap_or(true)
}

fn set_default(definitions: &mut [SettingDefinition], id: &str, preview: SettingDefaultPreview) {
    if let Some(definition) = definitions
        .iter_mut()
        .find(|definition| definition.id.as_str() == id)
    {
        definition.default_preview = Some(preview);
    }
}

pub(crate) fn apply_model_sampler_defaults(
    definitions: &mut [SettingDefinition],
    model: &ModelArtifact,
    settings: Option<&ResolvedSettings>,
) {
    let Some(ArtifactNativeIdentity::Ninfer(identity)) = model.native_identity.as_ref() else {
        return;
    };
    let thinking = reviewed_effective_thinking(settings);
    let (mut temperature, top_p, top_k, min_p, presence_penalty, frequency_penalty) =
        match (identity.model_id.as_str(), thinking) {
            ("qwen3.6-27b" | "qwen3.8-27b", true) => ("1.0", "0.95", "20", "0.0", "0.0", "0.0"),
            ("qwen3.6-27b" | "qwen3.8-27b", false) => ("0.7", "0.8", "20", "0.0", "1.5", "0.0"),
            ("qwen3.6-35b-a3b", true) => ("1.0", "0.95", "20", "0.0", "1.5", "0.0"),
            ("qwen3.6-35b-a3b", false) => ("0.7", "0.8", "20", "0.0", "1.5", "0.0"),
            _ => return,
        };
    let greedy = settings.is_some_and(|settings| {
        matches!(
            settings.value("ninfer.greedy"),
            Some(SettingValue::Toggle(true))
        )
    });
    if greedy {
        temperature = "0.0";
    }
    for (id, value) in [
        ("ninfer.temperature", temperature),
        ("ninfer.top_p", top_p),
        ("ninfer.top_k", top_k),
        ("ninfer.min_p", min_p),
        ("ninfer.presence_penalty", presence_penalty),
        ("ninfer.frequency_penalty", frequency_penalty),
    ] {
        set_default(
            definitions,
            id,
            SettingDefaultPreview::new(
                value,
                if id == "ninfer.temperature" && greedy {
                    SettingDefaultSource::Derived
                } else {
                    SettingDefaultSource::Model
                },
            )
            .with_detail(if id == "ninfer.temperature" && greedy {
                "The reviewed runtime's configured greedy mode forces exact argmax".to_owned()
            } else {
                format!(
                    "Reviewed NInfer preset for model {} in {} mode",
                    identity.model_id,
                    if thinking { "thinking" } else { "non-thinking" }
                )
            }),
        );
    }
}

pub(crate) fn apply_runtime_bounds(definitions: &mut [SettingDefinition]) {
    if let Some(context) = definitions
        .iter_mut()
        .find(|definition| definition.id.as_str() == "ninfer.context_length")
    {
        context.kind = SettingKind::UnsignedInteger {
            minimum: Some(1),
            maximum: Some(MAX_NINFER_CLI_INTEGER),
        };
    }
    if let Some(parallel) = definitions
        .iter_mut()
        .find(|definition| definition.id.as_str() == "ninfer.parallel_requests")
    {
        parallel.kind = SettingKind::UnsignedInteger {
            minimum: Some(1),
            maximum: Some(8),
        };
        parallel.description =
            "Maximum active same-model request concurrency in ninfer-serve".to_owned();
    }
    if let Some(top_k) = definitions
        .iter_mut()
        .find(|definition| definition.id.as_str() == "ninfer.top_k")
    {
        top_k.kind = SettingKind::UnsignedInteger {
            minimum: Some(0),
            maximum: Some(20),
        };
    }
    if let Some(budget) = definitions
        .iter_mut()
        .find(|definition| definition.id.as_str() == "ninfer.reasoning_budget")
    {
        budget.kind = SettingKind::Integer {
            minimum: Some(1),
            maximum: Some(i64::from(u32::MAX)),
        };
        budget.description =
            "Positive process-default thinking budget; explicit request effort/toggle controls remain separate"
                .to_owned();
    }
    if let Some(effort) = definitions
        .iter_mut()
        .find(|definition| definition.id.as_str() == "ninfer.reasoning_effort")
        && let SettingKind::Choice { choices } = &mut effort.kind
    {
        choices.retain(|choice| matches!(choice.as_str(), "none" | "low" | "medium" | "xhigh"));
    }
}

pub(crate) fn apply_model_capabilities(
    definitions: &mut [SettingDefinition],
    model: &ModelArtifact,
    settings: Option<&ResolvedSettings>,
    dflash_vision_supported: bool,
) {
    let vision_requested = settings.is_some_and(|settings| {
        matches!(
            settings.value("ninfer.vision"),
            Some(SettingValue::Toggle(true))
        )
    });
    if dflash_target(model) && (!vision_requested || dflash_vision_supported) {
        return;
    }
    if let Some(definition) = definitions
        .iter_mut()
        .find(|definition| definition.id.as_str() == "ninfer.speculative_backend")
        && let SettingKind::Choice { choices } = &mut definition.kind
    {
        choices.retain(|choice| choice != "dflash");
    }
}

pub(crate) fn validate_model_settings(
    settings: &ResolvedSettings,
    model: &ModelArtifact,
    dflash_vision_supported: bool,
) -> Result<(), String> {
    if choice_value(settings, "ninfer.speculative_backend").map_err(|error| error.to_string())?
        == Some("dflash")
    {
        if !dflash_target(model) {
            return Err(
                "NInfer DFlash is supported only for the exact qwen3.6-35b-a3b/groupwise-int target"
                    .to_owned(),
            );
        }
        if toggle_value(settings, "ninfer.vision").map_err(|error| error.to_string())? == Some(true)
            && !dflash_vision_supported
        {
            return Err("this exact NInfer runtime does not support DFlash with Vision".to_owned());
        }
    }
    Ok(())
}

fn dflash_target(model: &ModelArtifact) -> bool {
    matches!(
        model.native_identity.as_ref(),
        Some(ArtifactNativeIdentity::Ninfer(identity))
            if identity.model_id == "qwen3.6-35b-a3b"
                && identity.weights_id == "groupwise-int"
    )
}

fn choices(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn definition(
    id: &str,
    label: &str,
    description: &str,
    kind: SettingKind,
    _runtime_note: Option<&str>,
) -> SettingDefinition {
    SettingDefinition {
        id: SettingId::new(id).expect("static NInfer setting ID"),
        label: label.to_owned(),
        description: description.to_owned(),
        kind,
        scope: SettingScope::Runtime {
            engine_id: crate::ENGINE_ID.to_owned(),
        },
        category: category(id),
        supported: true,
        unsupported_reason: None,
        unit: None,
        default_preview: None,
    }
}

fn toggle(id: &str, label: &str, description: &str) -> SettingDefinition {
    definition(
        id,
        label,
        description,
        SettingKind::Toggle,
        Some("exact runtime default"),
    )
}

fn category(id: &str) -> SettingCategory {
    if matches!(id, "ninfer.thinking" | "ninfer.preserve_thinking") {
        SettingCategory::Reasoning
    } else if id.contains("specul") || id.contains("draft") {
        SettingCategory::Speculation
    } else if id.contains("kv_") || id == "ninfer.cuda_graph" || id.contains("media") {
        SettingCategory::KvMemory
    } else if id.contains("prefix") || id.contains("continuation") || id.contains("cache") {
        SettingCategory::Cache
    } else {
        SettingCategory::Advanced
    }
}

fn unsigned(
    id: &str,
    label: &str,
    description: &str,
    minimum: u64,
    maximum: Option<u64>,
) -> SettingDefinition {
    definition(
        id,
        label,
        description,
        SettingKind::UnsignedInteger {
            minimum: Some(minimum),
            maximum,
        },
        Some("exact runtime default"),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingExecutionPath {
    LaunchOption(&'static str),
    NortedRequestDefault,
    VirtualLaunchControl(&'static str),
    Unsupported,
}

pub(crate) fn execution_path_for_setting(id: &str) -> SettingExecutionPath {
    use SettingExecutionPath::{
        LaunchOption, NortedRequestDefault, Unsupported, VirtualLaunchControl,
    };

    match id {
        "ninfer.context_length" => LaunchOption("--max-context"),
        "ninfer.parallel_requests" => LaunchOption("--max-concurrency"),
        "ninfer.temperature" => LaunchOption("--temperature"),
        "ninfer.top_p" => LaunchOption("--top-p"),
        "ninfer.top_k" => LaunchOption("--top-k"),
        "ninfer.min_p" => LaunchOption("--min-p"),
        "ninfer.seed" => LaunchOption("--seed"),
        "ninfer.presence_penalty" => LaunchOption("--presence-penalty"),
        "ninfer.frequency_penalty" => LaunchOption("--frequency-penalty"),
        "ninfer.max_output_tokens" => LaunchOption("--default-max-tokens"),
        "ninfer.reasoning_budget" => LaunchOption("--default-thinking-budget"),
        "ninfer.reasoning_effort"
        | "ninfer.reasoning"
        | "ninfer.stop_strings"
        | "ninfer.system_prompt" => NortedRequestDefault,
        "ninfer.kv_dtype" => LaunchOption("--kv-dtype"),
        "ninfer.kv_capacity" => LaunchOption("--kv-capacity"),
        "ninfer.prefill_chunk" => LaunchOption("--prefill-chunk"),
        "ninfer.speculation" => VirtualLaunchControl("--spec"),
        "ninfer.speculative_backend" => LaunchOption("--spec"),
        "ninfer.draft_tokens" => LaunchOption("--draft-tokens"),
        "ninfer.lm_head_draft" => LaunchOption("--lm-head-draft"),
        "ninfer.vision" => LaunchOption("--vision"),
        "ninfer.greedy" => LaunchOption("--greedy"),
        "ninfer.cuda_graph" => LaunchOption("--no-cuda-graph"),
        "ninfer.prefix_reuse" => LaunchOption("--no-prefix-reuse"),
        "ninfer.thinking" => LaunchOption("--no-thinking"),
        "ninfer.preserve_thinking" => LaunchOption("--preserve-thinking"),
        "ninfer.device_state_slots" => LaunchOption("--device-state-slots"),
        "ninfer.host_state_slots" => LaunchOption("--host-state-slots"),
        "ninfer.host_kv_mib" => LaunchOption("--host-kv-mib"),
        "ninfer.max_private_continuations" => LaunchOption("--max-private-continuations"),
        "ninfer.max_shared_prefixes" => LaunchOption("--max-shared-prefixes"),
        "ninfer.max_long_anchors_per_continuation" => {
            LaunchOption("--max-long-anchors-per-continuation")
        }
        "ninfer.max_pending_requests" => LaunchOption("--max-pending-requests"),
        "ninfer.pending_timeout_ms" => LaunchOption("--pending-timeout-ms"),
        "ninfer.log_stats_interval_ms" => LaunchOption("--log-stats-interval-ms"),
        "ninfer.max_request_mib" => LaunchOption("--max-request-mib"),
        "ninfer.media_cache_mib" => LaunchOption("--media-cache-mib"),
        "ninfer.media_live_mib" => LaunchOption("--media-live-mib"),
        "ninfer.media_preprocess_threads" => LaunchOption("--media-preprocess-threads"),
        "ninfer.response_store_max_records" => LaunchOption("--response-store-max-records"),
        "ninfer.response_store_max_mib" => LaunchOption("--response-store-max-mib"),
        "ninfer.cors" => LaunchOption("--cors"),
        "ninfer.log_level" => LaunchOption("--log-level"),
        "ninfer.context_cost_presets" => LaunchOption("--context-cost-presets"),
        _ => Unsupported,
    }
}

pub(crate) fn translate(
    settings: &ResolvedSettings,
    model: &ModelArtifact,
    native_arguments: &[String],
    dflash_vision_supported: bool,
) -> Result<Vec<OsString>, EngineError> {
    for id in settings.configured.keys() {
        let option = match execution_path_for_setting(id.as_str()) {
            SettingExecutionPath::LaunchOption(option) => option,
            SettingExecutionPath::NortedRequestDefault
            | SettingExecutionPath::VirtualLaunchControl(_) => continue,
            SettingExecutionPath::Unsupported => {
                return Err(EngineError::InvalidConfiguration(format!(
                    "setting `{id}` has no NInfer execution path"
                )));
            }
        };
        if let Some(argument) = find_native_option(native_arguments, option) {
            return Err(EngineError::InvalidConfiguration(format!(
                "structured setting `{id}` conflicts with native NInfer argument `{argument}`"
            )));
        }
    }

    if unsigned_value(settings, "ninfer.context_length")?
        .is_some_and(|value| value == 0 || value > MAX_NINFER_CLI_INTEGER)
    {
        return Err(EngineError::InvalidConfiguration(format!(
            "context_length must be in 1..={MAX_NINFER_CLI_INTEGER} for NInfer"
        )));
    }
    if unsigned_value(settings, "ninfer.parallel_requests")?
        .is_some_and(|value| !(1..=8).contains(&value))
    {
        return Err(EngineError::InvalidConfiguration(
            "parallel_requests must be in 1..=8 for NInfer".to_owned(),
        ));
    }

    let reasoning = choice_value(settings, "ninfer.reasoning")?;
    let effort = choice_value(settings, "ninfer.reasoning_effort")?;
    if matches!((reasoning, effort), (Some("on"), Some("none")))
        || matches!(
            (reasoning, effort),
            (Some("off"), Some("low" | "medium" | "xhigh"))
        )
    {
        return Err(EngineError::InvalidConfiguration(
            "reasoning and reasoning_effort configure conflicting NInfer thinking modes".to_owned(),
        ));
    }

    let speculation = toggle_value(settings, "ninfer.speculation")?;
    let speculative = choice_value(settings, "ninfer.speculative_backend")?;
    let draft_tokens = unsigned_value(settings, "ninfer.draft_tokens")?;
    if draft_tokens.is_some() && speculative.is_none() {
        return Err(EngineError::InvalidConfiguration(
            "ninfer.draft_tokens requires an explicit speculative backend".to_owned(),
        ));
    }
    let speculation_enabled = speculation.unwrap_or(speculative.is_some());
    if toggle_value(settings, "ninfer.greedy")? == Some(true)
        && [
            "ninfer.temperature",
            "ninfer.top_p",
            "ninfer.top_k",
            "ninfer.min_p",
            "ninfer.seed",
        ]
        .into_iter()
        .any(|id| settings.value(id).is_some())
    {
        return Err(EngineError::InvalidConfiguration(
            "ninfer.greedy cannot be combined with sampler defaults that it would override"
                .to_owned(),
        ));
    }
    if toggle_value(settings, "ninfer.vision")? == Some(true)
        && speculative == Some("dflash")
        && !dflash_vision_supported
    {
        return Err(EngineError::InvalidConfiguration(
            "this exact NInfer runtime does not support DFlash with Vision".to_owned(),
        ));
    }
    if toggle_value(settings, "ninfer.vision")? != Some(true)
        && [
            "ninfer.media_cache_mib",
            "ninfer.media_live_mib",
            "ninfer.media_preprocess_threads",
        ]
        .into_iter()
        .any(|id| settings.value(id).is_some())
    {
        return Err(EngineError::InvalidConfiguration(
            "NInfer media resource settings require `ninfer.vision=on`".to_owned(),
        ));
    }
    if let Some(SettingValue::Integer(value)) = settings.value("ninfer.reasoning_budget")
        && *value <= 0
    {
        return Err(EngineError::InvalidConfiguration(
            "NInfer default reasoning budget must be a positive token count".to_owned(),
        ));
    }
    if !speculation_enabled && toggle_value(settings, "ninfer.lm_head_draft")? == Some(true) {
        return Err(EngineError::InvalidConfiguration(
            "ninfer.lm_head_draft=on requires speculation and an explicit backend".to_owned(),
        ));
    }
    match (speculation_enabled, speculative) {
        (false, _) => {}
        (true, None) => {
            return Err(EngineError::InvalidConfiguration(
                "ninfer.speculation=on requires ninfer.speculative_backend".to_owned(),
            ));
        }
        (true, Some("mtp")) => {
            if draft_tokens.is_none_or(|value| !(1..=5).contains(&value)) {
                return Err(EngineError::InvalidConfiguration(
                    "NInfer MTP requires ninfer.draft_tokens in 1..=5".to_owned(),
                ));
            }
        }
        (true, Some("dflash")) => {
            if draft_tokens.is_none_or(|value| !(1..=15).contains(&value)) {
                return Err(EngineError::InvalidConfiguration(
                    "NInfer DFlash requires ninfer.draft_tokens in 1..=15".to_owned(),
                ));
            }
            if !dflash_target(model) {
                return Err(EngineError::InvalidConfiguration(
                    "NInfer DFlash is supported only for the exact qwen3.6-35b-a3b/groupwise-int target"
                        .to_owned(),
                ));
            }
        }
        (true, Some(value)) => {
            return Err(EngineError::InvalidConfiguration(format!(
                "unsupported NInfer speculative backend `{value}`"
            )));
        }
    }
    if let Some(prefill) = unsigned_value(settings, "ninfer.prefill_chunk")?
        && prefill % 128 != 0
    {
        return Err(EngineError::InvalidConfiguration(
            "ninfer.prefill_chunk must be a positive multiple of 128".to_owned(),
        ));
    }
    if let (Some(context), Some(capacity)) = (
        unsigned_value(settings, "ninfer.context_length")?,
        unsigned_integer_or_choice_value(settings, "ninfer.kv_capacity")?,
    ) && capacity < context
    {
        return Err(EngineError::InvalidConfiguration(
            "explicit ninfer.kv_capacity must be at least context_length".to_owned(),
        ));
    }
    if toggle_value(settings, "ninfer.prefix_reuse")? == Some(false)
        && settings.configured.keys().any(|id| {
            matches!(
                id.as_str(),
                "ninfer.device_state_slots"
                    | "ninfer.host_state_slots"
                    | "ninfer.host_kv_mib"
                    | "ninfer.max_private_continuations"
                    | "ninfer.max_shared_prefixes"
                    | "ninfer.max_long_anchors_per_continuation"
            )
        })
    {
        return Err(EngineError::InvalidConfiguration(
            "ninfer.prefix_reuse=off cannot be combined with NInfer cache-capacity settings"
                .to_owned(),
        ));
    }

    let mut arguments = Vec::new();
    for (id, resolved) in &settings.configured {
        let option = match execution_path_for_setting(id.as_str()) {
            SettingExecutionPath::LaunchOption(option) => option,
            SettingExecutionPath::NortedRequestDefault
            | SettingExecutionPath::VirtualLaunchControl(_) => continue,
            SettingExecutionPath::Unsupported => {
                return Err(EngineError::InvalidConfiguration(format!(
                    "setting `{id}` has no NInfer execution path"
                )));
            }
        };
        if !speculation_enabled
            && matches!(
                id.as_str(),
                "ninfer.speculative_backend" | "ninfer.draft_tokens" | "ninfer.lm_head_draft"
            )
        {
            continue;
        }
        match &resolved.value {
            SettingValue::FlagEnabled => arguments.push(OsString::from(option)),
            SettingValue::Toggle(value) => match id.as_str() {
                "ninfer.cuda_graph" | "ninfer.prefix_reuse" | "ninfer.thinking" if !value => {
                    arguments.push(OsString::from(option));
                }
                "ninfer.lm_head_draft" | "ninfer.preserve_thinking" if *value => {
                    arguments.push(OsString::from(option));
                }
                "ninfer.vision" | "ninfer.greedy" | "ninfer.cors" if *value => {
                    arguments.push(OsString::from(option));
                }
                "ninfer.cuda_graph"
                | "ninfer.prefix_reuse"
                | "ninfer.thinking"
                | "ninfer.lm_head_draft"
                | "ninfer.preserve_thinking"
                | "ninfer.vision"
                | "ninfer.greedy"
                | "ninfer.cors" => {}
                _ => {
                    return Err(EngineError::InvalidConfiguration(format!(
                        "setting `{id}` has an invalid toggle mapping for NInfer"
                    )));
                }
            },
            SettingValue::UnsignedInteger(value) => {
                push_value(&mut arguments, option, value);
            }
            SettingValue::Float(value) => push_value(&mut arguments, option, value),
            SettingValue::Integer(value) => push_value(&mut arguments, option, value),
            SettingValue::Choice(value) => push_value(&mut arguments, option, value),
            SettingValue::Path(value) => {
                arguments.push(OsString::from(option));
                arguments.push(value.as_os_str().to_owned());
            }
            SettingValue::UnsignedIntegerOrChoice(
                UnsignedIntegerOrChoiceValue::UnsignedInteger(value),
            ) => push_value(&mut arguments, option, value),
            SettingValue::UnsignedIntegerOrChoice(UnsignedIntegerOrChoiceValue::Choice(value))
                if id.as_str() == "ninfer.seed" && value == "random" => {}
            SettingValue::UnsignedIntegerOrChoice(UnsignedIntegerOrChoiceValue::Choice(value)) => {
                push_value(&mut arguments, option, value)
            }
            _ => {
                return Err(EngineError::InvalidConfiguration(format!(
                    "setting `{id}` has an invalid value for NInfer"
                )));
            }
        }
    }
    Ok(arguments)
}

fn push_value(arguments: &mut Vec<OsString>, option: &str, value: impl ToString) {
    arguments.push(OsString::from(option));
    arguments.push(OsString::from(value.to_string()));
}

fn find_native_option<'a>(arguments: &'a [String], option: &str) -> Option<&'a str> {
    arguments.iter().find_map(|argument| {
        (argument == option
            || argument
                .strip_prefix(option)
                .is_some_and(|suffix| suffix.starts_with('=')))
        .then_some(argument.as_str())
    })
}

fn unsigned_value(settings: &ResolvedSettings, id: &str) -> Result<Option<u64>, EngineError> {
    match settings.value(id) {
        Some(SettingValue::UnsignedInteger(value)) => Ok(Some(*value)),
        None => Ok(None),
        Some(_) => Err(EngineError::InvalidConfiguration(format!(
            "setting `{id}` must be an unsigned integer"
        ))),
    }
}

fn unsigned_integer_or_choice_value(
    settings: &ResolvedSettings,
    id: &str,
) -> Result<Option<u64>, EngineError> {
    match settings.value(id) {
        Some(SettingValue::UnsignedIntegerOrChoice(
            UnsignedIntegerOrChoiceValue::UnsignedInteger(value),
        )) => Ok(Some(*value)),
        Some(SettingValue::UnsignedIntegerOrChoice(UnsignedIntegerOrChoiceValue::Choice(_)))
        | None => Ok(None),
        Some(_) => Err(EngineError::InvalidConfiguration(format!(
            "setting `{id}` must be an unsigned integer or choice"
        ))),
    }
}

fn choice_value<'a>(
    settings: &'a ResolvedSettings,
    id: &str,
) -> Result<Option<&'a str>, EngineError> {
    match settings.value(id) {
        Some(SettingValue::Choice(value)) => Ok(Some(value)),
        None => Ok(None),
        Some(_) => Err(EngineError::InvalidConfiguration(format!(
            "setting `{id}` must be a choice"
        ))),
    }
}

fn toggle_value(settings: &ResolvedSettings, id: &str) -> Result<Option<bool>, EngineError> {
    match settings.value(id) {
        Some(SettingValue::Toggle(value)) => Ok(Some(*value)),
        None => Ok(None),
        Some(_) => Err(EngineError::InvalidConfiguration(format!(
            "setting `{id}` must be a toggle"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use norted_core::{
        ArtifactNativeIdentity, ModelId, NinferArtifactIdentity, ResolvedSetting, SettingId,
        SettingSource,
    };

    use super::*;

    fn model(model_id: &str) -> ModelArtifact {
        ModelArtifact {
            id: ModelId("fixture".to_owned()),
            display_name: "fixture".to_owned(),
            path: PathBuf::from("fixture.ninfer"),
            format: norted_core::ArtifactFormat::Ninfer,
            size_bytes: 1,
            created: 1,
            hash: None,
            architecture: None,
            context_length: None,
            provenance: None,
            native_identity: Some(ArtifactNativeIdentity::Ninfer(NinferArtifactIdentity {
                container_version: 2,
                model_id: model_id.to_owned(),
                weights_id: "groupwise-int".to_owned(),
            })),
            auxiliary_artifacts: Vec::new(),
            norted_package: None,
        }
    }

    fn settings(values: &[(&str, SettingValue)]) -> ResolvedSettings {
        ResolvedSettings {
            engine_id: crate::ENGINE_ID.to_owned(),
            model_profile_id: None,
            configured: values
                .iter()
                .map(|(id, value)| {
                    (
                        SettingId::new(*id).expect("ID"),
                        ResolvedSetting {
                            value: value.clone(),
                            source: SettingSource::Invocation,
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>(),
            effective: BTreeMap::new(),
        }
    }

    fn translated(values: &[(&str, SettingValue)], model_id: &str) -> Vec<String> {
        translate(&settings(values), &model(model_id), &[], true)
            .expect("setting translation")
            .into_iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn omitted_settings_emit_no_flags() {
        assert!(
            translate(&settings(&[]), &model("qwen3.6-27b"), &[], true)
                .expect("translation")
                .is_empty()
        );
    }

    #[test]
    fn common_and_typed_ninfer_settings_translate_exactly() {
        let arguments = translated(
            &[
                (
                    "ninfer.context_length",
                    SettingValue::UnsignedInteger(32_768),
                ),
                ("ninfer.parallel_requests", SettingValue::UnsignedInteger(4)),
                ("ninfer.kv_dtype", SettingValue::Choice("fp8".to_owned())),
                (
                    "ninfer.kv_capacity",
                    SettingValue::UnsignedIntegerOrChoice(UnsignedIntegerOrChoiceValue::Choice(
                        "auto".to_owned(),
                    )),
                ),
                ("ninfer.prefill_chunk", SettingValue::UnsignedInteger(256)),
                ("ninfer.cuda_graph", SettingValue::Toggle(false)),
            ],
            "qwen3.6-27b",
        );
        for expected in [
            ["--max-context", "32768"],
            ["--max-concurrency", "4"],
            ["--kv-dtype", "fp8"],
            ["--kv-capacity", "auto"],
            ["--prefill-chunk", "256"],
        ] {
            assert!(
                arguments
                    .windows(2)
                    .any(|window| window == expected.as_slice()),
                "missing translated pair {expected:?} in {arguments:?}"
            );
        }
        assert!(arguments.iter().any(|value| value == "--no-cuda-graph"));
    }

    #[test]
    fn common_definitions_match_the_explicit_ninfer_surface() {
        let definitions = definitions();
        for common in common_setting_definitions_for(crate::ENGINE_ID, NINFER_COMMON_SETTINGS) {
            assert_eq!(
                definitions
                    .iter()
                    .find(|definition| definition.id == common.id),
                Some(&common)
            );
        }
        for absent in [
            "ninfer.repeat_penalty",
            "ninfer.reasoning_budget_message",
            "ninfer.structured_output_schema",
            "ninfer.context_overflow",
        ] {
            assert!(
                definitions
                    .iter()
                    .all(|definition| definition.id.as_str() != absent),
                "{absent} must not belong to the NInfer settings surface"
            );
        }
    }

    #[test]
    fn parallel_range_and_kv_choices_are_typed_by_the_generic_schema() {
        let mut definitions = definitions();
        apply_runtime_bounds(&mut definitions);
        let parallel = definitions
            .iter()
            .find(|definition| definition.id.as_str() == "ninfer.parallel_requests")
            .expect("parallel definition");
        assert!(matches!(
            parallel.kind,
            SettingKind::UnsignedInteger {
                minimum: Some(1),
                maximum: Some(8)
            }
        ));
        let capacity = definitions
            .iter()
            .find(|definition| definition.id.as_str() == "ninfer.kv_capacity")
            .expect("KV capacity definition");
        assert!(matches!(
            &capacity.kind,
            SettingKind::UnsignedIntegerOrChoice { choices, .. }
                if choices == &["auto".to_owned()]
        ));
    }

    #[test]
    fn speculative_cross_validation_is_exact() {
        let missing_draft = settings(&[(
            "ninfer.speculative_backend",
            SettingValue::Choice("mtp".to_owned()),
        )]);
        assert!(translate(&missing_draft, &model("qwen3.6-27b"), &[], true).is_err());

        let mtp = settings(&[
            (
                "ninfer.speculative_backend",
                SettingValue::Choice("mtp".to_owned()),
            ),
            ("ninfer.draft_tokens", SettingValue::UnsignedInteger(6)),
        ]);
        assert!(translate(&mtp, &model("qwen3.6-27b"), &[], true).is_err());

        let dflash = settings(&[
            (
                "ninfer.speculative_backend",
                SettingValue::Choice("dflash".to_owned()),
            ),
            ("ninfer.draft_tokens", SettingValue::UnsignedInteger(7)),
        ]);
        assert!(translate(&dflash, &model("qwen3.6-27b"), &[], true).is_err());
        assert!(translate(&dflash, &model("qwen3.6-35b-a3b"), &[], true).is_ok());

        let missing_backend = settings(&[("ninfer.lm_head_draft", SettingValue::Toggle(true))]);
        assert!(translate(&missing_backend, &model("qwen3.6-27b"), &[], true).is_err());

        let valid_mtp = translated(
            &[
                (
                    "ninfer.speculative_backend",
                    SettingValue::Choice("mtp".to_owned()),
                ),
                ("ninfer.draft_tokens", SettingValue::UnsignedInteger(5)),
                ("ninfer.lm_head_draft", SettingValue::Toggle(true)),
            ],
            "qwen3.6-27b",
        );
        assert!(valid_mtp.windows(2).any(|pair| pair == ["--spec", "mtp"]));
        assert!(
            valid_mtp
                .windows(2)
                .any(|pair| pair == ["--draft-tokens", "5"])
        );
        assert!(valid_mtp.iter().any(|value| value == "--lm-head-draft"));
    }

    #[test]
    fn model_capabilities_filter_and_reject_dflash_for_non_target_models() {
        let non_target = model("qwen3.6-27b");
        let mut non_target_definitions = definitions();
        apply_model_capabilities(&mut non_target_definitions, &non_target, None, true);
        let choices = non_target_definitions
            .iter()
            .find(|definition| definition.id.as_str() == "ninfer.speculative_backend")
            .and_then(|definition| match &definition.kind {
                SettingKind::Choice { choices } => Some(choices.as_slice()),
                _ => None,
            })
            .expect("speculative backend choices");
        assert_eq!(choices, ["mtp"]);

        let dflash = settings(&[(
            "ninfer.speculative_backend",
            SettingValue::Choice("dflash".to_owned()),
        )]);
        assert!(
            validate_model_settings(&dflash, &non_target, true)
                .unwrap_err()
                .contains("exact qwen3.6-35b-a3b/groupwise-int")
        );

        let target = model("qwen3.6-35b-a3b");
        let mut target_definitions = definitions();
        apply_model_capabilities(&mut target_definitions, &target, None, true);
        let target_choices = target_definitions
            .iter()
            .find(|definition| definition.id.as_str() == "ninfer.speculative_backend")
            .and_then(|definition| match &definition.kind {
                SettingKind::Choice { choices } => Some(choices.as_slice()),
                _ => None,
            })
            .expect("target speculative backend choices");
        assert_eq!(target_choices, ["mtp", "dflash"]);
        assert!(validate_model_settings(&dflash, &target, true).is_ok());

        let dflash_vision = settings(&[
            (
                "ninfer.speculative_backend",
                SettingValue::Choice("dflash".to_owned()),
            ),
            ("ninfer.draft_tokens", SettingValue::UnsignedInteger(7)),
            ("ninfer.vision", SettingValue::Toggle(true)),
        ]);
        assert!(translate(&dflash_vision, &target, &[], true).is_ok());
        assert!(translate(&dflash_vision, &target, &[], false).is_err());
    }

    #[test]
    fn explicit_kv_capacity_must_cover_explicit_context() {
        let undersized = settings(&[
            ("ninfer.context_length", SettingValue::UnsignedInteger(8192)),
            (
                "ninfer.kv_capacity",
                SettingValue::UnsignedIntegerOrChoice(
                    UnsignedIntegerOrChoiceValue::UnsignedInteger(4096),
                ),
            ),
        ]);
        assert!(translate(&undersized, &model("qwen3.6-27b"), &[], true).is_err());

        let automatic = settings(&[
            ("ninfer.context_length", SettingValue::UnsignedInteger(8192)),
            (
                "ninfer.kv_capacity",
                SettingValue::UnsignedIntegerOrChoice(UnsignedIntegerOrChoiceValue::Choice(
                    "auto".to_owned(),
                )),
            ),
        ]);
        assert!(translate(&automatic, &model("qwen3.6-27b"), &[], true).is_ok());
    }

    #[test]
    fn structured_settings_conflict_with_both_native_option_forms() {
        let structured = settings(&[(
            "ninfer.speculative_backend",
            SettingValue::Choice("mtp".to_owned()),
        )]);
        for native in [
            vec!["--spec".to_owned(), "mtp".to_owned()],
            vec!["--spec=mtp".to_owned()],
        ] {
            assert!(translate(&structured, &model("qwen3.6-27b"), &native, true).is_err());
        }
    }
}
