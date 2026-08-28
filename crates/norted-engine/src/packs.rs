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
    CatalogError, RuntimeCatalog, RuntimeCatalogEntry, RuntimeCatalogProvider,
    RuntimeCatalogSnapshot, compatibility_for, detect_host_capabilities,
};
use crate::installer::{RuntimeInstallError, RuntimeInstaller};
use crate::store::{RuntimeLease, RuntimeStore, RuntimeStoreError, RuntimeStoreSnapshot};
use crate::{CompatibilityDecision, EngineError, EngineRegistry, InstallationState};

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
                .then_with(|| {
                    compare_versions(
                        &right.manifest.identity.version,
                        &left.manifest.identity.version,
                    )
                })
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
        let host = self.host.read().await.clone();
        let RuntimeCatalogSnapshot {
            mut entries,
            provider_errors,
            fetched_at_unix,
        } = self.catalog.search(query, &host, force_refresh).await;
        for entry in &mut entries {
            entry.compatibility = match self.registry.get(&entry.available.identity.engine_id) {
                Some(adapter) => match adapter.runtime_management_compatibility() {
                    CompatibilityDecision::Supported => {
                        match adapter.available_runtime_compatibility(&entry.available) {
                            CompatibilityDecision::Supported => entry.compatibility.clone(),
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
        self.validate_model_candidate(&runtime, model, &host)?;
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
        let host = self.refresh_host_capabilities().await;
        let list = self.list().await?;
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
            self.validate_model_candidate(&status.runtime, model, &host)?;
            return Ok(RuntimeSelection {
                runtime: status.runtime.clone(),
                source: RuntimeSelectionSource::Invocation,
                notices: Vec::new(),
            });
        }
        let mut notices = Vec::new();
        if let Some(runtime_id) = list.selections.model_overrides.get(&model.id) {
            match self.selected_candidate(&list, runtime_id, model) {
                Ok(runtime) => {
                    return Ok(RuntimeSelection {
                        runtime,
                        source: RuntimeSelectionSource::ModelOverride,
                        notices,
                    });
                }
                Err(error) => notices.push(format!(
                    "model runtime selection `{runtime_id}` is unavailable ({error}); using a reported fallback"
                )),
            }
        }
        if let Some(runtime_id) = list.selections.format_defaults.get(&model.format) {
            match self.selected_candidate(&list, runtime_id, model) {
                Ok(runtime) => {
                    return Ok(RuntimeSelection {
                        runtime,
                        source: RuntimeSelectionSource::FormatDefault,
                        notices,
                    });
                }
                Err(error) => notices.push(format!(
                    "{} default runtime `{runtime_id}` is unavailable ({error}); using a reported fallback",
                    model.format.as_str().to_ascii_uppercase()
                )),
            }
        }
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
            if let RuntimeCompatibility::Incompatible(reason) = &status.compatibility {
                rejected.push(format!("{}: {reason}", status.runtime.manifest.runtime_id));
                continue;
            }
            match self.model_candidate_compatibility(&status.runtime, model, &host) {
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
                    compare_versions(
                        &right.0.runtime.manifest.identity.version,
                        &left.0.runtime.manifest.identity.version,
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
            runtime,
            source: RuntimeSelectionSource::Fallback,
            notices,
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
            match self.model_candidate_compatibility(&status.runtime, model, &host) {
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
                    compare_versions(
                        &right.0.runtime.manifest.identity.version,
                        &left.0.runtime.manifest.identity.version,
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
        self.validate_model_candidate(&status.runtime, model, &list.host)?;
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
    ) -> Result<(), RuntimePackError> {
        let compatibility = self.model_candidate_compatibility(runtime, model, host)?;
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
            adapter.runtime_model_compatibility(runtime, model, host),
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
            requirements: Default::default(),
            acquisition_method: RuntimeAcquisitionMethod::ExternalBinary,
            source_url: None,
            downloaded_archive_sha256: None,
            additional_downloaded_archive_sha256: Vec::new(),
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
    let primary_digest = available
        .download
        .digest
        .as_ref()
        .map(|digest| &digest.value);
    let additional_digests = available
        .additional_downloads
        .iter()
        .filter_map(|download| download.digest.as_ref().map(|digest| digest.value.clone()))
        .collect::<Vec<_>>();
    let all_additional_digests_present =
        additional_digests.len() == available.additional_downloads.len();
    let matches = manifest.runtime_id == available.runtime_id
        && manifest.identity == available.identity
        && manifest.supported_formats == available.supported_formats
        && manifest.requirements == available.requirements
        && manifest.source_url.as_deref() == Some(available.source_url.as_str())
        && manifest.downloaded_archive_sha256.as_ref() == primary_digest
        && all_additional_digests_present
        && manifest.additional_downloaded_archive_sha256 == additional_digests;
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
        | RuntimeAcquisitionMethod::PreseededOfficialPack => 0,
        RuntimeAcquisitionMethod::ExternalBinary => 1,
    }
}

fn same_update_line(identity: &RuntimeIdentity, candidate: &AvailableRuntime) -> bool {
    let candidate = &candidate.identity;
    candidate.engine_id == identity.engine_id
        && candidate.package_family == identity.package_family
        && candidate.platform == identity.platform
        && candidate.architecture == identity.architecture
        && candidate.accelerator == identity.accelerator
        && candidate.variant == identity.variant
        && candidate.package.provider_id == identity.package.provider_id
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
