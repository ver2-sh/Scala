use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::{AppPaths, ModelId};

pub const SERVE_PROFILES_STATE_VERSION: u32 = 3;

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LoadSettingId(String);

impl LoadSettingId {
    pub fn new(value: impl Into<String>) -> Result<Self, LoadSettingsError> {
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

impl std::fmt::Display for LoadSettingId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::str::FromStr for LoadSettingId {
    type Err = LoadSettingsError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for LoadSettingId {
    type Error = LoadSettingsError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<LoadSettingId> for String {
    fn from(value: LoadSettingId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum LoadSettingValue {
    Toggle(bool),
    FlagEnabled,
    Integer(i64),
    UnsignedInteger(u64),
    UnsignedIntegerOrChoice(UnsignedIntegerOrChoiceValue),
    Float(f64),
    String(String),
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

impl std::fmt::Display for LoadSettingValue {
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
pub enum LoadSettingKind {
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
    Choice {
        /// An empty list is an unresolved/open choice at the generic adapter
        /// layer. Exact schemas replace it with a non-empty, closed list or
        /// mark the setting unsupported before validating resolved settings.
        choices: Vec<String>,
    },
    Path,
    GpuOffload,
}

impl LoadSettingKind {
    pub fn parse(
        &self,
        id: &LoadSettingId,
        raw: &str,
    ) -> Result<LoadSettingValue, LoadSettingsError> {
        let invalid = |reason: String| LoadSettingsError::InvalidValue {
            setting_id: id.clone(),
            value: raw.to_owned(),
            reason,
        };
        match self {
            Self::Toggle => match raw {
                "true" | "on" | "enabled" => Ok(LoadSettingValue::Toggle(true)),
                "false" | "off" | "disabled" => Ok(LoadSettingValue::Toggle(false)),
                _ => Err(invalid("expected true/false or on/off".to_owned())),
            },
            Self::OneWayFlag => match raw {
                "true" | "on" | "enabled" => Ok(LoadSettingValue::FlagEnabled),
                _ => Err(invalid(
                    "this is a one-way flag; set it to true/on or unset it to restore the upstream default"
                        .to_owned(),
                )),
            },
            Self::Integer { minimum, maximum } => {
                let value = raw
                    .parse::<i64>()
                    .map_err(|_| invalid("expected a signed integer".to_owned()))?;
                validate_bounds(id, raw, value, *minimum, *maximum)?;
                Ok(LoadSettingValue::Integer(value))
            }
            Self::UnsignedInteger { minimum, maximum } => {
                let value = raw
                    .parse::<u64>()
                    .map_err(|_| invalid("expected a non-negative integer".to_owned()))?;
                validate_bounds(id, raw, value, *minimum, *maximum)?;
                Ok(LoadSettingValue::UnsignedInteger(value))
            }
            Self::UnsignedIntegerOrChoice {
                minimum,
                maximum,
                choices,
            } => {
                if choices.iter().any(|choice| choice == raw) {
                    return Ok(LoadSettingValue::UnsignedIntegerOrChoice(
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
                Ok(LoadSettingValue::UnsignedIntegerOrChoice(
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
                Ok(LoadSettingValue::Float(value))
            }
            Self::String => {
                if raw.is_empty() || raw.contains('\0') {
                    Err(invalid("expected a non-empty string without NUL bytes".to_owned()))
                } else {
                    Ok(LoadSettingValue::String(raw.to_owned()))
                }
            }
            Self::Choice { choices } => {
                if (choices.is_empty() && !raw.is_empty() && !raw.contains('\0'))
                    || choices.iter().any(|choice| choice == raw)
                {
                    Ok(LoadSettingValue::Choice(raw.to_owned()))
                } else {
                    Err(invalid(format!("expected one of: {}", choices.join(", "))))
                }
            }
            Self::Path => {
                if raw.is_empty() || raw.contains('\0') {
                    Err(invalid("expected a non-empty path without NUL bytes".to_owned()))
                } else {
                    Ok(LoadSettingValue::Path(PathBuf::from(raw)))
                }
            }
            Self::GpuOffload => match raw {
                "none" => Ok(LoadSettingValue::GpuOffload(GpuOffload::None)),
                "auto" => Ok(LoadSettingValue::GpuOffload(GpuOffload::Auto)),
                "all" => Ok(LoadSettingValue::GpuOffload(GpuOffload::All)),
                _ => raw
                    .parse::<u64>()
                    .map(|value| LoadSettingValue::GpuOffload(GpuOffload::Layers(value)))
                    .map_err(|_| invalid("expected none, auto, all, or an exact layer count".to_owned())),
            },
        }
    }

    pub fn accepts(
        &self,
        id: &LoadSettingId,
        value: &LoadSettingValue,
    ) -> Result<(), LoadSettingsError> {
        let raw = value.to_string();
        let valid = match (self, value) {
            (Self::Toggle, LoadSettingValue::Toggle(_))
            | (Self::OneWayFlag, LoadSettingValue::FlagEnabled) => true,
            (Self::String, LoadSettingValue::String(value)) => {
                !value.is_empty() && !value.contains('\0')
            }
            (Self::Path, LoadSettingValue::Path(value)) => {
                let value = value.to_string_lossy();
                !value.is_empty() && !value.contains('\0')
            }
            (Self::GpuOffload, LoadSettingValue::GpuOffload(_)) => true,
            (Self::Integer { minimum, maximum }, LoadSettingValue::Integer(value)) => {
                validate_bounds(id, &raw, *value, *minimum, *maximum)?;
                true
            }
            (
                Self::UnsignedInteger { minimum, maximum },
                LoadSettingValue::UnsignedInteger(value),
            ) => {
                validate_bounds(id, &raw, *value, *minimum, *maximum)?;
                true
            }
            (
                Self::UnsignedIntegerOrChoice {
                    minimum, maximum, ..
                },
                LoadSettingValue::UnsignedIntegerOrChoice(
                    UnsignedIntegerOrChoiceValue::UnsignedInteger(value),
                ),
            ) => {
                validate_bounds(id, &raw, *value, *minimum, *maximum)?;
                true
            }
            (
                Self::UnsignedIntegerOrChoice { choices, .. },
                LoadSettingValue::UnsignedIntegerOrChoice(UnsignedIntegerOrChoiceValue::Choice(
                    value,
                )),
            ) => choices.contains(value),
            (Self::Float { minimum, maximum }, LoadSettingValue::Float(value)) => {
                if !value.is_finite() {
                    false
                } else {
                    validate_bounds(id, &raw, *value, *minimum, *maximum)?;
                    true
                }
            }
            (Self::Choice { choices }, LoadSettingValue::Choice(value)) => {
                (choices.is_empty() && !value.is_empty() && !value.contains('\0'))
                    || choices.contains(value)
            }
            _ => false,
        };
        valid
            .then_some(())
            .ok_or_else(|| LoadSettingsError::InvalidValue {
                setting_id: id.clone(),
                value: raw,
                reason: "value does not match the setting definition".to_owned(),
            })
    }
}

fn validate_bounds<T>(
    id: &LoadSettingId,
    raw: &str,
    value: T,
    minimum: Option<T>,
    maximum: Option<T>,
) -> Result<(), LoadSettingsError>
where
    T: PartialOrd + std::fmt::Display + Copy,
{
    if minimum.is_some_and(|minimum| value < minimum) {
        return Err(LoadSettingsError::InvalidValue {
            setting_id: id.clone(),
            value: raw.to_owned(),
            reason: format!("must be at least {}", minimum.expect("checked")),
        });
    }
    if maximum.is_some_and(|maximum| value > maximum) {
        return Err(LoadSettingsError::InvalidValue {
            setting_id: id.clone(),
            value: raw.to_owned(),
            reason: format!("must be at most {}", maximum.expect("checked")),
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "scope")]
pub enum LoadSettingScope {
    Common,
    Engine { engine_id: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoadSettingDefinition {
    pub id: LoadSettingId,
    pub label: String,
    pub description: String,
    pub kind: LoadSettingKind,
    pub scope: LoadSettingScope,
    #[serde(default = "default_true")]
    pub supported: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsupported_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommendation: Option<String>,
}

fn default_true() -> bool {
    true
}

impl LoadSettingDefinition {
    pub fn parse(&self, raw: &str) -> Result<LoadSettingValue, LoadSettingsError> {
        self.kind.parse(&self.id, raw)
    }

    pub fn validate_value(&self, value: &LoadSettingValue) -> Result<(), LoadSettingsError> {
        self.kind.accepts(&self.id, value)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LoadSettingsPatch(pub BTreeMap<LoadSettingId, LoadSettingValue>);

impl LoadSettingsPatch {
    pub fn insert(&mut self, id: LoadSettingId, value: LoadSettingValue) {
        self.0.insert(id, value);
    }

    pub fn remove(&mut self, id: &LoadSettingId) -> Option<LoadSettingValue> {
        self.0.remove(id)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&LoadSettingId, &LoadSettingValue)> {
        self.0.iter()
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ServeProfileName(String);

impl ServeProfileName {
    pub fn new(value: impl Into<String>) -> Result<Self, LoadSettingsError> {
        let value = value.into();
        validate_profile_name(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ServeProfileName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::str::FromStr for ServeProfileName {
    type Err = LoadSettingsError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for ServeProfileName {
    type Error = LoadSettingsError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ServeProfileName> for String {
    fn from(value: ServeProfileName) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServeProfilesState {
    pub version: u32,
    pub global_defaults: LoadSettingsPatch,
    pub engine_defaults: BTreeMap<String, LoadSettingsPatch>,
    pub model_defaults: BTreeMap<ModelId, LoadSettingsPatch>,
    pub profiles: BTreeMap<ServeProfileName, crate::ServeProfile>,
    pub model_assignments: BTreeMap<ModelId, ServeProfileName>,
    #[serde(default)]
    pub builder_profile_assignments: BTreeMap<ModelId, String>,
    #[serde(default)]
    pub raw_profile_models: BTreeSet<ModelId>,
}

impl Default for ServeProfilesState {
    fn default() -> Self {
        Self {
            version: SERVE_PROFILES_STATE_VERSION,
            global_defaults: LoadSettingsPatch::default(),
            engine_defaults: BTreeMap::new(),
            model_defaults: BTreeMap::new(),
            profiles: BTreeMap::new(),
            model_assignments: BTreeMap::new(),
            builder_profile_assignments: BTreeMap::new(),
            raw_profile_models: BTreeSet::new(),
        }
    }
}

impl ServeProfilesState {
    pub fn validate(&self) -> Result<(), LoadSettingsError> {
        if self.version != SERVE_PROFILES_STATE_VERSION {
            return Err(LoadSettingsError::UnsupportedStateVersion {
                found: self.version,
                supported: SERVE_PROFILES_STATE_VERSION,
            });
        }
        for id in self.global_defaults.0.keys() {
            validate_setting_id(id.as_str())?;
            if id.namespace().is_some() {
                return Err(LoadSettingsError::InvalidGlobalSetting(id.clone()));
            }
        }
        for (engine_id, patch) in &self.engine_defaults {
            validate_engine_id(engine_id)?;
            for id in patch.0.keys() {
                validate_setting_id(id.as_str())?;
                if !id.applies_to_engine(engine_id) {
                    return Err(LoadSettingsError::WrongEngineScope {
                        setting_id: id.clone(),
                        engine_id: engine_id.clone(),
                    });
                }
            }
        }
        for name in self.profiles.keys() {
            validate_profile_name(name.as_str())?;
        }
        for patch in self.model_defaults.values() {
            for id in patch.0.keys() {
                validate_setting_id(id.as_str())?;
            }
        }
        for (name, profile) in &self.profiles {
            for id in profile.load.settings.0.keys() {
                validate_setting_id(id.as_str())?;
            }
            profile
                .validate()
                .map_err(LoadSettingsError::InvalidServeProfile)?;
            if profile.id != name.as_str() {
                return Err(LoadSettingsError::InvalidServeProfile(format!(
                    "persisted Serve Profile `{}` is stored under mismatched key `{name}`",
                    profile.id
                )));
            }
            if profile.read_only {
                return Err(LoadSettingsError::InvalidServeProfile(
                    "persisted user Serve Profiles must be mutable".to_owned(),
                ));
            }
        }
        for (model_id, profile) in &self.model_assignments {
            if !self.profiles.contains_key(profile) {
                return Err(LoadSettingsError::MissingAssignedProfile {
                    model_id: model_id.clone(),
                    profile: profile.clone(),
                });
            }
        }
        for profile_id in self.builder_profile_assignments.values() {
            crate::validate_profile_id(profile_id)
                .map_err(LoadSettingsError::InvalidServeProfile)?;
        }
        for model_id in self.model_assignments.keys() {
            if self.builder_profile_assignments.contains_key(model_id) {
                return Err(LoadSettingsError::InvalidServeProfile(format!(
                    "model `{model_id}` has both local and Builder Serve Profile assignments"
                )));
            }
        }
        for model_id in self.raw_profile_models.iter() {
            if self.model_assignments.contains_key(model_id)
                || self.builder_profile_assignments.contains_key(model_id)
            {
                return Err(LoadSettingsError::InvalidServeProfile(format!(
                    "model `{model_id}` has both a Serve Profile assignment and None/raw selection"
                )));
            }
        }
        Ok(())
    }

    pub fn resolve(
        &self,
        model_id: &ModelId,
        engine_id: &str,
        invocation_profile: Option<&ServeProfileName>,
        invocation: &LoadSettingsPatch,
        structured_path_base: &Path,
    ) -> Result<ResolvedLoadSettings, LoadSettingsError> {
        let selected_profile = invocation_profile
            .cloned()
            .or_else(|| self.model_assignments.get(model_id).cloned());
        let mut effective = BTreeMap::new();
        apply_layer(
            &mut effective,
            &self.global_defaults,
            engine_id,
            LoadSettingSource::GlobalDefault,
        );
        if let Some(settings) = self.engine_defaults.get(engine_id) {
            apply_layer(
                &mut effective,
                settings,
                engine_id,
                LoadSettingSource::EngineDefault {
                    engine_id: engine_id.to_owned(),
                },
            );
        }
        if let Some(settings) = self.model_defaults.get(model_id) {
            apply_layer(
                &mut effective,
                settings,
                engine_id,
                LoadSettingSource::ModelDefault {
                    model_id: model_id.clone(),
                },
            );
        }
        if let Some(profile_name) = &selected_profile {
            let profile = self
                .profiles
                .get(profile_name)
                .ok_or_else(|| LoadSettingsError::ProfileNotFound(profile_name.clone()))?;
            apply_layer(
                &mut effective,
                &profile.load.settings,
                engine_id,
                LoadSettingSource::ServeProfile {
                    profile_id: profile.id.clone(),
                },
            );
        }
        apply_layer(
            &mut effective,
            invocation,
            engine_id,
            LoadSettingSource::Invocation,
        );
        resolve_structured_paths(&mut effective, structured_path_base)?;
        Ok(ResolvedLoadSettings {
            engine_id: engine_id.to_owned(),
            selected_profile,
            effective,
        })
    }

    pub fn create_profile(&mut self, name: ServeProfileName) -> Result<(), LoadSettingsError> {
        if name.as_str() == "none" {
            return Err(LoadSettingsError::InvalidServeProfile(
                "`none` is reserved for None / Raw runtime defaults".to_owned(),
            ));
        }
        if self.profiles.contains_key(&name) {
            return Err(LoadSettingsError::ProfileAlreadyExists(name));
        }
        self.profiles
            .insert(name.clone(), crate::ServeProfile::local(name.as_str()));
        Ok(())
    }

    pub fn delete_profile(&mut self, name: &ServeProfileName) -> Result<(), LoadSettingsError> {
        if !self.profiles.contains_key(name) {
            return Err(LoadSettingsError::ProfileNotFound(name.clone()));
        }
        let models = self
            .model_assignments
            .iter()
            .filter_map(|(model, profile)| (profile == name).then_some(model.clone()))
            .collect::<Vec<_>>();
        if !models.is_empty() {
            return Err(LoadSettingsError::ProfileAssigned {
                profile: name.clone(),
                models,
            });
        }
        self.profiles.remove(name);
        Ok(())
    }

    pub fn assign_profile(
        &mut self,
        model_id: ModelId,
        profile: Option<ServeProfileName>,
    ) -> Result<(), LoadSettingsError> {
        if let Some(profile) = profile {
            if !self.profiles.contains_key(&profile) {
                return Err(LoadSettingsError::ProfileNotFound(profile));
            }
            self.builder_profile_assignments.remove(&model_id);
            self.raw_profile_models.remove(&model_id);
            self.model_assignments.insert(model_id, profile);
        } else {
            self.model_assignments.remove(&model_id);
            self.builder_profile_assignments.remove(&model_id);
            self.raw_profile_models.insert(model_id);
        }
        Ok(())
    }

    pub fn assign_builder_profile(&mut self, model_id: ModelId, profile_id: String) {
        self.model_assignments.remove(&model_id);
        self.raw_profile_models.remove(&model_id);
        self.builder_profile_assignments
            .insert(model_id, profile_id);
    }

    pub fn inherit_recommended_profile(&mut self, model_id: &ModelId) {
        self.model_assignments.remove(model_id);
        self.builder_profile_assignments.remove(model_id);
        self.raw_profile_models.remove(model_id);
    }
}

fn resolve_structured_paths(
    effective: &mut BTreeMap<LoadSettingId, ResolvedLoadSetting>,
    base: &Path,
) -> Result<(), LoadSettingsError> {
    for (id, setting) in effective.iter_mut() {
        let LoadSettingValue::Path(path) = &mut setting.value else {
            continue;
        };
        if path.is_absolute() {
            continue;
        }
        if !base.is_absolute() {
            return Err(LoadSettingsError::InvalidValue {
                setting_id: id.clone(),
                value: path.display().to_string(),
                reason: "Norted's structured-path base is not absolute".to_owned(),
            });
        }

        let original = path.clone();
        let mut resolved = base.to_path_buf();
        let mut relative_depth = 0_usize;
        for component in original.components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::Normal(part) => {
                    resolved.push(part);
                    relative_depth += 1;
                }
                std::path::Component::ParentDir if relative_depth > 0 => {
                    resolved.pop();
                    relative_depth -= 1;
                }
                std::path::Component::ParentDir => {
                    return Err(LoadSettingsError::InvalidValue {
                        setting_id: id.clone(),
                        value: original.display().to_string(),
                        reason: format!(
                            "relative structured paths resolve beneath {} and cannot escape it with `..`; use an absolute path for another location",
                            base.display()
                        ),
                    });
                }
                std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                    return Err(LoadSettingsError::InvalidValue {
                        setting_id: id.clone(),
                        value: original.display().to_string(),
                        reason: "structured paths must be fully absolute or relative without a root/drive prefix"
                            .to_owned(),
                    });
                }
            }
        }
        *path = resolved;
    }
    Ok(())
}

fn apply_layer(
    effective: &mut BTreeMap<LoadSettingId, ResolvedLoadSetting>,
    patch: &LoadSettingsPatch,
    engine_id: &str,
    source: LoadSettingSource,
) {
    for (id, value) in patch.iter() {
        if id.applies_to_engine(engine_id) {
            effective.insert(
                id.clone(),
                ResolvedLoadSetting {
                    value: value.clone(),
                    source: source.clone(),
                },
            );
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "source")]
pub enum LoadSettingSource {
    GlobalDefault,
    EngineDefault { engine_id: String },
    ModelDefault { model_id: ModelId },
    ServeProfile { profile_id: String },
    Invocation,
}

impl std::fmt::Display for LoadSettingSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GlobalDefault => formatter.write_str("global-default"),
            Self::EngineDefault { engine_id } => write!(formatter, "engine-default:{engine_id}"),
            Self::ModelDefault { model_id } => write!(formatter, "model-default:{model_id}"),
            Self::ServeProfile { profile_id } => write!(formatter, "serve-profile:{profile_id}"),
            Self::Invocation => formatter.write_str("invocation"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedLoadSetting {
    pub value: LoadSettingValue,
    pub source: LoadSettingSource,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResolvedLoadSettings {
    pub engine_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_profile: Option<ServeProfileName>,
    #[serde(default)]
    pub effective: BTreeMap<LoadSettingId, ResolvedLoadSetting>,
}

impl ResolvedLoadSettings {
    pub fn is_empty(&self) -> bool {
        self.effective.is_empty()
    }

    pub fn value(&self, id: &str) -> Option<&LoadSettingValue> {
        self.effective
            .iter()
            .find_map(|(candidate, setting)| (candidate.as_str() == id).then_some(&setting.value))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LoadSettingsSchema {
    pub engine_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<crate::RuntimeId>,
    pub definitions: Vec<LoadSettingDefinition>,
}

impl LoadSettingsSchema {
    pub fn definition(&self, id: &LoadSettingId) -> Option<&LoadSettingDefinition> {
        self.definitions
            .iter()
            .find(|definition| &definition.id == id)
    }

    pub fn validate(&self, settings: &ResolvedLoadSettings) -> Result<(), LoadSettingsError> {
        for (id, setting) in &settings.effective {
            let definition = self
                .definition(id)
                .ok_or_else(|| LoadSettingsError::UnknownSetting(id.clone()))?;
            if !definition.supported {
                return Err(LoadSettingsError::UnsupportedSetting {
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
pub struct ServeProfilesStore {
    path: PathBuf,
    lock_path: PathBuf,
}

impl ServeProfilesStore {
    pub fn new(paths: &AppPaths) -> Self {
        Self {
            path: paths.serve_profiles_file.clone(),
            lock_path: paths.serve_profiles_lock_file.clone(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn read(&self) -> Result<ServeProfilesState, ServeProfilesError> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || read_state(&path))
            .await
            .map_err(|error| ServeProfilesError::Task(error.to_string()))?
    }

    pub async fn update<F, T>(&self, update: F) -> Result<T, ServeProfilesError>
    where
        F: FnOnce(&mut ServeProfilesState) -> Result<T, LoadSettingsError> + Send + 'static,
        T: Send + 'static,
    {
        let path = self.path.clone();
        let lock_path = self.lock_path.clone();
        tokio::task::spawn_blocking(move || {
            if let Some(parent) = lock_path.parent() {
                std::fs::create_dir_all(parent).map_err(|source| ServeProfilesError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            let lock = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&lock_path)
                .map_err(|source| ServeProfilesError::Io {
                    path: lock_path.clone(),
                    source,
                })?;
            lock.lock_exclusive()
                .map_err(|source| ServeProfilesError::Io {
                    path: lock_path,
                    source,
                })?;
            let mut state = read_state(&path)?;
            let result = update(&mut state)?;
            state.validate()?;
            write_state(&path, &state)?;
            Ok(result)
        })
        .await
        .map_err(|error| ServeProfilesError::Task(error.to_string()))?
    }
}

fn read_state(path: &Path) -> Result<ServeProfilesState, ServeProfilesError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ServeProfilesState::default());
        }
        Err(source) => {
            return Err(ServeProfilesError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let state: ServeProfilesState =
        serde_json::from_slice(&bytes).map_err(|source| ServeProfilesError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    state.validate()?;
    Ok(state)
}

fn write_state(path: &Path, state: &ServeProfilesState) -> Result<(), ServeProfilesError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|source| ServeProfilesError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let bytes = serde_json::to_vec_pretty(state).map_err(ServeProfilesError::Serialize)?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|source| ServeProfilesError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    temporary
        .write_all(&bytes)
        .and_then(|_| temporary.write_all(b"\n"))
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|source| ServeProfilesError::Io {
            path: temporary.path().to_path_buf(),
            source,
        })?;
    temporary
        .persist(path)
        .map_err(|error| ServeProfilesError::Io {
            path: path.to_path_buf(),
            source: error.error,
        })?;
    if let Ok(directory) = std::fs::File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn validate_setting_id(value: &str) -> Result<(), LoadSettingsError> {
    if value.is_empty() || value.len() > 128 {
        return Err(LoadSettingsError::InvalidSettingId(value.to_owned()));
    }
    if value.split('.').any(|part| !valid_id_component(part)) {
        return Err(LoadSettingsError::InvalidSettingId(value.to_owned()));
    }
    Ok(())
}

fn validate_engine_id(value: &str) -> Result<(), LoadSettingsError> {
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
    {
        return Err(LoadSettingsError::InvalidEngineId(value.to_owned()));
    }
    Ok(())
}

fn valid_id_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn validate_profile_name(value: &str) -> Result<(), LoadSettingsError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        return Err(LoadSettingsError::InvalidProfileName(value.to_owned()));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum LoadSettingsError {
    #[error("load setting ID `{0}` is invalid; use lowercase dot-separated identifiers")]
    InvalidSettingId(String),
    #[error("engine ID `{0}` is invalid")]
    InvalidEngineId(String),
    #[error("profile name `{0}` is invalid; use 1-64 lowercase letters, digits, '-' or '_'")]
    InvalidProfileName(String),
    #[error("load setting `{setting_id}` value `{value}` is invalid: {reason}")]
    InvalidValue {
        setting_id: LoadSettingId,
        value: String,
        reason: String,
    },
    #[error("unknown load setting `{0}`")]
    UnknownSetting(LoadSettingId),
    #[error("load setting `{setting_id}` is unsupported: {reason}")]
    UnsupportedSetting {
        setting_id: LoadSettingId,
        reason: String,
    },
    #[error("global defaults cannot contain engine-specific setting `{0}`")]
    InvalidGlobalSetting(LoadSettingId),
    #[error("setting `{setting_id}` does not belong in defaults for engine `{engine_id}`")]
    WrongEngineScope {
        setting_id: LoadSettingId,
        engine_id: String,
    },
    #[error("profile `{0}` does not exist")]
    ProfileNotFound(ServeProfileName),
    #[error("profile `{0}` already exists")]
    ProfileAlreadyExists(ServeProfileName),
    #[error(
        "profile `{profile}` is assigned to model(s): {models:?}; clear those assignments before deleting it"
    )]
    ProfileAssigned {
        profile: ServeProfileName,
        models: Vec<ModelId>,
    },
    #[error("profile `{profile}` is assigned to model `{model_id}` but does not exist")]
    MissingAssignedProfile {
        model_id: ModelId,
        profile: ServeProfileName,
    },
    #[error("invalid Serve Profile: {0}")]
    InvalidServeProfile(String),
    #[error(
        "Serve Profile state schema version {found} is unsupported; this build supports {supported}"
    )]
    UnsupportedStateVersion { found: u32, supported: u32 },
}

#[derive(Debug, thiserror::Error)]
pub enum ServeProfilesError {
    #[error("Serve Profile state I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Serve Profile state at {path} is invalid JSON: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("could not serialize Serve Profile state: {0}")]
    Serialize(serde_json::Error),
    #[error(transparent)]
    Invalid(#[from] LoadSettingsError),
    #[error("Serve Profile state task failed: {0}")]
    Task(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> LoadSettingId {
        LoadSettingId::new(value).expect("setting ID")
    }

    #[test]
    fn setting_id_preserves_a_dotted_engine_namespace() {
        let setting = id("llama.cpp.threads");
        assert_eq!(setting.namespace(), Some("llama.cpp"));
        assert!(setting.applies_to_engine("llama.cpp"));
        assert!(!setting.applies_to_engine("q27"));
    }

    #[test]
    fn resolver_uses_the_required_precedence_and_filters_other_engines() {
        let model_id = ModelId("model".to_owned());
        let profile = ServeProfileName::new("coding").expect("profile name");
        let mut state = ServeProfilesState::default();
        state
            .global_defaults
            .insert(id("context_length"), LoadSettingValue::UnsignedInteger(1));
        state
            .engine_defaults
            .entry("llama.cpp".to_owned())
            .or_default()
            .insert(id("context_length"), LoadSettingValue::UnsignedInteger(2));
        state
            .model_defaults
            .entry(model_id.clone())
            .or_default()
            .insert(id("context_length"), LoadSettingValue::UnsignedInteger(3));
        let mut serve_profile = crate::ServeProfile::local(profile.as_str());
        serve_profile.load.settings = LoadSettingsPatch(BTreeMap::from([
            (id("context_length"), LoadSettingValue::UnsignedInteger(4)),
            (id("q27.kv_fp16"), LoadSettingValue::FlagEnabled),
        ]));
        state.profiles.insert(profile.clone(), serve_profile);
        state.model_assignments.insert(model_id.clone(), profile);
        let invocation = LoadSettingsPatch(BTreeMap::from([(
            id("context_length"),
            LoadSettingValue::UnsignedInteger(5),
        )]));

        let resolved = state
            .resolve(
                &model_id,
                "llama.cpp",
                None,
                &invocation,
                &std::env::temp_dir(),
            )
            .expect("resolve");
        assert_eq!(
            resolved.value("context_length"),
            Some(&LoadSettingValue::UnsignedInteger(5))
        );
        assert_eq!(
            resolved.effective[&id("context_length")].source,
            LoadSettingSource::Invocation
        );
        assert!(!resolved.effective.contains_key(&id("q27.kv_fp16")));
        assert!(
            state.profiles[&ServeProfileName::new("coding").expect("name")]
                .load
                .settings
                .0
                .contains_key(&id("q27.kv_fp16"))
        );
    }

    #[test]
    fn removing_a_higher_layer_value_falls_through() {
        let model_id = ModelId("model".to_owned());
        let mut state = ServeProfilesState::default();
        state.global_defaults.insert(
            id("parallel_requests"),
            LoadSettingValue::UnsignedInteger(2),
        );
        let model = state.model_defaults.entry(model_id.clone()).or_default();
        model.insert(
            id("parallel_requests"),
            LoadSettingValue::UnsignedInteger(8),
        );
        model.remove(&id("parallel_requests"));

        let resolved = state
            .resolve(
                &model_id,
                "llama.cpp",
                None,
                &LoadSettingsPatch::default(),
                &std::env::temp_dir(),
            )
            .expect("resolve");
        assert_eq!(
            resolved.value("parallel_requests"),
            Some(&LoadSettingValue::UnsignedInteger(2))
        );
    }

    #[test]
    fn assigned_profile_cannot_be_deleted() {
        let model = ModelId("model".to_owned());
        let profile = ServeProfileName::new("coding").expect("profile name");
        let mut state = ServeProfilesState::default();
        state
            .create_profile(profile.clone())
            .expect("create profile");
        state
            .assign_profile(model.clone(), Some(profile.clone()))
            .expect("assign profile");
        assert!(matches!(
            state.delete_profile(&profile),
            Err(LoadSettingsError::ProfileAssigned { models, .. }) if models == vec![model]
        ));
        assert!(state.profiles.contains_key(&profile));
    }

    #[test]
    fn invocation_overrides_do_not_mutate_persisted_state() {
        let state = ServeProfilesState::default();
        let before = state.clone();
        let invocation = LoadSettingsPatch(BTreeMap::from([(
            id("context_length"),
            LoadSettingValue::UnsignedInteger(8192),
        )]));
        let _ = state
            .resolve(
                &ModelId("model".to_owned()),
                "llama.cpp",
                None,
                &invocation,
                &std::env::temp_dir(),
            )
            .expect("resolve invocation");
        assert_eq!(state, before);
    }

    #[test]
    fn relative_structured_paths_resolve_beneath_the_stable_data_directory() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let data_dir = temporary.path().join("data");
        let model = ModelId("model".to_owned());
        let mut state = ServeProfilesState::default();
        state
            .model_defaults
            .entry(model.clone())
            .or_default()
            .insert(
                id("q27.prefix_cache_path"),
                LoadSettingValue::Path(PathBuf::from("cache/q27")),
            );

        let resolved = state
            .resolve(
                &model,
                "q27",
                None,
                &LoadSettingsPatch::default(),
                &data_dir,
            )
            .expect("resolve relative path");
        assert_eq!(
            resolved.value("q27.prefix_cache_path"),
            Some(&LoadSettingValue::Path(data_dir.join("cache/q27")))
        );
        assert_eq!(
            state.model_defaults[&model].0[&id("q27.prefix_cache_path")],
            LoadSettingValue::Path(PathBuf::from("cache/q27"))
        );
    }

    #[test]
    fn relative_structured_paths_cannot_escape_the_data_directory() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let invocation = LoadSettingsPatch(BTreeMap::from([(
            id("q27.prefix_cache_path"),
            LoadSettingValue::Path(PathBuf::from("../outside")),
        )]));
        let error = ServeProfilesState::default()
            .resolve(
                &ModelId("model".to_owned()),
                "q27",
                None,
                &invocation,
                temporary.path(),
            )
            .expect_err("path escape");
        assert!(error.to_string().contains("cannot escape"));
    }

    #[test]
    fn exact_schema_rejects_a_configured_unsupported_setting() {
        let setting_id = id("llama.cpp.flash_attention");
        let schema = LoadSettingsSchema {
            engine_id: "llama.cpp".to_owned(),
            runtime_id: None,
            definitions: vec![LoadSettingDefinition {
                id: setting_id.clone(),
                label: "Flash attention".to_owned(),
                description: String::new(),
                kind: LoadSettingKind::Choice {
                    choices: vec!["auto".to_owned(), "on".to_owned(), "off".to_owned()],
                },
                scope: LoadSettingScope::Engine {
                    engine_id: "llama.cpp".to_owned(),
                },
                supported: false,
                unsupported_reason: Some("not advertised by this executable".to_owned()),
                unit: None,
                upstream_default: None,
                recommendation: None,
            }],
        };
        let settings = ResolvedLoadSettings {
            engine_id: "llama.cpp".to_owned(),
            selected_profile: None,
            effective: BTreeMap::from([(
                setting_id,
                ResolvedLoadSetting {
                    value: LoadSettingValue::Choice("on".to_owned()),
                    source: LoadSettingSource::Invocation,
                },
            )]),
        };

        assert!(matches!(
            schema.validate(&settings),
            Err(LoadSettingsError::UnsupportedSetting { .. })
        ));
    }

    #[tokio::test]
    async fn profile_store_round_trips_an_atomic_update() {
        let temporary = tempfile::tempdir().expect("temporary profile state");
        let root = temporary.path();
        let paths = AppPaths {
            config_dir: root.join("config"),
            config_file: root.join("config/config.toml"),
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            cache_dir: root.join("cache"),
            log_dir: root.join("logs"),
            runtimes_dir: root.join("data/runtimes"),
            runtime_cache_dir: root.join("cache/runtime-packs"),
            runtime_selections_file: root.join("data/runtime-selections.json"),
            serve_profiles_file: root.join("data/serve-profiles.json"),
            serve_profiles_lock_file: root.join("data/.serve-profiles.lock"),
        };
        let store = ServeProfilesStore::new(&paths);
        let profile = ServeProfileName::new("long-context").expect("profile name");
        let written = profile.clone();
        store
            .update(move |state| {
                state.create_profile(written.clone())?;
                state
                    .profiles
                    .get_mut(&written)
                    .expect("created profile")
                    .load
                    .settings
                    .insert(
                        id("context_length"),
                        LoadSettingValue::UnsignedInteger(131_072),
                    );
                Ok(())
            })
            .await
            .expect("atomic update");
        store
            .update(|state| {
                state.global_defaults.insert(
                    id("parallel_requests"),
                    LoadSettingValue::UnsignedInteger(2),
                );
                Ok(())
            })
            .await
            .expect("replace existing state atomically");
        let state = store.read().await.expect("read stored state");
        assert_eq!(
            state.profiles[&profile]
                .load
                .settings
                .0
                .get(&id("context_length")),
            Some(&LoadSettingValue::UnsignedInteger(131_072))
        );
        assert_eq!(
            state.global_defaults.0[&id("parallel_requests")],
            LoadSettingValue::UnsignedInteger(2)
        );
        assert!(paths.serve_profiles_file.is_file());
    }
}
