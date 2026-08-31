use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use norted_core::{
    AppPaths, ArtifactFormat, AvailableRuntime, EngineInstallation, HostCapabilities,
    InstalledRuntime, ModelArtifact, ModelId, RUNTIME_MANIFEST_SCHEMA_VERSION,
    RuntimeAcquisitionMethod, RuntimeCompatibility, RuntimeId, RuntimeIdentity, RuntimeManifest,
    RuntimeOperationProgress, RuntimePackageIdentity, RuntimeProbeObservation,
    RuntimeReleaseChannel, RuntimeSelection, RuntimeSelectionSource, RuntimeSelections,
    RuntimeUpdatePreference, RuntimeUpdateState,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock, broadcast};

use crate::catalog::{
    CatalogError, GitHubComparisonStatus, RuntimeCatalog, RuntimeCatalogEntry,
    RuntimeCatalogProvider, RuntimeCatalogSnapshot, compatibility_for, detect_host_capabilities,
};
use crate::installer::{RuntimeInstallError, RuntimeInstaller};
use crate::store::{RuntimeLease, RuntimeStore, RuntimeStoreError, RuntimeStoreSnapshot};
use crate::{
    CompatibilityDecision, EngineError, EngineRegistry, InstallationState, ModelServingCapabilities,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledRuntimeStatus {
    pub runtime: InstalledRuntime,
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
    operation: Arc<Mutex<()>>,
}

impl RuntimePackManager {
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
            host: Arc::new(RwLock::new(
                HostCapabilities::current_without_accelerator_probe(),
            )),
            operation: Arc::new(Mutex::new(())),
        }))
    }

    pub fn progress(&self) -> broadcast::Receiver<RuntimeOperationProgress> {
        self.installer.subscribe()
    }

    pub async fn refresh_host_capabilities(&self) -> HostCapabilities {
        let host = detect_host_capabilities().await;
        *self.host.write().await = host.clone();
        host
    }

    pub async fn host_capabilities(&self) -> HostCapabilities {
        self.host.read().await.clone()
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
            .settings_schema(&selection.runtime, model, &host)
            .await
            .map_err(RuntimePackError::Adapter)?;
        Ok((selection, schema))
    }

    pub async fn list(&self) -> Result<RuntimeListSnapshot, RuntimePackError> {
        let RuntimeStoreSnapshot {
            mut runtimes,
            mut warnings,
        } = self.store.scan().await?;
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
                .then_with(|| compare_installed_recency(right, left))
        });
        let selections = self.store.selections().await?;
        let host = self.host.read().await.clone();
        let installed = runtimes
            .into_iter()
            .map(|runtime| {
                let compatibility = match self.registry.get(&runtime.manifest.identity.engine_id) {
                    Some(adapter) => match adapter.runtime_management_compatibility() {
                        CompatibilityDecision::Supported => {
                            match adapter.runtime_compatibility(&runtime) {
                                CompatibilityDecision::Supported => {
                                    compatibility_for_installed(&runtime, &host)
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
                    compatibility,
                    selected_for: selected_for(&selections, &runtime.manifest.runtime_id),
                    runtime,
                }
            })
            .collect();
        Ok(RuntimeListSnapshot {
            host,
            selections,
            installed,
            warnings,
        })
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
        let mut source_prerequisite_results = Vec::<(
            norted_core::RuntimeSourceBuildSystem,
            norted_core::RuntimeSourceBuildPrerequisites,
            RuntimeCompatibility,
        )>::new();
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
                let compatibility = if let Some((_, _, compatibility)) = source_prerequisite_results
                    .iter()
                    .find(|(system, prerequisites, _)| {
                        *system == plan.recipe.build_system && prerequisites == &plan.prerequisites
                    }) {
                    compatibility.clone()
                } else {
                    let compatibility = self.installer.source_build_compatibility(plan).await;
                    source_prerequisite_results.push((
                        plan.recipe.build_system,
                        plan.prerequisites.clone(),
                        compatibility.clone(),
                    ));
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
        let _operation = self.operation.lock().await;
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
        let _operation = self.operation.lock().await;
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
        let _operation = self.operation.lock().await;
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
        let _operation = self.operation.lock().await;
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
        let _operation = self.operation.lock().await;
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
        let _operation = self.operation.lock().await;
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
                accelerator: self.model_candidate_accelerator(&status.runtime, model, &host),
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
                        accelerator: self.model_candidate_accelerator(&runtime, model, &list.host),
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
                Err(error) => notices.push(format!(
                    "model runtime selection `{runtime_id}` is unavailable ({error}); using a reported fallback"
                )),
            }
        }
        if let Some(runtime_id) = list.selections.format_defaults.get(&model.format) {
            match self.selected_candidate(list, runtime_id, model, settings) {
                Ok(runtime)
                    if required_engine_id.is_none_or(|required| {
                        runtime.manifest.identity.engine_id == required
                    }) => {
                    return Ok(RuntimeSelection {
                        accelerator: self.model_candidate_accelerator(&runtime, model, &list.host),
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
                Err(error) => notices.push(format!(
                    "{} default runtime `{runtime_id}` is unavailable ({error}); using a reported fallback",
                    model.format.as_str().to_ascii_uppercase()
                )),
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
                .then_with(|| compare_installed_recency(&right.0.runtime, &left.0.runtime))
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
            accelerator: self.model_candidate_accelerator(&runtime, model, &host),
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

        let selected_runtime_id = self
            .resolve_from_snapshot(model, None, settings, None, &list)
            .ok()
            .map(|selection| selection.runtime.manifest.runtime_id);

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
            text_output: true,
            responses: true,
            chat_completions: true,
            streaming: true,
            tools: false,
            vision: false,
            structured_output: false,
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
                    .filter(|result| same_update_line(identity, &result.entry.available))
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
                .filter(|result| same_update_line(identity, &result.entry.available))
                .collect::<Vec<_>>();
            let channel_available = update_line.iter().any(|result| {
                update_channel_matches(&preference, published.as_ref(), &result.entry.available)
            });
            let newer = catalog
                .results
                .iter()
                .filter(|result| result.entry.compatibility.is_usable())
                .filter(|result| same_update_line(identity, &result.entry.available))
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
                .then_with(|| compare_installed_recency(&right.0.runtime, &left.0.runtime))
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
        settings: Option<&norted_core::ResolvedSettings>,
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
            adapter.runtime_model_compatibility(runtime, model, host, settings),
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

    fn model_candidate_accelerator(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        host: &HostCapabilities,
    ) -> Option<norted_core::AcceleratorDevice> {
        self.registry
            .get(&runtime.manifest.identity.engine_id)
            .and_then(|adapter| adapter.runtime_model_accelerator(runtime, model, host))
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
                    provenance.source == plan.source
                        && provenance.recipe_version == plan.recipe.recipe_version
                        && provenance.build_system == plan.recipe.build_system
                        && provenance.cmake_configuration_arguments
                            == plan.recipe.cmake_configuration_arguments
                        && provenance.build_target == plan.recipe.build_target
                        && provenance.accelerator_target == plan.recipe.accelerator_target
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

fn same_update_line(identity: &RuntimeIdentity, candidate: &AvailableRuntime) -> bool {
    let candidate = &candidate.identity;
    candidate.engine_id == identity.engine_id
        && (candidate.package_family == identity.package_family
            || q27_logical_update_family(identity, candidate))
        && candidate.platform == identity.platform
        && candidate.architecture == identity.architecture
        && candidate.accelerator == identity.accelerator
        && candidate.variant == identity.variant
        && candidate.package.provider_id == identity.package.provider_id
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
) -> Ordering {
    if q27_semantic_release_identity(installed)
        && q27_semantic_release_identity(&left.identity)
        && q27_semantic_release_identity(&right.identity)
    {
        return compare_versions(&left.identity.version, &right.identity.version)
            .then_with(|| left.published_at_unix.cmp(&right.published_at_unix));
    }
    left.published_at_unix.cmp(&right.published_at_unix)
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
) -> RuntimeUpdateState {
    match comparison.status {
        GitHubComparisonStatus::Identical
            if comparison.ahead_by == 0 && comparison.behind_by == 0 =>
        {
            if matches!(preference, RuntimeUpdatePreference::Pinned) {
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

fn compare_installed_recency(left: &InstalledRuntime, right: &InstalledRuntime) -> Ordering {
    let left_manifest = &left.manifest;
    let right_manifest = &right.manifest;
    let same_update_line = left_manifest.identity.engine_id == right_manifest.identity.engine_id
        && (left_manifest.identity.package_family == right_manifest.identity.package_family
            || q27_logical_update_family(&left_manifest.identity, &right_manifest.identity))
        && left_manifest.identity.platform == right_manifest.identity.platform
        && left_manifest.identity.architecture == right_manifest.identity.architecture
        && left_manifest.identity.accelerator == right_manifest.identity.accelerator
        && left_manifest.identity.variant == right_manifest.identity.variant
        && left_manifest.identity.package.provider_id
            == right_manifest.identity.package.provider_id;
    if same_update_line
        && let (Some(left_source), Some(right_source)) = (
            left_manifest.source_build.as_ref(),
            right_manifest.source_build.as_ref(),
        )
    {
        // Commit time provides deterministic offline recency for installed
        // snapshots only. It is not ancestry evidence; update availability is
        // decided separately by source_history_update_state using Git history.
        return left_source
            .source
            .commit_timestamp_unix
            .cmp(&right_source.source.commit_timestamp_unix)
            .then_with(|| {
                left_source
                    .source
                    .commit_sha
                    .cmp(&right_source.source.commit_sha)
            })
            .then_with(|| left_manifest.runtime_id.cmp(&right_manifest.runtime_id));
    }
    compare_versions(
        &left_manifest.identity.version,
        &right_manifest.identity.version,
    )
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

    use norted_core::{
        ArtifactFormat, AvailableRuntime, InstalledRuntime, RUNTIME_MANIFEST_SCHEMA_VERSION,
        RuntimeAcquisitionMethod, RuntimeAcquisitionPlan, RuntimeArchiveFormat, RuntimeDownload,
        RuntimeId, RuntimeIdentity, RuntimeManifest, RuntimePackageIdentity,
        RuntimeProbeObservation, RuntimeReleaseChannel, RuntimeRequirements,
        RuntimeSourceBuildProvenance, RuntimeSourceBuildSystem, RuntimeSourceBuildToolchain,
        RuntimeSourceSnapshot, RuntimeUpdatePreference, RuntimeUpdateState,
    };

    use super::{
        compare_installed_recency, same_update_line, semantic_release_update_state,
        source_history_update_state,
    };
    use crate::{GitHubCompare, GitHubComparisonStatus};

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
            ),
            RuntimeUpdateState::Pinned {
                newer_runtime_id: Some(runtime_id),
                ..
            } if runtime_id == candidate
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
        assert!(same_update_line(&installed_binary, &source));
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
        assert!(same_update_line(&installed_source, &newer_binary));
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
        assert!(!same_update_line(&installed_binary, &w8));

        let mut other_provider = source.clone();
        other_provider.identity.package.provider_id = "another-provider".to_owned();
        other_provider.runtime_id = RuntimeId::from_identity(&other_provider.identity);
        assert!(!same_update_line(&installed_binary, &other_provider));
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
        fallback_candidates.sort_by(|left, right| compare_installed_recency(right, left));
        assert_eq!(
            fallback_candidates[0].manifest.runtime_id,
            newer.manifest.runtime_id
        );

        let release_v1 = installed_fixture("v1.9.0", None);
        let release_v2 = installed_fixture("v2.0.0", None);
        assert_eq!(
            compare_installed_recency(&release_v2, &release_v1),
            std::cmp::Ordering::Greater
        );
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
