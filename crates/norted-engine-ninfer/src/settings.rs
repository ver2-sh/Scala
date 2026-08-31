use std::ffi::OsString;

use norted_core::{
    ArtifactNativeIdentity, ModelArtifact, ResolvedSettings, SettingCategory, SettingDefinition,
    SettingId, SettingKind, SettingScope, SettingValue, UnsignedIntegerOrChoiceValue,
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
                choices: choices(&["bf16", "int8", "fp8"]),
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
            Some("derived by the exact runtime when omitted"),
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
            Some("runtime default/off when omitted"),
        ),
        definition(
            "ninfer.speculative_backend",
            "Speculative backend",
            "Explicit NInfer speculative backend; unset keeps speculation off/default",
            SettingKind::Choice {
                choices: choices(&["mtp", "dflash"]),
            },
            Some("off when omitted"),
        ),
        definition(
            "ninfer.draft_tokens",
            "Draft tokens",
            "Speculative draft-token window",
            SettingKind::UnsignedInteger {
                minimum: Some(1),
                maximum: Some(15),
            },
            Some("requires an explicit speculative backend"),
        ),
        toggle(
            "ninfer.lm_head_draft",
            "LM-head draft",
            "Use NInfer's optimized proposal head with an explicit speculative backend",
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
            "ninfer.max_cache_markers_per_request",
            "Cache markers",
            "Maximum cache markers per request",
            0,
            Some(MAX_NINFER_CLI_INTEGER),
        ),
    ]);
    definitions
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
    }
}

fn toggle(id: &str, label: &str, description: &str) -> SettingDefinition {
    definition(
        id,
        label,
        description,
        SettingKind::Toggle,
        Some("runtime default when omitted"),
    )
}

fn category(id: &str) -> SettingCategory {
    if matches!(id, "ninfer.thinking" | "ninfer.preserve_thinking") {
        SettingCategory::Reasoning
    } else if id.contains("specul") || id.contains("draft") {
        SettingCategory::Speculation
    } else if id.contains("kv_") || id == "ninfer.cuda_graph" {
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
        "reasoning_effort" => "",
        "ninfer.kv_dtype" => "--kv-dtype",
        "ninfer.kv_capacity" => "--kv-capacity",
        "ninfer.prefill_chunk" => "--prefill-chunk",
        "ninfer.speculation" => "",
        "ninfer.speculative_backend" => "--spec",
        "ninfer.draft_tokens" => "--draft-tokens",
        "ninfer.lm_head_draft" => "--lm-head-draft",
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
        "ninfer.max_cache_markers_per_request" => "--max-cache-markers-per-request",
        _ => "",
    }
}

pub(crate) fn translate(
    settings: &ResolvedSettings,
    model: &ModelArtifact,
    native_arguments: &[String],
) -> Result<Vec<OsString>, EngineError> {
    for id in settings.effective.keys() {
        if matches!(id.as_str(), "ninfer.speculation" | "reasoning_effort") {
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
            let exact_target = matches!(
                model.native_identity.as_ref(),
                Some(ArtifactNativeIdentity::Ninfer(identity))
                    if identity.model_id == "qwen3.6-35b-a3b"
                        && identity.weights_id == "groupwise-int"
            );
            if !exact_target {
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
                    | "ninfer.max_cache_markers_per_request"
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
        if matches!(id.as_str(), "ninfer.speculation" | "reasoning_effort") {
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
                "ninfer.cuda_graph"
                | "ninfer.prefix_reuse"
                | "ninfer.thinking"
                | "ninfer.lm_head_draft"
                | "ninfer.preserve_thinking" => {}
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
            SettingValue::Choice(value) => push_value(&mut arguments, option, value),
            SettingValue::UnsignedIntegerOrChoice(
                UnsignedIntegerOrChoiceValue::UnsignedInteger(value),
            ) => push_value(&mut arguments, option, value),
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
