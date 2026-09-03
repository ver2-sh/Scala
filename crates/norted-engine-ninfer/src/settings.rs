use std::ffi::OsString;

use norted_core::{
    ArtifactNativeIdentity, ModelArtifact, ResolvedSettings, SettingCategory,
    SettingDefaultPreview, SettingDefaultSource, SettingDefinition, SettingId, SettingKind,
    SettingScope, SettingValue, UnsignedIntegerOrChoiceValue,
};
use norted_engine::{EngineError, common_setting_definitions};

const MAX_NINFER_CLI_INTEGER: u64 = i32::MAX as u64;

pub(crate) fn definitions() -> Vec<SettingDefinition> {
    let mut definitions = common_setting_definitions();
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
    ]);
    definitions
}

pub(crate) fn apply_reviewed_runtime_defaults(
    definitions: &mut [SettingDefinition],
    settings: Option<&ResolvedSettings>,
) {
    for (id, value) in [
        ("context_length", "runtime default: 8192"),
        ("parallel_requests", "runtime default: 1"),
        ("temperature", "model/thinking-mode default"),
        ("top_p", "model/thinking-mode default"),
        ("top_k", "model/thinking-mode default"),
        ("min_p", "model/thinking-mode default"),
        ("seed", "runtime-selected random seed"),
        ("presence_penalty", "model/thinking-mode default"),
        ("frequency_penalty", "model/thinking-mode default"),
        ("max_output_tokens", "runtime default: 8192"),
        ("ninfer.kv_dtype", "runtime default: BF16"),
        ("ninfer.kv_capacity", "runtime default: matches context"),
        ("ninfer.prefill_chunk", "runtime default: 1024"),
        ("ninfer.speculation", "runtime default: off"),
        ("ninfer.speculative_backend", "runtime default: off"),
        ("ninfer.draft_tokens", "runtime default: unused"),
        ("ninfer.lm_head_draft", "runtime default: off"),
        ("ninfer.vision", "runtime default: off"),
        ("ninfer.greedy", "runtime default: off"),
        ("ninfer.cuda_graph", "runtime default: enabled"),
        ("ninfer.prefix_reuse", "runtime default: enabled"),
        ("ninfer.thinking", "runtime default: enabled"),
        ("ninfer.preserve_thinking", "runtime default: disabled"),
        (
            "ninfer.device_state_slots",
            "runtime automatic: concurrency",
        ),
        ("ninfer.host_state_slots", "runtime default: 8"),
        ("ninfer.host_kv_mib", "runtime default: 8192 MiB"),
        (
            "ninfer.max_private_continuations",
            "runtime automatic: 2 × concurrency",
        ),
        (
            "ninfer.max_shared_prefixes",
            "runtime automatic: concurrency",
        ),
        (
            "ninfer.max_long_anchors_per_continuation",
            "runtime default: 2",
        ),
        ("ninfer.max_pending_requests", "runtime default: 16"),
        ("ninfer.pending_timeout_ms", "runtime default: 30000 ms"),
        ("ninfer.log_stats_interval_ms", "runtime default: 5000 ms"),
        ("ninfer.max_request_mib", "runtime default: 384 MiB"),
        ("ninfer.media_cache_mib", "runtime default: 1024 MiB"),
        ("ninfer.media_live_mib", "runtime default: 2048 MiB"),
        (
            "ninfer.media_preprocess_threads",
            "runtime-selected from host concurrency",
        ),
        ("ninfer.response_store_max_records", "runtime default: 1024"),
        ("ninfer.response_store_max_mib", "runtime default: 256 MiB"),
    ] {
        if let Some(definition) = definitions
            .iter_mut()
            .find(|definition| definition.id.as_str() == id)
        {
            definition.upstream_default = Some(value.to_owned());
        }
    }

    for (id, value) in [
        ("context_length", "8192"),
        ("parallel_requests", "1"),
        ("max_output_tokens", "8192"),
        ("reasoning", "on"),
        ("reasoning_budget", "None"),
        ("ninfer.kv_dtype", "BF16"),
        ("ninfer.prefill_chunk", "1024"),
        ("ninfer.speculation", "disabled"),
        ("ninfer.speculative_backend", "off"),
        ("ninfer.draft_tokens", "unused"),
        ("ninfer.lm_head_draft", "disabled"),
        ("ninfer.vision", "disabled"),
        ("ninfer.greedy", "disabled"),
        ("ninfer.cuda_graph", "enabled"),
        ("ninfer.prefix_reuse", "enabled"),
        ("ninfer.thinking", "enabled"),
        ("ninfer.preserve_thinking", "disabled"),
        ("ninfer.host_state_slots", "8"),
        ("ninfer.host_kv_mib", "8192 MiB"),
        ("ninfer.max_long_anchors_per_continuation", "2"),
        ("ninfer.max_pending_requests", "16"),
        ("ninfer.pending_timeout_ms", "30000 ms"),
        ("ninfer.log_stats_interval_ms", "5000 ms"),
        ("ninfer.max_request_mib", "384 MiB"),
        ("ninfer.media_cache_mib", "1024 MiB"),
        ("ninfer.media_live_mib", "2048 MiB"),
        ("ninfer.response_store_max_records", "1024"),
        ("ninfer.response_store_max_mib", "256 MiB"),
    ] {
        set_default(
            definitions,
            id,
            SettingDefaultPreview::new(value, SettingDefaultSource::Runtime),
        );
    }
    set_default(
        definitions,
        "seed",
        SettingDefaultPreview::new("random per request", SettingDefaultSource::StartupDynamic)
            .with_detail("NInfer creates a fresh random seed for each request when none is set"),
    );

    let concurrency = settings
        .and_then(|settings| settings.value("parallel_requests"))
        .and_then(|value| match value {
            SettingValue::UnsignedInteger(value) => Some(*value),
            _ => None,
        })
        .unwrap_or(1);
    let context = settings
        .and_then(|settings| settings.value("context_length"))
        .and_then(|value| match value {
            SettingValue::UnsignedInteger(value) => Some(*value),
            _ => None,
        })
        .unwrap_or(8192);
    for (id, value, formula) in [
        (
            "ninfer.kv_capacity",
            context,
            "The reviewed runtime defaults KV capacity to the effective context length",
        ),
        (
            "ninfer.device_state_slots",
            concurrency,
            "The reviewed runtime defaults extra device checkpoint slots to effective concurrency",
        ),
        (
            "ninfer.max_private_continuations",
            concurrency.saturating_mul(2),
            "The reviewed runtime defaults private continuations to twice effective concurrency",
        ),
        (
            "ninfer.max_shared_prefixes",
            concurrency,
            "The reviewed runtime defaults shared prefixes to effective concurrency",
        ),
    ] {
        set_default(
            definitions,
            id,
            SettingDefaultPreview::new(value.to_string(), SettingDefaultSource::Derived)
                .with_detail(formula),
        );
    }
    let media_threads = std::thread::available_parallelism()
        .map(|parallelism| parallelism.get())
        .unwrap_or(1)
        .min(16);
    set_default(
        definitions,
        "ninfer.media_preprocess_threads",
        SettingDefaultPreview::new(media_threads.to_string(), SettingDefaultSource::Derived)
            .with_detail(
                "Derived from detected host concurrency using the reviewed runtime's maximum of 16 workers",
            ),
    );
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
    let request_thinking = settings
        .and_then(|settings| settings.value("reasoning"))
        .and_then(|value| match value {
            SettingValue::Choice(value) if value == "on" => Some(true),
            SettingValue::Choice(value) if value == "off" => Some(false),
            SettingValue::Choice(value) if value == "auto" => None,
            _ => None,
        });
    let process_thinking = settings
        .and_then(|settings| settings.value("ninfer.thinking"))
        .and_then(|value| match value {
            SettingValue::Toggle(value) => Some(*value),
            _ => None,
        })
        .unwrap_or(true);
    let thinking = request_thinking.unwrap_or(process_thinking);
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
        ("temperature", temperature),
        ("top_p", top_p),
        ("top_k", top_k),
        ("min_p", min_p),
        ("presence_penalty", presence_penalty),
        ("frequency_penalty", frequency_penalty),
    ] {
        set_default(
            definitions,
            id,
            SettingDefaultPreview::new(
                value,
                if id == "temperature" && greedy {
                    SettingDefaultSource::Derived
                } else {
                    SettingDefaultSource::Model
                },
            )
            .with_detail(if id == "temperature" && greedy {
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
        .find(|definition| definition.id.as_str() == "context_length")
    {
        context.kind = SettingKind::UnsignedInteger {
            minimum: Some(1),
            maximum: Some(MAX_NINFER_CLI_INTEGER),
        };
    }
    if let Some(parallel) = definitions
        .iter_mut()
        .find(|definition| definition.id.as_str() == "parallel_requests")
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
        .find(|definition| definition.id.as_str() == "top_k")
    {
        top_k.kind = SettingKind::UnsignedInteger {
            minimum: Some(0),
            maximum: Some(20),
        };
    }
    if let Some(budget) = definitions
        .iter_mut()
        .find(|definition| definition.id.as_str() == "reasoning_budget")
    {
        budget.kind = SettingKind::Integer {
            minimum: Some(1),
            maximum: Some(i64::from(u32::MAX)),
        };
        budget.description =
            "Positive process-default thinking budget; explicit request effort/toggle controls remain separate"
                .to_owned();
    }
}

pub(crate) fn apply_model_capabilities(
    definitions: &mut [SettingDefinition],
    model: &ModelArtifact,
) {
    if dflash_target(model) {
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
) -> Result<(), String> {
    if choice_value(settings, "ninfer.speculative_backend").map_err(|error| error.to_string())?
        == Some("dflash")
        && !dflash_target(model)
    {
        return Err(
            "NInfer DFlash is supported only for the exact qwen3.6-35b-a3b/groupwise-int text target"
                .to_owned(),
        );
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
    upstream_default: Option<&str>,
) -> SettingDefinition {
    SettingDefinition {
        id: SettingId::new(id).expect("static NInfer setting ID"),
        label: label.to_owned(),
        description: description.to_owned(),
        kind,
        scope: SettingScope::Engine {
            engine_id: crate::ENGINE_ID.to_owned(),
        },
        category: category(id),
        supported: true,
        unsupported_reason: None,
        unit: None,
        upstream_default: upstream_default.map(str::to_owned),
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

pub(crate) fn option_for_setting(id: &str) -> &'static str {
    match id {
        "context_length" => "--max-context",
        "parallel_requests" => "--max-concurrency",
        "temperature" => "--temperature",
        "top_p" => "--top-p",
        "top_k" => "--top-k",
        "min_p" => "--min-p",
        "seed" => "--seed",
        "presence_penalty" => "--presence-penalty",
        "frequency_penalty" => "--frequency-penalty",
        "max_output_tokens" => "--default-max-tokens",
        "reasoning_budget" => "--default-thinking-budget",
        "reasoning_effort" | "reasoning" | "stop_strings" | "system_prompt" => "",
        "ninfer.kv_dtype" => "--kv-dtype",
        "ninfer.kv_capacity" => "--kv-capacity",
        "ninfer.prefill_chunk" => "--prefill-chunk",
        "ninfer.speculation" => "",
        "ninfer.speculative_backend" => "--spec",
        "ninfer.draft_tokens" => "--draft-tokens",
        "ninfer.lm_head_draft" => "--lm-head-draft",
        "ninfer.vision" => "--vision",
        "ninfer.greedy" => "--greedy",
        "ninfer.cuda_graph" => "--no-cuda-graph",
        "ninfer.prefix_reuse" => "--no-prefix-reuse",
        "ninfer.thinking" => "--no-thinking",
        "ninfer.preserve_thinking" => "--preserve-thinking",
        "ninfer.device_state_slots" => "--device-state-slots",
        "ninfer.host_state_slots" => "--host-state-slots",
        "ninfer.host_kv_mib" => "--host-kv-mib",
        "ninfer.max_private_continuations" => "--max-private-continuations",
        "ninfer.max_shared_prefixes" => "--max-shared-prefixes",
        "ninfer.max_long_anchors_per_continuation" => "--max-long-anchors-per-continuation",
        "ninfer.max_pending_requests" => "--max-pending-requests",
        "ninfer.pending_timeout_ms" => "--pending-timeout-ms",
        "ninfer.log_stats_interval_ms" => "--log-stats-interval-ms",
        "ninfer.max_request_mib" => "--max-request-mib",
        "ninfer.media_cache_mib" => "--media-cache-mib",
        "ninfer.media_live_mib" => "--media-live-mib",
        "ninfer.media_preprocess_threads" => "--media-preprocess-threads",
        "ninfer.response_store_max_records" => "--response-store-max-records",
        "ninfer.response_store_max_mib" => "--response-store-max-mib",
        _ => "",
    }
}

pub(crate) fn translate(
    settings: &ResolvedSettings,
    model: &ModelArtifact,
    native_arguments: &[String],
) -> Result<Vec<OsString>, EngineError> {
    for id in settings.effective.keys() {
        if matches!(
            id.as_str(),
            "ninfer.speculation"
                | "reasoning_effort"
                | "reasoning"
                | "stop_strings"
                | "system_prompt"
        ) {
            continue;
        }
        let option = option_for_setting(id.as_str());
        if let Some(argument) = find_native_option(native_arguments, option) {
            return Err(EngineError::InvalidConfiguration(format!(
                "structured setting `{id}` conflicts with native NInfer argument `{argument}`"
            )));
        }
    }

    if unsigned_value(settings, "context_length")?
        .is_some_and(|value| value == 0 || value > MAX_NINFER_CLI_INTEGER)
    {
        return Err(EngineError::InvalidConfiguration(format!(
            "context_length must be in 1..={MAX_NINFER_CLI_INTEGER} for NInfer"
        )));
    }
    if unsigned_value(settings, "parallel_requests")?.is_some_and(|value| !(1..=8).contains(&value))
    {
        return Err(EngineError::InvalidConfiguration(
            "parallel_requests must be in 1..=8 for NInfer".to_owned(),
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
        && ["temperature", "top_p", "top_k", "min_p", "seed"]
            .into_iter()
            .any(|id| settings.value(id).is_some())
    {
        return Err(EngineError::InvalidConfiguration(
            "ninfer.greedy cannot be combined with sampler defaults that it would override"
                .to_owned(),
        ));
    }
    if toggle_value(settings, "ninfer.vision")? == Some(true) && speculative == Some("dflash") {
        return Err(EngineError::InvalidConfiguration(
            "NInfer vision cannot be combined with the DFlash speculative backend".to_owned(),
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
    if let Some(SettingValue::Integer(value)) = settings.value("reasoning_budget")
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
                    "NInfer DFlash is supported only for the exact qwen3.6-35b-a3b/groupwise-int text target"
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
        unsigned_value(settings, "context_length")?,
        unsigned_integer_or_choice_value(settings, "ninfer.kv_capacity")?,
    ) && capacity < context
    {
        return Err(EngineError::InvalidConfiguration(
            "explicit ninfer.kv_capacity must be at least context_length".to_owned(),
        ));
    }
    if toggle_value(settings, "ninfer.prefix_reuse")? == Some(false)
        && settings.effective.keys().any(|id| {
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
    for (id, resolved) in &settings.effective {
        if matches!(
            id.as_str(),
            "ninfer.speculation"
                | "reasoning_effort"
                | "reasoning"
                | "stop_strings"
                | "system_prompt"
        ) {
            continue;
        }
        if !speculation_enabled
            && matches!(
                id.as_str(),
                "ninfer.speculative_backend" | "ninfer.draft_tokens" | "ninfer.lm_head_draft"
            )
        {
            continue;
        }
        let option = option_for_setting(id.as_str());
        match &resolved.value {
            SettingValue::FlagEnabled => arguments.push(OsString::from(option)),
            SettingValue::Toggle(value) => match id.as_str() {
                "ninfer.cuda_graph" | "ninfer.prefix_reuse" | "ninfer.thinking" if !value => {
                    arguments.push(OsString::from(option));
                }
                "ninfer.lm_head_draft" | "ninfer.preserve_thinking" if *value => {
                    arguments.push(OsString::from(option));
                }
                "ninfer.vision" | "ninfer.greedy" if *value => {
                    arguments.push(OsString::from(option));
                }
                "ninfer.cuda_graph"
                | "ninfer.prefix_reuse"
                | "ninfer.thinking"
                | "ninfer.lm_head_draft"
                | "ninfer.preserve_thinking"
                | "ninfer.vision"
                | "ninfer.greedy" => {}
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
            SettingValue::UnsignedIntegerOrChoice(
                UnsignedIntegerOrChoiceValue::UnsignedInteger(value),
            ) => push_value(&mut arguments, option, value),
            SettingValue::UnsignedIntegerOrChoice(UnsignedIntegerOrChoiceValue::Choice(value))
                if id.as_str() == "seed" && value == "random" => {}
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
            effective: values
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
        }
    }

    fn translated(values: &[(&str, SettingValue)], model_id: &str) -> Vec<String> {
        translate(&settings(values), &model(model_id), &[])
            .expect("setting translation")
            .into_iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn omitted_settings_emit_no_flags() {
        assert!(
            translate(&settings(&[]), &model("qwen3.6-27b"), &[])
                .expect("translation")
                .is_empty()
        );
    }

    #[test]
    fn common_and_typed_ninfer_settings_translate_exactly() {
        let arguments = translated(
            &[
                ("context_length", SettingValue::UnsignedInteger(32_768)),
                ("parallel_requests", SettingValue::UnsignedInteger(4)),
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
    fn advertised_common_definitions_remain_engine_neutral() {
        let definitions = definitions();
        for common in common_setting_definitions() {
            assert_eq!(
                definitions
                    .iter()
                    .find(|definition| definition.id == common.id),
                Some(&common)
            );
        }
    }

    #[test]
    fn parallel_range_and_kv_choices_are_typed_by_the_generic_schema() {
        let mut definitions = definitions();
        apply_runtime_bounds(&mut definitions);
        let parallel = definitions
            .iter()
            .find(|definition| definition.id.as_str() == "parallel_requests")
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
        assert!(translate(&missing_draft, &model("qwen3.6-27b"), &[]).is_err());

        let mtp = settings(&[
            (
                "ninfer.speculative_backend",
                SettingValue::Choice("mtp".to_owned()),
            ),
            ("ninfer.draft_tokens", SettingValue::UnsignedInteger(6)),
        ]);
        assert!(translate(&mtp, &model("qwen3.6-27b"), &[]).is_err());

        let dflash = settings(&[
            (
                "ninfer.speculative_backend",
                SettingValue::Choice("dflash".to_owned()),
            ),
            ("ninfer.draft_tokens", SettingValue::UnsignedInteger(7)),
        ]);
        assert!(translate(&dflash, &model("qwen3.6-27b"), &[]).is_err());
        assert!(translate(&dflash, &model("qwen3.6-35b-a3b"), &[]).is_ok());

        let missing_backend = settings(&[("ninfer.lm_head_draft", SettingValue::Toggle(true))]);
        assert!(translate(&missing_backend, &model("qwen3.6-27b"), &[]).is_err());

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
        apply_model_capabilities(&mut non_target_definitions, &non_target);
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
            validate_model_settings(&dflash, &non_target)
                .unwrap_err()
                .contains("exact qwen3.6-35b-a3b/groupwise-int")
        );

        let target = model("qwen3.6-35b-a3b");
        let mut target_definitions = definitions();
        apply_model_capabilities(&mut target_definitions, &target);
        let target_choices = target_definitions
            .iter()
            .find(|definition| definition.id.as_str() == "ninfer.speculative_backend")
            .and_then(|definition| match &definition.kind {
                SettingKind::Choice { choices } => Some(choices.as_slice()),
                _ => None,
            })
            .expect("target speculative backend choices");
        assert_eq!(target_choices, ["mtp", "dflash"]);
        assert!(validate_model_settings(&dflash, &target).is_ok());
    }

    #[test]
    fn explicit_kv_capacity_must_cover_explicit_context() {
        let undersized = settings(&[
            ("context_length", SettingValue::UnsignedInteger(8192)),
            (
                "ninfer.kv_capacity",
                SettingValue::UnsignedIntegerOrChoice(
                    UnsignedIntegerOrChoiceValue::UnsignedInteger(4096),
                ),
            ),
        ]);
        assert!(translate(&undersized, &model("qwen3.6-27b"), &[]).is_err());

        let automatic = settings(&[
            ("context_length", SettingValue::UnsignedInteger(8192)),
            (
                "ninfer.kv_capacity",
                SettingValue::UnsignedIntegerOrChoice(UnsignedIntegerOrChoiceValue::Choice(
                    "auto".to_owned(),
                )),
            ),
        ]);
        assert!(translate(&automatic, &model("qwen3.6-27b"), &[]).is_ok());
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
            assert!(translate(&structured, &model("qwen3.6-27b"), &native).is_err());
        }
    }
}
