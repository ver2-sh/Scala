//! Read-only, public discovery of a configured profile, never live capacity.
use crate::{ApiCapability, EngineAdapter, EngineFeature, ModelCapabilities};
use scala_core::{InstalledRuntime, ModelArtifact, ResolvedSettings};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelDiscoveryMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_endpoints: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_modalities: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct ProfileDiscovery {
    pub metadata: ModelDiscoveryMetadata,
    pub capabilities: Option<ModelCapabilities>,
}

impl ProfileDiscovery {
    pub(crate) fn from_resolved(
        adapter: &dyn EngineAdapter,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        settings: &ResolvedSettings,
    ) -> Self {
        let features = adapter.serving_features(runtime, model, Some(settings));
        let capabilities =
            crate::decision::reported_model_capabilities(adapter, runtime, model, settings);
        let mut endpoints: Vec<String> = [
            (ApiCapability::ChatCompletions, "/chat/completions"),
            (ApiCapability::Responses, "/responses"),
            (ApiCapability::Completions, "/completions"),
            (ApiCapability::Embeddings, "/embeddings"),
        ]
        .into_iter()
        .filter(|(api, _)| {
            adapter.supports_model_capability(model, *api)
                && (features.contains(&EngineFeature::TextGeneration)
                    || *api == ApiCapability::Embeddings)
        })
        .map(|(_, route)| route.to_owned())
        .collect();
        if capabilities
            .as_ref()
            .is_some_and(|c| c.decision || c.decision_candidate)
        {
            endpoints.push("/systemone".into());
        }
        if capabilities.as_ref().is_some_and(|c| c.openai_decisions) {
            endpoints.push("/decisions".into());
        }
        let generation = features.contains(&EngineFeature::TextGeneration);
        Self {
            metadata: ModelDiscoveryMetadata {
                context_length: token_limit(settings, "context_length"),
                max_output_tokens: token_limit(settings, "max_output_tokens"),
                supported_endpoints: Some(endpoints),
                tool_call: generation.then_some(features.contains(&EngineFeature::ToolCalling)),
                input_modalities: generation.then(|| {
                    let mut input = vec!["text".into()];
                    if features.contains(&EngineFeature::Vision) {
                        input.push("image".into());
                    }
                    input
                }),
                output_modalities: generation.then(|| vec!["text".into()]),
            },
            capabilities,
        }
    }
}

fn token_limit(settings: &ResolvedSettings, suffix: &str) -> Option<u64> {
    // Effective values are materialized only after exact schema validation.
    // Automatic/unlimited policies and unresolved defaults have no scalar.
    let id = format!("{}.{}", settings.engine_id, suffix);
    settings
        .effective
        .iter()
        .find(|(key, _)| key.as_str() == id)
        .and_then(|(_, value)| value.value.parse::<u64>().ok())
        .filter(|value| *value > 0 && *value <= 9_007_199_254_740_991)
}

#[cfg(test)]
mod tests {
    use super::*;
    use scala_core::{EffectiveSetting, SettingId, SettingSource};

    #[test]
    fn automatic_unlimited_unknown_and_unsafe_limits_are_not_scalars() {
        let mut settings = ResolvedSettings {
            engine_id: "fixture".into(),
            ..Default::default()
        };
        assert_eq!(token_limit(&settings, "context_length"), None);
        for (value, expected) in [
            ("auto", None),
            ("unlimited", None),
            ("-1", None),
            ("0", None),
            ("9007199254740992", None),
            ("500000", Some(500000)),
        ] {
            settings.effective.insert(
                SettingId::new("fixture.context_length").unwrap(),
                EffectiveSetting {
                    value: value.into(),
                    source: SettingSource::RuntimeDefault,
                    requested_value: None,
                    detail: None,
                },
            );
            assert_eq!(token_limit(&settings, "context_length"), expected);
        }
        settings.engine_id = "other".into();
        assert_eq!(token_limit(&settings, "context_length"), None);
    }
}
