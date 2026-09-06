use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use norted_core::{
    AppPaths, ArtifactFormat, AvailableRuntime, EngineInstallation, HostCapabilities,
    InstalledRuntime, ModelArtifact, ModelId, ModelProfile, RUNTIME_MANIFEST_SCHEMA_VERSION,
    ResolvedSettings, RuntimeAcquisitionMethod, RuntimeCompatibility, RuntimeId, RuntimeIdentity,
    RuntimeManifest, RuntimeOperationProgress, RuntimePackageIdentity, RuntimeProbeObservation,
    RuntimeReleaseChannel, RuntimeSelection, RuntimeSelectionSource, RuntimeSelections,
    RuntimeUpdatePreference, RuntimeUpdateState, SettingId, SettingsError, SettingsPatch,
    SettingsState, effective_cmake_configuration_arguments,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock, broadcast};

use crate::catalog::{
    CatalogError, GitHubComparisonStatus, RuntimeCatalog, RuntimeCatalogEntry,
    RuntimeCatalogProvider, RuntimeCatalogSnapshot, compatibility_for, detect_host_capabilities,
};
use crate::installer::{
    RuntimeInstallError, RuntimeInstaller, SourceBuildPrerequisiteEvaluationKey,
};
use crate::store::{
    RuntimeLease, RuntimeStore, RuntimeStoreError, RuntimeStoreIssue, RuntimeStoreSnapshot,
};
use crate::{
    CompatibilityDecision, EngineError, EngineFeature, EngineRegistry, InstallationState,
    ModelServingCapabilities,
};

