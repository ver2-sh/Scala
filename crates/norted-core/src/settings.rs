use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use crate::{AppPaths, ModelProfileId, RuntimeId};

pub const SETTINGS_STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SettingId(String);

impl SettingId {
    pub fn new(value: impl Into<String>) -> Result<Self, SettingsError> {
        let value = value.into();
        validate_setting_id(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn namespace(&self) -> Option<&str> {
        self.0.rsplit_once('.').map(|(namespace, _)| namespace)
    }

    pub fn applies_to_engine(&self, engine_id: &str) -> bool {
        self.namespace()
            .is_none_or(|namespace| namespace == engine_id)
    }
}

impl std::fmt::Display for SettingId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::str::FromStr for SettingId {
    type Err = SettingsError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for SettingId {
    type Error = SettingsError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SettingId> for String {
    fn from(value: SettingId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum SettingValue {
    Toggle(bool),
    FlagEnabled,
    Integer(i64),
    UnsignedInteger(u64),
    UnsignedIntegerOrChoice(UnsignedIntegerOrChoiceValue),
    Float(f64),
    String(String),
    StringList(Vec<String>),
    Json(serde_json::Value),
    Choice(String),
    Path(PathBuf),
    GpuOffload(GpuOffload),
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum UnsignedIntegerOrChoiceValue {
    UnsignedInteger(u64),
    Choice(String),
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode", content = "layers")]
pub enum GpuOffload {
    None,
    Auto,
    All,
    Layers(u64),
}

impl std::fmt::Display for SettingValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Toggle(value) => value.fmt(formatter),
            Self::FlagEnabled => formatter.write_str("enabled"),
            Self::Integer(value) => value.fmt(formatter),
            Self::UnsignedInteger(value) => value.fmt(formatter),
            Self::UnsignedIntegerOrChoice(UnsignedIntegerOrChoiceValue::UnsignedInteger(value)) => {
                value.fmt(formatter)
            }
            Self::UnsignedIntegerOrChoice(UnsignedIntegerOrChoiceValue::Choice(value)) => {
                value.fmt(formatter)
            }
            Self::Float(value) => value.fmt(formatter),
            Self::String(value) | Self::Choice(value) => value.fmt(formatter),
            Self::StringList(value) => serde_json::to_string(value)
                .unwrap_or_else(|_| "[]".to_owned())
                .fmt(formatter),
            Self::Json(value) => value.fmt(formatter),
            Self::Path(value) => value.display().fmt(formatter),
            Self::GpuOffload(GpuOffload::None) => formatter.write_str("none"),
            Self::GpuOffload(GpuOffload::Auto) => formatter.write_str("auto"),
            Self::GpuOffload(GpuOffload::All) => formatter.write_str("all"),
            Self::GpuOffload(GpuOffload::Layers(value)) => value.fmt(formatter),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SettingKind {
    Toggle,
    OneWayFlag,
    Integer {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        minimum: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        maximum: Option<i64>,
    },
    UnsignedInteger {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        minimum: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        maximum: Option<u64>,
    },
    UnsignedIntegerOrChoice {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        minimum: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        maximum: Option<u64>,
        choices: Vec<String>,
    },
    Float {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        minimum: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        maximum: Option<f64>,
    },
    String,
    StringList,
    JsonObject,
    Choice {
        choices: Vec<String>,
    },
    Path,
    GpuOffload,
}

impl SettingKind {
    pub fn parse(&self, id: &SettingId, raw: &str) -> Result<SettingValue, SettingsError> {
        let invalid = |reason: String| SettingsError::InvalidValue {
            setting_id: id.clone(),
            value: raw.to_owned(),
            reason,
        };
        match self {
            Self::Toggle => match raw {
                "true" | "on" | "enabled" => Ok(SettingValue::Toggle(true)),
                "false" | "off" | "disabled" => Ok(SettingValue::Toggle(false)),
                _ => Err(invalid("expected true/false or on/off".to_owned())),
            },
            Self::OneWayFlag => match raw {
                "true" | "on" | "enabled" => Ok(SettingValue::FlagEnabled),
                _ => Err(invalid(
                    "set this one-way flag to true/on, or unset it to inherit".to_owned(),
                )),
            },
            Self::Integer { minimum, maximum } => {
                let value = raw
                    .parse::<i64>()
                    .map_err(|_| invalid("expected a signed integer".to_owned()))?;
                validate_bounds(id, raw, value, *minimum, *maximum)?;
                Ok(SettingValue::Integer(value))
            }
            Self::UnsignedInteger { minimum, maximum } => {
                let value = raw
                    .parse::<u64>()
                    .map_err(|_| invalid("expected a non-negative integer".to_owned()))?;
                validate_bounds(id, raw, value, *minimum, *maximum)?;
                Ok(SettingValue::UnsignedInteger(value))
            }
            Self::UnsignedIntegerOrChoice {
                minimum,
                maximum,
                choices,
            } => {
                if choices.iter().any(|choice| choice == raw) {
                    return Ok(SettingValue::UnsignedIntegerOrChoice(
                        UnsignedIntegerOrChoiceValue::Choice(raw.to_owned()),
                    ));
                }
                let value = raw.parse::<u64>().map_err(|_| {
                    invalid(format!(
                        "expected a non-negative integer or one of: {}",
                        choices.join(", ")
                    ))
                })?;
                validate_bounds(id, raw, value, *minimum, *maximum)?;
                Ok(SettingValue::UnsignedIntegerOrChoice(
                    UnsignedIntegerOrChoiceValue::UnsignedInteger(value),
                ))
            }
            Self::Float { minimum, maximum } => {
                let value = raw
                    .parse::<f64>()
                    .map_err(|_| invalid("expected a finite number".to_owned()))?;
                if !value.is_finite() {
                    return Err(invalid("expected a finite number".to_owned()));
                }
                validate_bounds(id, raw, value, *minimum, *maximum)?;
                Ok(SettingValue::Float(value))
            }
            Self::String => (!raw.is_empty() && !raw.contains('\0'))
                .then(|| SettingValue::String(raw.to_owned()))
                .ok_or_else(|| invalid("expected a non-empty string without NUL bytes".to_owned())),
            Self::StringList => {
                let values = serde_json::from_str::<Vec<String>>(raw).map_err(|_| {
                    invalid("expected a JSON array of non-empty strings".to_owned())
                })?;
                (!values.is_empty()
                    && values
                        .iter()
                        .all(|value| !value.is_empty() && !value.contains('\0')))
                .then_some(SettingValue::StringList(values))
                .ok_or_else(|| {
                    invalid(
                        "expected a non-empty JSON array of non-empty strings without NUL bytes"
                            .to_owned(),
                    )
                })
            }
            Self::JsonObject => {
                let value = serde_json::from_str::<serde_json::Value>(raw)
                    .map_err(|error| invalid(format!("expected a JSON object: {error}")))?;
                value
                    .is_object()
                    .then_some(SettingValue::Json(value))
                    .ok_or_else(|| invalid("expected a JSON object".to_owned()))
            }
            Self::Choice { choices } => {
                ((choices.is_empty() && !raw.is_empty() && !raw.contains('\0'))
                    || choices.iter().any(|choice| choice == raw))
                .then(|| SettingValue::Choice(raw.to_owned()))
                .ok_or_else(|| invalid(format!("expected one of: {}", choices.join(", "))))
            }
            Self::Path => (!raw.is_empty() && !raw.contains('\0'))
                .then(|| SettingValue::Path(PathBuf::from(raw)))
                .ok_or_else(|| invalid("expected a non-empty path without NUL bytes".to_owned())),
            Self::GpuOffload => match raw {
                "none" => Ok(SettingValue::GpuOffload(GpuOffload::None)),
                "auto" => Ok(SettingValue::GpuOffload(GpuOffload::Auto)),
                "all" => Ok(SettingValue::GpuOffload(GpuOffload::All)),
                _ => raw
                    .parse::<u64>()
                    .map(|value| SettingValue::GpuOffload(GpuOffload::Layers(value)))
                    .map_err(|_| {
                        invalid("expected none, auto, all, or an exact layer count".to_owned())
                    }),
            },
        }
    }

    pub fn accepts(&self, id: &SettingId, value: &SettingValue) -> Result<(), SettingsError> {
        let parsed = self.parse(id, &value.to_string())?;
        let valid = matches!(
            (self, value, parsed),
            (
                Self::OneWayFlag,
                SettingValue::FlagEnabled,
                SettingValue::FlagEnabled
            ) | (
                Self::String,
                SettingValue::String(_),
                SettingValue::String(_)
            ) | (
                Self::StringList,
                SettingValue::StringList(_),
                SettingValue::StringList(_)
            ) | (
                Self::JsonObject,
                SettingValue::Json(_),
                SettingValue::Json(_)
            ) | (
                Self::Choice { .. },
                SettingValue::Choice(_),
                SettingValue::Choice(_)
            ) | (Self::Path, SettingValue::Path(_), SettingValue::Path(_))
                | (
                    Self::Toggle,
                    SettingValue::Toggle(_),
                    SettingValue::Toggle(_)
                )
                | (
                    Self::Integer { .. },
                    SettingValue::Integer(_),
                    SettingValue::Integer(_)
                )
                | (
                    Self::UnsignedInteger { .. },
                    SettingValue::UnsignedInteger(_),
                    SettingValue::UnsignedInteger(_)
                )
                | (
                    Self::UnsignedIntegerOrChoice { .. },
                    SettingValue::UnsignedIntegerOrChoice(_),
                    SettingValue::UnsignedIntegerOrChoice(_)
                )
                | (
                    Self::Float { .. },
                    SettingValue::Float(_),
                    SettingValue::Float(_)
                )
                | (
                    Self::GpuOffload,
                    SettingValue::GpuOffload(_),
                    SettingValue::GpuOffload(_)
                )
        );
        valid
            .then_some(())
            .ok_or_else(|| SettingsError::InvalidValue {
                setting_id: id.clone(),
                value: value.to_string(),
                reason: "value does not match the setting definition".to_owned(),
            })
    }
}

fn validate_bounds<T>(
    id: &SettingId,
    raw: &str,
    value: T,
    minimum: Option<T>,
    maximum: Option<T>,
) -> Result<(), SettingsError>
where
    T: PartialOrd + std::fmt::Display + Copy,
{
    if minimum.is_some_and(|minimum| value < minimum) {
        return Err(SettingsError::InvalidValue {
            setting_id: id.clone(),
            value: raw.to_owned(),
            reason: format!("must be at least {}", minimum.expect("checked")),
        });
    }
    if maximum.is_some_and(|maximum| value > maximum) {
        return Err(SettingsError::InvalidValue {
            setting_id: id.clone(),
            value: raw.to_owned(),
            reason: format!("must be at most {}", maximum.expect("checked")),
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingCategory {
    #[default]
    General,
    Downloads,
    Load,
    Generation,
    Reasoning,
    Prompt,
    KvMemory,
    Speculation,
    Cache,
    Advanced,
}

impl std::fmt::Display for SettingCategory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::General => "General",
            Self::Downloads => "Downloads",
            Self::Load => "Load",
            Self::Generation => "Generation",
            Self::Reasoning => "Reasoning",
            Self::Prompt => "Prompt",
            Self::KvMemory => "KV / Memory",
            Self::Speculation => "Speculation",
            Self::Cache => "Cache",
            Self::Advanced => "Advanced",
        })
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "scope")]
pub enum SettingScope {
    Global,
    Common,
    Engine { engine_id: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingDefinition {
    pub id: SettingId,
    pub label: String,
    pub description: String,
    pub kind: SettingKind,
    pub scope: SettingScope,
    #[serde(default)]
    pub category: SettingCategory,
    #[serde(default = "default_true")]
    pub supported: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsupported_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_default: Option<String>,
}

fn default_true() -> bool {
    true
}

impl SettingDefinition {
    pub fn parse(&self, raw: &str) -> Result<SettingValue, SettingsError> {
        self.kind.parse(&self.id, raw)
    }

    pub fn validate_value(&self, value: &SettingValue) -> Result<(), SettingsError> {
        self.kind.accepts(&self.id, value)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SettingsPatch(pub BTreeMap<SettingId, SettingValue>);

impl SettingsPatch {
    pub fn insert(&mut self, id: SettingId, value: SettingValue) {
        self.0.insert(id, value);
    }

    pub fn remove(&mut self, id: &SettingId) -> Option<SettingValue> {
        self.0.remove(id)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&SettingId, &SettingValue)> {
        self.0.iter()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SettingsState {
    pub version: u32,
    pub global_defaults: SettingsPatch,
    pub engine_defaults: BTreeMap<String, SettingsPatch>,
}

impl Default for SettingsState {
    fn default() -> Self {
        Self {
            version: SETTINGS_STATE_VERSION,
            global_defaults: SettingsPatch::default(),
            engine_defaults: BTreeMap::new(),
        }
    }
}

impl SettingsState {
    pub fn validate(&self) -> Result<(), SettingsError> {
        if self.version != SETTINGS_STATE_VERSION {
            return Err(SettingsError::UnsupportedStateVersion {
                found: self.version,
                supported: SETTINGS_STATE_VERSION,
            });
        }
        for id in self.global_defaults.0.keys() {
            if id
                .namespace()
                .is_some_and(|namespace| namespace != "server")
            {
                return Err(SettingsError::InvalidGlobalSetting(id.clone()));
            }
        }
        for (engine_id, patch) in &self.engine_defaults {
            crate::validate_engine_id(engine_id)?;
            for id in patch.0.keys() {
                if !id.applies_to_engine(engine_id) {
                    return Err(SettingsError::WrongEngineScope {
                        setting_id: id.clone(),
                        engine_id: engine_id.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    pub fn resolve(
        &self,
        profile_id: &ModelProfileId,
        engine_id: &str,
        profile_overrides: &SettingsPatch,
        invocation: &SettingsPatch,
        structured_path_base: &Path,
    ) -> Result<ResolvedSettings, SettingsError> {
        let mut effective = BTreeMap::new();
        apply_layer(
            &mut effective,
            &self.global_defaults,
            engine_id,
            SettingSource::GlobalDefault,
        );
        if let Some(defaults) = self.engine_defaults.get(engine_id) {
            apply_layer(
                &mut effective,
                defaults,
                engine_id,
                SettingSource::EngineDefault {
                    engine_id: engine_id.to_owned(),
                },
            );
        }
        apply_layer(
            &mut effective,
            profile_overrides,
            engine_id,
            SettingSource::ModelProfile {
                model_profile_id: profile_id.clone(),
            },
        );
        apply_layer(
            &mut effective,
            invocation,
            engine_id,
            SettingSource::Invocation,
        );
        resolve_structured_paths(&mut effective, structured_path_base)?;
        Ok(ResolvedSettings {
            engine_id: engine_id.to_owned(),
            model_profile_id: Some(profile_id.clone()),
            effective,
        })
    }
}

fn apply_layer(
    effective: &mut BTreeMap<SettingId, ResolvedSetting>,
    patch: &SettingsPatch,
    engine_id: &str,
    source: SettingSource,
) {
    suppress_inherited_semantic_alternatives(effective, patch, engine_id);
    for (id, value) in patch.iter() {
        if id.applies_to_engine(engine_id) {
            effective.insert(
                id.clone(),
                ResolvedSetting {
                    value: value.clone(),
                    source: source.clone(),
                },
            );
        }
    }
}

fn suppress_inherited_semantic_alternatives(
    effective: &mut BTreeMap<SettingId, ResolvedSetting>,
    patch: &SettingsPatch,
    engine_id: &str,
) {
    if engine_id != "llama.cpp" {
        return;
    }
    let contains = |id: &str| patch.0.keys().any(|candidate| candidate.as_str() == id);
    let mut suppress = Vec::new();

    if contains("llama.cpp.chat_template") {
        suppress.extend([
            "llama.cpp.chat_template_file",
            "llama.cpp.chat_template_sha256",
        ]);
    }
    if contains("llama.cpp.chat_template_file") {
        suppress.extend(["llama.cpp.chat_template", "llama.cpp.chat_template_sha256"]);
    }
    if contains("llama.cpp.cpu_moe_all") {
        suppress.push("llama.cpp.cpu_moe_layers");
    }
    if contains("llama.cpp.cpu_moe_layers") {
        suppress.push("llama.cpp.cpu_moe_all");
    }
    if matches!(
        patch
            .0
            .iter()
            .find(|(id, _)| id.as_str() == "llama.cpp.speculative_mode")
            .map(|(_, value)| value),
        Some(SettingValue::Choice(mode))
            if mode == "off" || mode == "draft-mtp" || mode.starts_with("ngram-")
    ) {
        suppress.extend([
            "llama.cpp.speculative_draft_model",
            "llama.cpp.speculative_draft_sha256",
        ]);
    }

    for id in suppress {
        if let Some(id) = effective
            .keys()
            .find(|candidate| candidate.as_str() == id)
            .cloned()
        {
            effective.remove(&id);
        }
    }
}

fn resolve_structured_paths(
    effective: &mut BTreeMap<SettingId, ResolvedSetting>,
    base: &Path,
) -> Result<(), SettingsError> {
    for (id, setting) in effective.iter_mut() {
        let SettingValue::Path(path) = &mut setting.value else {
            continue;
        };
        *path = resolve_structured_path(id, path, base)?;
    }
    Ok(())
}

fn resolve_structured_path(
    id: &SettingId,
    path: &Path,
    base: &Path,
) -> Result<PathBuf, SettingsError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    if !base.is_absolute() {
        return Err(SettingsError::InvalidValue {
            setting_id: id.clone(),
            value: path.display().to_string(),
            reason: "the structured-path base is not absolute".to_owned(),
        });
    }
    let mut resolved = base.to_path_buf();
    let mut depth = 0_usize;
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(part) => {
                resolved.push(part);
                depth += 1;
            }
            std::path::Component::ParentDir if depth > 0 => {
                resolved.pop();
                depth -= 1;
            }
            std::path::Component::ParentDir => {
                return Err(SettingsError::InvalidValue {
                    setting_id: id.clone(),
                    value: path.display().to_string(),
                    reason: format!(
                        "relative structured paths resolve beneath {} and cannot escape it",
                        base.display()
                    ),
                });
            }
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                return Err(SettingsError::InvalidValue {
                    setting_id: id.clone(),
                    value: path.display().to_string(),
                    reason: "path must be fully absolute or safely relative".to_owned(),
                });
            }
        }
    }
    Ok(resolved)
}

pub async fn bounded_setting_file_sha256(
    id: &SettingId,
    path: &Path,
    base: &Path,
    maximum_bytes: u64,
) -> Result<String, SettingsError> {
    let resolved = resolve_structured_path(id, path, base)?;
    let file =
        tokio::fs::File::open(&resolved)
            .await
            .map_err(|error| SettingsError::InvalidValue {
                setting_id: id.clone(),
                value: path.display().to_string(),
                reason: format!("could not open {}: {error}", resolved.display()),
            })?;
    let metadata = file
        .metadata()
        .await
        .map_err(|error| SettingsError::InvalidValue {
            setting_id: id.clone(),
            value: path.display().to_string(),
            reason: format!("could not inspect {}: {error}", resolved.display()),
        })?;
    if !metadata.is_file() || metadata.len() > maximum_bytes {
        return Err(SettingsError::InvalidValue {
            setting_id: id.clone(),
            value: path.display().to_string(),
            reason: format!("file must be regular and no larger than {maximum_bytes} bytes"),
        });
    }
    let mut file = file;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 8 * 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|error| SettingsError::InvalidValue {
                setting_id: id.clone(),
                value: path.display().to_string(),
                reason: format!("could not read {}: {error}", resolved.display()),
            })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "source")]
pub enum SettingSource {
    GlobalDefault,
    EngineDefault { engine_id: String },
    ModelProfile { model_profile_id: ModelProfileId },
    Invocation,
}

impl std::fmt::Display for SettingSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GlobalDefault => formatter.write_str("global-default"),
            Self::EngineDefault { engine_id } => write!(formatter, "engine-default:{engine_id}"),
            Self::ModelProfile { model_profile_id } => {
                write!(formatter, "model-profile:{model_profile_id}")
            }
            Self::Invocation => formatter.write_str("invocation"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedSetting {
    pub value: SettingValue,
    pub source: SettingSource,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResolvedSettings {
    pub engine_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_profile_id: Option<ModelProfileId>,
    #[serde(default)]
    pub effective: BTreeMap<SettingId, ResolvedSetting>,
}

impl ResolvedSettings {
    pub fn is_empty(&self) -> bool {
        self.effective.is_empty()
    }

    pub fn value(&self, id: &str) -> Option<&SettingValue> {
        self.effective
            .iter()
            .find_map(|(candidate, setting)| (candidate.as_str() == id).then_some(&setting.value))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SettingsSchema {
    pub engine_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<RuntimeId>,
    pub definitions: Vec<SettingDefinition>,
}

impl SettingsSchema {
    pub fn definition(&self, id: &SettingId) -> Option<&SettingDefinition> {
        self.definitions
            .iter()
            .find(|definition| &definition.id == id)
    }

    pub fn validate(&self, settings: &ResolvedSettings) -> Result<(), SettingsError> {
        for (id, setting) in &settings.effective {
            let definition = self
                .definition(id)
                .ok_or_else(|| SettingsError::UnknownSetting(id.clone()))?;
            if !definition.supported {
                return Err(SettingsError::UnsupportedSetting {
                    setting_id: id.clone(),
                    reason: definition.unsupported_reason.clone().unwrap_or_else(|| {
                        "the exact runtime does not advertise this setting".to_owned()
                    }),
                });
            }
            definition.validate_value(&setting.value)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
    lock_path: PathBuf,
}

impl SettingsStore {
    pub fn new(paths: &AppPaths) -> Self {
        Self {
            path: paths.settings_file.clone(),
            lock_path: paths.settings_lock_file.clone(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn read(&self) -> Result<SettingsState, StateStoreError> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || read_settings_state(&path))
            .await
            .map_err(|error| StateStoreError::Task(error.to_string()))?
    }

    pub async fn update<F, T>(&self, update: F) -> Result<T, StateStoreError>
    where
        F: FnOnce(&mut SettingsState) -> Result<T, SettingsError> + Send + 'static,
        T: Send + 'static,
    {
        let path = self.path.clone();
        let lock_path = self.lock_path.clone();
        tokio::task::spawn_blocking(move || {
            let _lock = lock_file(&lock_path)?;
            let mut state = read_settings_state(&path)?;
            let result = update(&mut state)?;
            state.validate()?;
            write_json_state(&path, &state)?;
            Ok(result)
        })
        .await
        .map_err(|error| StateStoreError::Task(error.to_string()))?
    }
}

fn read_settings_state(path: &Path) -> Result<SettingsState, StateStoreError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SettingsState::default());
        }
        Err(source) => {
            return Err(StateStoreError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let state: SettingsState =
        serde_json::from_slice(&bytes).map_err(|source| StateStoreError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    state.validate()?;
    Ok(state)
}

pub(crate) fn lock_file(path: &Path) -> Result<std::fs::File, StateStoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| StateStoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|source| StateStoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    lock.lock_exclusive()
        .map_err(|source| StateStoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(lock)
}

pub(crate) fn write_json_state<T: Serialize>(
    path: &Path,
    state: &T,
) -> Result<(), StateStoreError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|source| StateStoreError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let bytes = serde_json::to_vec_pretty(state).map_err(StateStoreError::Serialize)?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|source| StateStoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    temporary
        .write_all(&bytes)
        .and_then(|_| temporary.write_all(b"\n"))
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|source| StateStoreError::Io {
            path: temporary.path().to_path_buf(),
            source,
        })?;
    temporary
        .persist(path)
        .map_err(|error| StateStoreError::Io {
            path: path.to_path_buf(),
            source: error.error,
        })?;
    if let Ok(directory) = std::fs::File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn validate_setting_id(value: &str) -> Result<(), SettingsError> {
    if value.is_empty()
        || value.len() > 128
        || value.split('.').any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .next()
                    .is_some_and(|byte| byte.is_ascii_lowercase())
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
    {
        return Err(SettingsError::InvalidSettingId(value.to_owned()));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("setting ID `{0}` is invalid; use lowercase dot-separated identifiers")]
    InvalidSettingId(String),
    #[error("engine ID `{0}` is invalid")]
    InvalidEngineId(String),
    #[error("setting `{setting_id}` value `{value}` is invalid: {reason}")]
    InvalidValue {
        setting_id: SettingId,
        value: String,
        reason: String,
    },
    #[error("unknown setting `{0}`")]
    UnknownSetting(SettingId),
    #[error("setting `{setting_id}` is unsupported: {reason}")]
    UnsupportedSetting {
        setting_id: SettingId,
        reason: String,
    },
    #[error("global defaults cannot contain engine-specific setting `{0}`")]
    InvalidGlobalSetting(SettingId),
    #[error("setting `{setting_id}` does not belong to engine `{engine_id}`")]
    WrongEngineScope {
        setting_id: SettingId,
        engine_id: String,
    },
    #[error("state schema version {found} is unsupported; this build supports {supported}")]
    UnsupportedStateVersion { found: u32, supported: u32 },
    #[error("model profile ID `{0}` is invalid")]
    InvalidModelProfileId(String),
    #[error("model profile display name must be non-empty and at most 128 characters")]
    InvalidDisplayName,
    #[error("model profile `{0}` does not exist")]
    ModelProfileNotFound(ModelProfileId),
    #[error("model profile `{0}` already exists")]
    ModelProfileAlreadyExists(ModelProfileId),
    #[error("model profile is invalid: {0}")]
    InvalidModelProfile(String),
}

#[derive(Debug, thiserror::Error)]
pub enum StateStoreError {
    #[error("state I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("state at {path} is invalid JSON: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("could not serialize state: {0}")]
    Serialize(serde_json::Error),
    #[error(transparent)]
    Invalid(#[from] SettingsError),
    #[error("state task failed: {0}")]
    Task(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> SettingId {
        SettingId::new(value).expect("setting ID")
    }

    #[test]
    fn resolution_has_exact_four_layer_precedence() {
        let profile_id = ModelProfileId::new("quality").expect("profile ID");
        let mut state = SettingsState::default();
        state
            .global_defaults
            .insert(id("temperature"), SettingValue::Float(0.1));
        state
            .engine_defaults
            .entry("q27".to_owned())
            .or_default()
            .insert(id("temperature"), SettingValue::Float(0.2));
        let profile = SettingsPatch(BTreeMap::from([(
            id("temperature"),
            SettingValue::Float(0.3),
        )]));
        let invocation = SettingsPatch(BTreeMap::from([(
            id("temperature"),
            SettingValue::Float(0.4),
        )]));
        let resolved = state
            .resolve(
                &profile_id,
                "q27",
                &profile,
                &invocation,
                &std::env::temp_dir(),
            )
            .expect("resolve");
        assert_eq!(
            resolved.value("temperature"),
            Some(&SettingValue::Float(0.4))
        );
        assert_eq!(
            resolved.effective[&id("temperature")].source,
            SettingSource::Invocation
        );
    }
}
