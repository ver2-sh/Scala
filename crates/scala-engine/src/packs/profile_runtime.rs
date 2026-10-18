//! Model Profile-owned runtime targets, independent of artifact identity.
use super::*;
use scala_core::{ModelProfilesState, SettingsError};

impl RuntimePackManager {
    /// Persist only this profile's exact runtime pin. Clearing remains possible
    /// when its old runtime or artifact is missing. No runtime is installed or
    /// launched, and artifact/format selections are never rewritten here.
    pub async fn set_profile_runtime(
        &self,
        profile: &ModelProfile,
        model: Option<&ModelArtifact>,
        runtime_id: Option<RuntimeId>,
        settings: &SettingsState,
        base: &Path,
    ) -> Result<ModelProfilesState, RuntimePackError> {
        let _operation = self.operation.try_lock().map_err(|_| {
            RuntimePackError::Selection(
                "Busy: runtime mutation or benchmark reservation in progress".into(),
            )
        })?;
        let _lease = match runtime_id.as_ref() {
            Some(id) => Some(self.acquire_runtime_lease(id).await?),
            None => None,
        };
        let original_hash = profile.content_hash();
        let mut candidate = profile.clone();
        candidate.runtime_id = runtime_id;
        if candidate.runtime_id.is_some() {
            let model = model
                .filter(|m| m.id == candidate.model_id)
                .ok_or_else(|| {
                    RuntimePackError::Selection(format!(
                        "Bound artifact for Model Profile {} is unavailable",
                        profile.id
                    ))
                })?;
            self.validate_profile_settings(settings, &candidate, model, &candidate.overrides, base)
                .await?;
        }
        self.store
            .model_profiles()
            .update(move |state| {
                let current = state
                    .profiles
                    .get_mut(&candidate.id)
                    .ok_or_else(|| SettingsError::ModelProfileNotFound(candidate.id.clone()))?;
                if current.content_hash() != original_hash {
                    return Err(SettingsError::InvalidModelProfile(
                        "Model Profile changed during runtime selection; refresh and retry".into(),
                    ));
                }
                current.runtime_id = candidate.runtime_id;
                Ok(state.clone())
            })
            .await
            .map_err(|error| RuntimePackError::Selection(error.to_string()))
    }

    /// Read-time reference labels used by runtime inventory/removal controls.
    pub(super) async fn add_profile_bindings(
        &self,
        installed: &mut [InstalledRuntimeStatus],
        warnings: &mut Vec<String>,
    ) {
        match self.store.model_profiles().read().await {
            Ok(profiles) => {
                for status in installed {
                    status.selected_for.extend(
                        profiles
                            .profiles
                            .values()
                            .filter(|p| {
                                p.runtime_id.as_ref() == Some(&status.runtime.manifest.runtime_id)
                            })
                            .map(|p| format!("Model Profile {}", p.id)),
                    );
                }
            }
            Err(error) => warnings.push(format!(
                "Model Profile runtime references unavailable: {error}"
            )),
        }
    }

    pub async fn settings_schema_for_profile_from_local_inspection(
        &self,
        model: &ModelArtifact,
        profile: &ModelProfile,
        settings: Option<&scala_core::ResolvedSettings>,
        inspection: &RuntimeLocalInspection,
    ) -> Result<(RuntimeSelection, scala_core::SettingsSchema), RuntimePackError> {
        let mut selection = self.resolve_from_local_inspection_with_runtime(
            model,
            Some(profile.engine_id.as_str()),
            profile.runtime_id.as_ref(),
            settings,
            inspection,
        )?;
        if profile.runtime_id.is_some() {
            selection.source = RuntimeSelectionSource::ModelProfileOverride;
        }
        let adapter = self
            .registry
            .get(profile.engine_id.as_str())
            .ok_or_else(|| {
                RuntimePackError::Selection(crate::unregistered_engine_reason(
                    profile.engine_id.as_str(),
                ))
            })?;
        let schema = adapter
            .settings_schema(&selection.runtime, model, &inspection.host, settings)
            .await
            .map_err(RuntimePackError::Adapter)?;
        Ok((selection, schema))
    }
}