#[derive(Debug, thiserror::Error)]
pub enum RuntimePackError {
    #[error(transparent)]
    Store(#[from] RuntimeStoreError),
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error(transparent)]
    Install(#[from] RuntimeInstallError),
    #[error("runtime `{0}` is not installed")]
    NotInstalled(RuntimeId),
    #[error("runtime `{runtime_id}` is incompatible: {reason}")]
    Incompatible {
        runtime_id: RuntimeId,
        reason: String,
    },
    #[error(
        "runtime `{runtime_id}` is selected for {selection}; clear or remap that selection first"
    )]
    Selected {
        runtime_id: RuntimeId,
        selection: String,
    },
    #[error("runtime `{0}` is currently running and cannot be removed")]
    Active(RuntimeId),
    #[error("runtime selection failed: {0}")]
    Selection(String),
    #[error("engine adapter failed: {0}")]
    Adapter(#[from] EngineError),
    #[error(transparent)]
    Settings(#[from] SettingsError),
}

#[derive(Debug, Clone)]
pub struct ModelProfileEngineSwitchCandidate {
    pub overrides: SettingsPatch,
    pub removed: Vec<SettingId>,
    pub resolved: ResolvedSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledRuntimeStatus {
    pub runtime: InstalledRuntime,
    /// Newest local lineage for this engine; equally recent variants share the badge.
    pub latest_installed: bool,
    pub compatibility: RuntimeCompatibility,
    pub selected_for: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeListSnapshot {
    pub host: HostCapabilities,
    pub selections: RuntimeSelections,
    pub installed: Vec<InstalledRuntimeStatus>,
    pub warnings: Vec<String>,
}

/// A fail-soft, offline view of existing runtime state. Missing store and
/// selection files are represented by empty defaults; malformed independent
/// state is retained as a structured error without hiding valid runtimes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeLocalInspection {
    pub host: HostCapabilities,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selections: Option<RuntimeSelections>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selections_error: Option<String>,
    pub installed: Vec<InstalledRuntimeStatus>,
    pub store_issues: Vec<RuntimeStoreIssue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store_error: Option<String>,
    pub adapter_warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSearchResult {
    pub entry: RuntimeCatalogEntry,
    pub installed: bool,
    pub selected_for: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSearchSnapshot {
    pub host: HostCapabilities,
    pub results: Vec<RuntimeSearchResult>,
    pub provider_errors: Vec<crate::RuntimeProviderError>,
    pub fetched_at_unix: Option<i64>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeModelCandidate {
    pub runtime_id: RuntimeId,
    pub compatibility: RuntimeCompatibility,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeUpdateCheck {
    pub runtime: InstalledRuntime,
    pub state: RuntimeUpdateState,
}

#[derive(Clone)]
pub struct RuntimePackManager {
    registry: EngineRegistry,
    store: Arc<RuntimeStore>,
    catalog: RuntimeCatalog,
    installer: RuntimeInstaller,
    host: Arc<RwLock<HostCapabilities>>,
    host_initialized: Arc<std::sync::atomic::AtomicBool>,
    operation: Arc<Mutex<()>>,
}

impl RuntimePackManager {
    pub fn normalize_settings(
        &self,
        engine_id: &str,
        settings: &mut norted_core::ResolvedSettings,
    ) -> Result<(), RuntimePackError> {
        let adapter = self.registry.get(engine_id).ok_or_else(|| {
            RuntimePackError::Selection(format!("bound engine `{engine_id}` is not registered"))
        })?;
        adapter
            .normalize_settings(settings)
            .map_err(RuntimePackError::Adapter)
    }

    pub async fn validate_configuration(
        &self,
        runtime: &InstalledRuntime,
        model: Option<&norted_core::ModelArtifact>,
        settings: &norted_core::ResolvedSettings,
    ) -> Result<(), RuntimePackError> {
        let adapter = self.registry.get(&settings.engine_id).ok_or_else(|| {
            RuntimePackError::Selection(format!("unknown engine `{}`", settings.engine_id))
        })?;
        adapter
            .validate_configuration(runtime, model, &self.host_capabilities().await, settings)
            .map_err(RuntimePackError::Adapter)
    }

    pub async fn validate_runtime_settings(
        &self,
        schema: &norted_core::SettingsSchema,
        settings: &norted_core::ResolvedSettings,
    ) -> Result<(), RuntimePackError> {
        schema
            .validate(settings)
            .map_err(|error| RuntimePackError::Selection(error.to_string()))?;
        let list = self.list().await?;
        let runtime = list
            .installed
            .iter()
            .find(|status| Some(&status.runtime.manifest.runtime_id) == schema.runtime_id.as_ref())
            .ok_or_else(|| {
                RuntimePackError::Selection("No installed runtime selected".to_owned())
            })?;
        self.validate_configuration(&runtime.runtime, None, settings)
            .await
    }

    pub async fn validate_profile_settings(
        &self,
        state: &SettingsState,
        profile: &ModelProfile,
        model: &norted_core::ModelArtifact,
        overrides: &SettingsPatch,
        base: &Path,
    ) -> Result<(), RuntimePackError> {
        let mut resolved = state
            .resolve(
                &profile.id,
                profile.engine_id.as_str(),
                overrides,
                &SettingsPatch::default(),
                base,
            )
            .map_err(|error| RuntimePackError::Selection(error.to_string()))?;
        self.normalize_settings(profile.engine_id.as_str(), &mut resolved)?;
        let (selection, schema) = self
            .settings_schema_for_model_for_engine_with_settings(
                model,
                profile.engine_id.as_str(),
                None,
                Some(&resolved),
            )
            .await?;
        schema
            .validate(&resolved)
            .map_err(|error| RuntimePackError::Selection(error.to_string()))?;
        self.validate_configuration(&selection.runtime, Some(model), &resolved)
            .await
    }

    pub fn compatible_engine_ids(&self, model: &ModelArtifact) -> Vec<String> {
        self.registry
            .compatible_with(model)
            .into_iter()
            .map(|adapter| adapter.identity().id)
            .collect()
    }

    pub fn new(
        paths: &AppPaths,
        registry: EngineRegistry,
        providers: impl IntoIterator<Item = Arc<dyn RuntimeCatalogProvider>>,
    ) -> Result<Arc<Self>, RuntimePackError> {
        let store = Arc::new(RuntimeStore::new(paths));
        let catalog = RuntimeCatalog::new(paths.runtime_cache_dir.clone(), providers)?;
        let authorities = catalog.authorities();
        let installer = RuntimeInstaller::new(
            Arc::clone(&store),
            registry.clone(),
            paths.runtime_cache_dir.clone(),
            authorities,
        )?;
        Ok(Arc::new(Self {
            registry,
            store,
            catalog,
            installer,
            host_initialized: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            host: Arc::new(RwLock::new(
                HostCapabilities::current_without_accelerator_probe(),
            )),
            operation: Arc::new(Mutex::new(())),
        }))
    }

    /// Keep installed-runtime mutations out of an admitted benchmark. Serving
    /// resolution remains read-only and does not acquire this operation guard.
    pub(crate) fn try_reserve_benchmark(&self) -> Result<tokio::sync::OwnedMutexGuard<()>, String> {
        Arc::clone(&self.operation)
            .try_lock_owned()
            .map_err(|_| "Busy: runtime-pack mutation in progress".to_owned())
    }

    pub fn progress(&self) -> broadcast::Receiver<RuntimeOperationProgress> {
        self.installer.subscribe()
    }

    pub async fn refresh_host_capabilities(&self) -> HostCapabilities {
        let host = detect_host_capabilities().await;
        *self.host.write().await = host.clone();
        self.host_initialized
            .store(true, std::sync::atomic::Ordering::Release);
        host
    }

    pub async fn host_capabilities(&self) -> HostCapabilities {
        self.host.read().await.clone()
    }

    /// Editors reuse host observations; load admission and explicit runtime refresh still probe afresh.
    pub async fn resolve_for_settings(
        &self,
        model: &ModelArtifact,
        engine_id: &str,
        explicit: Option<&RuntimeId>,
        settings: Option<&norted_core::ResolvedSettings>,
    ) -> Result<RuntimeSelection, RuntimePackError> {
        if !self
            .host_initialized
            .load(std::sync::atomic::Ordering::Acquire)
        {
            self.refresh_host_capabilities().await;
        }
        self.resolve_from_snapshot(
            model,
            explicit,
            settings,
            Some(engine_id),
            &self.list().await?,
        )
    }

    pub async fn settings_schema_for_model(
        &self,
        model: &norted_core::ModelArtifact,
        explicit_runtime: Option<&norted_core::RuntimeId>,
    ) -> Result<(norted_core::RuntimeSelection, norted_core::SettingsSchema), RuntimePackError>
    {
        self.settings_schema_for_model_with_settings(model, explicit_runtime, None)
            .await
    }

    pub async fn settings_schema_for_model_with_settings(
        &self,
        model: &norted_core::ModelArtifact,
        explicit_runtime: Option<&norted_core::RuntimeId>,
        settings: Option<&norted_core::ResolvedSettings>,
    ) -> Result<(norted_core::RuntimeSelection, norted_core::SettingsSchema), RuntimePackError>
    {
        let selection = self
            .resolve_with_settings(model, explicit_runtime, settings)
            .await?;
        let engine_id = &selection.runtime.manifest.identity.engine_id;
        let adapter = self.registry.get(engine_id).ok_or_else(|| {
            RuntimePackError::Selection(format!(
                "selected runtime uses unregistered engine `{engine_id}`"
            ))
        })?;
        let host = self.host_capabilities().await;
        let schema = adapter
            .settings_schema(&selection.runtime, model, &host, settings)
            .await
            .map_err(RuntimePackError::Adapter)?;
        Ok((selection, schema))
    }

    /// Returns runtime-owned, model-independent schemas for engines whose
    /// format runtime has been explicitly selected. If one engine is selected
    /// through multiple formats with different runtimes, no single engine
    /// default can be represented truthfully and that engine is omitted.
    pub async fn selected_runtime_settings_schemas(
        &self,
        settings_state: &norted_core::SettingsState,
        structured_path_base: &Path,
    ) -> Result<(BTreeMap<String, norted_core::SettingsSchema>, Vec<String>), RuntimePackError>
    {
        if !self
            .host_initialized
            .load(std::sync::atomic::Ordering::Acquire)
        {
            self.refresh_host_capabilities().await;
        }
        let host = self.host_capabilities().await;
        let list = self.list().await?;
        let mut schemas = BTreeMap::<String, norted_core::SettingsSchema>::new();
        let mut warnings = Vec::new();
        let mut attempted = BTreeMap::<String, RuntimeId>::new();
        let mut ambiguous = BTreeSet::new();
        for runtime_id in list.selections.format_defaults.values() {
            let Some(status) = list
                .installed
                .iter()
                .find(|status| &status.runtime.manifest.runtime_id == runtime_id)
            else {
                continue;
            };
            let engine_id = status.runtime.manifest.identity.engine_id.clone();
            if ambiguous.contains(&engine_id) {
                continue;
            }
            if attempted
                .get(&engine_id)
                .is_some_and(|attempted_runtime| attempted_runtime != runtime_id)
            {
                schemas.remove(&engine_id);
                ambiguous.insert(engine_id);
                continue;
            }
            if attempted
                .insert(engine_id.clone(), runtime_id.clone())
                .is_some()
            {
                continue;
            }
            let Some(adapter) = self.registry.get(&engine_id) else {
                warnings.push(format!(
                    "{engine_id}: selected runtime uses an unregistered engine"
                ));
                continue;
            };
            let settings =
                match settings_state.resolve_runtime_defaults(&engine_id, structured_path_base) {
                    Ok(settings) => settings,
                    Err(error) => {
                        warnings.push(format!("{engine_id}: {error}"));
                        continue;
                    }
                };
            match adapter
                .runtime_settings_schema(&status.runtime, &host, Some(&settings))
                .await
            {
                Ok(mut schema) => {
                    schema.retain_override_definitions(&settings);
                    let mut resolved = settings.clone();
                    if let Err(error) = schema
                        .materialize_runtime_configuration(&mut resolved)
                        .and_then(|()| schema.validate(&resolved))
                        .and_then(|()| schema.materialize_effective(&mut resolved))
                    {
                        warnings.push(format!("{engine_id}: {error}"));
                    }
                    if let Err(error) =
                        adapter.validate_configuration(&status.runtime, None, &host, &settings)
                    {
                        warnings.push(format!("{engine_id}: {error}"));
                    }
                    schemas.insert(engine_id, schema);
                }
                Err(error) => warnings.push(format!("{engine_id}: {error}")),
            }
        }
        for adapter in self.registry.adapters() {
            let engine_id = adapter.identity().id;
            if schemas.contains_key(&engine_id) {
                continue;
            }
            let runtime_id = attempted.get(&engine_id).cloned();
            let reason = if ambiguous.contains(&engine_id) {
                "Multiple runtime selections for this engine; choose a single baseline"
            } else if runtime_id.is_some() {
                "Selected runtime metadata/probe failed; see runtime diagnostics"
            } else {
                "No runtime selected/installed for this engine"
            };
            let mut definitions = adapter.setting_definitions();
            for definition in &mut definitions {
                definition.supported = false;
                definition.unsupported_reason = Some(reason.to_owned());
                definition.default_preview = None;
            }
            let mut schema = norted_core::SettingsSchema {
                engine_id: engine_id.clone(),
                runtime_id,
                definitions,
            };
            if let Ok(settings) =
                settings_state.resolve_runtime_defaults(&engine_id, structured_path_base)
            {
                schema.retain_override_definitions(&settings);
            }
            schemas.insert(engine_id, schema);
        }
        Ok((schemas, warnings))
    }

    pub fn model_settings_schema_for_engine(
        &self,
        model: &norted_core::ModelArtifact,
        engine_id: &str,
    ) -> Result<norted_core::SettingsSchema, RuntimePackError> {
        let adapter = self.registry.get(engine_id).ok_or_else(|| {
            RuntimePackError::Selection(format!("bound engine `{engine_id}` is not registered"))
        })?;
        if let CompatibilityDecision::Unsupported { reason } = adapter.compatibility(model) {
            return Err(RuntimePackError::Selection(reason));
        }
        let definitions = adapter
            .model_setting_definitions(model)
            .map_err(RuntimePackError::Adapter)?;
        Ok(norted_core::SettingsSchema {
            engine_id: engine_id.to_owned(),
            runtime_id: None,
            definitions,
        })
    }

    pub async fn settings_schema_for_model_for_engine_with_settings(
        &self,
        model: &norted_core::ModelArtifact,
        engine_id: &str,
        explicit_runtime: Option<&norted_core::RuntimeId>,
        settings: Option<&norted_core::ResolvedSettings>,
    ) -> Result<(norted_core::RuntimeSelection, norted_core::SettingsSchema), RuntimePackError>
    {
        let selection = self
            .resolve_for_settings(model, engine_id, explicit_runtime, settings)
            .await?;
        let adapter = self.registry.get(engine_id).ok_or_else(|| {
            RuntimePackError::Selection(format!("bound engine `{engine_id}` is not registered"))
        })?;
        let host = self.host_capabilities().await;
        let schema = adapter
            .settings_schema(&selection.runtime, model, &host, settings)
            .await
            .map_err(RuntimePackError::Adapter)?;
        Ok((selection, schema))
    }

    /// Preflights a Model Profile engine change against the selected target
    /// runtime. Stored overrides are retained only when the exact target
    /// definition accepts both their ID and value.
    pub async fn model_profile_engine_switch_candidate(
        &self,
        settings_state: &SettingsState,
        profile: &ModelProfile,
        model: &ModelArtifact,
        target_engine_id: &str,
        structured_path_base: &Path,
    ) -> Result<ModelProfileEngineSwitchCandidate, RuntimePackError> {
        let runtime_defaults =
            settings_state.resolve_runtime_defaults(target_engine_id, structured_path_base)?;
        let (_, mut schema) = self
            .settings_schema_for_model_for_engine_with_settings(
                model,
                target_engine_id,
                None,
                Some(&runtime_defaults),
            )
            .await?;
        let mut overrides = profile.overrides.clone();
        let mut removed = BTreeSet::new();

        loop {
            overrides.0.retain(|id, value| {
                let compatible = schema
                    .definition(id)
                    .is_some_and(|definition| definition.validate_value(value).is_ok());
                if !compatible {
                    removed.insert(id.clone());
                }
                compatible
            });

            let mut resolved = settings_state.resolve(
                &profile.id,
                target_engine_id,
                &overrides,
                &SettingsPatch::default(),
                structured_path_base,
            )?;
            self.normalize_settings(target_engine_id, &mut resolved)?;
            let (_, exact_schema) = self
                .settings_schema_for_model_for_engine_with_settings(
                    model,
                    target_engine_id,
                    None,
                    Some(&resolved),
                )
                .await?;

            let before = overrides.0.len();
            overrides.0.retain(|id, value| {
                let compatible = exact_schema
                    .definition(id)
                    .is_some_and(|definition| definition.validate_value(value).is_ok());
                if !compatible {
                    removed.insert(id.clone());
                }
                compatible
            });
            if overrides.0.len() != before {
                schema = exact_schema;
                continue;
            }

            exact_schema.materialize_runtime_configuration(&mut resolved)?;
            exact_schema.validate(&resolved)?;
            exact_schema.materialize_effective(&mut resolved)?;
            return Ok(ModelProfileEngineSwitchCandidate {
                overrides,
                removed: removed.into_iter().collect(),
                resolved,
            });
        }
    }

    pub async fn list(&self) -> Result<RuntimeListSnapshot, RuntimePackError> {
        let RuntimeStoreSnapshot { runtimes, issues } = self.store.scan().await?;
        let mut warnings = issues
            .into_iter()
            .map(|issue| issue.message)
            .collect::<Vec<_>>();
        let mut runtimes = self
            .collect_external_runtimes(runtimes, &mut warnings)
            .await;
        runtimes.sort_by(|left, right| {
            left.manifest
                .identity
                .engine_id
                .cmp(&right.manifest.identity.engine_id)
                .then_with(|| {
                    left.manifest
                        .identity
                        .variant
                        .cmp(&right.manifest.identity.variant)
                })
                .then_with(|| compare_installed_recency_with_registry(&self.registry, right, left))
        });
        let selections = self.store.selections().await?;
        let host = self.host.read().await.clone();
        let installed = self.assess_installed(runtimes, &selections, &host);
        Ok(RuntimeListSnapshot {
            host,
            selections,
            installed,
            warnings,
        })
    }

    /// Inspects installed runtimes, selections, adapters, and host compatibility
    /// without initializing the store or consulting runtime providers.
    pub async fn inspect_local(&self) -> RuntimeLocalInspection {
        let host = self.refresh_host_capabilities().await;
        let (runtimes, store_issues, store_error) = match self.store.inspect_existing().await {
            Ok(snapshot) => (snapshot.runtimes, snapshot.issues, None),
            Err(error) => (Vec::new(), Vec::new(), Some(error.to_string())),
        };
        let (selections, selections_error) = match self.store.selections().await {
            Ok(selections) => (Some(selections), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let mut adapter_warnings = Vec::new();
        let mut runtimes = self
            .collect_external_runtimes(runtimes, &mut adapter_warnings)
            .await;
        runtimes.sort_by(|left, right| {
            left.manifest
                .identity
                .engine_id
                .cmp(&right.manifest.identity.engine_id)
                .then_with(|| {
                    left.manifest
                        .identity
                        .variant
                        .cmp(&right.manifest.identity.variant)
                })
                .then_with(|| compare_installed_recency_with_registry(&self.registry, right, left))
        });
        let empty_selections = RuntimeSelections::default();
        let installed = self.assess_installed(
            runtimes,
            selections.as_ref().unwrap_or(&empty_selections),
            &host,
        );
        RuntimeLocalInspection {
            host,
            selections,
            selections_error,
            installed,
            store_issues,
            store_error,
            adapter_warnings,
        }
    }

    async fn collect_external_runtimes(
        &self,
        mut runtimes: Vec<InstalledRuntime>,
        warnings: &mut Vec<String>,
    ) -> Vec<InstalledRuntime> {
        for adapter in self.registry.adapters() {
            match adapter.probe().await {
                Ok(probe) => match probe.installation {
                    InstallationState::Installed { installation } if probe.healthy => {
                        match external_runtime(
                            &adapter.identity().id,
                            adapter.capabilities().artifact_formats,
                            *installation,
                            probe.detail,
                        ) {
                            Ok(runtime) => runtimes.push(runtime),
                            Err(error) => warnings.push(error.to_string()),
                        }
                    }
                    InstallationState::Invalid { reason } => warnings.push(format!(
                        "{} external runtime is invalid: {reason}",
                        adapter.identity().id
                    )),
                    InstallationState::NotInstalled | InstallationState::Installed { .. } => {}
                },
                Err(error) => warnings.push(format!(
                    "{} external runtime probe failed: {error}",
                    adapter.identity().id
                )),
            }
        }
        runtimes
    }

    fn assess_installed(
        &self,
        runtimes: Vec<InstalledRuntime>,
        selections: &RuntimeSelections,
        host: &HostCapabilities,
    ) -> Vec<InstalledRuntimeStatus> {
        let latest_installed = runtimes
            .iter()
            .filter(|runtime| {
                let engine_id = &runtime.manifest.identity.engine_id;
                let adapter = self.registry.get(engine_id);
                !runtimes.iter().any(|candidate| {
                    candidate.manifest.identity.engine_id == *engine_id
                        && compare_installed_recency_inner(
                            candidate,
                            runtime,
                            adapter.as_deref(),
                            true,
                        ) == Ordering::Greater
                })
            })
            .map(|runtime| runtime.manifest.runtime_id.clone())
            .collect::<BTreeSet<_>>();
        runtimes
            .into_iter()
            .map(|runtime| {
                let compatibility = match self.registry.get(&runtime.manifest.identity.engine_id) {
                    Some(adapter) => match adapter.runtime_management_compatibility() {
                        CompatibilityDecision::Supported => {
                            match adapter.runtime_compatibility(&runtime) {
                                CompatibilityDecision::Supported => {
                                    compatibility_for_installed(&runtime, host)
                                }
                                CompatibilityDecision::Unsupported { reason } => {
                                    RuntimeCompatibility::Incompatible(reason)
                                }
                            }
                        }
                        CompatibilityDecision::Unsupported { reason } => {
                            RuntimeCompatibility::Incompatible(reason)
                        }
                    },
                    None => RuntimeCompatibility::Incompatible(
                        "engine adapter is not registered".to_owned(),
                    ),
                };
                InstalledRuntimeStatus {
                    latest_installed: latest_installed.contains(&runtime.manifest.runtime_id),
                    compatibility,
                    selected_for: selected_for(selections, &runtime.manifest.runtime_id),
                    runtime,
                }
            })
            .collect()
    }

    pub async fn search(
        &self,
        query: &str,
        force_refresh: bool,
    ) -> Result<RuntimeSearchSnapshot, RuntimePackError> {
        self.search_internal(query, force_refresh, None).await
    }

    pub async fn search_for_model(
        &self,
        query: &str,
        model: &ModelArtifact,
        force_refresh: bool,
    ) -> Result<RuntimeSearchSnapshot, RuntimePackError> {
        self.search_for_model_with_settings(query, model, force_refresh, None)
            .await
    }

    pub async fn search_for_model_with_settings(
        &self,
        query: &str,
        model: &ModelArtifact,
        force_refresh: bool,
        settings: Option<&norted_core::ResolvedSettings>,
    ) -> Result<RuntimeSearchSnapshot, RuntimePackError> {
        self.search_internal(query, force_refresh, Some((model, settings)))
            .await
    }

    async fn search_internal(
        &self,
        query: &str,
        force_refresh: bool,
        model: Option<(&ModelArtifact, Option<&norted_core::ResolvedSettings>)>,
    ) -> Result<RuntimeSearchSnapshot, RuntimePackError> {
        let host = self.host.read().await.clone();
        let RuntimeCatalogSnapshot {
            mut entries,
            provider_errors,
            fetched_at_unix,
        } = self.catalog.search(query, &host, force_refresh).await;
        let mut preferences = std::collections::BTreeMap::new();
        let mut source_prerequisite_results =
            Vec::<(SourceBuildPrerequisiteEvaluationKey, RuntimeCompatibility)>::new();
        for entry in &mut entries {
            entry.compatibility = match self.registry.get(&entry.available.identity.engine_id) {
                Some(adapter) => match adapter.runtime_management_compatibility() {
                    CompatibilityDecision::Supported => {
                        match adapter.available_runtime_compatibility(&entry.available) {
                            CompatibilityDecision::Supported => {
                                if let Some((model, settings)) = model {
                                    preferences.insert(
                                        entry.available.runtime_id.clone(),
                                        adapter.available_runtime_model_preference(
                                            &entry.available,
                                            model,
                                            &host,
                                        ),
                                    );
                                    combine_compatibility(
                                        entry.compatibility.clone(),
                                        adapter.available_runtime_model_compatibility(
                                            &entry.available,
                                            model,
                                            &host,
                                            settings,
                                        ),
                                    )
                                } else {
                                    entry.compatibility.clone()
                                }
                            }
                            CompatibilityDecision::Unsupported { reason } => {
                                RuntimeCompatibility::Incompatible(reason)
                            }
                        }
                    }
                    CompatibilityDecision::Unsupported { reason } => {
                        RuntimeCompatibility::Incompatible(reason)
                    }
                },
                None => RuntimeCompatibility::Incompatible(
                    "engine adapter is not registered".to_owned(),
                ),
            };
            if !matches!(entry.compatibility, RuntimeCompatibility::Incompatible(_))
                && let Some(plan) = entry.available.source_build()
            {
                let prerequisite_key = SourceBuildPrerequisiteEvaluationKey::from_plan(plan);
                let compatibility = if let Some((_, compatibility)) = source_prerequisite_results
                    .iter()
                    .find(|(key, _)| key == &prerequisite_key)
                {
                    compatibility.clone()
                } else {
                    let compatibility = self.installer.source_build_compatibility(plan).await;
                    source_prerequisite_results.push((prerequisite_key, compatibility.clone()));
                    compatibility
                };
                entry.compatibility =
                    combine_compatibility(entry.compatibility.clone(), compatibility);
            }
        }
        if model.is_some() {
            entries.sort_by(|left, right| {
                left.compatibility
                    .preference_rank()
                    .cmp(&right.compatibility.preference_rank())
                    .then_with(|| {
                        preferences
                            .get(&left.available.runtime_id)
                            .unwrap_or(&u16::MAX)
                            .cmp(
                                preferences
                                    .get(&right.available.runtime_id)
                                    .unwrap_or(&u16::MAX),
                            )
                    })
                    .then_with(|| {
                        left.available
                            .identity
                            .engine_id
                            .cmp(&right.available.identity.engine_id)
                    })
                    .then_with(|| {
                        left.available
                            .identity
                            .variant
                            .cmp(&right.available.identity.variant)
                    })
                    .then_with(|| {
                        right
                            .available
                            .published_at_unix
                            .cmp(&left.available.published_at_unix)
                    })
            });
        }
        let list = self.list().await?;
        let installed = list
            .installed
            .iter()
            .map(|status| status.runtime.manifest.runtime_id.clone())
            .collect::<BTreeSet<_>>();
        let results = entries
            .into_iter()
            .map(|entry| RuntimeSearchResult {
                installed: installed.contains(&entry.available.runtime_id),
                selected_for: selected_for(&list.selections, &entry.available.runtime_id),
                entry,
            })
            .collect();
        Ok(RuntimeSearchSnapshot {
            host,
            results,
            provider_errors,
            fetched_at_unix,
        })
    }

    pub async fn install(
        &self,
        runtime_id: &RuntimeId,
    ) -> Result<InstalledRuntime, RuntimePackError> {
        let host = self.refresh_host_capabilities().await;
        let _operation = self.operation.try_lock().map_err(|_| {
            RuntimePackError::Selection(
                "Busy: runtime mutation or benchmark reservation in progress".to_owned(),
            )
        })?;
        let entry = self.catalog.find(runtime_id, &host, false).await?;
        let adapter = self
            .registry
            .get(&entry.available.identity.engine_id)
            .ok_or_else(|| RuntimePackError::Incompatible {
                runtime_id: runtime_id.clone(),
                reason: "engine adapter is not registered".to_owned(),
            })?;
        if let CompatibilityDecision::Unsupported { reason } =
            adapter.runtime_management_compatibility()
        {
            return Err(RuntimePackError::Incompatible {
                runtime_id: runtime_id.clone(),
                reason,
            });
        }
        if let CompatibilityDecision::Unsupported { reason } =
            adapter.available_runtime_compatibility(&entry.available)
        {
            return Err(RuntimePackError::Incompatible {
                runtime_id: runtime_id.clone(),
                reason,
            });
        }
        if let RuntimeCompatibility::Incompatible(reason) = entry.compatibility {
            return Err(RuntimePackError::Incompatible {
                runtime_id: runtime_id.clone(),
                reason,
            });
        }
        if let Some(runtime) = self.store.get(runtime_id).await? {
            ensure_catalog_provenance_matches(&runtime, &entry.available)?;
            return Ok(runtime);
        }
        // This per-runtime cross-process lease is intentionally separate from
        // both the store transaction lock and running-runtime leases. It is
        // held across acquisition/build, then activate takes its short-lived
        // locks in the existing order without any reverse dependency.
        let _installation_lease = self.store.acquire_installation_lease(runtime_id).await?;
        if let Some(runtime) = self.store.get(runtime_id).await? {
            ensure_catalog_provenance_matches(&runtime, &entry.available)?;
            return Ok(runtime);
        }
        let available = self
            .catalog
            .verify_install_candidate(&entry.available)
            .await?;
        self.installer.install(&available).await.map_err(Into::into)
    }

    pub async fn remove(
        &self,
        runtime_id: &RuntimeId,
        active_runtime: Option<&RuntimeId>,
    ) -> Result<(), RuntimePackError> {
        let _operation = self.operation.try_lock().map_err(|_| {
            RuntimePackError::Selection(
                "Busy: runtime mutation or benchmark reservation in progress".to_owned(),
            )
        })?;
        if active_runtime == Some(runtime_id) {
            return Err(RuntimePackError::Active(runtime_id.clone()));
        }
        match self.store.remove(runtime_id).await {
            Err(RuntimeStoreError::RuntimeInUse(_)) => {
                Err(RuntimePackError::Active(runtime_id.clone()))
            }
            Err(RuntimeStoreError::RuntimeSelected {
                runtime_id,
                selection,
            }) => Err(RuntimePackError::Selected {
                runtime_id,
                selection,
            }),
            result => result.map_err(Into::into),
        }
    }

    pub async fn acquire_runtime_lease(
        &self,
        runtime_id: &RuntimeId,
    ) -> Result<RuntimeLease, RuntimePackError> {
        self.store
            .acquire_runtime_lease(runtime_id)
            .await
            .map_err(Into::into)
    }

    pub async fn select_format(
        &self,
        format: ArtifactFormat,
        runtime_id: RuntimeId,
    ) -> Result<RuntimeSelections, RuntimePackError> {
        self.select_format_with_preference(format, runtime_id, RuntimeUpdatePreference::Pinned)
            .await
    }

    pub async fn select_format_with_preference(
        &self,
        format: ArtifactFormat,
        runtime_id: RuntimeId,
        update_preference: RuntimeUpdatePreference,
    ) -> Result<RuntimeSelections, RuntimePackError> {
        let host = self.refresh_host_capabilities().await;
        let _operation = self.operation.try_lock().map_err(|_| {
            RuntimePackError::Selection(
                "Busy: runtime mutation or benchmark reservation in progress".to_owned(),
            )
        })?;
        let transaction = self.store.acquire_transaction().await?;
        let runtime = self
            .installed(&runtime_id)
            .await?
            .ok_or_else(|| RuntimePackError::NotInstalled(runtime_id.clone()))?;
        self.validate_format_candidate(&runtime, format, &host)?;
        let mut selections = self.store.selections().await?;
        let previous = selections
            .format_defaults
            .insert(format, runtime_id.clone());
        if let Some(previous) = previous
            && selected_for(&selections, &previous).is_empty()
        {
            selections.update_preferences.remove(&previous);
        }
        selections
            .update_preferences
            .insert(runtime_id, update_preference);
        self.store
            .write_selections(&selections, &transaction)
            .await?;
        Ok(selections)
    }

    pub async fn select_model(
        &self,
        model: &ModelArtifact,
        runtime_id: RuntimeId,
    ) -> Result<RuntimeSelections, RuntimePackError> {
        self.select_model_with_preference(model, runtime_id, RuntimeUpdatePreference::Pinned)
            .await
    }

    pub async fn select_model_with_preference(
        &self,
        model: &ModelArtifact,
        runtime_id: RuntimeId,
        update_preference: RuntimeUpdatePreference,
    ) -> Result<RuntimeSelections, RuntimePackError> {
        let host = self.refresh_host_capabilities().await;
        let _operation = self.operation.try_lock().map_err(|_| {
            RuntimePackError::Selection(
                "Busy: runtime mutation or benchmark reservation in progress".to_owned(),
            )
        })?;
        let transaction = self.store.acquire_transaction().await?;
        let runtime = self
            .installed(&runtime_id)
            .await?
            .ok_or_else(|| RuntimePackError::NotInstalled(runtime_id.clone()))?;
        self.validate_model_candidate(&runtime, model, &host, None)?;
        let mut selections = self.store.selections().await?;
        let previous = selections
            .model_overrides
            .insert(model.id.clone(), runtime_id.clone());
        if let Some(previous) = previous
            && selected_for(&selections, &previous).is_empty()
        {
            selections.update_preferences.remove(&previous);
        }
        selections
            .update_preferences
            .insert(runtime_id, update_preference);
        self.store
            .write_selections(&selections, &transaction)
            .await?;
        Ok(selections)
    }

    pub async fn clear_format_selection(
        &self,
        format: ArtifactFormat,
    ) -> Result<RuntimeSelections, RuntimePackError> {
        let _operation = self.operation.try_lock().map_err(|_| {
            RuntimePackError::Selection(
                "Busy: runtime mutation or benchmark reservation in progress".to_owned(),
            )
        })?;
        let transaction = self.store.acquire_transaction().await?;
        let mut selections = self.store.selections().await?;
        if let Some(runtime_id) = selections.format_defaults.remove(&format)
            && selected_for(&selections, &runtime_id).is_empty()
        {
            selections.update_preferences.remove(&runtime_id);
        }
        self.store
            .write_selections(&selections, &transaction)
            .await?;
        Ok(selections)
    }

    pub async fn clear_model_selection(
        &self,
        model_id: &ModelId,
    ) -> Result<RuntimeSelections, RuntimePackError> {
        let _operation = self.operation.try_lock().map_err(|_| {
            RuntimePackError::Selection(
                "Busy: runtime mutation or benchmark reservation in progress".to_owned(),
            )
        })?;
        let transaction = self.store.acquire_transaction().await?;
        let mut selections = self.store.selections().await?;
        if let Some(runtime_id) = selections.model_overrides.remove(model_id)
            && selected_for(&selections, &runtime_id).is_empty()
        {
            selections.update_preferences.remove(&runtime_id);
        }
        self.store
            .write_selections(&selections, &transaction)
            .await?;
        Ok(selections)
    }

    pub async fn resolve(
        &self,
        model: &ModelArtifact,
        explicit: Option<&RuntimeId>,
    ) -> Result<RuntimeSelection, RuntimePackError> {
        self.resolve_with_settings(model, explicit, None).await
    }

    pub async fn resolve_with_settings(
        &self,
        model: &ModelArtifact,
        explicit: Option<&RuntimeId>,
        settings: Option<&norted_core::ResolvedSettings>,
    ) -> Result<RuntimeSelection, RuntimePackError> {
        self.refresh_host_capabilities().await;
        let list = self.list().await?;
        self.resolve_from_snapshot(model, explicit, settings, None, &list)
    }

    pub async fn resolve_for_engine_with_settings(
        &self,
        model: &ModelArtifact,
        engine_id: &str,
        explicit: Option<&RuntimeId>,
        settings: Option<&norted_core::ResolvedSettings>,
    ) -> Result<RuntimeSelection, RuntimePackError> {
        self.refresh_host_capabilities().await;
        let list = self.list().await?;
        self.resolve_from_snapshot(model, explicit, settings, Some(engine_id), &list)
    }

    /// Applies the normal runtime-selection and compatibility authority to a
    /// previously captured read-only local inspection.
    pub fn resolve_from_local_inspection(
        &self,
        model: &ModelArtifact,
        engine_id: Option<&str>,
        settings: Option<&norted_core::ResolvedSettings>,
        inspection: &RuntimeLocalInspection,
    ) -> Result<RuntimeSelection, RuntimePackError> {
        if let Some(error) = &inspection.store_error {
            return Err(RuntimePackError::Selection(format!(
                "runtime store is unavailable: {error}"
            )));
        }
        let selections = inspection.selections.clone().ok_or_else(|| {
            RuntimePackError::Selection(format!(
                "runtime selections are unavailable: {}",
                inspection
                    .selections_error
                    .as_deref()
                    .unwrap_or("unknown error")
            ))
        })?;
        let list = RuntimeListSnapshot {
            host: inspection.host.clone(),
            selections,
            installed: inspection.installed.clone(),
            warnings: inspection
                .store_issues
                .iter()
                .map(|issue| issue.message.clone())
                .chain(inspection.adapter_warnings.iter().cloned())
                .collect(),
        };
        self.resolve_from_snapshot(model, None, settings, engine_id, &list)
    }

    /// Resolves a runtime from a read-only inspection and applies the exact
    /// adapter/runtime settings contract without rescanning or initializing
    /// any store state.
    pub async fn settings_schema_from_local_inspection(
        &self,
        model: &ModelArtifact,
        engine_id: &str,
        settings: Option<&norted_core::ResolvedSettings>,
        inspection: &RuntimeLocalInspection,
    ) -> Result<(RuntimeSelection, norted_core::SettingsSchema), RuntimePackError> {
        let selection =
            self.resolve_from_local_inspection(model, Some(engine_id), settings, inspection)?;
        let adapter = self.registry.get(engine_id).ok_or_else(|| {
            RuntimePackError::Selection(format!("bound engine `{engine_id}` is not registered"))
        })?;
        let schema = adapter
            .settings_schema(&selection.runtime, model, &inspection.host, settings)
            .await
            .map_err(RuntimePackError::Adapter)?;
        Ok((selection, schema))
    }

    fn resolve_from_snapshot(
        &self,
        model: &ModelArtifact,
        explicit: Option<&RuntimeId>,
        settings: Option<&norted_core::ResolvedSettings>,
        required_engine_id: Option<&str>,
        list: &RuntimeListSnapshot,
    ) -> Result<RuntimeSelection, RuntimePackError> {
        let host = list.host.clone();
        if let Some(runtime_id) = explicit {
            let status = list
                .installed
                .iter()
                .find(|status| &status.runtime.manifest.runtime_id == runtime_id)
                .ok_or_else(|| RuntimePackError::NotInstalled(runtime_id.clone()))?;
            if let RuntimeCompatibility::Incompatible(reason) = &status.compatibility {
                return Err(RuntimePackError::Incompatible {
                    runtime_id: runtime_id.clone(),
                    reason: reason.clone(),
                });
            }
            if required_engine_id
                .is_some_and(|required| status.runtime.manifest.identity.engine_id != required)
            {
                return Err(RuntimePackError::Incompatible {
                    runtime_id: runtime_id.clone(),
                    reason: format!(
                        "Model Profile binds engine `{}` but runtime uses `{}`",
                        required_engine_id.expect("checked"),
                        status.runtime.manifest.identity.engine_id
                    ),
                });
            }
            self.validate_model_candidate(&status.runtime, model, &host, settings)?;
            return Ok(RuntimeSelection {
                runtime: status.runtime.clone(),
                source: RuntimeSelectionSource::Invocation,
                notices: Vec::new(),
                accelerator_binding: self.model_candidate_accelerator_binding(
                    &status.runtime,
                    model,
                    &host,
                    settings,
                ),
            });
        }
        let mut notices = Vec::new();
        if let Some(runtime_id) = list.selections.model_overrides.get(&model.id) {
            match self.selected_candidate(list, runtime_id, model, settings) {
                Ok(runtime)
                    if required_engine_id.is_none_or(|required| {
                        runtime.manifest.identity.engine_id == required
                    }) => {
                    return Ok(RuntimeSelection {
                        accelerator_binding: self.model_candidate_accelerator_binding(
                            &runtime,
                            model,
                            &list.host,
                            settings,
                        ),
                        runtime,
                        source: RuntimeSelectionSource::ModelOverride,
                        notices,
                    });
                }
                Ok(runtime) => notices.push(format!(
                    "ignored model runtime selection `{}` because it uses engine `{}` instead of bound engine `{}`",
                    runtime.manifest.runtime_id,
                    runtime.manifest.identity.engine_id,
                    required_engine_id.expect("mismatched runtime requires an engine")
                )),
                Err(error) => return Err(error),
            }
        }
        if let Some(runtime_id) = list.selections.format_defaults.get(&model.format) {
            match self.selected_candidate(list, runtime_id, model, settings) {
                Ok(runtime)
                    if required_engine_id.is_none_or(|required| {
                        runtime.manifest.identity.engine_id == required
                    }) => {
                    return Ok(RuntimeSelection {
                        accelerator_binding: self.model_candidate_accelerator_binding(
                            &runtime,
                            model,
                            &list.host,
                            settings,
                        ),
                        runtime,
                        source: RuntimeSelectionSource::FormatDefault,
                        notices,
                    });
                }
                Ok(runtime) => notices.push(format!(
                    "ignored format runtime selection `{}` because it uses engine `{}` instead of bound engine `{}`",
                    runtime.manifest.runtime_id,
                    runtime.manifest.identity.engine_id,
                    required_engine_id.expect("mismatched runtime requires an engine")
                )),
                Err(error) => return Err(error),
            }
        }
        let mut compatible = Vec::new();
        let mut rejected = Vec::new();
        for status in &list.installed {
            if required_engine_id
                .is_some_and(|required| status.runtime.manifest.identity.engine_id != required)
            {
                continue;
            }
            if !status
                .runtime
                .manifest
                .supported_formats
                .contains(&model.format)
            {
                continue;
            }
            if let RuntimeCompatibility::Incompatible(reason) = &status.compatibility {
                rejected.push(format!("{}: {reason}", status.runtime.manifest.runtime_id));
                continue;
            }
            match self.model_candidate_compatibility(&status.runtime, model, &host, settings) {
                Ok(compatibility) if compatibility.is_usable() => compatible.push((
                    status,
                    compatibility,
                    self.model_candidate_preference(&status.runtime, model, &host),
                )),
                Ok(RuntimeCompatibility::Incompatible(reason)) => {
                    rejected.push(format!("{}: {reason}", status.runtime.manifest.runtime_id))
                }
                Ok(_) => unreachable!("all non-incompatible states are usable"),
                Err(error) => rejected.push(error.to_string()),
            }
        }
        compatible.sort_by(|left, right| {
            left.1
                .preference_rank()
                .cmp(&right.1.preference_rank())
                .then_with(|| left.2.cmp(&right.2))
                .then_with(|| {
                    acquisition_rank(&left.0.runtime).cmp(&acquisition_rank(&right.0.runtime))
                })
                .then_with(|| {
                    compare_installed_recency_with_registry(
                        &self.registry,
                        &right.0.runtime,
                        &left.0.runtime,
                    )
                })
                .then_with(|| {
                    left.0
                        .runtime
                        .manifest
                        .runtime_id
                        .cmp(&right.0.runtime.manifest.runtime_id)
                })
        });
        let runtime = compatible
            .first()
            .map(|(status, _, _)| status.runtime.clone())
            .ok_or_else(|| {
                let detail = if rejected.is_empty() {
                    String::new()
                } else {
                    format!("; candidate rejections: {}", rejected.join("; "))
                };
                RuntimePackError::Selection(format!(
                    "no compatible installed runtime is available for model `{}` ({}){}",
                    model.id,
                    model.format.as_str(),
                    detail
                ))
            })?;
        Ok(RuntimeSelection {
            accelerator_binding: self
                .model_candidate_accelerator_binding(&runtime, model, &host, settings),
            runtime,
            source: RuntimeSelectionSource::Fallback,
            notices,
        })
    }

    pub async fn model_serving_capabilities(
        &self,
        model: &ModelArtifact,
        active_model: Option<&ModelId>,
    ) -> Result<ModelServingCapabilities, RuntimePackError> {
        self.model_serving_capabilities_with_settings(model, active_model, None)
            .await
    }

    pub async fn model_serving_capabilities_with_settings(
        &self,
        model: &ModelArtifact,
        active_model: Option<&ModelId>,
        settings: Option<&norted_core::ResolvedSettings>,
    ) -> Result<ModelServingCapabilities, RuntimePackError> {
        self.refresh_host_capabilities().await;
        let list = self.list().await?;

        let mut compatible_engine_ids = self
            .registry
            .compatible_with(model)
            .into_iter()
            .map(|adapter| adapter.identity().id)
            .collect::<Vec<_>>();
        compatible_engine_ids.sort();
        compatible_engine_ids.dedup();

        let mut compatible_installed_runtime_ids = list
            .installed
            .iter()
            .filter(|status| {
                status
                    .runtime
                    .manifest
                    .supported_formats
                    .contains(&model.format)
            })
            .filter_map(|status| {
                self.model_candidate_compatibility(&status.runtime, model, &list.host, settings)
                    .ok()
                    .filter(RuntimeCompatibility::is_usable)
                    .map(|_| status.runtime.manifest.runtime_id.clone())
            })
            .collect::<Vec<_>>();
        compatible_installed_runtime_ids.sort();
        compatible_installed_runtime_ids.dedup();

        let selected = self
            .resolve_from_snapshot(model, None, settings, None, &list)
            .ok();
        let selected_runtime_id = selected
            .as_ref()
            .map(|selection| selection.runtime.manifest.runtime_id.clone());
        let selected_adapter = selected.as_ref().and_then(|selection| {
            self.registry
                .get(&selection.runtime.manifest.identity.engine_id)
        });
        let selected_features = selected
            .as_ref()
            .zip(selected_adapter.as_ref())
            .map(|(selection, adapter)| {
                adapter.serving_features(&selection.runtime, model, settings)
            })
            .unwrap_or_default();
        let structured_output = if let Some(selection) = selected.as_ref()
            && let Some(adapter) = selected_adapter.as_ref()
        {
            adapter
                .settings_schema(&selection.runtime, model, &list.host, settings)
                .await
                .ok()
                .and_then(|schema| {
                    let id = norted_core::SettingId::new(format!(
                        "{}.structured_output_schema",
                        selection.runtime.manifest.identity.engine_id
                    ))
                    .ok()?;
                    schema
                        .definition(&id)
                        .map(|definition| definition.supported)
                })
                .unwrap_or(false)
        } else {
            false
        };

        let text_generation = !self.registry.compatible_with(model).iter().any(|adapter| {
            adapter.supports_model_capability(model, crate::ApiCapability::Embeddings)
                && !adapter.supports_model_capability(model, crate::ApiCapability::ChatCompletions)
                && !adapter.supports_model_capability(model, crate::ApiCapability::Responses)
        });
        Ok(ModelServingCapabilities {
            model_id: model.id.clone(),
            format: model.format,
            native_identity: model.native_identity.clone(),
            package: super::norted_package_summary(model),
            compatible_engine_ids,
            compatible_installed_runtime_ids,
            selected_runtime_id,
            active: active_model == Some(&model.id),
            text_input: true,
            text_output: text_generation,
            responses: text_generation,
            chat_completions: text_generation,
            streaming: text_generation,
            tools: selected_features.contains(&EngineFeature::ToolCalling),
            vision: selected_features.contains(&EngineFeature::Vision),
            structured_output,
        })
    }

    pub async fn check_updates(&self) -> Result<Vec<RuntimeUpdateCheck>, RuntimePackError> {
        let list = self.list().await?;
        let catalog = self.search("", true).await?;
        let mut checks = Vec::new();
        for status in list.installed {
            if status.runtime.manifest.acquisition_method
                == RuntimeAcquisitionMethod::ExternalBinary
            {
                checks.push(RuntimeUpdateCheck {
                    runtime: status.runtime,
                    state: RuntimeUpdateState::Unmanaged,
                });
                continue;
            }
            let identity = &status.runtime.manifest.identity;
            let adapter = self.registry.get(&identity.engine_id);
            let provider_error = catalog
                .provider_errors
                .iter()
                .find(|error| error.provider_id == identity.package.provider_id);
            if status.runtime.manifest.acquisition_method == RuntimeAcquisitionMethod::SourceBuild {
                let preference = list
                    .selections
                    .update_preferences
                    .get(&status.runtime.manifest.runtime_id)
                    .cloned()
                    .unwrap_or(RuntimeUpdatePreference::Latest);
                let update_line = catalog
                    .results
                    .iter()
                    .filter(|result| result.entry.compatibility.is_usable())
                    .filter(|result| {
                        same_update_line(identity, &result.entry.available, adapter.as_deref())
                    })
                    .filter(|result| {
                        source_recipe_ordering(
                            identity,
                            &result.entry.available.identity,
                            adapter.as_deref(),
                        ) != Ordering::Less
                    })
                    .collect::<Vec<_>>();
                let channel_available = update_line.iter().any(|result| {
                    update_channel_matches(&preference, None, &result.entry.available)
                });
                let candidate = update_line
                    .iter()
                    .copied()
                    .filter(|result| {
                        matches!(preference, RuntimeUpdatePreference::Pinned)
                            || update_channel_matches(&preference, None, &result.entry.available)
                    })
                    .max_by(|left, right| {
                        compare_source_update_candidates(
                            identity,
                            &left.entry.available,
                            &right.entry.available,
                            adapter.as_deref(),
                        )
                    });
                let state = if let Some(error) = provider_error {
                    if error.using_stale_cache {
                        RuntimeUpdateState::ProviderError(error.message.clone())
                    } else {
                        RuntimeUpdateState::CatalogUnavailable(error.message.clone())
                    }
                } else if !channel_available
                    && !matches!(preference, RuntimeUpdatePreference::Pinned)
                {
                    RuntimeUpdateState::ChannelUnavailable {
                        preference: preference.clone(),
                    }
                } else if let Some(candidate) = candidate {
                    let installed_source = status
                        .runtime
                        .manifest
                        .source_build
                        .as_ref()
                        .map(|source| &source.source);
                    let candidate_source = candidate
                        .entry
                        .available
                        .source_build()
                        .map(|source| &source.source);
                    match (installed_source, candidate_source) {
                        (Some(installed), Some(candidate_source)) => {
                            match self
                                .catalog
                                .compare_source_history(installed, candidate_source)
                                .await
                            {
                                Ok(comparison) => source_history_update_state(
                                    &preference,
                                    &candidate.entry.available.runtime_id,
                                    &candidate.entry.available.identity.version,
                                    &comparison,
                                    source_recipe_ordering(
                                        identity,
                                        &candidate.entry.available.identity,
                                        adapter.as_deref(),
                                    ),
                                ),
                                Err(error) => RuntimeUpdateState::ProviderError(error.to_string()),
                            }
                        }
                        (Some(_), None)
                            if q27_semantic_release_identity(identity)
                                && q27_semantic_release_identity(
                                    &candidate.entry.available.identity,
                                ) =>
                        {
                            semantic_release_update_state(
                                &preference,
                                identity,
                                &candidate.entry.available,
                            )
                        }
                        _ => RuntimeUpdateState::ProviderError(
                            "source-built runtime or update candidate is missing source provenance"
                                .to_owned(),
                        ),
                    }
                } else {
                    RuntimeUpdateState::NoLongerPublished
                };
                checks.push(RuntimeUpdateCheck {
                    runtime: status.runtime,
                    state,
                });
                continue;
            }
            let mut published = catalog
                .results
                .iter()
                .find(|result| {
                    result.entry.available.runtime_id == status.runtime.manifest.runtime_id
                })
                .map(|result| result.entry.available.clone());
            let mut exact_lookup_error = None;
            if published.is_none() && provider_error.is_none() {
                match self
                    .catalog
                    .find_reference(&status.runtime.manifest.runtime_id, &list.host)
                    .await
                {
                    Ok(Some(entry)) => published = Some(entry.available),
                    Ok(None) => {}
                    Err(error) => exact_lookup_error = Some(error.to_string()),
                }
            }
            let preference = list
                .selections
                .update_preferences
                .get(&status.runtime.manifest.runtime_id)
                .cloned()
                .unwrap_or(RuntimeUpdatePreference::Latest);
            let update_line = catalog
                .results
                .iter()
                .filter(|result| result.entry.compatibility.is_usable())
                .filter(|result| {
                    same_update_line(identity, &result.entry.available, adapter.as_deref())
                })
                .collect::<Vec<_>>();
            let channel_available = update_line.iter().any(|result| {
                update_channel_matches(&preference, published.as_ref(), &result.entry.available)
            });
            let newer = catalog
                .results
                .iter()
                .filter(|result| result.entry.compatibility.is_usable())
                .filter(|result| {
                    same_update_line(identity, &result.entry.available, adapter.as_deref())
                })
                .filter(|result| {
                    update_channel_matches(&preference, published.as_ref(), &result.entry.available)
                })
                .filter(|result| {
                    compare_versions(&result.entry.available.identity.version, &identity.version)
                        == Ordering::Greater
                })
                .max_by(|left, right| {
                    compare_versions(
                        &left.entry.available.identity.version,
                        &right.entry.available.identity.version,
                    )
                });
            let provenance_conflict = published.as_ref().and_then(|published| {
                ensure_catalog_provenance_matches(&status.runtime, published).err()
            });
            let state = if let Some(error) = provenance_conflict {
                RuntimeUpdateState::ProviderError(error.to_string())
            } else if let Some(error) = exact_lookup_error {
                RuntimeUpdateState::ProviderError(error)
            } else if let Some(error) = provider_error {
                if error.using_stale_cache {
                    RuntimeUpdateState::ProviderError(error.message.clone())
                } else {
                    RuntimeUpdateState::CatalogUnavailable(error.message.clone())
                }
            } else if published.is_none() {
                RuntimeUpdateState::NoLongerPublished
            } else if !channel_available && !matches!(preference, RuntimeUpdatePreference::Pinned) {
                RuntimeUpdateState::ChannelUnavailable {
                    preference: preference.clone(),
                }
            } else if matches!(preference, RuntimeUpdatePreference::Pinned) {
                RuntimeUpdateState::Pinned {
                    newer_runtime_id: newer.map(|newer| newer.entry.available.runtime_id.clone()),
                    newer_version: newer
                        .map(|newer| newer.entry.available.identity.version.clone()),
                }
            } else if let Some(newer) = newer {
                RuntimeUpdateState::NewerCompatibleVersion {
                    runtime_id: newer.entry.available.runtime_id.clone(),
                    version: newer.entry.available.identity.version.clone(),
                }
            } else {
                RuntimeUpdateState::Current
            };
            checks.push(RuntimeUpdateCheck {
                runtime: status.runtime,
                state,
            });
        }
        Ok(checks)
    }

    pub async fn update(
        &self,
        runtime_id: &RuntimeId,
    ) -> Result<InstalledRuntime, RuntimePackError> {
        let check = self
            .check_updates()
            .await?
            .into_iter()
            .find(|check| &check.runtime.manifest.runtime_id == runtime_id)
            .ok_or_else(|| RuntimePackError::NotInstalled(runtime_id.clone()))?;
        match check.state {
            RuntimeUpdateState::NewerCompatibleVersion { runtime_id, .. } => {
                self.install(&runtime_id).await
            }
            RuntimeUpdateState::Pinned {
                newer_runtime_id: Some(newer),
                ..
            } => self.install(&newer).await,
            RuntimeUpdateState::Pinned {
                newer_runtime_id: None,
                ..
            } => Ok(check.runtime),
            RuntimeUpdateState::Current => Ok(check.runtime),
            state => Err(RuntimePackError::Selection(format!(
                "runtime update is unavailable: {state:?}"
            ))),
        }
    }

    pub async fn installed(
        &self,
        runtime_id: &RuntimeId,
    ) -> Result<Option<InstalledRuntime>, RuntimePackError> {
        Ok(self
            .list()
            .await?
            .installed
            .into_iter()
            .find(|status| &status.runtime.manifest.runtime_id == runtime_id)
            .map(|status| status.runtime))
    }

    pub async fn compatible_installed_for_model(
        &self,
        model: &ModelArtifact,
    ) -> Result<Vec<RuntimeModelCandidate>, RuntimePackError> {
        self.compatible_installed_for_model_with_settings(model, None)
            .await
    }

    pub async fn compatible_installed_for_model_with_settings(
        &self,
        model: &ModelArtifact,
        settings: Option<&norted_core::ResolvedSettings>,
    ) -> Result<Vec<RuntimeModelCandidate>, RuntimePackError> {
        let host = self.refresh_host_capabilities().await;
        let list = self.list().await?;
        let mut compatible = Vec::new();
        let mut rejected = Vec::new();
        for status in &list.installed {
            if !status
                .runtime
                .manifest
                .supported_formats
                .contains(&model.format)
            {
                continue;
            }
            match self.model_candidate_compatibility(&status.runtime, model, &host, settings) {
                Ok(compatibility) if compatibility.is_usable() => compatible.push((
                    status,
                    compatibility,
                    self.model_candidate_preference(&status.runtime, model, &host),
                )),
                Ok(RuntimeCompatibility::Incompatible(reason)) => rejected.push(format!(
                    "runtime `{}` is incompatible: {reason}",
                    status.runtime.manifest.runtime_id
                )),
                Ok(_) => unreachable!("all non-incompatible states are usable"),
                Err(error) => rejected.push(error.to_string()),
            }
        }
        compatible.sort_by(|left, right| {
            left.1
                .preference_rank()
                .cmp(&right.1.preference_rank())
                .then_with(|| left.2.cmp(&right.2))
                .then_with(|| {
                    acquisition_rank(&left.0.runtime).cmp(&acquisition_rank(&right.0.runtime))
                })
                .then_with(|| {
                    compare_installed_recency_with_registry(
                        &self.registry,
                        &right.0.runtime,
                        &left.0.runtime,
                    )
                })
                .then_with(|| {
                    left.0
                        .runtime
                        .manifest
                        .runtime_id
                        .cmp(&right.0.runtime.manifest.runtime_id)
                })
        });
        if compatible.is_empty() {
            let detail = if rejected.is_empty() {
                String::new()
            } else {
                format!(": {}", rejected.join("; "))
            };
            return Err(RuntimePackError::Selection(format!(
                "no compatible installed runtime supports model `{}`{detail}",
                model.id
            )));
        }
        Ok(compatible
            .into_iter()
            .map(|(status, compatibility, _)| RuntimeModelCandidate {
                runtime_id: status.runtime.manifest.runtime_id.clone(),
                compatibility,
            })
            .collect())
    }

    fn selected_candidate(
        &self,
        list: &RuntimeListSnapshot,
        runtime_id: &RuntimeId,
        model: &ModelArtifact,
        settings: Option<&norted_core::ResolvedSettings>,
    ) -> Result<InstalledRuntime, RuntimePackError> {
        let status = list
            .installed
            .iter()
            .find(|status| &status.runtime.manifest.runtime_id == runtime_id)
            .ok_or_else(|| RuntimePackError::NotInstalled(runtime_id.clone()))?;
        if let RuntimeCompatibility::Incompatible(reason) = &status.compatibility {
            return Err(RuntimePackError::Incompatible {
                runtime_id: runtime_id.clone(),
                reason: reason.clone(),
            });
        }
        self.validate_model_candidate(&status.runtime, model, &list.host, settings)?;
        Ok(status.runtime.clone())
    }

    fn validate_format_candidate(
        &self,
        runtime: &InstalledRuntime,
        format: ArtifactFormat,
        host: &HostCapabilities,
    ) -> Result<(), RuntimePackError> {
        if !runtime.manifest.supported_formats.contains(&format) {
            return Err(RuntimePackError::Incompatible {
                runtime_id: runtime.manifest.runtime_id.clone(),
                reason: format!("runtime does not support {}", format.as_str()),
            });
        }
        let adapter = self
            .registry
            .get(&runtime.manifest.identity.engine_id)
            .ok_or_else(|| RuntimePackError::Incompatible {
                runtime_id: runtime.manifest.runtime_id.clone(),
                reason: "engine adapter is not registered".to_owned(),
            })?;
        if let CompatibilityDecision::Unsupported { reason } =
            adapter.runtime_management_compatibility()
        {
            return Err(RuntimePackError::Incompatible {
                runtime_id: runtime.manifest.runtime_id.clone(),
                reason,
            });
        }
        if let CompatibilityDecision::Unsupported { reason } =
            adapter.runtime_compatibility(runtime)
        {
            return Err(RuntimePackError::Incompatible {
                runtime_id: runtime.manifest.runtime_id.clone(),
                reason,
            });
        }
        if !adapter.capabilities().accepts(format) {
            return Err(RuntimePackError::Incompatible {
                runtime_id: runtime.manifest.runtime_id.clone(),
                reason: "engine adapter does not support this artifact format".to_owned(),
            });
        }
        let compatibility = compatibility_for_installed(runtime, host);
        if let RuntimeCompatibility::Incompatible(reason) = compatibility {
            return Err(RuntimePackError::Incompatible {
                runtime_id: runtime.manifest.runtime_id.clone(),
                reason,
            });
        }
        Ok(())
    }

    fn validate_model_candidate(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        host: &HostCapabilities,
        settings: Option<&norted_core::ResolvedSettings>,
    ) -> Result<(), RuntimePackError> {
        let compatibility = self.model_candidate_compatibility(runtime, model, host, settings)?;
        if let RuntimeCompatibility::Incompatible(reason) = compatibility {
            return Err(RuntimePackError::Incompatible {
                runtime_id: runtime.manifest.runtime_id.clone(),
                reason,
            });
        }
        Ok(())
    }

    fn model_candidate_compatibility(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        host: &HostCapabilities,
        _settings: Option<&norted_core::ResolvedSettings>,
    ) -> Result<RuntimeCompatibility, RuntimePackError> {
        self.validate_format_candidate(runtime, model.format, host)?;
        let adapter = self
            .registry
            .get(&runtime.manifest.identity.engine_id)
            .ok_or_else(|| RuntimePackError::Incompatible {
                runtime_id: runtime.manifest.runtime_id.clone(),
                reason: "engine adapter is not registered".to_owned(),
            })?;
        if let CompatibilityDecision::Unsupported { reason } = adapter.compatibility(model) {
            return Ok(RuntimeCompatibility::Incompatible(reason));
        }
        Ok(combine_compatibility(
            compatibility_for_installed(runtime, host),
            adapter.runtime_model_compatibility(runtime, model, host, None),
        ))
    }

    fn model_candidate_preference(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        host: &HostCapabilities,
    ) -> u16 {
        self.registry
            .get(&runtime.manifest.identity.engine_id)
            .map_or(u16::MAX, |adapter| {
                adapter.runtime_model_preference(runtime, model, host)
            })
    }

    fn model_candidate_accelerator_binding(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        host: &HostCapabilities,
        settings: Option<&norted_core::ResolvedSettings>,
    ) -> Option<norted_core::AcceleratorBinding> {
        self.registry
            .get(&runtime.manifest.identity.engine_id)
            .and_then(|adapter| {
                adapter.runtime_model_accelerator_binding(runtime, model, host, settings)
            })
    }
}

fn combine_compatibility(
    host: RuntimeCompatibility,
    model: RuntimeCompatibility,
) -> RuntimeCompatibility {
    match (host, model) {
        (RuntimeCompatibility::Incompatible(reason), _)
        | (_, RuntimeCompatibility::Incompatible(reason)) => {
            RuntimeCompatibility::Incompatible(reason)
        }
        (RuntimeCompatibility::NeedsAttention(reason), _)
        | (_, RuntimeCompatibility::NeedsAttention(reason)) => {
            RuntimeCompatibility::NeedsAttention(reason)
        }
        (RuntimeCompatibility::Recommended, RuntimeCompatibility::Recommended) => {
            RuntimeCompatibility::Recommended
        }
        _ => RuntimeCompatibility::Compatible,
    }
}

fn external_runtime(
    engine_id: &str,
    supported_formats: Vec<ArtifactFormat>,
    installation: EngineInstallation,
    detail: String,
) -> Result<InstalledRuntime, RuntimePackError> {
    let binary_sha256 = installation.binary_sha256.clone().ok_or_else(|| {
        RuntimePackError::Selection(format!(
            "external {engine_id} binary has no observed SHA-256"
        ))
    })?;
    let binary_path = installation.binary_path.clone();
    let identity = RuntimeIdentity {
        engine_id: engine_id.to_owned(),
        package_family: engine_id.to_owned(),
        version: installation
            .engine
            .version
            .clone()
            .unwrap_or_else(|| "unknown".to_owned()),
        upstream_revision: installation.engine.revision.clone(),
        platform: installation.platform,
        architecture: installation.architecture,
        accelerator: "external".to_owned(),
        variant: installation
            .runtime_variant
            .unwrap_or_else(|| "external-binary".to_owned()),
        package: RuntimePackageIdentity {
            provider_id: "external-binary".to_owned(),
            repository: installation.source_repository,
            release_tag: None,
            asset_id: Some(binary_path.to_string_lossy().into_owned()),
            asset_name: binary_path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned),
            additional_assets: Vec::new(),
        },
    };
    let runtime_id = RuntimeId::from_identity(&identity);
    let runtime = InstalledRuntime {
        manifest: RuntimeManifest {
            schema_version: RUNTIME_MANIFEST_SCHEMA_VERSION,
            runtime_id,
            identity,
            supported_formats,
            supported_native_identities: Vec::new(),
            requirements: Default::default(),
            acquisition_method: RuntimeAcquisitionMethod::ExternalBinary,
            source_url: None,
            downloaded_archive_sha256: None,
            additional_downloaded_archive_sha256: Vec::new(),
            source_build: None,
            entrypoint: binary_path.clone(),
            entrypoint_sha256: binary_sha256,
            installed_at_unix: None,
            probe: RuntimeProbeObservation {
                compatible: true,
                observed_engine_id: engine_id.to_owned(),
                observed_version: installation.engine.version,
                observed_revision: installation.engine.revision,
                detail,
                observed_at_unix: installation.observed_at_unix,
            },
        },
        installation_root: binary_path.parent().map(PathBuf::from).unwrap_or_default(),
    };
    runtime
        .validate()
        .map_err(|error| RuntimePackError::Selection(error.to_string()))?;
    Ok(runtime)
}

fn ensure_catalog_provenance_matches(
    installed: &InstalledRuntime,
    available: &AvailableRuntime,
) -> Result<(), RuntimePackError> {
    let manifest = &installed.manifest;
    let common_matches = manifest.runtime_id == available.runtime_id
        && manifest.identity == available.identity
        && manifest.supported_formats == available.supported_formats
        && manifest.requirements == available.requirements
        && manifest.source_url.as_deref() == Some(available.source_url.as_str());
    let acquisition_matches = match &available.acquisition {
        norted_core::RuntimeAcquisitionPlan::ReleaseAsset {
            download,
            additional_downloads,
        } => {
            let primary_digest = download.digest.as_ref().map(|digest| &digest.value);
            let additional_digests = additional_downloads
                .iter()
                .filter_map(|download| download.digest.as_ref().map(|digest| digest.value.clone()))
                .collect::<Vec<_>>();
            matches!(
                manifest.acquisition_method,
                RuntimeAcquisitionMethod::OfficialReleaseAsset
                    | RuntimeAcquisitionMethod::PreseededOfficialPack
            ) && manifest.downloaded_archive_sha256.as_ref() == primary_digest
                && additional_digests.len() == additional_downloads.len()
                && manifest.additional_downloaded_archive_sha256 == additional_digests
                && manifest.source_build.is_none()
        }
        norted_core::RuntimeAcquisitionPlan::SourceBuild(plan) => {
            manifest.acquisition_method == RuntimeAcquisitionMethod::SourceBuild
                && manifest.downloaded_archive_sha256.is_none()
                && manifest.additional_downloaded_archive_sha256.is_empty()
                && manifest.source_build.as_ref().is_some_and(|provenance| {
                    source_build_provenance_matches_plan(provenance, plan)
                })
        }
    };
    let matches = common_matches && acquisition_matches;
    if matches {
        return Ok(());
    }
    Err(RuntimeStoreError::ProvenanceConflict {
        runtime_id: manifest.runtime_id.clone(),
        detail: "the current provider metadata no longer matches the immutable installed identity, requirements, source, or verified archive digests".to_owned(),
    }
    .into())
}

fn source_build_provenance_matches_plan(
    provenance: &norted_core::RuntimeSourceBuildProvenance,
    plan: &norted_core::RuntimeSourceBuildPlan,
) -> bool {
    effective_cmake_configuration_arguments(plan).is_ok_and(|effective_arguments| {
        let effective_matches = match (
            provenance.effective_cmake_configuration_arguments.as_ref(),
            effective_arguments.as_ref(),
        ) {
            (Some(recorded), Some(expected)) => recorded == expected,
            // Legacy schema-2 source manifests retained only the
            // provider-owned arguments. Accept them without inferring or
            // rewriting the omitted effective list.
            (None, _) => true,
            (Some(_), None) => false,
        };
        provenance.source == plan.source
            && provenance.recipe_version == plan.recipe.recipe_version
            && provenance.build_system == plan.recipe.build_system
            && provenance.cmake_configuration_arguments == plan.recipe.cmake_configuration_arguments
            && effective_matches
            && provenance.build_target == plan.recipe.build_target
            && provenance.accelerator_target == plan.recipe.accelerator_target
    })
}

fn compatibility_for_installed(
    runtime: &InstalledRuntime,
    host: &HostCapabilities,
) -> RuntimeCompatibility {
    compatibility_for(
        &runtime.manifest.identity.platform,
        &runtime.manifest.identity.architecture,
        &runtime.manifest.identity.accelerator,
        &runtime.manifest.requirements,
        host,
    )
}

fn selected_for(selections: &RuntimeSelections, runtime_id: &RuntimeId) -> Vec<String> {
    let mut selected = selections
        .format_defaults
        .iter()
        .filter(|(_, selected)| *selected == runtime_id)
        .map(|(format, _)| format!("{} default", format.as_str().to_ascii_uppercase()))
        .collect::<Vec<_>>();
    selected.extend(
        selections
            .model_overrides
            .iter()
            .filter(|(_, selected)| *selected == runtime_id)
            .map(|(model, _)| format!("model {model}")),
    );
    selected
}

fn acquisition_rank(runtime: &InstalledRuntime) -> u8 {
    match runtime.manifest.acquisition_method {
        RuntimeAcquisitionMethod::OfficialReleaseAsset
        | RuntimeAcquisitionMethod::PreseededOfficialPack
        | RuntimeAcquisitionMethod::SourceBuild => 0,
        RuntimeAcquisitionMethod::ExternalBinary => 1,
    }
}

fn same_update_line(
    identity: &RuntimeIdentity,
    candidate: &AvailableRuntime,
    adapter: Option<&dyn crate::EngineAdapter>,
) -> bool {
    same_runtime_update_line(identity, &candidate.identity, adapter)
}

fn same_runtime_update_line(
    identity: &RuntimeIdentity,
    candidate: &RuntimeIdentity,
    adapter: Option<&dyn crate::EngineAdapter>,
) -> bool {
    let installed_variant = adapter.map_or_else(
        || crate::RuntimeVariantUpdateIdentity::exact(identity),
        |adapter| adapter.runtime_variant_update_identity(identity),
    );
    let candidate_variant = adapter.map_or_else(
        || crate::RuntimeVariantUpdateIdentity::exact(candidate),
        |adapter| adapter.runtime_variant_update_identity(candidate),
    );
    candidate.engine_id == identity.engine_id
        && (candidate.package_family == identity.package_family
            || q27_logical_update_family(identity, candidate))
        && candidate.platform == identity.platform
        && candidate.architecture == identity.architecture
        && candidate.accelerator == identity.accelerator
        && candidate_variant.functional_variant == installed_variant.functional_variant
        && candidate.package.provider_id == identity.package.provider_id
}

fn source_recipe_ordering(
    installed: &RuntimeIdentity,
    candidate: &RuntimeIdentity,
    adapter: Option<&dyn crate::EngineAdapter>,
) -> Ordering {
    let installed = adapter.map_or_else(
        || crate::RuntimeVariantUpdateIdentity::exact(installed),
        |adapter| adapter.runtime_variant_update_identity(installed),
    );
    let candidate = adapter.map_or_else(
        || crate::RuntimeVariantUpdateIdentity::exact(candidate),
        |adapter| adapter.runtime_variant_update_identity(candidate),
    );
    if installed.functional_variant != candidate.functional_variant {
        return Ordering::Equal;
    }
    match (
        candidate.source_recipe_generation,
        installed.source_recipe_generation,
    ) {
        (Some(candidate), Some(installed)) => candidate.cmp(&installed),
        _ => Ordering::Equal,
    }
}

fn q27_logical_update_family(left: &RuntimeIdentity, right: &RuntimeIdentity) -> bool {
    fn family(identity: &RuntimeIdentity) -> bool {
        identity.package_family == "q27-official-release"
            || identity
                .package_family
                .starts_with("q27-official-source-q27-upstream-make-")
    }

    left.engine_id == "q27"
        && right.engine_id == "q27"
        && left.package.provider_id == "q27-official-github"
        && right.package.provider_id == "q27-official-github"
        && left.package.repository.as_deref() == Some("signalnine/q27")
        && right.package.repository.as_deref() == Some("signalnine/q27")
        && family(left)
        && family(right)
}

fn q27_semantic_release_identity(identity: &RuntimeIdentity) -> bool {
    q27_logical_update_family(identity, identity)
        && identity
            .package
            .release_tag
            .as_deref()
            .and_then(|tag| tag.strip_prefix('v'))
            == Some(identity.version.as_str())
}

fn compare_source_update_candidates(
    installed: &RuntimeIdentity,
    left: &AvailableRuntime,
    right: &AvailableRuntime,
    adapter: Option<&dyn crate::EngineAdapter>,
) -> Ordering {
    if q27_semantic_release_identity(installed)
        && q27_semantic_release_identity(&left.identity)
        && q27_semantic_release_identity(&right.identity)
    {
        return compare_versions(&left.identity.version, &right.identity.version)
            .then_with(|| left.published_at_unix.cmp(&right.published_at_unix));
    }
    source_recipe_ordering(&right.identity, &left.identity, adapter)
        .then_with(|| left.published_at_unix.cmp(&right.published_at_unix))
}

fn semantic_release_update_state(
    preference: &RuntimeUpdatePreference,
    installed: &RuntimeIdentity,
    candidate: &AvailableRuntime,
) -> RuntimeUpdateState {
    if compare_versions(&candidate.identity.version, &installed.version) == Ordering::Greater {
        if matches!(preference, RuntimeUpdatePreference::Pinned) {
            RuntimeUpdateState::Pinned {
                newer_runtime_id: Some(candidate.runtime_id.clone()),
                newer_version: Some(candidate.identity.version.clone()),
            }
        } else {
            RuntimeUpdateState::NewerCompatibleVersion {
                runtime_id: candidate.runtime_id.clone(),
                version: candidate.identity.version.clone(),
            }
        }
    } else if matches!(preference, RuntimeUpdatePreference::Pinned) {
        RuntimeUpdateState::Pinned {
            newer_runtime_id: None,
            newer_version: None,
        }
    } else {
        RuntimeUpdateState::Current
    }
}

fn source_history_update_state(
    preference: &RuntimeUpdatePreference,
    candidate_runtime_id: &RuntimeId,
    candidate_version: &str,
    comparison: &crate::GitHubCompare,
    recipe_ordering: Ordering,
) -> RuntimeUpdateState {
    match comparison.status {
        GitHubComparisonStatus::Identical
            if comparison.ahead_by == 0 && comparison.behind_by == 0 =>
        {
            if recipe_ordering == Ordering::Greater {
                if matches!(preference, RuntimeUpdatePreference::Pinned) {
                    RuntimeUpdateState::Pinned {
                        newer_runtime_id: Some(candidate_runtime_id.clone()),
                        newer_version: Some(candidate_version.to_owned()),
                    }
                } else {
                    RuntimeUpdateState::NewerCompatibleVersion {
                        runtime_id: candidate_runtime_id.clone(),
                        version: candidate_version.to_owned(),
                    }
                }
            } else if matches!(preference, RuntimeUpdatePreference::Pinned) {
                RuntimeUpdateState::Pinned {
                    newer_runtime_id: None,
                    newer_version: None,
                }
            } else {
                RuntimeUpdateState::Current
            }
        }
        GitHubComparisonStatus::Ahead if comparison.ahead_by > 0 && comparison.behind_by == 0 => {
            if matches!(preference, RuntimeUpdatePreference::Pinned) {
                RuntimeUpdateState::Pinned {
                    newer_runtime_id: Some(candidate_runtime_id.clone()),
                    newer_version: Some(candidate_version.to_owned()),
                }
            } else {
                RuntimeUpdateState::NewerCompatibleVersion {
                    runtime_id: candidate_runtime_id.clone(),
                    version: candidate_version.to_owned(),
                }
            }
        }
        _ => RuntimeUpdateState::ProviderError(format!(
            "current source HEAD is not a strict descendant of the installed commit (GitHub comparison: {:?}, ahead {}, behind {})",
            comparison.status, comparison.ahead_by, comparison.behind_by
        )),
    }
}

fn update_channel_matches(
    preference: &RuntimeUpdatePreference,
    published: Option<&AvailableRuntime>,
    candidate: &AvailableRuntime,
) -> bool {
    let channel = match preference {
        RuntimeUpdatePreference::Stable => RuntimeReleaseChannel::Stable,
        RuntimeUpdatePreference::Latest => RuntimeReleaseChannel::Latest,
        RuntimeUpdatePreference::Pinned => {
            if published.is_some_and(|published| {
                !published.prerelease || published.channels.contains(&RuntimeReleaseChannel::Stable)
            }) {
                RuntimeReleaseChannel::Stable
            } else {
                RuntimeReleaseChannel::Latest
            }
        }
    };
    candidate.channels.contains(&channel)
}

/// Conservative deletion proof layered on the resolver's update line and ordering.
/// Fallback timestamp, hash and RuntimeId tie-breakers are not replacement evidence.
pub(crate) fn is_proven_superseding_installed_runtime(
    registry: &crate::EngineRegistry,
    newer: &InstalledRuntime,
    older: &InstalledRuntime,
) -> bool {
    let new = &newer.manifest;
    let old = &older.manifest;
    let adapter = registry.get(&old.identity.engine_id);
    if !same_runtime_update_line(&old.identity, &new.identity, adapter.as_deref())
        || old.identity.package.repository != new.identity.package.repository
        || old.supported_formats != new.supported_formats
        || old.supported_native_identities != new.supported_native_identities
        || old.requirements != new.requirements
        || matches!(
            old.acquisition_method,
            norted_core::RuntimeAcquisitionMethod::ExternalBinary
        )
        || matches!(
            new.acquisition_method,
            norted_core::RuntimeAcquisitionMethod::ExternalBinary
        )
        || (old.acquisition_method != new.acquisition_method
            && !q27_logical_update_family(&old.identity, &new.identity))
    {
        return false;
    }
    let recipe_upgrade = match (&old.source_build, &new.source_build) {
        (Some(old_source), Some(new_source)) => {
            old_source.source == new_source.source
                && source_recipe_ordering(&old.identity, &new.identity, adapter.as_deref())
                    == Ordering::Greater
        }
        _ => false,
    };
    // Only recognized numeric releases (including llama.cpp bNNNN) prove a
    // version advance. Different Git snapshots need ancestry evidence, which
    // this offline operation does not have.
    let release_parts = |version: &str| -> Option<Vec<u64>> {
        version
            .trim_start_matches(['v', 'b'])
            .split('.')
            .map(|part| {
                if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                    None
                } else {
                    part.parse().ok()
                }
            })
            .collect()
    };
    let release_upgrade = match (
        release_parts(&new.identity.version),
        release_parts(&old.identity.version),
    ) {
        (Some(new), Some(old)) => new > old,
        _ => false,
    };
    (recipe_upgrade || release_upgrade)
        && compare_installed_recency_with_registry(registry, newer, older) == Ordering::Greater
}

fn compare_installed_recency_with_registry(
    registry: &crate::EngineRegistry,
    left: &InstalledRuntime,
    right: &InstalledRuntime,
) -> Ordering {
    let adapter = (left.manifest.identity.engine_id == right.manifest.identity.engine_id)
        .then(|| registry.get(&left.manifest.identity.engine_id))
        .flatten();
    compare_installed_recency(left, right, adapter.as_deref())
}

pub(crate) fn compare_installed_recency(
    left: &InstalledRuntime,
    right: &InstalledRuntime,
    adapter: Option<&dyn crate::EngineAdapter>,
) -> Ordering {
    compare_installed_recency_inner(left, right, adapter, false)
}

fn compare_installed_recency_inner(
    left: &InstalledRuntime,
    right: &InstalledRuntime,
    adapter: Option<&dyn crate::EngineAdapter>,
    lineage_only: bool,
) -> Ordering {
    let left_manifest = &left.manifest;
    let right_manifest = &right.manifest;
    let left_variant = adapter.map_or_else(
        || crate::RuntimeVariantUpdateIdentity::exact(&left_manifest.identity),
        |adapter| adapter.runtime_variant_update_identity(&left_manifest.identity),
    );
    let right_variant = adapter.map_or_else(
        || crate::RuntimeVariantUpdateIdentity::exact(&right_manifest.identity),
        |adapter| adapter.runtime_variant_update_identity(&right_manifest.identity),
    );
    let same_update_line =
        same_runtime_update_line(&left_manifest.identity, &right_manifest.identity, adapter);
    let recipe_ordering = match (
        left_variant.source_recipe_generation,
        right_variant.source_recipe_generation,
    ) {
        (Some(left), Some(right)) => left.cmp(&right),
        _ => Ordering::Equal,
    };
    if (same_update_line || lineage_only)
        && let (Some(left_source), Some(right_source)) = (
            left_manifest.source_build.as_ref(),
            right_manifest.source_build.as_ref(),
        )
    {
        // Commit time provides offline recency, not ancestry evidence.
        // Update availability is decided separately using Git history.
        let commit_ordering = left_source
            .source
            .commit_timestamp_unix
            .cmp(&right_source.source.commit_timestamp_unix);
        if lineage_only {
            // The local badge spans variants. Hashes and runtime IDs are not
            // recency evidence, and recipe generations only rank one update line.
            return commit_ordering.then(if same_update_line {
                recipe_ordering
            } else {
                Ordering::Equal
            });
        }
        return commit_ordering
            .then_with(|| {
                left_source
                    .source
                    .commit_sha
                    .cmp(&right_source.source.commit_sha)
            })
            .then(recipe_ordering)
            .then_with(|| left_manifest.runtime_id.cmp(&right_manifest.runtime_id));
    }
    compare_versions(
        &left_manifest.identity.version,
        &right_manifest.identity.version,
    )
    .then(if same_update_line {
        recipe_ordering
    } else {
        Ordering::Equal
    })
}

fn compare_versions(left: &str, right: &str) -> Ordering {
    let left_parts = version_parts(left);
    let right_parts = version_parts(right);
    left_parts.cmp(&right_parts).then_with(|| left.cmp(right))
}

fn version_parts(value: &str) -> Vec<u64> {
    value
        .split(|character: char| !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use async_trait::async_trait;
    use norted_core::{
        AcquisitionMethod, AppPaths, ArtifactFormat, AvailableRuntime, EngineInstallation,
        EngineRevision, InstalledRuntime, ModelArtifact, ModelId, RUNTIME_MANIFEST_SCHEMA_VERSION,
        RuntimeAcquisitionMethod, RuntimeAcquisitionPlan, RuntimeArchiveFormat, RuntimeDownload,
        RuntimeId, RuntimeIdentity, RuntimeManifest, RuntimePackageIdentity,
        RuntimeProbeObservation, RuntimeReleaseChannel, RuntimeRequirements,
        RuntimeSourceBuildPlan, RuntimeSourceBuildPrerequisites, RuntimeSourceBuildProvenance,
        RuntimeSourceBuildRecipe, RuntimeSourceBuildSystem, RuntimeSourceBuildToolchain,
        RuntimeSourceSnapshot, RuntimeUpdatePreference, RuntimeUpdateState, SettingDefinition,
        SettingId, SettingKind, SettingScope, SettingsSchema,
    };

    use super::{
        compare_installed_recency, same_update_line, semantic_release_update_state,
        source_build_provenance_matches_plan, source_history_update_state, source_recipe_ordering,
    };
    use crate::{
        EffectiveGenerationSettings, EngineAdapter, EngineCapabilities, EngineError,
        EngineIdentity, EngineProbe, EngineRegistry, GitHubCompare, GitHubComparisonStatus,
        InferenceOutput, InferenceRequest, InferenceStream, InstallationState, LaunchRequest,
        LaunchSpec, NativeOption, ProcessDescriptor, RuntimeCatalogProvider,
        RuntimeVariantUpdateIdentity, UpdateState,
    };

    struct BoundSchemaAdapter {
        id: &'static str,
    }

    #[async_trait]
    impl EngineAdapter for BoundSchemaAdapter {
        fn identity(&self) -> EngineIdentity {
            EngineIdentity {
                id: self.id.to_owned(),
                display_name: self.id.to_owned(),
                upstream_repository: String::new(),
            }
        }

        fn capabilities(&self) -> EngineCapabilities {
            EngineCapabilities {
                artifact_formats: vec![ArtifactFormat::Gguf],
                ..EngineCapabilities::default()
            }
        }

        fn runtime_variant_update_identity(
            &self,
            identity: &RuntimeIdentity,
        ) -> RuntimeVariantUpdateIdentity {
            let generation = match identity.variant.as_str() {
                "recipe-v1" => Some(1),
                "recipe-v2" => Some(2),
                "recipe-v3" => Some(3),
                _ => None,
            };
            generation.map_or_else(
                || RuntimeVariantUpdateIdentity::exact(identity),
                |source_recipe_generation| RuntimeVariantUpdateIdentity {
                    functional_variant: "fixture-functional-variant".to_owned(),
                    source_recipe_generation: Some(source_recipe_generation),
                },
            )
        }

        fn native_options(&self) -> Vec<NativeOption> {
            Vec::new()
        }

        async fn settings_schema(
            &self,
            runtime: &InstalledRuntime,
            _model: &ModelArtifact,
            _host: &norted_core::HostCapabilities,
            _settings: Option<&norted_core::ResolvedSettings>,
        ) -> Result<SettingsSchema, EngineError> {
            Ok(SettingsSchema {
                engine_id: self.id.to_owned(),
                runtime_id: Some(runtime.manifest.runtime_id.clone()),
                definitions: vec![SettingDefinition {
                    id: SettingId::new(format!("{}.marker", self.id)).expect("setting ID"),
                    label: self.id.to_owned(),
                    description: self.id.to_owned(),
                    kind: SettingKind::Toggle,
                    scope: SettingScope::Runtime {
                        engine_id: self.id.to_owned(),
                    },
                    category: norted_core::SettingCategory::Advanced,
                    supported: true,
                    unsupported_reason: None,
                    unit: None,
                    default_preview: None,
                }],
            })
        }

        async fn probe(&self) -> Result<EngineProbe, EngineError> {
            Ok(EngineProbe {
                installation: InstallationState::Installed {
                    installation: Box::new(EngineInstallation {
                        engine: EngineRevision {
                            engine_id: self.id.to_owned(),
                            version: Some("1".to_owned()),
                            revision: None,
                        },
                        source_repository: None,
                        acquisition_method: AcquisitionMethod::ExternalBinary,
                        binary_path: PathBuf::from(format!("/tmp/{}-server", self.id)),
                        binary_sha256: Some("a".repeat(64)),
                        build: None,
                        platform: std::env::consts::OS.to_owned(),
                        architecture: std::env::consts::ARCH.to_owned(),
                        runtime_variant: Some("fixture".to_owned()),
                        acquired_at_unix: None,
                        observed_at_unix: 1,
                    }),
                },
                update: UpdateState::Unknown,
                healthy: true,
                detail: "fixture".to_owned(),
            })
        }

        async fn probe_runtime(
            &self,
            _runtime: &InstalledRuntime,
        ) -> Result<RuntimeProbeObservation, EngineError> {
            unreachable!()
        }

        async fn build_launch_spec(
            &self,
            _request: LaunchRequest,
        ) -> Result<LaunchSpec, EngineError> {
            unreachable!()
        }

        async fn health(&self, _process: &ProcessDescriptor) -> Result<bool, EngineError> {
            unreachable!()
        }

        async fn effective_generation_settings(
            &self,
            _process: &ProcessDescriptor,
        ) -> Result<EffectiveGenerationSettings, EngineError> {
            unreachable!()
        }

        async fn infer(
            &self,
            _endpoint: &str,
            _request: InferenceRequest,
        ) -> Result<InferenceOutput, EngineError> {
            unreachable!()
        }

        async fn infer_stream(
            &self,
            _endpoint: &str,
            _request: InferenceRequest,
            _activity: crate::InferenceActivityReporter,
        ) -> Result<InferenceStream, EngineError> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn bound_engine_schema_resolution_never_selects_another_compatible_engine() {
        let temporary = tempfile::tempdir().expect("temporary directory");
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
            settings_file: root.join("data/settings.json"),
            settings_lock_file: root.join("data/.settings.lock"),
            model_profiles_file: root.join("data/model-profiles.json"),
            model_profiles_lock_file: root.join("data/.model-profiles.lock"),
        };
        paths.ensure_required().expect("application paths");
        let mut registry = EngineRegistry::default();
        registry
            .register(Arc::new(BoundSchemaAdapter { id: "fake_a" }))
            .expect("fake_a adapter");
        registry
            .register(Arc::new(BoundSchemaAdapter { id: "fake_b" }))
            .expect("fake_b adapter");
        let providers = Vec::<Arc<dyn RuntimeCatalogProvider>>::new();
        let manager =
            super::RuntimePackManager::new(&paths, registry, providers).expect("runtime manager");
        let model = ModelArtifact {
            id: ModelId("fixture".to_owned()),
            display_name: "Fixture".to_owned(),
            path: PathBuf::from("fixture.gguf"),
            format: ArtifactFormat::Gguf,
            size_bytes: 1,
            created: 1,
            hash: None,
            architecture: None,
            context_length: None,
            provenance: None,
            native_identity: None,
            auxiliary_artifacts: Vec::new(),
            norted_package: None,
        };

        let (selection, schema) = manager
            .settings_schema_for_model_for_engine_with_settings(&model, "fake_b", None, None)
            .await
            .expect("bound fake_b schema");
        assert_eq!(selection.runtime.manifest.identity.engine_id, "fake_b");
        assert_eq!(schema.engine_id, "fake_b");
        assert_eq!(schema.definitions[0].id.as_str(), "fake_b.marker");
    }

    #[test]
    fn source_update_states_follow_git_ancestry_only() {
        let candidate = RuntimeId::new("ninfer-candidate").expect("runtime ID");
        assert_eq!(
            source_history_update_state(
                &RuntimeUpdatePreference::Latest,
                &candidate,
                "git-new",
                &GitHubCompare {
                    status: GitHubComparisonStatus::Identical,
                    ahead_by: 0,
                    behind_by: 0,
                },
                std::cmp::Ordering::Equal,
            ),
            RuntimeUpdateState::Current
        );
        assert!(matches!(
            source_history_update_state(
                &RuntimeUpdatePreference::Latest,
                &candidate,
                "git-new",
                &GitHubCompare {
                    status: GitHubComparisonStatus::Ahead,
                    ahead_by: 3,
                    behind_by: 0,
                },
                std::cmp::Ordering::Equal,
            ),
            RuntimeUpdateState::NewerCompatibleVersion { runtime_id, .. }
                if runtime_id == candidate
        ));
        for status in [
            GitHubComparisonStatus::Behind,
            GitHubComparisonStatus::Diverged,
        ] {
            assert!(matches!(
                source_history_update_state(
                    &RuntimeUpdatePreference::Latest,
                    &candidate,
                    "git-new",
                    &GitHubCompare {
                        status,
                        ahead_by: 0,
                        behind_by: 1,
                    },
                    std::cmp::Ordering::Equal,
                ),
                RuntimeUpdateState::ProviderError(_)
            ));
        }
    }

    #[test]
    fn pinned_source_selection_reports_but_does_not_apply_descendant() {
        let candidate = RuntimeId::new("ninfer-candidate").expect("runtime ID");
        assert!(matches!(
            source_history_update_state(
                &RuntimeUpdatePreference::Pinned,
                &candidate,
                "git-new",
                &GitHubCompare {
                    status: GitHubComparisonStatus::Ahead,
                    ahead_by: 1,
                    behind_by: 0,
                },
                std::cmp::Ordering::Equal,
            ),
            RuntimeUpdateState::Pinned {
                newer_runtime_id: Some(runtime_id),
                ..
            } if runtime_id == candidate
        ));
    }

    #[test]
    fn newer_recipe_generation_updates_an_identical_source_revision() {
        let candidate = RuntimeId::new("llama-candidate").expect("runtime ID");
        assert!(matches!(
            source_history_update_state(
                &RuntimeUpdatePreference::Latest,
                &candidate,
                "b12345",
                &GitHubCompare {
                    status: GitHubComparisonStatus::Identical,
                    ahead_by: 0,
                    behind_by: 0,
                },
                std::cmp::Ordering::Greater,
            ),
            RuntimeUpdateState::NewerCompatibleVersion { runtime_id, .. }
                if runtime_id == candidate
        ));
        assert!(matches!(
            source_history_update_state(
                &RuntimeUpdatePreference::Pinned,
                &candidate,
                "b12345",
                &GitHubCompare {
                    status: GitHubComparisonStatus::Identical,
                    ahead_by: 0,
                    behind_by: 0,
                },
                std::cmp::Ordering::Greater,
            ),
            RuntimeUpdateState::Pinned {
                newer_runtime_id: Some(runtime_id),
                ..
            } if runtime_id == candidate
        ));
    }

    #[test]
    fn recipe_update_lines_are_explicit_and_never_offer_a_downgrade() {
        let adapter = BoundSchemaAdapter { id: "fixture" };
        let installed_v1 = q27_identity("1.0.0", "recipe-v1", "fixture-family");
        let installed_v3 = q27_identity("1.0.0", "recipe-v3", "fixture-family");
        let candidate_v2 = available_fixture(q27_identity("1.0.0", "recipe-v2", "fixture-family"));
        let candidate_v3 = available_fixture(installed_v3.clone());
        let unknown_v4 = available_fixture(q27_identity("1.0.0", "recipe-v4", "fixture-family"));

        assert!(same_update_line(
            &installed_v1,
            &candidate_v3,
            Some(&adapter)
        ));
        assert_eq!(
            source_recipe_ordering(&installed_v1, &candidate_v3.identity, Some(&adapter)),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            source_recipe_ordering(&installed_v3, &candidate_v2.identity, Some(&adapter)),
            std::cmp::Ordering::Less
        );
        assert!(!same_update_line(
            &installed_v3,
            &unknown_v4,
            Some(&adapter)
        ));
    }

    #[test]
    fn q27_binary_and_source_releases_share_only_their_exact_logical_update_line() {
        let installed_binary = q27_identity("0.6.2", "w12", "q27-official-release");
        let source = available_fixture(q27_identity(
            "0.10.0",
            "w12",
            "q27-official-source-q27-upstream-make-v2-4770e05",
        ));
        assert!(same_update_line(&installed_binary, &source, None));
        assert!(matches!(
            semantic_release_update_state(
                &RuntimeUpdatePreference::Latest,
                &installed_binary,
                &source,
            ),
            RuntimeUpdateState::NewerCompatibleVersion { ref runtime_id, ref version }
                if runtime_id == &source.runtime_id && version == "0.10.0"
        ));
        assert!(matches!(
            semantic_release_update_state(
                &RuntimeUpdatePreference::Pinned,
                &installed_binary,
                &source,
            ),
            RuntimeUpdateState::Pinned { newer_runtime_id: Some(ref runtime_id), .. }
                if runtime_id == &source.runtime_id
        ));

        let installed_source = q27_identity(
            "0.10.0",
            "w12",
            "q27-official-source-q27-upstream-make-v2-4770e05",
        );
        let newer_binary = available_fixture(q27_identity("0.11.0", "w12", "q27-official-release"));
        assert!(same_update_line(&installed_source, &newer_binary, None));
        assert!(matches!(
            semantic_release_update_state(
                &RuntimeUpdatePreference::Stable,
                &installed_source,
                &newer_binary,
            ),
            RuntimeUpdateState::NewerCompatibleVersion { ref runtime_id, .. }
                if runtime_id == &newer_binary.runtime_id
        ));

        let w8 = available_fixture(q27_identity(
            "0.10.0",
            "w8",
            "q27-official-source-q27-upstream-make-v2-4770e05",
        ));
        assert!(!same_update_line(&installed_binary, &w8, None));

        let mut other_provider = source.clone();
        other_provider.identity.package.provider_id = "another-provider".to_owned();
        other_provider.runtime_id = RuntimeId::from_identity(&other_provider.identity);
        assert!(!same_update_line(&installed_binary, &other_provider, None));
    }

    #[test]
    fn same_day_source_fallback_uses_commit_recency_not_sha_digits() {
        let older = installed_fixture(
            "git-20260828-99999999",
            Some((1_787_961_600, "9999999999999999999999999999999999999999")),
        );
        let newer = installed_fixture(
            "git-20260828-aaaaaaaa",
            Some((1_787_965_200, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")),
        );
        assert_eq!(
            super::compare_versions(
                &older.manifest.identity.version,
                &newer.manifest.identity.version
            ),
            std::cmp::Ordering::Greater,
            "the legacy numeric comparator demonstrates the SHA-fragment bug"
        );

        let mut fallback_candidates = [older, newer.clone()];
        fallback_candidates.sort_by(|left, right| compare_installed_recency(right, left, None));
        assert_eq!(
            fallback_candidates[0].manifest.runtime_id,
            newer.manifest.runtime_id
        );

        let release_v1 = installed_fixture("v1.9.0", None);
        let release_v2 = installed_fixture("v2.0.0", None);
        assert_eq!(
            compare_installed_recency(&release_v2, &release_v1, None),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            super::compare_installed_recency_inner(&release_v2, &release_v1, None, true),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn installed_recipe_generations_rank_with_engine_owned_update_identity() {
        let adapter = BoundSchemaAdapter {
            id: "fixture-engine",
        };
        let mut v2 = installed_fixture(
            "git-20260831-aaaaaaaa",
            Some((1_788_134_400, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")),
        );
        v2.manifest.identity.variant = "recipe-v2".to_owned();
        v2.manifest.runtime_id = RuntimeId::from_identity(&v2.manifest.identity);
        v2.manifest
            .source_build
            .as_mut()
            .expect("source fixture")
            .recipe_version = "recipe-v2".to_owned();

        let mut v3 = v2.clone();
        v3.manifest.identity.variant = "recipe-v3".to_owned();
        v3.manifest.runtime_id = RuntimeId::from_identity(&v3.manifest.identity);
        v3.manifest
            .source_build
            .as_mut()
            .expect("source fixture")
            .recipe_version = "recipe-v3".to_owned();

        assert_eq!(
            compare_installed_recency(&v3, &v2, Some(&adapter)),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            super::compare_installed_recency_inner(&v3, &v2, Some(&adapter), true),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_installed_recency(&v2, &v3, Some(&adapter)),
            std::cmp::Ordering::Less,
            "a lower recipe generation cannot win through RuntimeId ordering"
        );

        let mut newer_source_v2 = v2.clone();
        newer_source_v2.manifest.identity.version = "git-20260901-bbbbbbbb".to_owned();
        newer_source_v2.manifest.identity.upstream_revision = Some("b".repeat(40));
        newer_source_v2.manifest.runtime_id =
            RuntimeId::from_identity(&newer_source_v2.manifest.identity);
        let source = newer_source_v2
            .manifest
            .source_build
            .as_mut()
            .expect("source fixture");
        source.source.commit_timestamp_unix += 86_400;
        source.source.commit_sha = "b".repeat(40);
        assert_eq!(
            compare_installed_recency(&newer_source_v2, &v3, Some(&adapter)),
            std::cmp::Ordering::Greater,
            "a genuinely newer upstream source still outranks an older source recipe generation"
        );

        let mut intentional_variant = v3.clone();
        intentional_variant.manifest.identity.variant = "specialist".to_owned();
        intentional_variant.manifest.runtime_id =
            RuntimeId::from_identity(&intentional_variant.manifest.identity);
        let source = intentional_variant
            .manifest
            .source_build
            .as_mut()
            .expect("source fixture");
        source.source.commit_timestamp_unix += 86_400;
        source.source.commit_sha = "c".repeat(40);
        assert_eq!(
            compare_installed_recency(&v3, &intentional_variant, Some(&adapter)),
            std::cmp::Ordering::Equal,
            "recipe generations must not cross-rank distinct functional variants"
        );
    }

    #[test]
    fn default_variant_recency_preserves_q27_and_ninfer_style_behavior() {
        let older = installed_fixture("v1.9.0", None);
        let newer = installed_fixture("v2.0.0", None);
        assert_eq!(
            compare_installed_recency(&newer, &older, None),
            std::cmp::Ordering::Greater
        );

        let older_source = installed_fixture(
            "git-20260830-ffffffff",
            Some((1_788_048_000, "ffffffffffffffffffffffffffffffffffffffff")),
        );
        let newer_source = installed_fixture(
            "git-20260831-00000000",
            Some((1_788_134_400, "0000000000000000000000000000000000000000")),
        );
        assert_eq!(
            compare_installed_recency(&newer_source, &older_source, None),
            std::cmp::Ordering::Greater
        );
        // The badge comparison spans functional variants for every engine,
        // without using SHA order or installation time as recency evidence.
        for engine_id in ["llama.cpp", "q27", "ninfer", "future-engine"] {
            let adapter = BoundSchemaAdapter { id: engine_id };
            let mut older = older_source.clone();
            let mut newer = newer_source.clone();
            older.manifest.identity.engine_id = engine_id.to_owned();
            newer.manifest.identity.engine_id = engine_id.to_owned();
            newer.manifest.identity.variant = "specialist".to_owned();
            assert_eq!(
                super::compare_installed_recency_inner(&newer, &older, Some(&adapter), true),
                std::cmp::Ordering::Greater,
            );
            older
                .manifest
                .source_build
                .as_mut()
                .expect("source fixture")
                .source
                .commit_timestamp_unix = newer
                .manifest
                .source_build
                .as_ref()
                .expect("source fixture")
                .source
                .commit_timestamp_unix;
            assert_eq!(
                super::compare_installed_recency_inner(&newer, &older, Some(&adapter), true),
                std::cmp::Ordering::Equal,
                "equal commit times must not be ranked by SHA",
            );
            older.manifest.source_build = newer.manifest.source_build.clone();
            assert_eq!(
                super::compare_installed_recency_inner(&newer, &older, Some(&adapter), true),
                std::cmp::Ordering::Equal,
                "variants of the same newest lineage share the badge",
            );
        }
    }

    #[test]
    fn source_provenance_matches_effective_cmake_configuration_and_legacy_manifests() {
        let installed = installed_fixture(
            "git-20260831-aaaaaaaa",
            Some((1_788_134_400, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")),
        );
        let legacy = installed
            .manifest
            .source_build
            .expect("source-build provenance");
        let plan = RuntimeSourceBuildPlan {
            source: legacy.source.clone(),
            recipe: RuntimeSourceBuildRecipe {
                recipe_version: legacy.recipe_version.clone(),
                build_system: RuntimeSourceBuildSystem::Cmake,
                build_definition_sha256: None,
                cmake_configuration_arguments: Vec::new(),
                build_target: legacy.build_target.clone(),
                entrypoint: PathBuf::from("build/fixture"),
                accelerator_target: legacy.accelerator_target.clone(),
                rejected_build_environment: Vec::new(),
            },
            prerequisites: RuntimeSourceBuildPrerequisites {
                minimum_cmake_version: "3.18".to_owned(),
                minimum_cuda_version: Some("13.0".to_owned()),
                maximum_cuda_version_exclusive: Some("14.0".to_owned()),
                requires_ninja: true,
                requires_cpp20_compiler: false,
                requires_make: false,
                minimum_cpp_standard: Some(17),
                cpp_compiler: None,
                cuda_compiler: Some(PathBuf::from("/usr/local/cuda/bin/nvcc")),
                requires_pkg_config: false,
                pkg_config_modules: BTreeMap::new(),
            },
        };

        assert!(
            source_build_provenance_matches_plan(&legacy, &plan),
            "legacy manifests without an effective list remain readable and match their recipe"
        );

        let mut current = legacy.clone();
        current.effective_cmake_configuration_arguments = Some(vec![
            "-DCMAKE_CUDA_COMPILER=/usr/local/cuda/bin/nvcc".to_owned(),
        ]);
        assert!(source_build_provenance_matches_plan(&current, &plan));

        current.effective_cmake_configuration_arguments =
            Some(vec!["-DCMAKE_CUDA_COMPILER=/other/nvcc".to_owned()]);
        assert!(!source_build_provenance_matches_plan(&current, &plan));
    }

    fn installed_fixture(version: &str, source: Option<(i64, &str)>) -> InstalledRuntime {
        let identity = RuntimeIdentity {
            engine_id: "fixture-engine".to_owned(),
            package_family: "fixture-family".to_owned(),
            version: version.to_owned(),
            upstream_revision: source.map(|(_, commit)| commit.to_owned()),
            platform: "linux".to_owned(),
            architecture: "x86_64".to_owned(),
            accelerator: "cuda-sm120".to_owned(),
            variant: "default".to_owned(),
            package: RuntimePackageIdentity {
                provider_id: "fixture-provider".to_owned(),
                repository: Some("owner/repository".to_owned()),
                release_tag: source.is_none().then(|| version.to_owned()),
                asset_id: source.is_none().then(|| "1".to_owned()),
                asset_name: source.is_none().then(|| "runtime.tar.gz".to_owned()),
                additional_assets: Vec::new(),
            },
        };
        let runtime_id = RuntimeId::from_identity(&identity);
        let source_build =
            source.map(
                |(commit_timestamp_unix, commit_sha)| RuntimeSourceBuildProvenance {
                    source: RuntimeSourceSnapshot {
                        repository: "owner/repository".to_owned(),
                        repository_url: "https://github.com/owner/repository.git".to_owned(),
                        source_branch: "main".to_owned(),
                        commit_sha: commit_sha.to_owned(),
                        tree_sha: "b".repeat(40),
                        commit_timestamp_unix,
                        source_provider: "github".to_owned(),
                    },
                    recipe_version: "fixture-v1".to_owned(),
                    build_system: RuntimeSourceBuildSystem::Cmake,
                    build_definition_sha256: None,
                    cmake_configuration_arguments: Vec::new(),
                    effective_cmake_configuration_arguments: None,
                    build_target: "fixture".to_owned(),
                    toolchain: RuntimeSourceBuildToolchain {
                        cmake_version: "4.0".to_owned(),
                        ninja_version: "1.12".to_owned(),
                        make_version: "not required".to_owned(),
                        cpp_compiler: "fixture-c++".to_owned(),
                        nvcc_version: "13.0".to_owned(),
                        pkg_config_version: "2.0".to_owned(),
                        system_dependencies: BTreeMap::new(),
                    },
                    build_platform: "linux".to_owned(),
                    build_architecture: "x86_64".to_owned(),
                    accelerator_target: "sm_120".to_owned(),
                    built_at_unix: commit_timestamp_unix,
                    entrypoint: PathBuf::from("server"),
                    entrypoint_sha256: "c".repeat(64),
                },
            );
        InstalledRuntime {
            manifest: RuntimeManifest {
                schema_version: RUNTIME_MANIFEST_SCHEMA_VERSION,
                runtime_id,
                identity,
                supported_formats: Vec::new(),
                supported_native_identities: Vec::new(),
                requirements: RuntimeRequirements::default(),
                acquisition_method: if source_build.is_some() {
                    RuntimeAcquisitionMethod::SourceBuild
                } else {
                    RuntimeAcquisitionMethod::OfficialReleaseAsset
                },
                source_url: None,
                downloaded_archive_sha256: source_build.is_none().then(|| "d".repeat(64)),
                additional_downloaded_archive_sha256: Vec::new(),
                source_build,
                entrypoint: PathBuf::from("server"),
                entrypoint_sha256: "c".repeat(64),
                installed_at_unix: Some(1),
                probe: RuntimeProbeObservation {
                    compatible: true,
                    observed_engine_id: "fixture-engine".to_owned(),
                    observed_version: Some(version.to_owned()),
                    observed_revision: None,
                    detail: "fixture".to_owned(),
                    observed_at_unix: 1,
                },
            },
            installation_root: PathBuf::new(),
        }
    }

    fn q27_identity(version: &str, variant: &str, package_family: &str) -> RuntimeIdentity {
        RuntimeIdentity {
            engine_id: "q27".to_owned(),
            package_family: package_family.to_owned(),
            version: version.to_owned(),
            upstream_revision: package_family.contains("source").then(|| "a".repeat(40)),
            platform: "linux".to_owned(),
            architecture: "x86_64".to_owned(),
            accelerator: "cuda".to_owned(),
            variant: variant.to_owned(),
            package: RuntimePackageIdentity {
                provider_id: "q27-official-github".to_owned(),
                repository: Some("signalnine/q27".to_owned()),
                release_tag: Some(format!("v{version}")),
                asset_id: (!package_family.contains("source")).then(|| "1".to_owned()),
                asset_name: (!package_family.contains("source"))
                    .then(|| format!("q27-v{version}-linux-x86_64.tar.gz")),
                additional_assets: Vec::new(),
            },
        }
    }

    fn available_fixture(identity: RuntimeIdentity) -> AvailableRuntime {
        AvailableRuntime {
            runtime_id: RuntimeId::from_identity(&identity),
            identity,
            display_name: "fixture".to_owned(),
            supported_formats: vec![ArtifactFormat::Q27],
            source_url: "https://github.com/signalnine/q27".to_owned(),
            published_at_unix: Some(1),
            channels: vec![RuntimeReleaseChannel::Stable, RuntimeReleaseChannel::Latest],
            prerelease: false,
            acquisition: RuntimeAcquisitionPlan::ReleaseAsset {
                download: RuntimeDownload {
                    url:
                        "https://github.com/signalnine/q27/releases/download/fixture/runtime.tar.gz"
                            .to_owned(),
                    size_bytes: 1,
                    digest: None,
                    archive_format: RuntimeArchiveFormat::TarGz,
                    entrypoint_names: vec!["q27-server".to_owned()],
                },
                additional_downloads: Vec::new(),
            },
            supported_native_identities: Vec::new(),
            requirements: RuntimeRequirements::default(),
        }
    }
}
