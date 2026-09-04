use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::settings::{lock_file, write_json_state};
use crate::{AppPaths, ModelId, SettingsError, SettingsPatch, StateStoreError};

pub const MODEL_PROFILES_STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    #[default]
    Primary,
    Auxiliary,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ModelProfileId(String);

impl ModelProfileId {
    pub fn new(value: impl Into<String>) -> Result<Self, SettingsError> {
        let value = value.into();
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
            return Err(SettingsError::InvalidModelProfileId(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ModelProfileId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::str::FromStr for ModelProfileId {
    type Err = SettingsError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for ModelProfileId {
    type Error = SettingsError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ModelProfileId> for String {
    fn from(value: ModelProfileId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct EngineId(String);

impl EngineId {
    pub fn new(value: impl Into<String>) -> Result<Self, SettingsError> {
        let value = value.into();
        validate_engine_id(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for EngineId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::str::FromStr for EngineId {
    type Err = SettingsError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for EngineId {
    type Error = SettingsError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<EngineId> for String {
    fn from(value: EngineId) -> Self {
        value.0
    }
}

pub fn validate_engine_id(value: &str) -> Result<(), SettingsError> {
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
    {
        return Err(SettingsError::InvalidEngineId(value.to_owned()));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProfile {
    pub id: ModelProfileId,
    pub display_name: String,
    pub model_id: ModelId,
    pub engine_id: EngineId,
    #[serde(default)]
    pub role: ModelRole,
    #[serde(default)]
    pub overrides: SettingsPatch,
}

impl ModelProfile {
    pub fn new(
        id: ModelProfileId,
        display_name: impl Into<String>,
        model_id: ModelId,
        engine_id: EngineId,
    ) -> Result<Self, SettingsError> {
        let profile = Self {
            id,
            display_name: display_name.into(),
            model_id,
            engine_id,
            role: ModelRole::default(),
            overrides: SettingsPatch::default(),
        };
        profile.validate()?;
        Ok(profile)
    }

    pub fn validate(&self) -> Result<(), SettingsError> {
        ModelProfileId::new(self.id.as_str())?;
        validate_engine_id(self.engine_id.as_str())?;
        if self.display_name.trim().is_empty() || self.display_name.chars().count() > 128 {
            return Err(SettingsError::InvalidDisplayName);
        }
        for id in self.overrides.0.keys() {
            if !id.applies_to_engine(self.engine_id.as_str()) {
                return Err(SettingsError::WrongEngineScope {
                    setting_id: id.clone(),
                    engine_id: self.engine_id.to_string(),
                });
            }
        }
        Ok(())
    }

    pub fn content_hash(&self) -> String {
        #[derive(Serialize)]
        struct StableContent<'a> {
            id: &'a ModelProfileId,
            model_id: &'a ModelId,
            engine_id: &'a EngineId,
            role: ModelRole,
            overrides: &'a SettingsPatch,
        }

        let bytes = serde_json::to_vec(&StableContent {
            id: &self.id,
            model_id: &self.model_id,
            engine_id: &self.engine_id,
            role: self.role,
            overrides: &self.overrides,
        })
        .expect("Model Profile content is serializable");
        format!("{:x}", Sha256::digest(bytes))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelProfilesState {
    pub version: u32,
    pub profiles: BTreeMap<ModelProfileId, ModelProfile>,
}

impl Default for ModelProfilesState {
    fn default() -> Self {
        Self {
            version: MODEL_PROFILES_STATE_VERSION,
            profiles: BTreeMap::new(),
        }
    }
}

impl ModelProfilesState {
    pub fn validate(&self) -> Result<(), SettingsError> {
        if self.version != MODEL_PROFILES_STATE_VERSION {
            return Err(SettingsError::UnsupportedStateVersion {
                found: self.version,
                supported: MODEL_PROFILES_STATE_VERSION,
            });
        }
        for (id, profile) in &self.profiles {
            profile.validate()?;
            if &profile.id != id {
                return Err(SettingsError::InvalidModelProfile(format!(
                    "profile `{}` is stored under mismatched key `{id}`",
                    profile.id
                )));
            }
        }
        Ok(())
    }

    pub fn create(
        &mut self,
        id: ModelProfileId,
        display_name: impl Into<String>,
        model_id: ModelId,
        engine_id: EngineId,
    ) -> Result<(), SettingsError> {
        if self.profiles.contains_key(&id) {
            return Err(SettingsError::ModelProfileAlreadyExists(id));
        }
        let profile = ModelProfile::new(id.clone(), display_name, model_id, engine_id)?;
        self.profiles.insert(id, profile);
        Ok(())
    }

    pub fn duplicate(
        &mut self,
        source: &ModelProfileId,
        destination: ModelProfileId,
        display_name: impl Into<String>,
    ) -> Result<(), SettingsError> {
        if self.profiles.contains_key(&destination) {
            return Err(SettingsError::ModelProfileAlreadyExists(destination));
        }
        let mut profile = self
            .profiles
            .get(source)
            .cloned()
            .ok_or_else(|| SettingsError::ModelProfileNotFound(source.clone()))?;
        profile.id = destination.clone();
        profile.display_name = display_name.into();
        profile.validate()?;
        self.profiles.insert(destination, profile);
        Ok(())
    }

    pub fn delete(&mut self, id: &ModelProfileId) -> Result<ModelProfile, SettingsError> {
        self.profiles
            .remove(id)
            .ok_or_else(|| SettingsError::ModelProfileNotFound(id.clone()))
    }
}

#[derive(Debug, Clone)]
pub struct ModelProfilesStore {
    path: PathBuf,
    lock_path: PathBuf,
}

impl ModelProfilesStore {
    pub fn new(paths: &AppPaths) -> Self {
        Self {
            path: paths.model_profiles_file.clone(),
            lock_path: paths.model_profiles_lock_file.clone(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn read(&self) -> Result<ModelProfilesState, StateStoreError> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || read_state(&path))
            .await
            .map_err(|error| StateStoreError::Task(error.to_string()))?
    }

    pub async fn update<F, T>(&self, update: F) -> Result<T, StateStoreError>
    where
        F: FnOnce(&mut ModelProfilesState) -> Result<T, SettingsError> + Send + 'static,
        T: Send + 'static,
    {
        let path = self.path.clone();
        let lock_path = self.lock_path.clone();
        tokio::task::spawn_blocking(move || {
            let _lock = lock_file(&lock_path)?;
            let mut state = read_state(&path)?;
            let result = update(&mut state)?;
            state.validate()?;
            write_json_state(&path, &state)?;
            Ok(result)
        })
        .await
        .map_err(|error| StateStoreError::Task(error.to_string()))?
    }
}

fn read_state(path: &Path) -> Result<ModelProfilesState, StateStoreError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ModelProfilesState::default());
        }
        Err(source) => {
            return Err(StateStoreError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let state: ModelProfilesState =
        serde_json::from_slice(&bytes).map_err(|source| StateStoreError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    state.validate()?;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SettingId, SettingValue};

    #[test]
    fn two_profiles_can_bind_the_same_artifact_with_different_overrides() {
        let model = ModelId("same-artifact".to_owned());
        let engine = EngineId::new("q27").expect("engine ID");
        let mut state = ModelProfilesState::default();
        for id in ["quality", "fast"] {
            state
                .create(
                    ModelProfileId::new(id).expect("profile ID"),
                    id,
                    model.clone(),
                    engine.clone(),
                )
                .expect("create profile");
        }
        state
            .profiles
            .get_mut(&ModelProfileId::new("quality").expect("profile ID"))
            .expect("quality")
            .overrides
            .insert(
                SettingId::new("q27.temperature").expect("setting ID"),
                SettingValue::Float(0.8),
            );
        assert_eq!(state.profiles.len(), 2);
        assert!(
            state
                .profiles
                .values()
                .all(|profile| profile.model_id == model)
        );
        assert_ne!(
            state.profiles[&ModelProfileId::new("quality").expect("profile ID")].content_hash(),
            state.profiles[&ModelProfileId::new("fast").expect("profile ID")].content_hash()
        );
        let quality = &state.profiles[&ModelProfileId::new("quality").expect("profile ID")];
        let content_hash = quality.content_hash();
        let mut renamed = quality.clone();
        renamed.display_name = "Renamed display label".to_owned();
        assert_eq!(renamed.content_hash(), content_hash);
    }
}
