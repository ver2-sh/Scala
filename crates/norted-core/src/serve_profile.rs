use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ArtifactFormat, ArtifactNativeIdentity, LoadSettingId, LoadSettingSource, LoadSettingValue,
    LoadSettingsPatch, ModelArtifact, ResolvedLoadSetting, ResolvedLoadSettings,
};

pub const SERVE_PROFILE_SCHEMA: &str = "norted.serve-profile";
pub const SERVE_PROFILE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServeProfileSource {
    UserLocal,
    BuilderRecommended,
    BuilderLegacyPolicy,
    BuiltIn,
}

impl std::fmt::Display for ServeProfileSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::UserLocal => "user/local",
            Self::BuilderRecommended => "Builder",
            Self::BuilderLegacyPolicy => "Builder legacy policy (synthesized)",
            Self::BuiltIn => "built-in",
        })
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServeProfileApplicability {
    pub artifact_formats: Vec<ArtifactFormat>,
    pub architecture: Option<String>,
    pub family: Option<String>,
    pub required_model_capabilities: Vec<String>,
    pub native_model_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptMode {
    #[default]
    RuntimeDefault,
    ExternalTemplate,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptDelivery {
    #[default]
    RuntimeChat,
    RawCompletions,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFilter {
    #[default]
    None,
    DirkSharpReasoning,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalTemplateReference {
    pub identity: String,
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServePromptProfile {
    pub mode: PromptMode,
    pub delivery: PromptDelivery,
    pub template: Option<ExternalTemplateReference>,
    pub render_generation_prompt: bool,
    pub thinking_enabled: bool,
    pub response_filter: ResponseFilter,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GenerationDefaults {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<u64>,
    pub min_p: Option<f64>,
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ThinkingPolicy {
    pub default: bool,
    pub required: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServeGenerationProfile {
    pub defaults: GenerationDefaults,
    pub thinking: ThinkingPolicy,
    pub allowed_user_overrides: Vec<String>,
}

impl ServeGenerationProfile {
    pub fn allows_override(&self, name: &str) -> bool {
        self.allowed_user_overrides
            .iter()
            .any(|candidate| candidate == name)
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContextPolicy {
    pub preferred: Option<u64>,
    pub minimum: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServeLoadProfile {
    pub settings: LoadSettingsPatch,
    pub context: ContextPolicy,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServeCapability {
    RawCompletionPromptInput,
    ExternalTemplateApplication,
    Thinking,
    UnlimitedThinkingBudget,
    TemperatureTopP,
    TopKMinP,
    Mtp,
    SuffixDrafting,
    ObservedWMax,
    FastHeadControl,
    StartupBannerObservation,
    ServedContextProof,
    KvModeProof,
    NinferCudaGraph,
    NinferPrefixReuse,
    NinferStartupObservation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Q27MtpStrategy {
    pub enabled: bool,
    pub required: bool,
    pub depth_policy: String,
    pub maximum_depth: String,
    pub minimum_probability: f64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToggleOverridePolicy {
    pub default: bool,
    pub user_override_allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Q27ServeStrategy {
    pub mtp: Q27MtpStrategy,
    pub suffix_drafting: bool,
    pub suffix_width_from_runtime_w_max: bool,
    pub fast_head: ToggleOverridePolicy,
    pub kv_quality_order: Vec<String>,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NinferSpeculativeProfile {
    pub speculative_decoding: bool,
    pub backend: Option<String>,
    pub draft_tokens: Option<u64>,
    pub optimized_proposal_head: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NinferServeStrategy {
    pub kv_cache: String,
    pub kv_dtype: String,
    pub cuda_graph_required: bool,
    pub prefix_reuse_required: bool,
    pub text_only_default: bool,
    pub default_speculative_profile: String,
    pub speculative_profiles: BTreeMap<String, NinferSpeculativeProfile>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServeEngineProfiles {
    pub q27: Option<Q27ServeStrategy>,
    pub ninfer: Option<NinferServeStrategy>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServeProfile {
    pub schema: String,
    pub schema_version: u32,
    pub id: String,
    pub display_name: String,
    pub description: Option<String>,
    pub source: ServeProfileSource,
    pub read_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_profile_sha256: Option<String>,
    #[serde(default)]
    pub applicability: ServeProfileApplicability,
    #[serde(default)]
    pub prompt: ServePromptProfile,
    #[serde(default)]
    pub generation: ServeGenerationProfile,
    #[serde(default)]
    pub load: ServeLoadProfile,
    #[serde(default)]
    pub requirements: Vec<ServeCapability>,
    #[serde(default)]
    pub engine: ServeEngineProfiles,
}

impl ServeProfile {
    pub fn local(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            schema: SERVE_PROFILE_SCHEMA.to_owned(),
            schema_version: SERVE_PROFILE_SCHEMA_VERSION,
            display_name: id.clone(),
            id,
            description: None,
            source: ServeProfileSource::UserLocal,
            read_only: false,
            source_profile_sha256: None,
            applicability: ServeProfileApplicability::default(),
            prompt: ServePromptProfile::default(),
            generation: ServeGenerationProfile::default(),
            load: ServeLoadProfile::default(),
            requirements: Vec::new(),
            engine: ServeEngineProfiles::default(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SERVE_PROFILE_SCHEMA
            || self.schema_version != SERVE_PROFILE_SCHEMA_VERSION
        {
            return Err(format!(
                "unsupported Serve Profile schema {} v{}; expected {} v{}",
                self.schema,
                self.schema_version,
                SERVE_PROFILE_SCHEMA,
                SERVE_PROFILE_SCHEMA_VERSION
            ));
        }
        validate_profile_id(&self.id)?;
        if self.display_name.trim().is_empty() {
            return Err("Serve Profile display_name cannot be empty".to_owned());
        }
        if self.source == ServeProfileSource::UserLocal && self.read_only {
            return Err("a user/local Serve Profile cannot be read-only".to_owned());
        }
        if self.source != ServeProfileSource::UserLocal && !self.read_only {
            return Err("Builder and built-in Serve Profiles must be read-only".to_owned());
        }
        if self.prompt.mode == PromptMode::ExternalTemplate {
            let template = self.prompt.template.as_ref().ok_or_else(|| {
                "external-template Serve Profile has no template reference".to_owned()
            })?;
            validate_sha256(&template.sha256, "template")?;
            if template.identity.trim().is_empty() || template.path.as_os_str().is_empty() {
                return Err("external template identity/path cannot be empty".to_owned());
            }
        } else if self.prompt.template.is_some() {
            return Err("runtime-default prompt mode cannot bind an external template".to_owned());
        }
        for value in [
            self.generation.defaults.temperature,
            self.generation.defaults.top_p,
            self.generation.defaults.min_p,
        ]
        .into_iter()
        .flatten()
        {
            if !value.is_finite() {
                return Err("Serve Profile generation values must be finite".to_owned());
            }
        }
        if let (Some(minimum), Some(preferred)) =
            (self.load.context.minimum, self.load.context.preferred)
            && preferred < minimum
        {
            return Err(format!(
                "Serve Profile preferred context {preferred} is below required minimum {minimum}"
            ));
        }
        if self.load.context.minimum == Some(0) || self.load.context.preferred == Some(0) {
            return Err("Serve Profile context values must be positive".to_owned());
        }
        if self
            .generation
            .defaults
            .temperature
            .is_some_and(|value| value < 0.0)
            || self
                .generation
                .defaults
                .top_p
                .is_some_and(|value| !(0.0..=1.0).contains(&value))
            || self
                .generation
                .defaults
                .min_p
                .is_some_and(|value| !(0.0..=1.0).contains(&value))
        {
            return Err(
                "Serve Profile generation defaults are outside their valid ranges".to_owned(),
            );
        }
        if self.generation.thinking.required && !self.generation.thinking.default {
            return Err(
                "Serve Profile cannot require thinking while selecting thinking_default=false"
                    .to_owned(),
            );
        }
        if self.engine.q27.is_some()
            && !self
                .applicability
                .artifact_formats
                .contains(&ArtifactFormat::Q27)
        {
            return Err("q27 strategy requires q27 artifact applicability".to_owned());
        }
        if self.engine.ninfer.is_some()
            && !self
                .applicability
                .artifact_formats
                .contains(&ArtifactFormat::Ninfer)
        {
            return Err("NInfer strategy requires NInfer artifact applicability".to_owned());
        }
        if let Some(q27) = &self.engine.q27 {
            if q27.kv_quality_order.is_empty()
                || !q27.mtp.minimum_probability.is_finite()
                || !(0.0..=1.0).contains(&q27.mtp.minimum_probability)
            {
                return Err("q27 strategy has an invalid KV/MTP policy".to_owned());
            }
            if q27.mtp.required && !q27.mtp.enabled {
                return Err("q27 MTP cannot be required while disabled".to_owned());
            }
            if q27.suffix_drafting && !q27.mtp.enabled {
                return Err("q27 suffix drafting requires MTP to be enabled".to_owned());
            }
            if q27.suffix_width_from_runtime_w_max && !q27.suffix_drafting {
                return Err(
                    "q27 runtime W_MAX suffix width requires suffix drafting to be enabled"
                        .to_owned(),
                );
            }
        }
        if let Some(ninfer) = &self.engine.ninfer {
            if !ninfer
                .speculative_profiles
                .contains_key(&ninfer.default_speculative_profile)
            {
                return Err(format!(
                    "NInfer default strategy `{}` is not declared",
                    ninfer.default_speculative_profile
                ));
            }
        }
        if let Some(hash) = &self.source_profile_sha256 {
            validate_sha256(hash, "source profile")?;
        }
        Ok(())
    }

    pub fn basic_applicability(&self, model: &ModelArtifact) -> Result<(), String> {
        if !self.applicability.artifact_formats.is_empty()
            && !self.applicability.artifact_formats.contains(&model.format)
        {
            return Err(format!(
                "Serve Profile `{}` applies to {}, not {}",
                self.display_name,
                self.applicability
                    .artifact_formats
                    .iter()
                    .map(|format| format.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                model.format
            ));
        }
        if !self.applicability.native_model_ids.is_empty() {
            let observed = match &model.native_identity {
                Some(ArtifactNativeIdentity::Ninfer(identity)) => identity.model_id.as_str(),
                None => {
                    return Err(format!(
                        "Serve Profile `{}` requires a proven native model identity",
                        self.display_name
                    ));
                }
            };
            if !self
                .applicability
                .native_model_ids
                .iter()
                .any(|candidate| candidate == observed)
            {
                return Err(format!(
                    "Serve Profile `{}` does not admit native model identity `{observed}`",
                    self.display_name
                ));
            }
        }
        Ok(())
    }

    pub fn requires_runtime_recipe(&self) -> bool {
        self.prompt != ServePromptProfile::default()
            || self.generation.defaults != GenerationDefaults::default()
            || self.generation.thinking != ThinkingPolicy::default()
            || !self.requirements.is_empty()
            || self.engine.q27.is_some()
            || self.engine.ninfer.is_some()
    }

    pub fn effective_requirements(&self) -> BTreeSet<ServeCapability> {
        let mut requirements = self.requirements.iter().copied().collect::<BTreeSet<_>>();
        if self.prompt.mode == PromptMode::ExternalTemplate {
            requirements.insert(ServeCapability::ExternalTemplateApplication);
        }
        if self.prompt.delivery == PromptDelivery::RawCompletions {
            requirements.insert(ServeCapability::RawCompletionPromptInput);
        }
        if self.prompt.thinking_enabled
            || self.generation.thinking.default
            || self.generation.thinking.required
            || self.engine.q27.is_some()
            || self.engine.ninfer.is_some()
        {
            requirements.insert(ServeCapability::Thinking);
        }
        if self.generation.defaults.temperature.is_some()
            || self.generation.defaults.top_p.is_some()
        {
            requirements.insert(ServeCapability::TemperatureTopP);
        }
        if self.generation.defaults.top_k.is_some() || self.generation.defaults.min_p.is_some() {
            requirements.insert(ServeCapability::TopKMinP);
        }
        if self.load.context.minimum.is_some() {
            requirements.insert(ServeCapability::ServedContextProof);
        }
        if let Some(q27) = &self.engine.q27 {
            requirements.insert(ServeCapability::StartupBannerObservation);
            requirements.insert(ServeCapability::KvModeProof);
            requirements.insert(ServeCapability::FastHeadControl);
            if q27.mtp.enabled || q27.mtp.required {
                requirements.insert(ServeCapability::Mtp);
            }
            if q27.suffix_drafting {
                requirements.insert(ServeCapability::SuffixDrafting);
            }
            if q27.suffix_width_from_runtime_w_max {
                requirements.insert(ServeCapability::ObservedWMax);
            }
        }
        if let Some(ninfer) = &self.engine.ninfer {
            requirements.insert(ServeCapability::NinferStartupObservation);
            if ninfer.cuda_graph_required {
                requirements.insert(ServeCapability::NinferCudaGraph);
            }
            if ninfer.prefix_reuse_required {
                requirements.insert(ServeCapability::NinferPrefixReuse);
            }
            if ninfer
                .speculative_profiles
                .get(&ninfer.default_speculative_profile)
                .is_some_and(|strategy| strategy.speculative_decoding)
            {
                requirements.insert(ServeCapability::Mtp);
            }
        }
        requirements
    }

    pub fn fork_local(&self, id: impl Into<String>, display_name: impl Into<String>) -> Self {
        let mut fork = self.clone();
        fork.id = id.into();
        fork.display_name = display_name.into();
        fork.source = ServeProfileSource::UserLocal;
        fork.read_only = false;
        fork.source_profile_sha256 = Some(self.content_hash());
        fork
    }

    pub fn content_hash(&self) -> String {
        let mut material = self.clone();
        material.source_profile_sha256 = None;
        if matches!(
            material.source,
            ServeProfileSource::BuilderRecommended | ServeProfileSource::BuilderLegacyPolicy
        ) {
            // Builder and synthesized-legacy copies of one portable recipe are the same
            // profile even after their package-relative template has been resolved locally.
            material.source = ServeProfileSource::BuilderRecommended;
            if let Some(template) = material.prompt.template.as_mut()
                && template.path.is_absolute()
                && let Some(filename) = template.path.file_name()
            {
                template.path = PathBuf::from(filename);
            }
        }
        let bytes = serde_json::to_vec(&material).expect("Serve Profile is serializable");
        format!("{:x}", Sha256::digest(bytes))
    }

    pub fn resolve_builder_template(&mut self, package_root: &Path) -> Result<(), String> {
        let Some(template) = self.prompt.template.as_mut() else {
            return Ok(());
        };
        if template.path.is_absolute() {
            return Err("Builder Serve Profile template path must be relative".to_owned());
        }
        if template
            .path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
        {
            return Err("Builder Serve Profile template path is unsafe".to_owned());
        }
        let root = package_root
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let path = root
            .join(&template.path)
            .canonicalize()
            .map_err(|error| format!("could not resolve Serve Profile template: {error}"))?;
        if !path.starts_with(&root) || !path.is_file() {
            return Err("Serve Profile template escapes or is missing from its package".to_owned());
        }
        template.path = path;
        Ok(())
    }
}

pub fn apply_serve_profile_load_policy(
    profile: Option<&ServeProfile>,
    settings: &mut ResolvedLoadSettings,
) -> Result<(), String> {
    let Some(profile) = profile else {
        return Ok(());
    };
    let source = LoadSettingSource::ServeProfile {
        profile_id: profile.id.clone(),
    };
    for (id, value) in profile.load.settings.iter() {
        if id.applies_to_engine(&settings.engine_id)
            && !settings
                .effective
                .get(id)
                .is_some_and(|setting| matches!(setting.source, LoadSettingSource::Invocation))
        {
            settings.effective.insert(
                id.clone(),
                ResolvedLoadSetting {
                    value: value.clone(),
                    source: source.clone(),
                },
            );
        }
    }
    let context_id = LoadSettingId::new("context_length").map_err(|error| error.to_string())?;
    let context_is_selected_layer = settings
        .effective
        .get(&context_id)
        .is_some_and(|setting| setting_is_selected_layer(setting, &profile.id));
    if !context_is_selected_layer && let Some(preferred) = profile.load.context.preferred {
        settings.effective.insert(
            context_id.clone(),
            ResolvedLoadSetting {
                value: LoadSettingValue::UnsignedInteger(preferred),
                source: source.clone(),
            },
        );
    }
    if let Some(minimum) = profile.load.context.minimum {
        match settings
            .effective
            .get(&context_id)
            .map(|setting| &setting.value)
        {
            Some(LoadSettingValue::UnsignedInteger(value)) if *value >= minimum => {}
            Some(LoadSettingValue::UnsignedInteger(value)) => {
                return Err(format!(
                    "Serve Profile `{}` requires at least {minimum} served tokens; requested {value}",
                    profile.display_name
                ));
            }
            Some(_) => return Err("context_length has an invalid Serve Profile value".to_owned()),
            None => {
                settings.effective.insert(
                    context_id,
                    ResolvedLoadSetting {
                        value: LoadSettingValue::UnsignedInteger(minimum),
                        source: source.clone(),
                    },
                );
            }
        }
    } else if !settings.effective.contains_key(&context_id)
        && let Some(preferred) = profile.load.context.preferred
    {
        settings.effective.insert(
            context_id,
            ResolvedLoadSetting {
                value: LoadSettingValue::UnsignedInteger(preferred),
                source: source.clone(),
            },
        );
    }
    if let Some(q27) = &profile.engine.q27 {
        let kv_fp16 = LoadSettingId::new("q27.kv_fp16").map_err(|error| error.to_string())?;
        if let Some(setting) = settings.effective.get(&kv_fp16) {
            if setting_is_selected_layer(setting, &profile.id) {
                return Err(format!(
                    "Serve Profile `{}` selects KV mode through its ordered quality strategy; `q27.kv_fp16` conflicts",
                    profile.display_name
                ));
            }
            settings.effective.remove(&kv_fp16);
        }
        let id = LoadSettingId::new("q27.fast_head").map_err(|error| error.to_string())?;
        match settings.effective.get(&id) {
            Some(setting) if setting_is_selected_layer(setting, &profile.id) => {
                let LoadSettingValue::Toggle(value) = &setting.value else {
                    return Err("q27.fast_head has an invalid Serve Profile value".to_owned());
                };
                if *value != q27.fast_head.default && !q27.fast_head.user_override_allowed {
                    return Err(format!(
                        "Serve Profile `{}` locks q27.fast_head={}; requested {value}",
                        profile.display_name, q27.fast_head.default
                    ));
                }
            }
            Some(_) | None => {
                settings.effective.insert(
                    id,
                    ResolvedLoadSetting {
                        value: LoadSettingValue::Toggle(q27.fast_head.default),
                        source: source.clone(),
                    },
                );
            }
        }
    }
    if let Some(ninfer) = &profile.engine.ninfer {
        enforce_choice(
            settings,
            "ninfer.kv_dtype",
            &ninfer.kv_dtype,
            &source,
            &profile.id,
            &profile.display_name,
        )?;
        apply_ninfer_thinking_policy(profile, settings, &source)?;
        for (id, required, message) in [
            (
                "ninfer.no_cuda_graph",
                ninfer.cuda_graph_required,
                "CUDA graph decode",
            ),
            (
                "ninfer.no_prefix_reuse",
                ninfer.prefix_reuse_required,
                "prefix reuse",
            ),
        ] {
            if required {
                let setting_id = LoadSettingId::new(id).map_err(|error| error.to_string())?;
                if let Some(setting) = settings.effective.get(&setting_id) {
                    if setting_is_selected_layer(setting, &profile.id) {
                        return Err(format!(
                            "Serve Profile `{}` requires {message}; `{id}` conflicts",
                            profile.display_name
                        ));
                    }
                    settings.effective.remove(&setting_id);
                }
            }
        }
        apply_ninfer_speculative_strategy(profile, ninfer, settings, &source)?;
    }
    Ok(())
}

fn apply_ninfer_thinking_policy(
    profile: &ServeProfile,
    settings: &mut ResolvedLoadSettings,
    source: &LoadSettingSource,
) -> Result<(), String> {
    let id = LoadSettingId::new("ninfer.no_thinking").map_err(|error| error.to_string())?;
    if !profile.generation.thinking.default {
        settings.effective.insert(
            id,
            ResolvedLoadSetting {
                value: LoadSettingValue::FlagEnabled,
                source: source.clone(),
            },
        );
        return Ok(());
    }
    let Some(setting) = settings.effective.get(&id) else {
        return Ok(());
    };
    if matches!(setting.source, LoadSettingSource::Invocation) {
        if profile.generation.thinking.required {
            return Err(format!(
                "Serve Profile `{}` requires thinking; `ninfer.no_thinking` conflicts",
                profile.display_name
            ));
        }
        if profile.generation.allows_override("ninfer.no_thinking") {
            return Ok(());
        }
        return Err(format!(
            "Serve Profile `{}` does not allow the `ninfer.no_thinking` invocation override",
            profile.display_name
        ));
    }
    settings.effective.remove(&id);
    Ok(())
}

fn apply_ninfer_speculative_strategy(
    profile: &ServeProfile,
    strategy: &NinferServeStrategy,
    settings: &mut ResolvedLoadSettings,
    source: &LoadSettingSource,
) -> Result<(), String> {
    let selector_id =
        LoadSettingId::new("ninfer.package_profile").map_err(|error| error.to_string())?;
    let selected = match settings.effective.get(&selector_id) {
        Some(setting) if setting_is_selected_layer(setting, &profile.id) => {
            let LoadSettingValue::Choice(value) = &setting.value else {
                return Err("ninfer.package_profile must be a choice".to_owned());
            };
            if value != &strategy.default_speculative_profile
                && !profile.generation.allows_override("ninfer.package_profile")
            {
                return Err(format!(
                    "Serve Profile `{}` locks NInfer strategy `{}`; requested `{value}`",
                    profile.display_name, strategy.default_speculative_profile
                ));
            }
            value.clone()
        }
        Some(_) | None => {
            settings.effective.insert(
                selector_id,
                ResolvedLoadSetting {
                    value: LoadSettingValue::Choice(strategy.default_speculative_profile.clone()),
                    source: source.clone(),
                },
            );
            strategy.default_speculative_profile.clone()
        }
    };
    let selected_strategy = strategy
        .speculative_profiles
        .get(&selected)
        .ok_or_else(|| {
            format!(
                "Serve Profile `{}` does not declare NInfer strategy `{selected}`",
                profile.display_name
            )
        })?;
    for raw_id in [
        "ninfer.speculative_backend",
        "ninfer.draft_tokens",
        "ninfer.lm_head_draft",
    ] {
        let id = LoadSettingId::new(raw_id).map_err(|error| error.to_string())?;
        if settings
            .effective
            .get(&id)
            .is_some_and(|setting| !setting_is_selected_layer(setting, &profile.id))
        {
            settings.effective.remove(&id);
        }
    }
    if !selected_strategy.speculative_decoding {
        for id in [
            "ninfer.speculative_backend",
            "ninfer.draft_tokens",
            "ninfer.lm_head_draft",
        ] {
            if settings.value(id).is_some() {
                return Err(format!(
                    "Serve Profile `{}` strategy `{selected}` disables speculation; `{id}` conflicts",
                    profile.display_name
                ));
            }
        }
        return Ok(());
    }
    let backend = selected_strategy.backend.as_deref().ok_or_else(|| {
        format!(
            "Serve Profile `{}` NInfer strategy `{selected}` enables speculation without a backend",
            profile.display_name
        )
    })?;
    let draft_tokens = selected_strategy.draft_tokens.ok_or_else(|| {
        format!(
            "Serve Profile `{}` NInfer strategy `{selected}` has no draft-token count",
            profile.display_name
        )
    })?;
    insert_required_setting(
        settings,
        "ninfer.speculative_backend",
        LoadSettingValue::Choice(backend.to_owned()),
        source,
        &profile.id,
        &profile.display_name,
    )?;
    insert_required_setting(
        settings,
        "ninfer.draft_tokens",
        LoadSettingValue::UnsignedInteger(draft_tokens),
        source,
        &profile.id,
        &profile.display_name,
    )?;
    if selected_strategy.optimized_proposal_head.unwrap_or(false) {
        insert_required_setting(
            settings,
            "ninfer.lm_head_draft",
            LoadSettingValue::FlagEnabled,
            source,
            &profile.id,
            &profile.display_name,
        )?;
    } else if settings.value("ninfer.lm_head_draft").is_some() {
        return Err(format!(
            "Serve Profile `{}` NInfer strategy `{selected}` does not use lm_head_draft",
            profile.display_name
        ));
    }
    Ok(())
}

fn setting_is_selected_layer(setting: &ResolvedLoadSetting, profile_id: &str) -> bool {
    matches!(setting.source, LoadSettingSource::Invocation)
        || matches!(
            &setting.source,
            LoadSettingSource::ServeProfile {
                profile_id: selected
            } if selected == profile_id
        )
}

fn insert_required_setting(
    settings: &mut ResolvedLoadSettings,
    raw_id: &str,
    required: LoadSettingValue,
    source: &LoadSettingSource,
    profile_id: &str,
    display_name: &str,
) -> Result<(), String> {
    let id = LoadSettingId::new(raw_id).map_err(|error| error.to_string())?;
    match settings.effective.get(&id) {
        Some(setting) if setting.value == required => Ok(()),
        Some(setting) if setting_is_selected_layer(setting, profile_id) => Err(format!(
            "Serve Profile `{display_name}` requires {raw_id}={required}; requested {}",
            setting.value
        )),
        Some(_) | None => {
            settings.effective.insert(
                id,
                ResolvedLoadSetting {
                    value: required,
                    source: source.clone(),
                },
            );
            Ok(())
        }
    }
}

fn enforce_choice(
    settings: &mut ResolvedLoadSettings,
    raw_id: &str,
    required: &str,
    source: &LoadSettingSource,
    profile_id: &str,
    display_name: &str,
) -> Result<(), String> {
    let id = LoadSettingId::new(raw_id).map_err(|error| error.to_string())?;
    match settings.effective.get(&id) {
        Some(setting) if setting.value == LoadSettingValue::Choice(required.to_owned()) => Ok(()),
        Some(setting) if setting_is_selected_layer(setting, profile_id) => Err(format!(
            "Serve Profile `{display_name}` requires {raw_id}={required}; requested {}",
            setting.value
        )),
        Some(_) | None => {
            settings.effective.insert(
                id,
                ResolvedLoadSetting {
                    value: LoadSettingValue::Choice(required.to_owned()),
                    source: source.clone(),
                },
            );
            Ok(())
        }
    }
}

pub fn validate_profile_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 96
        || !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        return Err(format!(
            "Serve Profile ID `{value}` is invalid; use lowercase letters, digits, '-' or '_'"
        ));
    }
    Ok(())
}

fn validate_sha256(value: &str, label: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(format!("Serve Profile {label} SHA-256 is invalid"));
    }
    Ok(())
}
