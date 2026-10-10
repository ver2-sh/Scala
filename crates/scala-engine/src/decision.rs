//! Native decision contracts. No text-generation translation or emulation.

use std::collections::BTreeMap;

use scala_core::{
    ArtifactNativeIdentity, InstalledRuntime, ModelArtifact, ModelId, ModelProfileId,
    ResolvedSettings, RuntimeIdentity,
};
use serde::{Deserialize, Serialize};

use crate::{ApiCapability, EngineAdapter, EngineFeature, InferenceUsage, ModelCapabilities};

/// Preserve JSON context without serializing objects/arrays into prompt text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DecisionContent {
    String(String),
    Object(serde_json::Map<String, serde_json::Value>),
    Array(Vec<serde_json::Value>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecisionQuestion {
    Choice {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<DecisionContent>,
        criteria: BTreeMap<String, Option<DecisionContent>>,
    },
    Score {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<DecisionContent>,
        criteria: Vec<DecisionContent>,
    },
    Noul {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<DecisionContent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoulCriteria {
    #[serde(rename = "true", default, skip_serializing_if = "Option::is_none")]
    pub positive: Option<DecisionContent>,
    #[serde(rename = "false", default, skip_serializing_if = "Option::is_none")]
    pub negative: Option<DecisionContent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionRequest {
    pub model_profile_id: ModelProfileId,
    pub state: DecisionContent,
    pub questions: BTreeMap<String, DecisionQuestion>,
}

impl DecisionRequest {
    /// Structural checks only; model-specific limits belong to the native adapter.
    pub fn validate(&self) -> Result<(), String> {
        if self.questions.is_empty() {
            return Err("`questions` must contain at least one named question.".into());
        }
        for (name, question) in &self.questions {
            if name.trim().is_empty() {
                return Err("Question names must not be empty.".into());
            }
            match question {
                DecisionQuestion::Choice { criteria, .. } if criteria.is_empty() => {
                    return Err(format!("Choice question `{name}` requires criteria."));
                }
                DecisionQuestion::Score { criteria, .. } if criteria.is_empty() => {
                    return Err(format!(
                        "Score question `{name}` requires ordered criteria."
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DecisionAnswer {
    Choice {
        choice: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        probabilities: Option<BTreeMap<String, f64>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confidence: Option<f64>,
        #[serde(flatten)]
        observations: DecisionObservations,
    },
    Score {
        score: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        legend: Option<BTreeMap<String, DecisionContent>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        probabilities: Option<BTreeMap<String, f64>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confidence: Option<f64>,
        #[serde(flatten)]
        observations: DecisionObservations,
    },
    Noul {
        noul: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        probabilities: Option<BTreeMap<String, f64>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confidence: Option<f64>,
        #[serde(flatten)]
        observations: DecisionObservations,
    },
}

/// Optional native abstention observations. Absent fields stay absent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DecisionObservations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_probability: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abstained: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionOutput {
    /// Native model identity, if reported. Never inferred from the profile alias.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub answers: BTreeMap<String, DecisionAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<InferenceUsage>,
}

/// Public projection of existing launch provenance; no private paths or endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionIdentity {
    pub model_profile_id: ModelProfileId,
    pub model_id: ModelId,
    pub content_sha256: Option<String>,
    pub native_identity: Option<ArtifactNativeIdentity>,
    pub runtime: RuntimeIdentity,
}

pub struct RoutedDecisionOutput {
    pub output: DecisionOutput,
    pub identity: DecisionIdentity,
}

/// Exact loaded execution qualification. Format acceptance,
/// engine-wide declarations, filenames and package origin cannot grant support.
pub fn native_decision_supported(
    adapter: &dyn EngineAdapter,
    runtime: &InstalledRuntime,
    model: &ModelArtifact,
    settings: &ResolvedSettings,
) -> bool {
    let capabilities = adapter.capabilities();
    capabilities.api.contains(&ApiCapability::Decision)
        && capabilities.features.contains(&EngineFeature::Decision)
        && runtime.manifest.identity.engine_id == adapter.identity().id
        && runtime.manifest.probe.compatible
        && runtime.manifest.probe.observed_engine_id == adapter.identity().id
        && runtime.manifest.supported_formats.contains(&model.format)
        && settings.engine_id == adapter.identity().id
        && adapter.runtime_compatibility(runtime).is_supported()
        && adapter.compatibility(model).is_supported()
        && adapter.supports_model_capability(model, ApiCapability::Decision)
        && adapter
            .serving_features(runtime, model, Some(settings))
            .contains(&EngineFeature::Decision)
        && adapter.supports_native_decision(runtime, model, settings)
}

/// Discoverability only: the ordinary native request must still JIT-load and
/// pass `native_decision_supported`. No engine opts in by format or name alone.
pub fn native_decision_candidate(
    adapter: &dyn EngineAdapter,
    runtime: &InstalledRuntime,
    model: &ModelArtifact,
    settings: &ResolvedSettings,
) -> bool {
    let capabilities = adapter.capabilities();
    capabilities.api.contains(&ApiCapability::Decision)
        && capabilities.features.contains(&EngineFeature::Decision)
        && runtime.manifest.identity.engine_id == adapter.identity().id
        && runtime.manifest.runtime_id
            == scala_core::RuntimeId::from_identity(&runtime.manifest.identity)
        && runtime.manifest.probe.compatible
        && runtime.manifest.probe.observed_engine_id == adapter.identity().id
        && runtime.manifest.supported_formats.contains(&model.format)
        && settings.engine_id == adapter.identity().id
        && adapter.runtime_compatibility(runtime).is_supported()
        && adapter.compatibility(model).is_supported()
        && adapter.supports_model_capability(model, ApiCapability::Decision)
        && adapter.supports_native_decision_candidate(runtime, model, settings)
}

pub(crate) fn reported_model_capabilities(
    adapter: &dyn EngineAdapter,
    runtime: &InstalledRuntime,
    model: &ModelArtifact,
    settings: &ResolvedSettings,
) -> Option<ModelCapabilities> {
    let decision = native_decision_supported(adapter, runtime, model, settings);
    let candidate = native_decision_candidate(adapter, runtime, model, settings);
    let mut reported = adapter.model_capabilities(runtime, model, settings);
    if (decision || candidate) && reported.is_none() {
        reported = Some(ModelCapabilities {
            thinking: crate::ThinkingCapabilities {
                switchable: false,
                effort_options: Vec::new(),
            },
            decision,
            decision_candidate: candidate,
        });
    }
    if let Some(reported) = &mut reported {
        reported.decision = decision;
        reported.decision_candidate = candidate;
    }
    reported
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::*;
    use async_trait::async_trait;
    use scala_core::{ArtifactFormat, RuntimeProbeObservation};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(crate) struct DecisionAdapter {
        pub native: bool,
        pub api: bool,
        pub feature: bool,
        pub fail: bool,
        pub native_effort_admission: bool,
        pub calls: AtomicUsize,
        pub chat_history: std::sync::Mutex<Option<Vec<InferenceRequest>>>,
    }

    impl DecisionAdapter {
        pub fn new(native: bool) -> Self {
            Self {
                native,
                api: true,
                feature: true,
                fail: false,
                native_effort_admission: false,
                calls: AtomicUsize::new(0),
                chat_history: std::sync::Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl EngineAdapter for DecisionAdapter {
        fn validate_inference_request(
            &self,
            request: &InferenceRequest,
            defaults: &EffectiveGenerationSettings,
            _: &scala_core::SettingsSchema,
        ) -> Result<(), EngineError> {
            if self.chat_history.lock().unwrap().is_some() {
                // Recording fixture only; native admission is exercised by
                // each real adapter's contract tests.
                return self.validate_generation_settings(&request.generation_settings, defaults);
            }
            Err(EngineError::Unsupported(
                "synthetic decision adapter has no chat contract".into(),
            ))
        }

        fn validate_reasoning_admission(
            &self,
            settings: &GenerationSettingsPatch,
            capabilities: Option<&ModelCapabilities>,
        ) -> Result<(), EngineError> {
            if self.native_effort_admission {
                settings.validate_reasoning_switchability(capabilities)
            } else {
                settings.validate_reasoning_capabilities(capabilities)
            }
        }

        fn identity(&self) -> EngineIdentity {
            EngineIdentity {
                id: "fixture-decision".into(),
                display_name: "Fixture".into(),
                upstream_repository: String::new(),
            }
        }
        fn capabilities(&self) -> EngineCapabilities {
            let mut api = vec![
                ApiCapability::ChatCompletions,
                ApiCapability::Completions,
                ApiCapability::Embeddings,
            ];
            if self.api {
                api.push(ApiCapability::Decision);
            }
            let mut features = vec![
                EngineFeature::TextGeneration,
                EngineFeature::ToolCalling,
                EngineFeature::Vision,
            ];
            if self.feature {
                features.push(EngineFeature::Decision);
            }
            EngineCapabilities {
                artifact_formats: vec![
                    ArtifactFormat::Gguf,
                    ArtifactFormat::Q27,
                    ArtifactFormat::Ninfer,
                ],
                api,
                features,
            }
        }
        fn supports_native_decision(
            &self,
            runtime: &InstalledRuntime,
            model: &ModelArtifact,
            _: &ResolvedSettings,
        ) -> bool {
            self.native
                && runtime.manifest.identity.version == "1"
                && model.architecture.as_deref() == Some("native-decision-fixture")
        }
        async fn decide(
            &self,
            _: &str,
            request: DecisionRequest,
        ) -> Result<DecisionOutput, EngineError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.questions.len(), 3);
            assert!(matches!(request.state, DecisionContent::Object(_)));
            if self.fail {
                return Err(EngineError::Unsupported("native fixture".into()));
            }
            Ok(serde_json::from_value(json!({
                "model":"native-fixture-1",
                "answers":{
                    "route":{"type":"choice","choice":"accept","probabilities":{"accept":0.75,"reject":0.25},"confidence":0.6},
                    "urgency":{"type":"score","score":0.4},
                    "safe":{"type":"noul","noul":0.8}
                }
            })).unwrap())
        }
        fn native_options(&self) -> Vec<NativeOption> {
            Vec::new()
        }
        async fn probe(&self) -> Result<EngineProbe, EngineError> {
            Ok(EngineProbe {
                installation: InstallationState::NotInstalled,
                update: UpdateState::Unknown,
                healthy: true,
                detail: "synthetic".into(),
            })
        }
        async fn probe_runtime(
            &self,
            _: &InstalledRuntime,
        ) -> Result<RuntimeProbeObservation, EngineError> {
            panic!("must not probe a runtime")
        }
        async fn build_launch_spec(&self, _: LaunchRequest) -> Result<LaunchSpec, EngineError> {
            panic!("must not launch a process")
        }
        async fn health(&self, _: &ProcessDescriptor) -> Result<bool, EngineError> {
            panic!("must not contact a runtime")
        }
        async fn effective_generation_settings(
            &self,
            _: &ProcessDescriptor,
        ) -> Result<EffectiveGenerationSettings, EngineError> {
            panic!("must not contact a runtime")
        }
        async fn infer(
            &self,
            _: &str,
            request: InferenceRequest,
        ) -> Result<InferenceOutput, EngineError> {
            if let Some(history) = self.chat_history.lock().unwrap().as_mut() {
                history.push(request);
                return Err(EngineError::Unsupported("synthetic chat boundary".into()));
            }
            panic!("decision must never fall back to chat")
        }
        async fn infer_stream(
            &self,
            _: &str,
            request: InferenceRequest,
            _: InferenceActivityReporter,
        ) -> Result<InferenceStream, EngineError> {
            if let Some(history) = self.chat_history.lock().unwrap().as_mut() {
                history.push(request);
                return Err(EngineError::Unsupported("synthetic chat boundary".into()));
            }
            panic!("decision must never fall back to streaming chat")
        }
        async fn complete(
            &self,
            _: &str,
            _: CompletionRequest,
        ) -> Result<InferenceOutput, EngineError> {
            panic!("decision must never fall back to completion")
        }
    }

    pub(crate) fn runtime() -> InstalledRuntime {
        let identity = json!({
            "engine_id":"fixture-decision", "package_family":"fixture", "version":"1",
            "upstream_revision":null, "platform":std::env::consts::OS,
            "architecture":std::env::consts::ARCH, "accelerator":"cpu", "variant":"default",
            "package":{"provider_id":"fixture", "repository":null, "release_tag":"v1", "asset_id":"1", "asset_name":"fixture.zip"}
        });
        let id = scala_core::RuntimeId::from_identity(
            &serde_json::from_value(identity.clone()).unwrap(),
        );
        serde_json::from_value(json!({
            "manifest":{
                "schema_version":scala_core::RUNTIME_MANIFEST_SCHEMA_VERSION, "runtime_id":id,
                "identity":identity, "supported_formats":["gguf","q27","ninfer"], "requirements":scala_core::RuntimeRequirements::default(),
                "acquisition_method":"official_release_asset", "source_url":null,
                "downloaded_archive_sha256":"a".repeat(64), "entrypoint":"fixture-server",
                "entrypoint_sha256":"b".repeat(64), "installed_at_unix":1,
                "probe":{"compatible":true,"observed_engine_id":"fixture-decision","observed_version":"1","observed_revision":null,"detail":"synthetic","observed_at_unix":1}
            },
            "installation_root":"."
        })).unwrap()
    }

    pub(crate) fn model() -> ModelArtifact {
        ModelArtifact {
            id: ModelId("fixture".into()),
            display_name: "Fixture".into(),
            path: PathBuf::from("fixture.gguf"),
            format: ArtifactFormat::Gguf,
            size_bytes: 1,
            created: 1,
            hash: Some("c".repeat(64)),
            architecture: Some("native-decision-fixture".into()),
            context_length: None,
            provenance: None,
            native_identity: None,
            auxiliary_artifacts: Vec::new(),
            norted_package: None,
        }
    }

    pub(crate) fn request(profile: ModelProfileId) -> DecisionRequest {
        DecisionRequest {
            model_profile_id: profile,
            state: serde_json::from_value(json!({"message":"synthetic"})).unwrap(),
            questions: serde_json::from_value(json!({
                "route":{"type":"choice","criteria":{"accept":"allowed","reject":null}},
                "urgency":{"type":"score","criteria":["low",{"level":"high"}]},
                "safe":{"type":"noul","instructions":"Is this safe?","criteria":{"true":"safe","false":"unsafe"}}
            })).unwrap(),
        }
    }

    #[test]
    fn decision_requires_engine_runtime_and_model_qualification() {
        let mut adapter = DecisionAdapter::new(true);
        let mut runtime = runtime();
        let mut model = model();
        let mut settings = ResolvedSettings {
            engine_id: adapter.identity().id,
            ..Default::default()
        };
        assert!(native_decision_supported(
            &adapter, &runtime, &model, &settings
        ));
        let reported = reported_model_capabilities(&adapter, &runtime, &model, &settings).unwrap();
        assert!(reported.decision);
        assert_eq!(
            serde_json::to_value(&reported).unwrap()["decision"],
            json!(true)
        );
        assert!(!reported.thinking.switchable);
        adapter.api = false;
        assert!(!native_decision_supported(
            &adapter, &runtime, &model, &settings
        ));
        adapter.api = true;
        adapter.feature = false;
        assert!(!native_decision_supported(
            &adapter, &runtime, &model, &settings
        ));
        adapter.feature = true;
        runtime.manifest.identity.version = "unqualified".into();
        assert!(!native_decision_supported(
            &adapter, &runtime, &model, &settings
        ));
        runtime.manifest.identity.version = "1".into();
        model.architecture = None;
        assert!(!native_decision_supported(
            &adapter, &runtime, &model, &settings
        ));
        model.architecture = Some("native-decision-fixture".into());
        settings.engine_id = "other-engine".into();
        assert!(!native_decision_supported(
            &adapter, &runtime, &model, &settings
        ));
    }

    #[test]
    fn decision_formats_and_declarations_alone_grant_nothing() {
        let adapter = DecisionAdapter::new(false);
        let runtime = runtime();
        let settings = ResolvedSettings {
            engine_id: adapter.identity().id,
            ..Default::default()
        };
        for format in [
            ArtifactFormat::Gguf,
            ArtifactFormat::Q27,
            ArtifactFormat::Ninfer,
        ] {
            let mut model = model();
            model.format = format;
            model.display_name = "Norted Laya Jev System One".into();
            model.path = PathBuf::from(format!("Norted-decision.{}", format.as_str()));
            assert!(adapter.compatibility(&model).is_supported());
            assert!(!native_decision_supported(
                &adapter, &runtime, &model, &settings
            ));
            assert!(reported_model_capabilities(&adapter, &runtime, &model, &settings).is_none());
            for api in [
                ApiCapability::ChatCompletions,
                ApiCapability::Completions,
                ApiCapability::Embeddings,
            ] {
                assert!(adapter.supports_model_capability(&model, api));
            }
            assert!(
                adapter
                    .serving_features(&runtime, &model, Some(&settings))
                    .contains(&EngineFeature::ToolCalling)
            );
            assert!(
                adapter
                    .serving_features(&runtime, &model, Some(&settings))
                    .contains(&EngineFeature::Vision)
            );
        }
    }

    #[test]
    fn decision_answers_preserve_native_values_and_absence() {
        for answer in [
            json!({"type":"choice","choice":"a"}),
            json!({"type":"choice","choice":"b","probabilities":{"a":0.2,"b":0.8},"confidence":0.7}),
            json!({"type":"score","score":1.7,"legend":{"0":"low","1":"high"},"probabilities":{"0":0.3,"1":0.7},"confidence":0.5}),
            json!({"type":"noul","noul":0.98}),
        ] {
            let typed: DecisionAnswer = serde_json::from_value(answer.clone()).unwrap();
            assert_eq!(serde_json::to_value(typed).unwrap(), answer);
        }
    }
}
