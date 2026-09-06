use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use crate::{AppPaths, ModelProfileId, RuntimeId};

pub const SETTINGS_STATE_VERSION: u32 = 3;

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
        self.namespace() == Some(engine_id)
    }

    pub fn applies_to_scope(&self, scope: &SettingScope) -> bool {
        match scope {
            SettingScope::Server => self.namespace() == Some("server"),
            SettingScope::Runtime { engine_id } => self.applies_to_engine(engine_id),
        }
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
    pub fn constraints(&self) -> String {
        fn bounds<T: std::fmt::Display>(minimum: &Option<T>, maximum: &Option<T>) -> String {
            match (minimum, maximum) {
                (Some(low), Some(high)) => format!(" ({low} to {high})"),
                (Some(low), None) => format!(" (at least {low})"),
                (None, Some(high)) => format!(" (at most {high})"),
                (None, None) => String::new(),
            }
        }
        match self {
            Self::Toggle => "true or false".to_owned(),
            Self::OneWayFlag => "enabled; Inherit removes the flag".to_owned(),
            Self::Integer { minimum, maximum } => format!("integer{}", bounds(minimum, maximum)),
            Self::UnsignedInteger { minimum, maximum } => {
                format!("non-negative integer{}", bounds(minimum, maximum))
            }
            Self::UnsignedIntegerOrChoice {
                minimum,
                maximum,
                choices,
            } => {
                let mut text = format!("non-negative integer{}", bounds(minimum, maximum));
                if !choices.is_empty() {
                    text.push_str(&format!(" or {}", choices.join(", ")));
                }
                text
            }
            Self::Float { minimum, maximum } => {
                format!("finite number{}", bounds(minimum, maximum))
            }
            Self::String => "non-empty text".to_owned(),
            Self::StringList => "JSON array of non-empty strings".to_owned(),
            Self::JsonObject => "JSON object".to_owned(),
            Self::Choice { choices } if choices.is_empty() => "non-empty choice".to_owned(),
            Self::Choice { choices } => format!("one of: {}", choices.join(", ")),
            Self::Path => "file path".to_owned(),
            Self::GpuOffload => "none, auto, all, or an exact layer count".to_owned(),
        }
    }

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
    Server,
    Runtime { engine_id: String },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingDefaultSource {
    Norted,
    Runtime,
    Model,
    Derived,
    StartupDynamic,
}

impl std::fmt::Display for SettingDefaultSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Norted => "server execution policy",
            Self::Runtime => "runtime default",
            Self::Model => "model-dependent runtime default",
            Self::Derived => "derived runtime default",
            Self::StartupDynamic => "runtime startup policy",
        })
    }
}

/// An authoritative runtime-base value or genuine runtime policy proved or
/// deliberately owned by the adapter.
///
/// The value is safe for clients to render without interpreting prose. A
/// Norted-owned scalar may also be materialized into the launch contract.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct SettingDefaultPreview {
    pub value: String,
    pub source: SettingDefaultSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl SettingDefaultPreview {
    pub fn new(value: impl Into<String>, source: SettingDefaultSource) -> Self {
        Self {
            value: value.into(),
            source,
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
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
    pub default_preview: Option<SettingDefaultPreview>,
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

    pub fn validate_scope(&self) -> Result<(), SettingsError> {
        if self.id.applies_to_scope(&self.scope) {
            Ok(())
        } else {
            Err(SettingsError::DefinitionScopeMismatch {
                setting_id: self.id.clone(),
                scope: self.scope.clone(),
            })
        }
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
    pub server_settings: SettingsPatch,
    /// Explicit Settings overrides, independently owned by each engine. The serialized
    /// field name is retained as-is so existing user state does not need rewriting.
    pub runtime_defaults: BTreeMap<String, SettingsPatch>,
}

impl Default for SettingsState {
    fn default() -> Self {
        Self {
            version: SETTINGS_STATE_VERSION,
            server_settings: SettingsPatch::default(),
            runtime_defaults: BTreeMap::new(),
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
        for id in self.server_settings.0.keys() {
            if id.namespace() != Some("server") {
                return Err(SettingsError::InvalidServerSetting(id.clone()));
            }
        }
        for (engine_id, patch) in &self.runtime_defaults {
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
        let mut configured = self
            .resolve_runtime_defaults(engine_id, structured_path_base)?
            .configured;
        apply_layer(
            &mut configured,
            profile_overrides,
            engine_id,
            SettingSource::ModelProfile {
                model_profile_id: profile_id.clone(),
            },
        )?;
        apply_layer(
            &mut configured,
            invocation,
            engine_id,
            SettingSource::Invocation,
        )?;
        resolve_structured_paths(&mut configured, structured_path_base)?;
        Ok(ResolvedSettings {
            engine_id: engine_id.to_owned(),
            model_profile_id: Some(profile_id.clone()),
            configured,
            effective: BTreeMap::new(),
        })
    }

    /// Resolves independent, explicit Settings overrides for one engine.
    pub fn resolve_runtime_defaults(
        &self,
        engine_id: &str,
        structured_path_base: &Path,
    ) -> Result<ResolvedSettings, SettingsError> {
        let mut effective = BTreeMap::new();
        if let Some(defaults) = self.runtime_defaults.get(engine_id) {
            apply_layer(
                &mut effective,
                defaults,
                engine_id,
                SettingSource::SettingsOverride,
            )?;
        }
        resolve_structured_paths(&mut effective, structured_path_base)?;
        Ok(ResolvedSettings {
            engine_id: engine_id.to_owned(),
            model_profile_id: None,
            configured: effective,
            effective: BTreeMap::new(),
        })
    }
}

fn apply_layer(
    effective: &mut BTreeMap<SettingId, ResolvedSetting>,
    patch: &SettingsPatch,
    engine_id: &str,
    source: SettingSource,
) -> Result<(), SettingsError> {
    for (id, value) in patch.iter() {
        if !id.applies_to_engine(engine_id) {
            return Err(SettingsError::WrongEngineScope {
                setting_id: id.clone(),
                engine_id: engine_id.to_owned(),
            });
        }
        effective.insert(
            id.clone(),
            ResolvedSetting {
                value: value.clone(),
                source: source.clone(),
            },
        );
    }
    Ok(())
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
    RuntimeDefault,
    SettingsOverride,
    ModelProfile { model_profile_id: ModelProfileId },
    Invocation,
}

impl std::fmt::Display for SettingSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RuntimeDefault => formatter.write_str("runtime default"),
            Self::SettingsOverride => formatter.write_str("Settings override"),
            Self::ModelProfile { .. } => formatter.write_str("model profile"),
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
    /// Explicit values after Runtime customization, Model Profile, and
    /// invocation precedence. Runtime adapters consume only this map.
    #[serde(default)]
    pub configured: BTreeMap<SettingId, ResolvedSetting>,
    /// Complete authoritative values or genuine unresolved runtime policies
    /// for presentation and provenance.
    #[serde(default)]
    pub effective: BTreeMap<SettingId, EffectiveSetting>,
}

impl ResolvedSettings {
    pub fn is_empty(&self) -> bool {
        self.configured.is_empty()
    }

    pub fn value(&self, id: &str) -> Option<&SettingValue> {
        self.configured
            .iter()
            .find_map(|(candidate, setting)| (candidate.as_str() == id).then_some(&setting.value))
    }

    /// Looks up a protocol-level concept through this resolved runtime's own
    /// namespace. This never falls back to an unqualified or foreign ID.
    pub fn runtime_value(&self, suffix: &str) -> Option<&SettingValue> {
        let id = format!("{}.{}", self.engine_id, suffix);
        self.value(&id)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct EffectiveSetting {
    pub value: String,
    pub source: SettingSource,
    /// Pre-start policy or value when authoritative runtime observation
    /// changed the effective value. Absence means startup did not change the
    /// configured effective value, or the runtime reported a new setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SettingsSchema {
    pub engine_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<RuntimeId>,
    pub definitions: Vec<SettingDefinition>,
}

impl SettingsSchema {
    pub fn validate_contract(&self) -> Result<(), SettingsError> {
        crate::validate_engine_id(&self.engine_id)?;
        for definition in &self.definitions {
            definition.validate_scope()?;
            match (self.engine_id.as_str(), &definition.scope) {
                ("server", SettingScope::Server) => {}
                ("server", SettingScope::Runtime { .. }) => {
                    return Err(SettingsError::InvalidServerSetting(definition.id.clone()));
                }
                (_, SettingScope::Server) => {
                    return Err(SettingsError::WrongEngineScope {
                        setting_id: definition.id.clone(),
                        engine_id: self.engine_id.clone(),
                    });
                }
                (_, SettingScope::Runtime { engine_id }) if engine_id == &self.engine_id => {}
                (_, SettingScope::Runtime { .. }) => {
                    return Err(SettingsError::WrongEngineScope {
                        setting_id: definition.id.clone(),
                        engine_id: self.engine_id.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    pub fn definition(&self, id: &SettingId) -> Option<&SettingDefinition> {
        self.definitions
            .iter()
            .find(|definition| &definition.id == id)
    }

    /// Preserve stale local keys in editors without claiming that the runtime supports them.
    pub fn retain_override_definitions(&mut self, settings: &ResolvedSettings) {
        for id in settings.configured.keys() {
            if self.definition(id).is_some() {
                continue;
            }
            self.definitions.push(SettingDefinition {
                id: id.clone(), label: id.to_string(),
                description: "Stored override is not supported by the selected runtime. Use Inherit to remove it.".to_owned(),
                kind: SettingKind::String,
                scope: SettingScope::Runtime { engine_id: self.engine_id.clone() },
                category: SettingCategory::Advanced, supported: false,
                unsupported_reason: Some("No definition in the selected runtime schema".to_owned()),
                unit: None, default_preview: None,
            });
        }
    }

    pub fn validate(&self, settings: &ResolvedSettings) -> Result<(), SettingsError> {
        self.validate_contract()?;
        if settings.engine_id != self.engine_id {
            return Err(SettingsError::InvalidModelProfile(format!(
                "resolved settings belong to engine `{}`, not `{}`",
                settings.engine_id, self.engine_id
            )));
        }
        for (id, setting) in &settings.configured {
            if !id.applies_to_engine(&self.engine_id) {
                return Err(SettingsError::WrongEngineScope {
                    setting_id: id.clone(),
                    engine_id: self.engine_id.clone(),
                });
            }
            let definition =
                self.definition(id)
                    .ok_or_else(|| SettingsError::UnavailableSetting {
                        setting_id: id.clone(),
                        engine_id: self.engine_id.clone(),
                    })?;
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

    /// Materializes adapter-owned scalar defaults into the launch contract.
    /// Optional text/path/schema defaults remain absent so `none` is never
    /// mistaken for a literal user value.
    pub fn materialize_runtime_configuration(
        &self,
        settings: &mut ResolvedSettings,
    ) -> Result<(), SettingsError> {
        self.validate_contract()?;
        for definition in self
            .definitions
            .iter()
            .filter(|definition| definition.supported)
        {
            if settings.configured.contains_key(&definition.id)
                || matches!(
                    definition.kind,
                    SettingKind::String
                        | SettingKind::StringList
                        | SettingKind::JsonObject
                        | SettingKind::Path
                )
            {
                continue;
            }
            let Some(runtime_default) = definition.default_preview.as_ref() else {
                continue;
            };
            if runtime_default.source != SettingDefaultSource::Norted {
                continue;
            }
            let value = definition.parse(&runtime_default.value)?;
            settings.configured.insert(
                definition.id.clone(),
                ResolvedSetting {
                    value,
                    source: SettingSource::RuntimeDefault,
                },
            );
        }
        Ok(())
    }

    /// Builds the single authoritative effective-value view used by clients
    /// and provenance. Genuine runtime policies such as `auto` remain values;
    /// derivation information is secondary metadata and never replaces them.
    pub fn materialize_effective(
        &self,
        settings: &mut ResolvedSettings,
    ) -> Result<(), SettingsError> {
        self.validate_contract()?;
        let mut effective = BTreeMap::new();
        for definition in self
            .definitions
            .iter()
            .filter(|definition| definition.supported)
        {
            let Some(runtime_default) = definition.default_preview.as_ref() else {
                // Unknown before startup is distinct from unsupported. Do not invent a value.
                continue;
            };
            if is_ambiguous_effective_value(&runtime_default.value) {
                return Err(SettingsError::AmbiguousRuntimeDefault {
                    setting_id: definition.id.clone(),
                    value: runtime_default.value.clone(),
                });
            }
            effective.insert(
                definition.id.clone(),
                EffectiveSetting {
                    value: runtime_default.value.clone(),
                    source: SettingSource::RuntimeDefault,
                    requested_value: None,
                    detail: runtime_default.detail.clone(),
                },
            );
        }
        for (id, setting) in &settings.configured {
            let detail = if setting.source == SettingSource::RuntimeDefault {
                effective.get(id).and_then(|setting| setting.detail.clone())
            } else {
                None
            };
            effective.insert(
                id.clone(),
                EffectiveSetting {
                    value: setting.value.to_string(),
                    source: setting.source.clone(),
                    requested_value: None,
                    detail,
                },
            );
        }
        settings.effective = effective;
        Ok(())
    }
}

fn is_ambiguous_effective_value(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    normalized.is_empty()
        || matches!(
            normalized.as_str(),
            "default"
                | "inherited"
                | "automatic"
                | "automatic slots"
                | "automatic offload"
                | "host-selected"
                | "runtime fallback"
                | "runtime-selected load mode"
                | "template-detected"
                | "template-defined"
                | "architecture-selected"
                | "request-sized"
        )
        || normalized.starts_with("launch-sized")
        || normalized.starts_with("runtime/model")
        || normalized.starts_with("model/thinking-mode")
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
    #[error(
        "setting `{setting_id}` is unavailable in the selected `{engine_id}` runtime/model schema"
    )]
    UnavailableSetting {
        setting_id: SettingId,
        engine_id: String,
    },
    #[error("setting `{setting_id}` is unsupported: {reason}")]
    UnsupportedSetting {
        setting_id: SettingId,
        reason: String,
    },
    #[error("supported setting `{setting_id}` has ambiguous runtime default `{value}`")]
    AmbiguousRuntimeDefault {
        setting_id: SettingId,
        value: String,
    },
    #[error("server settings cannot contain inference setting `{0}`")]
    InvalidServerSetting(SettingId),
    #[error("setting definition `{setting_id}` does not belong to its declared scope {scope:?}")]
    DefinitionScopeMismatch {
        setting_id: SettingId,
        scope: SettingScope,
    },
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
    fn resolution_has_exact_three_layer_precedence() {
        let profile_id = ModelProfileId::new("quality").expect("profile ID");
        let mut state = SettingsState::default();
        state
            .runtime_defaults
            .entry("q27".to_owned())
            .or_default()
            .insert(id("q27.temperature"), SettingValue::Float(0.2));
        let profile = SettingsPatch(BTreeMap::from([(
            id("q27.temperature"),
            SettingValue::Float(0.3),
        )]));
        let invocation = SettingsPatch(BTreeMap::from([(
            id("q27.temperature"),
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
            resolved.value("q27.temperature"),
            Some(&SettingValue::Float(0.4))
        );
        assert_eq!(
            resolved.configured[&id("q27.temperature")].source,
            SettingSource::Invocation
        );
    }
}
