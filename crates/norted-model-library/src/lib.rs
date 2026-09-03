//! Managed model acquisition without model transformation or runtime ownership.

use std::collections::HashSet;
use std::fs::OpenOptions;
use std::ops::Deref;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use fs2::FileExt;
use futures_util::StreamExt;
use norted_core::{
    AppPaths, ArtifactFormat, ModelArtifact, ModelArtifactProvenance, ModelId, ModelLibraryReceipt,
    ModelLibraryReceiptMember, ModelRegistry, NortedPackageAcquisitionPlan,
    NortedPackageAcquisitionRole, inspect_gguf_metadata, inspect_ninfer_container,
    model_library_receipt_path, norted_package_manifest_name, plan_norted_package_acquisition,
    q27_tokenizer_candidate, recover_norted_package_primary_paths, select_q27_tokenizer_filename,
    validate_q27_tokenizer_header,
};
use reqwest::{Client, StatusCode, Url, header};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::broadcast;

mod download_manager;

use download_manager::DownloadManager;
pub use download_manager::{
    DEFAULT_MAX_PARALLEL_DOWNLOADS, DownloadAdmission, MAX_PARALLEL_DOWNLOADS_SETTING_ID,
    ModelDownloadJob, ModelDownloadJobId,
};

const RECEIPT_SCHEMA_VERSION: u32 = 2;
const MAX_PACKAGE_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ModelLibraryError {
    #[error("model catalog request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("model library I/O failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid model reference: {0}")]
    InvalidReference(String),
    #[error("model catalog entry is invalid: {0}")]
    InvalidCatalog(String),
    #[error("model artifact is invalid: {0}")]
    InvalidArtifact(String),
    #[error("model artifact already exists in the managed library: {0}")]
    AlreadyInstalled(PathBuf),
    #[error("model `{0}` is not a managed library artifact and cannot be removed")]
    NotManaged(ModelId),
    #[error("model `{0}` was not found")]
    NotFound(ModelId),
    #[error("download checksum mismatch for {filename}: expected {expected}, observed {observed}")]
    DigestMismatch {
        filename: String,
        expected: String,
        observed: String,
    },
}

type Result<T> = std::result::Result<T, ModelLibraryError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogFile {
    pub filename: String,
    pub format: ArtifactFormat,
    pub model_ref: String,
    pub size_bytes: Option<u64>,
    pub sha256: Option<String>,
    pub required_companions: Vec<String>,
    /// A same-format manifest exists in an ancestor directory. Valid package
    /// membership is established only when the exact reference is resolved.
    pub package_manifest: Option<String>,
    /// Remote entries are format candidates until local inspection succeeds.
    pub compatibility: CatalogCompatibility,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogCompatibility {
    Unverified,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogRepository {
    pub provider: String,
    pub repository: String,
    pub revision: String,
    pub artifacts: Vec<CatalogFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogSearch {
    pub query: String,
    pub repositories: Vec<CatalogRepository>,
}

#[derive(Debug, Clone)]
pub struct ResolvedFile {
    pub filename: String,
    pub url: Url,
    pub size_bytes: Option<u64>,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedArtifact {
    pub provider: String,
    pub repository: String,
    pub revision: String,
    pub primary: ResolvedFile,
    pub companions: Vec<ResolvedFile>,
    pub format: ArtifactFormat,
    pub package: Option<ResolvedPackage>,
}

#[derive(Debug, Clone)]
pub struct ResolvedPackage {
    pub manifest_filename: String,
    pub primary_filenames: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ModelRemovalPlan {
    acquisition_root: PathBuf,
    pub acquisition_id: String,
    pub affected_model_ids: Vec<ModelId>,
}

impl ModelRemovalPlan {
    pub fn acquisition_root(&self) -> &Path {
        &self.acquisition_root
    }
}

#[async_trait]
pub trait ModelCatalogProvider: Send + Sync {
    fn id(&self) -> &'static str;
    async fn search(
        &self,
        query: &str,
        format: Option<ArtifactFormat>,
    ) -> Result<Vec<CatalogRepository>>;
    async fn resolve(&self, model_ref: &str) -> Result<ResolvedArtifact>;
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelOperationPhase {
    Queued,
    Resolving,
    Downloading,
    Paused,
    Verifying,
    Validating,
    Installing,
    Installed,
    Failed,
    Cancelled,
}

impl ModelOperationPhase {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Installed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelOperationProgress {
    pub job_id: ModelDownloadJobId,
    pub model_ref: String,
    pub provider: Option<String>,
    pub repository: Option<String>,
    pub filename: String,
    pub phase: ModelOperationPhase,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ModelDownloadResult {
    pub artifact: ModelArtifact,
    pub already_installed: bool,
}

pub struct ModelLibrary {
    root: PathBuf,
    staging: PathBuf,
    download_cache: PathBuf,
    providers: Vec<Arc<dyn ModelCatalogProvider>>,
    progress: broadcast::Sender<ModelOperationProgress>,
    downloads: DownloadManager,
}

impl ModelLibrary {
    pub fn new(paths: &AppPaths) -> Self {
        Self::with_max_parallel_downloads(paths, DEFAULT_MAX_PARALLEL_DOWNLOADS)
    }

    pub fn with_max_parallel_downloads(paths: &AppPaths, maximum_parallel: usize) -> Self {
        let (progress, _) = broadcast::channel(256);
        Self {
            root: paths.data_dir.join("models"),
            staging: paths.data_dir.join("models").join(".norted-staging"),
            download_cache: paths.cache_dir.join("model-downloads"),
            providers: vec![Arc::new(HuggingFaceCatalogProvider::new())],
            progress,
            downloads: DownloadManager::new(maximum_parallel),
        }
    }

    pub fn managed_root(&self) -> &Path {
        &self.root
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ModelOperationProgress> {
        self.progress.subscribe()
    }

    pub fn download_jobs(&self) -> Vec<ModelDownloadJob> {
        self.downloads.snapshots()
    }

    pub fn max_parallel_downloads(&self) -> usize {
        self.downloads.maximum_parallel()
    }

    pub fn set_max_parallel_downloads(self: &Arc<Self>, maximum_parallel: usize) {
        let starts = self.downloads.set_maximum_parallel(maximum_parallel);
        self.spawn_download_jobs(starts);
    }

    pub fn enqueue_download(self: &Arc<Self>, model_ref: String) -> DownloadAdmission {
        let (admission, starts) = self.downloads.admit(model_ref);
        self.spawn_download_jobs(starts);
        admission
    }

    pub fn pause_download(
        self: &Arc<Self>,
        job_id: &ModelDownloadJobId,
    ) -> std::result::Result<ModelDownloadJob, String> {
        let (job, starts) = self.downloads.pause(job_id)?;
        self.spawn_download_jobs(starts);
        Ok(job)
    }

    pub fn resume_download(
        self: &Arc<Self>,
        job_id: &ModelDownloadJobId,
    ) -> std::result::Result<ModelDownloadJob, String> {
        let (job, starts) = self.downloads.resume(job_id)?;
        self.spawn_download_jobs(starts);
        Ok(job)
    }

    pub fn cancel_download(
        self: &Arc<Self>,
        job_id: &ModelDownloadJobId,
    ) -> std::result::Result<ModelDownloadJob, String> {
        let (job, transition) = self.downloads.cancel(job_id)?;
        self.apply_download_transition(transition);
        Ok(job)
    }

    pub async fn search(
        &self,
        query: &str,
        format: Option<ArtifactFormat>,
    ) -> Result<CatalogSearch> {
        let mut repositories = Vec::new();
        for provider in &self.providers {
            repositories.extend(provider.search(query, format).await?);
        }
        Ok(CatalogSearch {
            query: query.to_owned(),
            repositories,
        })
    }

    pub async fn download(&self, model_ref: &str) -> Result<ModelArtifact> {
        self.download_with_status(model_ref)
            .await
            .map(|result| result.artifact)
    }

    pub async fn download_with_status(&self, model_ref: &str) -> Result<ModelDownloadResult> {
        let job_id = ModelDownloadJobId::new();
        self.download_with_job(&job_id, model_ref, None)
            .await
            .map(DownloadOutcome::into_result)
    }

    async fn download_with_job(
        &self,
        job_id: &ModelDownloadJobId,
        model_ref: &str,
        generation: Option<u64>,
    ) -> Result<DownloadOutcome> {
        let resolving = DownloadProgressContext {
            job_id,
            model_ref,
            provider: None,
            repository: None,
            generation,
        };
        self.emit(
            &resolving,
            "",
            ModelOperationPhase::Resolving,
            0,
            None,
            "Resolving exact artifact",
        );
        let provider = self.provider_for(model_ref)?;
        let resolved = provider.resolve(model_ref).await?;
        self.download_resolved(job_id, model_ref, &resolved, generation)
            .await
    }

    fn spawn_download_jobs(self: &Arc<Self>, starts: Vec<download_manager::DownloadStart>) {
        for start in starts {
            let library = Arc::clone(self);
            tokio::spawn(async move {
                let job_id = start.id;
                let generation = start.generation;
                let mut stop = start.stop;
                let Some(model_ref) = library.downloads.model_ref(&job_id) else {
                    return;
                };
                let completion = tokio::select! {
                    biased;
                    changed = stop.changed() => {
                        let _ = changed;
                        None
                    }
                    result = library.download_with_job(&job_id, &model_ref, Some(generation)) => {
                        Some(match &result {
                            Ok(DownloadOutcome::Installed(model)) => (
                                ModelOperationPhase::Installed,
                                format!("Model installed as {}", model.id),
                            ),
                            Ok(DownloadOutcome::AlreadyInstalled(model)) => (
                                ModelOperationPhase::Installed,
                                format!("Already installed as {}", model.id),
                            ),
                            Err(error) => (ModelOperationPhase::Failed, error.to_string()),
                        })
                    }
                };
                let transition = library
                    .downloads
                    .finish_attempt(&job_id, generation, completion);
                library.apply_download_transition(transition);
            });
        }
    }

    fn apply_download_transition(
        self: &Arc<Self>,
        transition: download_manager::DownloadTransition,
    ) {
        for path in transition.cleanup {
            tokio::spawn(async move {
                if let Err(error) = tokio::fs::remove_file(&path).await
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!(path = %path.display(), %error, "could not clean model download partial");
                }
            });
        }
        self.spawn_download_jobs(transition.starts);
    }

    pub async fn import(&self, source: &Path) -> Result<ModelArtifact> {
        let source = source
            .canonicalize()
            .map_err(|source_error| ModelLibraryError::Io {
                path: source.to_path_buf(),
                source: source_error,
            })?;
        if !source.is_file() {
            return Err(ModelLibraryError::InvalidArtifact(format!(
                "{} is not a file",
                source.display()
            )));
        }
        let format = ArtifactFormat::from_path(&source).ok_or_else(|| {
            ModelLibraryError::InvalidArtifact(
                "expected a .gguf, .q27, or .ninfer artifact".to_owned(),
            )
        })?;
        let source_parent = source.parent().unwrap_or_else(|| Path::new("."));
        let package_root = local_package_root(&source, format).await?;
        validate_artifact(&source, format)?;
        let discovery_root = package_root.unwrap_or_else(|| source_parent.to_path_buf());
        let source_registry = ModelRegistry::discover(&[discovery_root]);
        let discovered_source = source_registry
            .artifacts()
            .iter()
            .find(|artifact| artifact.path == source)
            .cloned()
            .ok_or_else(|| {
                ModelLibraryError::InvalidArtifact(if source_registry.warnings().is_empty() {
                    "artifact was not admitted by local model discovery".to_owned()
                } else {
                    source_registry.warnings().join("; ")
                })
            })?;
        let companion =
            if format == ArtifactFormat::Q27 && discovered_source.norted_package.is_none() {
                Some(validate_raw_q27_tokenizer(&source)?)
            } else {
                None
            };
        let filename = safe_basename(&source)?;
        let import_identity = discovered_source
            .norted_package
            .as_ref()
            .map_or(source.as_path(), |package| package.manifest_path.as_path());
        let logical = format!(
            "local-import:{}",
            sha256_text(&import_identity.to_string_lossy())
        );
        let acquisition_id = logical.clone();
        let destination = self.root.join("imports").join(&logical[13..25]);
        let _acquisition_lock = self.acquisition_lock(&acquisition_id).await?;
        if destination.exists() {
            return Err(ModelLibraryError::AlreadyInstalled(destination));
        }
        let stage = self.create_stage().await?;
        let (selected_path, receipt_members) = if let Some(package) =
            &discovered_source.norted_package
        {
            let manifest_bytes =
                tokio::fs::read(&package.manifest_path)
                    .await
                    .map_err(|source| ModelLibraryError::Io {
                        path: package.manifest_path.clone(),
                        source,
                    })?;
            let plan = plan_norted_package_acquisition(&manifest_bytes, format)
                .map_err(ModelLibraryError::InvalidArtifact)?;
            let selected = source
                .strip_prefix(&package.package_root)
                .map_err(|_| {
                    ModelLibraryError::InvalidArtifact(
                        "package primary is outside its package root".to_owned(),
                    )
                })?
                .to_path_buf();
            if !plan.primary_files().any(|file| file.path == selected) {
                return Err(ModelLibraryError::InvalidArtifact(
                    "selected artifact is not a primary member of its Norted package".to_owned(),
                ));
            }
            for file in &plan.files {
                copy_relative_file(&package.package_root, &stage, &file.path).await?;
            }
            verify_package_files(&stage, &plan).await?;
            let installed = validate_package_stage(&stage, &plan)?;
            let mut members = Vec::with_capacity(installed.len());
            for artifact in installed {
                let relative = artifact
                    .path
                    .strip_prefix(&*stage)
                    .map_err(|_| {
                        ModelLibraryError::InvalidArtifact(
                            "staged package member escaped its acquisition root".to_owned(),
                        )
                    })?
                    .to_path_buf();
                let source_path = package.package_root.join(&relative);
                let provenance = ModelArtifactProvenance {
                    acquisition_id: acquisition_id.clone(),
                    provider: "local_import".to_owned(),
                    repository: None,
                    logical_id: Some(format!("{acquisition_id}:{}", slash_path(&relative))),
                    source: Some(source_path.display().to_string()),
                    revision: None,
                    remote_filename: None,
                    acquired_at_unix: unix_timestamp(),
                    size_bytes: artifact.size_bytes,
                    digest: None,
                };
                members.push(ModelLibraryReceiptMember {
                    path: relative,
                    format: artifact.format,
                    provenance,
                });
            }
            (selected, members)
        } else {
            let selected = PathBuf::from(&filename);
            copy_file(&source, &stage.join(&selected)).await?;
            if let Some(companion) = companion {
                let staged_companion = stage.join(safe_basename(&companion)?);
                copy_file(&companion, &staged_companion).await?;
                let paired = validate_raw_q27_tokenizer(&stage.join(&selected))?;
                if paired != staged_companion {
                    return Err(ModelLibraryError::InvalidArtifact(
                        "staged q27 tokenizer companion no longer matches the selected tokenizer"
                            .to_owned(),
                    ));
                }
            }
            validate_artifact(&stage.join(&selected), format)?;
            let provenance = ModelArtifactProvenance {
                acquisition_id: acquisition_id.clone(),
                provider: "local_import".to_owned(),
                repository: None,
                logical_id: Some(logical),
                source: Some(source.display().to_string()),
                revision: None,
                remote_filename: None,
                acquired_at_unix: unix_timestamp(),
                size_bytes: file_len(&source).await?,
                digest: None,
            };
            (
                selected.clone(),
                vec![ModelLibraryReceiptMember {
                    path: selected,
                    format,
                    provenance,
                }],
            )
        };
        write_receipt(&stage, &acquisition_id, receipt_members).await?;
        let _ = discover_exact(&stage, &selected_path)?;
        activate_stage(&stage, &destination).await?;
        discover_exact(&destination, &selected_path)
    }

    pub fn plan_removal(&self, model: &ModelArtifact) -> Result<ModelRemovalPlan> {
        let root = self
            .root
            .canonicalize()
            .map_err(|source| ModelLibraryError::Io {
                path: self.root.clone(),
                source,
            })?;
        let path = model
            .path
            .canonicalize()
            .map_err(|source| ModelLibraryError::Io {
                path: model.path.clone(),
                source,
            })?;
        let provenance = model
            .provenance
            .as_ref()
            .ok_or_else(|| ModelLibraryError::NotManaged(model.id.clone()))?;
        if !path.starts_with(&root) {
            return Err(ModelLibraryError::NotManaged(model.id.clone()));
        }
        let mut acquisition_root = path
            .parent()
            .ok_or_else(|| ModelLibraryError::NotManaged(model.id.clone()))?;
        let receipt_path = loop {
            let candidate = model_library_receipt_path(acquisition_root);
            if candidate.exists() {
                break candidate;
            }
            if acquisition_root == root || !acquisition_root.starts_with(&root) {
                return Err(ModelLibraryError::NotManaged(model.id.clone()));
            }
            acquisition_root = acquisition_root
                .parent()
                .ok_or_else(|| ModelLibraryError::NotManaged(model.id.clone()))?;
        };
        if acquisition_root == root
            || !acquisition_root.starts_with(&root)
            || acquisition_root.starts_with(&self.staging)
        {
            return Err(ModelLibraryError::NotManaged(model.id.clone()));
        }
        let bytes = std::fs::read(&receipt_path).map_err(|source| ModelLibraryError::Io {
            path: receipt_path.clone(),
            source,
        })?;
        let receipt: ModelLibraryReceipt = serde_json::from_slice(&bytes)
            .map_err(|error| ModelLibraryError::InvalidArtifact(error.to_string()))?;
        if receipt.schema_version != RECEIPT_SCHEMA_VERSION
            || receipt.acquisition_id != provenance.acquisition_id
            || receipt.members.is_empty()
        {
            return Err(ModelLibraryError::NotManaged(model.id.clone()));
        }
        let mut member_paths = HashSet::new();
        let mut selected = false;
        for member in &receipt.members {
            if !safe_relative(&member.path)
                || !member_paths.insert(member.path.clone())
                || member.provenance.acquisition_id != receipt.acquisition_id
            {
                return Err(ModelLibraryError::InvalidArtifact(
                    "managed acquisition receipt contains an unsafe or conflicting member"
                        .to_owned(),
                ));
            }
            let member_path = acquisition_root.join(&member.path);
            let canonical = member_path
                .canonicalize()
                .map_err(|source| ModelLibraryError::Io {
                    path: member_path.clone(),
                    source,
                })?;
            let metadata = canonical
                .metadata()
                .map_err(|source| ModelLibraryError::Io {
                    path: canonical.clone(),
                    source,
                })?;
            if !canonical.starts_with(acquisition_root)
                || !metadata.is_file()
                || metadata.len() != member.provenance.size_bytes
                || ArtifactFormat::from_path(&canonical) != Some(member.format)
            {
                return Err(ModelLibraryError::InvalidArtifact(format!(
                    "managed acquisition member {} no longer matches its receipt",
                    member.path.display()
                )));
            }
            selected |= canonical == path;
        }
        if !selected {
            return Err(ModelLibraryError::NotManaged(model.id.clone()));
        }
        let registry = ModelRegistry::discover(&[acquisition_root.to_path_buf()]);
        let mut affected_model_ids = Vec::with_capacity(receipt.members.len());
        for member in &receipt.members {
            let expected =
                acquisition_root
                    .join(&member.path)
                    .canonicalize()
                    .map_err(|source| ModelLibraryError::Io {
                        path: acquisition_root.join(&member.path),
                        source,
                    })?;
            let artifact = registry
                .artifacts()
                .iter()
                .find(|artifact| artifact.path == expected)
                .ok_or_else(|| {
                    ModelLibraryError::InvalidArtifact(if registry.warnings().is_empty() {
                        format!(
                            "managed acquisition member {} is no longer discoverable",
                            member.path.display()
                        )
                    } else {
                        registry.warnings().join("; ")
                    })
                })?;
            if artifact
                .provenance
                .as_ref()
                .map(|value| value.acquisition_id.as_str())
                != Some(receipt.acquisition_id.as_str())
            {
                return Err(ModelLibraryError::InvalidArtifact(format!(
                    "managed acquisition member {} has invalid provenance",
                    member.path.display()
                )));
            }
            affected_model_ids.push(artifact.id.clone());
        }
        affected_model_ids.sort();
        affected_model_ids.dedup();
        Ok(ModelRemovalPlan {
            acquisition_root: acquisition_root.to_path_buf(),
            acquisition_id: receipt.acquisition_id,
            affected_model_ids,
        })
    }

    pub async fn remove(&self, plan: &ModelRemovalPlan) -> Result<()> {
        let _acquisition_lock = self.acquisition_lock(&plan.acquisition_id).await?;
        let root = self
            .root
            .canonicalize()
            .map_err(|source| ModelLibraryError::Io {
                path: self.root.clone(),
                source,
            })?;
        let acquisition_root =
            plan.acquisition_root
                .canonicalize()
                .map_err(|source| ModelLibraryError::Io {
                    path: plan.acquisition_root.clone(),
                    source,
                })?;
        if acquisition_root != plan.acquisition_root
            || acquisition_root == root
            || !acquisition_root.starts_with(&root)
            || acquisition_root.starts_with(&self.staging)
            || !model_library_receipt_path(&acquisition_root).is_file()
        {
            return Err(ModelLibraryError::InvalidArtifact(
                "managed acquisition removal target is no longer safe".to_owned(),
            ));
        }
        tokio::fs::remove_dir_all(&acquisition_root)
            .await
            .map_err(|source| ModelLibraryError::Io {
                path: acquisition_root,
                source,
            })
    }

    fn provider_for(&self, model_ref: &str) -> Result<&Arc<dyn ModelCatalogProvider>> {
        self.providers
            .iter()
            .find(|provider| model_ref.starts_with(&format!("{}:", provider.id())))
            .ok_or_else(|| ModelLibraryError::InvalidReference(model_ref.to_owned()))
    }

    async fn download_resolved(
        &self,
        job_id: &ModelDownloadJobId,
        model_ref: &str,
        resolved: &ResolvedArtifact,
        generation: Option<u64>,
    ) -> Result<DownloadOutcome> {
        tokio::fs::create_dir_all(&self.download_cache)
            .await
            .map_err(|source| ModelLibraryError::Io {
                path: self.download_cache.clone(),
                source,
            })?;
        let destination = managed_destination(&self.root, resolved)?;
        let context = DownloadProgressContext {
            job_id,
            model_ref,
            provider: Some(&resolved.provider),
            repository: Some(&resolved.repository),
            generation,
        };
        self.downloads.register_partial_paths(
            job_id,
            generation,
            std::iter::once(&resolved.primary)
                .chain(resolved.companions.iter())
                .map(|file| download_partial_path(&self.download_cache, model_ref, file)),
        );
        let acquisition_id = remote_acquisition_id(resolved);
        let _acquisition_lock = self.acquisition_lock(&acquisition_id).await?;
        let primary_name = safe_remote_path(&resolved.primary.filename)?;
        if destination.exists() {
            let artifact = discover_exact(&destination, &primary_name)?;
            self.emit(
                &context,
                &resolved.primary.filename,
                ModelOperationPhase::Installed,
                artifact.size_bytes,
                Some(artifact.size_bytes),
                "Already installed",
            );
            return Ok(DownloadOutcome::AlreadyInstalled(artifact));
        }
        let stage = self.create_stage().await?;
        async {
            let total = resolved
                .primary
                .size_bytes
                .into_iter()
                .chain(
                    resolved
                        .companions
                        .iter()
                        .filter_map(|file| file.size_bytes),
                )
                .sum::<u64>();
            let total_known = resolved.primary.size_bytes.is_some()
                && resolved
                    .companions
                    .iter()
                    .all(|file| file.size_bytes.is_some());
            let mut completed = 0_u64;
            let mut destinations = HashSet::new();
            for file in std::iter::once(&resolved.primary).chain(resolved.companions.iter()) {
                let relative = safe_remote_path(&file.filename)?;
                if !destinations.insert(relative.clone()) {
                    return Err(ModelLibraryError::InvalidCatalog(format!(
                        "remote acquisition contains colliding path `{}`",
                        relative.display()
                    )));
                }
                let destination = stage.join(&relative);
                create_parent(&destination).await?;
                let bytes = self
                    .download_file(
                        &context,
                        file,
                        &destination,
                        completed,
                        total_known.then_some(total),
                    )
                    .await?;
                completed += bytes;
            }
            let primary_path = stage.join(&primary_name);
            self.emit(
                &context,
                &resolved.primary.filename,
                ModelOperationPhase::Validating,
                completed,
                total_known.then_some(total),
                "Inspecting downloaded artifact",
            );
            let receipt_members = if let Some(package) = &resolved.package {
                let manifest_path = stage.join(safe_remote_path(&package.manifest_filename)?);
                let manifest_bytes = tokio::fs::read(&manifest_path).await.map_err(|source| {
                    ModelLibraryError::Io {
                        path: manifest_path,
                        source,
                    }
                })?;
                let plan = plan_norted_package_acquisition(&manifest_bytes, resolved.format)
                    .map_err(ModelLibraryError::InvalidArtifact)?;
                let package_root = Path::new(&package.manifest_filename)
                    .parent()
                    .unwrap_or_else(|| Path::new(""));
                let staged_package_root = stage.join(package_root);
                verify_package_files(&staged_package_root, &plan).await?;
                let installed = validate_package_stage(&staged_package_root, &plan)?;
                let expected = package
                    .primary_filenames
                    .iter()
                    .map(|path| safe_remote_path(path))
                    .collect::<Result<HashSet<_>>>()?;
                let observed = installed
                    .iter()
                    .filter_map(|artifact| artifact.path.strip_prefix(&*stage).ok())
                    .map(Path::to_path_buf)
                    .collect::<HashSet<_>>();
                if observed != expected {
                    return Err(ModelLibraryError::InvalidArtifact(
                        "downloaded package primary membership differs from its manifest"
                            .to_owned(),
                    ));
                }
                let resolved_files = std::iter::once(&resolved.primary)
                    .chain(resolved.companions.iter())
                    .map(|file| (file.filename.as_str(), file))
                    .collect::<std::collections::HashMap<_, _>>();
                installed
                    .into_iter()
                    .map(|artifact| {
                        let relative = artifact
                            .path
                            .strip_prefix(&*stage)
                            .map_err(|_| {
                                ModelLibraryError::InvalidArtifact(
                                    "staged package member escaped its acquisition root".to_owned(),
                                )
                            })?
                            .to_path_buf();
                        let remote = slash_path(&relative);
                        let file = resolved_files.get(remote.as_str()).ok_or_else(|| {
                            ModelLibraryError::InvalidCatalog(format!(
                                "package primary `{remote}` was not resolved"
                            ))
                        })?;
                        Ok(ModelLibraryReceiptMember {
                            path: relative,
                            format: artifact.format,
                            provenance: remote_provenance(
                                resolved,
                                file,
                                &acquisition_id,
                                artifact.size_bytes,
                            ),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?
            } else {
                validate_artifact(&primary_path, resolved.format)?;
                if resolved.format == ArtifactFormat::Q27 {
                    let _ = validate_raw_q27_tokenizer(&primary_path)?;
                }
                vec![ModelLibraryReceiptMember {
                    path: primary_name.clone(),
                    format: resolved.format,
                    provenance: remote_provenance(
                        resolved,
                        &resolved.primary,
                        &acquisition_id,
                        file_len(&primary_path).await?,
                    ),
                }]
            };
            write_receipt(&stage, &acquisition_id, receipt_members).await?;
            let _ = discover_exact(&stage, &primary_name)?;
            if destination.exists() {
                return Err(ModelLibraryError::AlreadyInstalled(destination.clone()));
            }
            self.emit(
                &context,
                &resolved.primary.filename,
                ModelOperationPhase::Installing,
                completed,
                total_known.then_some(total),
                "Atomically activating model",
            );
            activate_stage(&stage, &destination).await?;
            let artifact = discover_exact(&destination, &primary_name)?;
            self.emit(
                &context,
                &resolved.primary.filename,
                ModelOperationPhase::Installed,
                completed,
                total_known.then_some(total),
                "Model installed",
            );
            Ok(DownloadOutcome::Installed(artifact))
        }
        .await
    }

    async fn acquisition_lock(&self, acquisition_id: &str) -> Result<AcquisitionLock> {
        let lock_path = self
            .download_cache
            .join("locks")
            .join(format!("{}.lock", sha256_text(acquisition_id)));
        let task_path = lock_path.clone();
        let file = tokio::task::spawn_blocking(move || {
            if let Some(parent) = task_path.parent() {
                std::fs::create_dir_all(parent).map_err(|source| ModelLibraryError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&task_path)
                .map_err(|source| ModelLibraryError::Io {
                    path: task_path.clone(),
                    source,
                })?;
            file.lock_exclusive()
                .map_err(|source| ModelLibraryError::Io {
                    path: task_path,
                    source,
                })?;
            Ok::<std::fs::File, ModelLibraryError>(file)
        })
        .await
        .map_err(|error| ModelLibraryError::Io {
            path: lock_path,
            source: std::io::Error::other(error),
        })??;
        Ok(AcquisitionLock { _file: file })
    }

    async fn download_file(
        &self,
        context: &DownloadProgressContext<'_>,
        file: &ResolvedFile,
        destination: &Path,
        completed_before: u64,
        total: Option<u64>,
    ) -> Result<u64> {
        let partial = download_partial_path(&self.download_cache, context.model_ref, file);
        let mut existing = tokio::fs::metadata(&partial)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if file
            .size_bytes
            .is_some_and(|expected_size| existing > expected_size)
        {
            tokio::fs::remove_file(&partial)
                .await
                .map_err(|source| ModelLibraryError::Io {
                    path: partial.clone(),
                    source,
                })?;
            existing = 0;
        }
        if file.size_bytes == Some(existing) && existing > 0 {
            if let Some(expected) = &file.sha256 {
                let observed = sha256_file(&partial).await?;
                if !observed.eq_ignore_ascii_case(expected) {
                    tokio::fs::remove_file(&partial).await.map_err(|source| {
                        ModelLibraryError::Io {
                            path: partial.clone(),
                            source,
                        }
                    })?;
                    return Err(ModelLibraryError::DigestMismatch {
                        filename: file.filename.clone(),
                        expected: expected.clone(),
                        observed,
                    });
                }
            }
            stage_download_partial(&partial, destination, context.generation.is_some()).await?;
            return Ok(existing);
        }
        let mut request = Client::new().get(file.url.clone());
        if existing > 0 {
            request = request.header(header::RANGE, format!("bytes={existing}-"));
        }
        let response = request.send().await?.error_for_status()?;
        let resumed = existing > 0 && response.status() == StatusCode::PARTIAL_CONTENT;
        let offset = if resumed { existing } else { 0 };
        let mut output = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(resumed)
            .truncate(!resumed)
            .open(&partial)
            .await
            .map_err(|source| ModelLibraryError::Io {
                path: partial.clone(),
                source,
            })?;
        let mut downloaded = offset;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            output
                .write_all(&chunk)
                .await
                .map_err(|source| ModelLibraryError::Io {
                    path: partial.clone(),
                    source,
                })?;
            downloaded += chunk.len() as u64;
            self.emit(
                context,
                &file.filename,
                ModelOperationPhase::Downloading,
                completed_before + downloaded,
                total,
                "Downloading model data",
            );
        }
        output
            .flush()
            .await
            .map_err(|source| ModelLibraryError::Io {
                path: partial.clone(),
                source,
            })?;
        drop(output);
        if let Some(expected_size) = file.size_bytes
            && downloaded != expected_size
        {
            return Err(ModelLibraryError::InvalidArtifact(format!(
                "{} has {downloaded} bytes; expected {expected_size}",
                file.filename
            )));
        }
        self.emit(
            context,
            &file.filename,
            ModelOperationPhase::Verifying,
            completed_before + downloaded,
            total,
            if file.sha256.is_some() {
                "Verifying SHA-256"
            } else {
                "Verifying downloaded size"
            },
        );
        if let Some(expected) = &file.sha256 {
            let observed = sha256_file(&partial).await?;
            if !observed.eq_ignore_ascii_case(expected) {
                let _ = tokio::fs::remove_file(&partial).await;
                return Err(ModelLibraryError::DigestMismatch {
                    filename: file.filename.clone(),
                    expected: expected.clone(),
                    observed,
                });
            }
        }
        stage_download_partial(&partial, destination, context.generation.is_some()).await?;
        Ok(downloaded)
    }

    async fn create_stage(&self) -> Result<StagingDirectory> {
        tokio::fs::create_dir_all(&self.staging)
            .await
            .map_err(|source| ModelLibraryError::Io {
                path: self.staging.clone(),
                source,
            })?;
        let stage = self.staging.join(format!("stage-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir(&stage)
            .await
            .map_err(|source| ModelLibraryError::Io {
                path: stage.clone(),
                source,
            })?;
        Ok(StagingDirectory(stage))
    }

    fn emit(
        &self,
        context: &DownloadProgressContext<'_>,
        filename: &str,
        phase: ModelOperationPhase,
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
        message: &str,
    ) {
        let progress = ModelOperationProgress {
            job_id: context.job_id.clone(),
            model_ref: context.model_ref.to_owned(),
            provider: context.provider.map(str::to_owned),
            repository: context.repository.map(str::to_owned),
            filename: filename.to_owned(),
            phase,
            downloaded_bytes,
            total_bytes,
            message: message.to_owned(),
        };
        self.downloads.update(&progress, context.generation);
        let _ = self.progress.send(progress);
    }
}

fn download_partial_path(cache: &Path, model_ref: &str, file: &ResolvedFile) -> PathBuf {
    cache.join(format!(
        "{}.part",
        sha256_text(&format!("{model_ref}\0{}", file.url))
    ))
}

async fn stage_download_partial(partial: &Path, destination: &Path, preserve: bool) -> Result<()> {
    let result = if preserve {
        tokio::fs::copy(partial, destination).await.map(|_| ())
    } else {
        tokio::fs::rename(partial, destination).await
    };
    result.map_err(|source| ModelLibraryError::Io {
        path: destination.to_path_buf(),
        source,
    })
}

pub fn setting_definition() -> norted_core::SettingDefinition {
    norted_core::SettingDefinition {
        id: norted_core::SettingId::new(MAX_PARALLEL_DOWNLOADS_SETTING_ID)
            .expect("static model download setting ID"),
        label: "Max parallel downloads".to_owned(),
        description: "Maximum number of separate model acquisitions that may download at once; additional downloads wait in FIFO order".to_owned(),
        kind: norted_core::SettingKind::UnsignedInteger {
            minimum: Some(1),
            maximum: None,
        },
        scope: norted_core::SettingScope::Global,
        category: norted_core::SettingCategory::Downloads,
        supported: true,
        unsupported_reason: None,
        unit: Some("downloads".to_owned()),
        upstream_default: Some(DEFAULT_MAX_PARALLEL_DOWNLOADS.to_string()),
        default_preview: Some(norted_core::SettingDefaultPreview::new(
            DEFAULT_MAX_PARALLEL_DOWNLOADS.to_string(),
            norted_core::SettingDefaultSource::Norted,
        )),
    }
}

pub fn max_parallel_downloads_from_settings(settings: &norted_core::SettingsState) -> usize {
    settings
        .global_defaults
        .iter()
        .find_map(|(id, value)| (id.as_str() == MAX_PARALLEL_DOWNLOADS_SETTING_ID).then_some(value))
        .and_then(|value| match value {
            norted_core::SettingValue::UnsignedInteger(value) => usize::try_from(*value).ok(),
            _ => None,
        })
        .unwrap_or(DEFAULT_MAX_PARALLEL_DOWNLOADS)
        .max(1)
}

struct DownloadProgressContext<'a> {
    job_id: &'a ModelDownloadJobId,
    model_ref: &'a str,
    provider: Option<&'a str>,
    repository: Option<&'a str>,
    generation: Option<u64>,
}

enum DownloadOutcome {
    Installed(ModelArtifact),
    AlreadyInstalled(ModelArtifact),
}

impl DownloadOutcome {
    fn into_result(self) -> ModelDownloadResult {
        match self {
            Self::Installed(artifact) => ModelDownloadResult {
                artifact,
                already_installed: false,
            },
            Self::AlreadyInstalled(artifact) => ModelDownloadResult {
                artifact,
                already_installed: true,
            },
        }
    }
}

struct AcquisitionLock {
    _file: std::fs::File,
}

struct StagingDirectory(PathBuf);

impl Deref for StagingDirectory {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %self.0.display(), %error, "could not clean model staging directory");
        }
    }
}

pub struct HuggingFaceCatalogProvider {
    client: Client,
    api_base: Url,
}

impl HuggingFaceCatalogProvider {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .user_agent(concat!("norted-server/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("static HTTP client configuration"),
            api_base: Url::parse("https://huggingface.co/").expect("static Hugging Face URL"),
        }
    }

    async fn repository(&self, repository: &str, revision: Option<&str>) -> Result<HfRepository> {
        let mut url = self.api_base.clone();
        {
            let mut segments = url.path_segments_mut().map_err(|_| {
                ModelLibraryError::InvalidCatalog(
                    "Hugging Face base URL cannot hold path segments".to_owned(),
                )
            })?;
            segments.push("api").push("models");
            for part in repository.split('/') {
                segments.push(part);
            }
            if let Some(revision) = revision {
                segments.push("revision").push(revision);
            }
        }
        url.query_pairs_mut().append_pair("blobs", "true");
        Ok(self
            .client
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    async fn manifest_bytes(&self, url: Url) -> Result<Vec<u8>> {
        let response = self.client.get(url).send().await?.error_for_status()?;
        if response
            .content_length()
            .is_some_and(|size| size > MAX_PACKAGE_MANIFEST_BYTES)
        {
            return Err(ModelLibraryError::InvalidCatalog(
                "Norted package manifest exceeds the bounded manifest size".to_owned(),
            ));
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if bytes.len().saturating_add(chunk.len())
                > usize::try_from(MAX_PACKAGE_MANIFEST_BYTES).expect("manifest bound fits usize")
            {
                return Err(ModelLibraryError::InvalidCatalog(
                    "Norted package manifest exceeds the bounded manifest size".to_owned(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    async fn resolve_package(
        &self,
        repository: &HfRepository,
        repository_id: &str,
        revision: &str,
        primary_filename: &str,
        format: ArtifactFormat,
    ) -> Result<Option<(ResolvedPackage, Vec<ResolvedFile>)>> {
        let manifest_name = norted_package_manifest_name(format);
        let primary_path = Path::new(primary_filename);
        let mut directory = primary_path.parent().unwrap_or_else(|| Path::new(""));
        loop {
            let manifest_path = directory.join(manifest_name);
            let manifest_filename = slash_path(&manifest_path);
            if let Some(manifest) = repository
                .siblings
                .iter()
                .find(|file| file.rfilename == manifest_filename)
            {
                let resolved_manifest =
                    manifest.resolved(&self.api_base, repository_id, revision)?;
                let bytes = self.manifest_bytes(resolved_manifest.url.clone()).await?;
                let selected_relative = primary_path.strip_prefix(directory).map_err(|_| {
                    ModelLibraryError::InvalidCatalog(
                        "remote package candidate is outside its manifest directory".to_owned(),
                    )
                })?;
                match plan_norted_package_acquisition(&bytes, format) {
                    Ok(plan) => {
                        if let Some(selected_member) = plan
                            .files
                            .iter()
                            .find(|file| file.path == selected_relative)
                        {
                            if selected_member.role != NortedPackageAcquisitionRole::Primary {
                                return Err(ModelLibraryError::InvalidCatalog(format!(
                                    "Norted package manifest `{manifest_filename}` declares `{primary_filename}` as a non-primary {} member; select a primary model artifact instead",
                                    package_role_name(selected_member.role)
                                )));
                            }
                            let primary_filenames = plan
                                .primary_files()
                                .map(|file| slash_path(&directory.join(&file.path)))
                                .collect::<Vec<_>>();
                            let mut files = Vec::with_capacity(plan.files.len());
                            for planned in &plan.files {
                                let filename = slash_path(&directory.join(&planned.path));
                                let sibling = repository
                                    .siblings
                                    .iter()
                                    .find(|file| file.rfilename == filename)
                                    .ok_or_else(|| {
                                        ModelLibraryError::InvalidCatalog(format!(
                                            "Norted package manifest references missing file `{filename}`"
                                        ))
                                    })?;
                                let mut resolved =
                                    sibling.resolved(&self.api_base, repository_id, revision)?;
                                if planned
                                    .size_bytes
                                    .zip(resolved.size_bytes)
                                    .is_some_and(|(expected, observed)| expected != observed)
                                {
                                    return Err(ModelLibraryError::InvalidCatalog(format!(
                                        "Norted package file `{filename}` catalog size disagrees with its manifest"
                                    )));
                                }
                                if planned
                                    .sha256
                                    .as_ref()
                                    .zip(resolved.sha256.as_ref())
                                    .is_some_and(|(expected, observed)| {
                                        !expected.eq_ignore_ascii_case(observed)
                                    })
                                {
                                    return Err(ModelLibraryError::InvalidCatalog(format!(
                                        "Norted package file `{filename}` catalog digest disagrees with its manifest"
                                    )));
                                }
                                resolved.size_bytes = planned.size_bytes.or(resolved.size_bytes);
                                resolved.sha256 = planned.sha256.clone().or(resolved.sha256);
                                files.push(resolved);
                            }
                            return Ok(Some((
                                ResolvedPackage {
                                    manifest_filename,
                                    primary_filenames,
                                },
                                files,
                            )));
                        }
                    }
                    Err(error) => {
                        if recover_norted_package_primary_paths(&bytes, format)
                            .is_some_and(|claims| claims.contains(selected_relative))
                        {
                            return Err(ModelLibraryError::InvalidCatalog(format!(
                                "Norted package manifest `{manifest_filename}` claims `{primary_filename}` but is invalid: {error}"
                            )));
                        }
                    }
                }
            }
            if directory.as_os_str().is_empty() {
                break;
            }
            directory = directory.parent().unwrap_or_else(|| Path::new(""));
        }
        Ok(None)
    }
}

impl Default for HuggingFaceCatalogProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ModelCatalogProvider for HuggingFaceCatalogProvider {
    fn id(&self) -> &'static str {
        "huggingface"
    }

    async fn search(
        &self,
        query: &str,
        format: Option<ArtifactFormat>,
    ) -> Result<Vec<CatalogRepository>> {
        let mut url = self.api_base.join("api/models").expect("static API path");
        url.query_pairs_mut()
            .append_pair("search", query)
            .append_pair("limit", "50")
            .append_pair("full", "true")
            .append_pair("blobs", "true");
        let repositories: Vec<HfRepository> = self
            .client
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        repositories
            .into_iter()
            .filter_map(|repo| repo.into_catalog(format).transpose())
            .collect()
    }

    async fn resolve(&self, model_ref: &str) -> Result<ResolvedArtifact> {
        let parsed = parse_hf_ref(model_ref)?;
        let repository = self
            .repository(&parsed.repository, Some(&parsed.revision))
            .await?;
        let revision = repository.sha.clone().unwrap_or(parsed.revision);
        let format = ArtifactFormat::from_path(Path::new(&parsed.filename)).ok_or_else(|| {
            ModelLibraryError::InvalidReference(
                "artifact must end in .gguf, .q27, or .ninfer".to_owned(),
            )
        })?;
        let primary = repository
            .siblings
            .iter()
            .find(|file| file.rfilename == parsed.filename)
            .ok_or_else(|| {
                ModelLibraryError::InvalidReference(format!(
                    "{} does not contain {} at revision {revision}",
                    parsed.repository, parsed.filename
                ))
            })?;
        let package = self
            .resolve_package(
                &repository,
                &parsed.repository,
                &revision,
                &parsed.filename,
                format,
            )
            .await?;
        let (resolved_primary, package, companions) = if let Some((package, mut files)) = package {
            let selected = files
                .iter()
                .position(|file| file.filename == parsed.filename)
                .ok_or_else(|| {
                    ModelLibraryError::InvalidCatalog(
                        "resolved package does not contain its selected primary".to_owned(),
                    )
                })?;
            let resolved_primary = files.remove(selected);
            (resolved_primary, Some(package), files)
        } else if format == ArtifactFormat::Q27 {
            let tokenizer_names = repository
                .siblings
                .iter()
                .filter(|file| {
                    file.rfilename.to_ascii_lowercase().ends_with(".tok")
                        && same_remote_directory(&file.rfilename, &parsed.filename)
                })
                .map(|file| file.rfilename.clone())
                .collect::<Vec<_>>();
            let selected = select_q27_tokenizer_filename(&parsed.filename, &tokenizer_names)
                .map_err(ModelLibraryError::InvalidCatalog)?
                .ok_or_else(|| {
                    ModelLibraryError::InvalidCatalog(format!(
                        "q27 artifact {} has no tokenizer companion",
                        parsed.filename
                    ))
                })?;
            let companions = vec![
                repository
                    .siblings
                    .iter()
                    .find(|file| file.rfilename == selected)
                    .expect("selected sibling exists")
                    .resolved(&self.api_base, &parsed.repository, &revision)?,
            ];
            (
                primary.resolved(&self.api_base, &parsed.repository, &revision)?,
                None,
                companions,
            )
        } else {
            (
                primary.resolved(&self.api_base, &parsed.repository, &revision)?,
                None,
                Vec::new(),
            )
        };
        Ok(ResolvedArtifact {
            provider: self.id().to_owned(),
            repository: parsed.repository.clone(),
            revision: revision.clone(),
            primary: resolved_primary,
            companions,
            format,
            package,
        })
    }
}

#[derive(Debug, Deserialize)]
struct HfRepository {
    id: String,
    sha: Option<String>,
    #[serde(default)]
    siblings: Vec<HfSibling>,
}

impl HfRepository {
    fn into_catalog(
        self,
        format_filter: Option<ArtifactFormat>,
    ) -> Result<Option<CatalogRepository>> {
        let revision = self.sha.unwrap_or_else(|| "main".to_owned());
        let mut artifacts = Vec::new();
        for sibling in &self.siblings {
            let Some(format) = ArtifactFormat::from_path(Path::new(&sibling.rfilename)) else {
                continue;
            };
            if format_filter.is_some_and(|filter| filter != format) {
                continue;
            }
            let package_manifest =
                nearest_package_manifest(&self.siblings, &sibling.rfilename, format);
            let required_companions = if format == ArtifactFormat::Q27 && package_manifest.is_none()
            {
                let tokenizers = self
                    .siblings
                    .iter()
                    .filter(|file| {
                        file.rfilename.to_ascii_lowercase().ends_with(".tok")
                            && same_remote_directory(&file.rfilename, &sibling.rfilename)
                    })
                    .map(|file| file.rfilename.clone())
                    .collect::<Vec<_>>();
                let Ok(Some(tokenizer)) =
                    select_q27_tokenizer_filename(&sibling.rfilename, &tokenizers)
                else {
                    continue;
                };
                vec![tokenizer]
            } else {
                Vec::new()
            };
            artifacts.push(CatalogFile {
                filename: sibling.rfilename.clone(),
                format,
                model_ref: format!("huggingface:{}@{}/{}", self.id, revision, sibling.rfilename),
                size_bytes: sibling.size(),
                sha256: sibling.sha256(),
                required_companions,
                package_manifest,
                compatibility: CatalogCompatibility::Unverified,
            });
        }
        Ok((!artifacts.is_empty()).then_some(CatalogRepository {
            provider: "huggingface".to_owned(),
            repository: self.id,
            revision,
            artifacts,
        }))
    }
}

#[derive(Debug, Deserialize)]
struct HfSibling {
    rfilename: String,
    size: Option<u64>,
    lfs: Option<HfLfs>,
}

#[derive(Debug, Deserialize)]
struct HfLfs {
    sha256: Option<String>,
    size: Option<u64>,
}

impl HfSibling {
    fn size(&self) -> Option<u64> {
        self.lfs.as_ref().and_then(|lfs| lfs.size).or(self.size)
    }
    fn sha256(&self) -> Option<String> {
        self.lfs.as_ref().and_then(|lfs| lfs.sha256.clone())
    }
    fn resolved(&self, base: &Url, repository: &str, revision: &str) -> Result<ResolvedFile> {
        let mut url = base.clone();
        {
            let mut segments = url.path_segments_mut().map_err(|_| {
                ModelLibraryError::InvalidCatalog(
                    "Hugging Face base URL cannot hold path segments".to_owned(),
                )
            })?;
            for part in repository.split('/') {
                segments.push(part);
            }
            segments.push("resolve").push(revision);
            for part in self.rfilename.split('/') {
                segments.push(part);
            }
        }
        Ok(ResolvedFile {
            filename: self.rfilename.clone(),
            url,
            size_bytes: self.size(),
            sha256: self.sha256(),
        })
    }
}

fn same_remote_directory(left: &str, right: &str) -> bool {
    Path::new(left).parent() == Path::new(right).parent()
}

fn nearest_package_manifest(
    siblings: &[HfSibling],
    artifact_filename: &str,
    format: ArtifactFormat,
) -> Option<String> {
    let manifest_name = norted_package_manifest_name(format);
    let mut directory = Path::new(artifact_filename)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    loop {
        let candidate = slash_path(&directory.join(manifest_name));
        if siblings.iter().any(|file| file.rfilename == candidate) {
            return Some(candidate);
        }
        if directory.as_os_str().is_empty() {
            return None;
        }
        directory = directory.parent().unwrap_or_else(|| Path::new(""));
    }
}

struct HfReference {
    repository: String,
    revision: String,
    filename: String,
}

fn parse_hf_ref(value: &str) -> Result<HfReference> {
    let rest = value.strip_prefix("huggingface:").ok_or_else(|| {
        ModelLibraryError::InvalidReference(
            "expected huggingface:<publisher>/<repository>@<revision>/<filename>".to_owned(),
        )
    })?;
    let (repository, rest) = rest.split_once('@').ok_or_else(|| {
        ModelLibraryError::InvalidReference(
            "Hugging Face reference is missing @revision".to_owned(),
        )
    })?;
    let (revision, filename) = rest.split_once('/').ok_or_else(|| {
        ModelLibraryError::InvalidReference("Hugging Face reference is missing filename".to_owned())
    })?;
    if repository.split('/').count() != 2
        || revision.is_empty()
        || filename.is_empty()
        || !safe_relative(Path::new(filename))
    {
        return Err(ModelLibraryError::InvalidReference(value.to_owned()));
    }
    Ok(HfReference {
        repository: repository.to_owned(),
        revision: revision.to_owned(),
        filename: filename.to_owned(),
    })
}

fn managed_destination(root: &Path, resolved: &ResolvedArtifact) -> Result<PathBuf> {
    let mut repo = resolved.repository.split('/');
    let publisher = safe_segment(repo.next().unwrap_or_default())?;
    let repository = safe_segment(repo.next().unwrap_or_default())?;
    let revision = safe_segment(&resolved.revision)?;
    let identity = resolved
        .package
        .as_ref()
        .map_or(resolved.primary.filename.as_str(), |package| {
            package.manifest_filename.as_str()
        });
    let artifact = &sha256_text(identity)[..16];
    Ok(root
        .join("huggingface")
        .join(publisher)
        .join(repository)
        .join(revision)
        .join(artifact))
}

fn remote_acquisition_id(resolved: &ResolvedArtifact) -> String {
    format!(
        "{}:{}@{}:{}",
        resolved.provider,
        resolved.repository,
        resolved.revision,
        resolved
            .package
            .as_ref()
            .map_or(resolved.primary.filename.as_str(), |package| package
                .manifest_filename
                .as_str())
    )
}

fn safe_segment(value: &str) -> Result<&str> {
    if value.is_empty() || value == "." || value == ".." || value.contains(['/', '\\']) {
        Err(ModelLibraryError::InvalidCatalog(format!(
            "unsafe path segment `{value}`"
        )))
    } else {
        Ok(value)
    }
}

fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.to_string_lossy().contains('\\')
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn safe_remote_path(filename: &str) -> Result<PathBuf> {
    let path = Path::new(filename);
    if !safe_relative(path) {
        return Err(ModelLibraryError::InvalidCatalog(format!(
            "unsafe remote filename `{filename}`"
        )));
    }
    Ok(path.to_path_buf())
}

fn safe_basename(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            ModelLibraryError::InvalidArtifact(format!(
                "invalid artifact filename {}",
                path.display()
            ))
        })
}

fn validate_artifact(path: &Path, format: ArtifactFormat) -> Result<()> {
    match format {
        ArtifactFormat::Gguf => inspect_gguf_metadata(path)
            .map(|_| ())
            .map_err(|error| ModelLibraryError::InvalidArtifact(error.to_string())),
        ArtifactFormat::Q27 => norted_engine_q27::validate_model_artifact(path)
            .map_err(ModelLibraryError::InvalidArtifact),
        ArtifactFormat::Ninfer => inspect_ninfer_container(path)
            .map(|_| ())
            .map_err(|error| ModelLibraryError::InvalidArtifact(error.to_string())),
    }
}

fn validate_raw_q27_tokenizer(primary: &Path) -> Result<PathBuf> {
    let mut warnings = Vec::new();
    let tokenizer = q27_tokenizer_candidate(primary, &mut warnings).ok_or_else(|| {
        ModelLibraryError::InvalidArtifact(warnings.first().cloned().unwrap_or_else(|| {
            format!(
                "q27 artifact {} is missing its required tokenizer companion",
                primary.display()
            )
        }))
    })?;
    let metadata = std::fs::metadata(&tokenizer).map_err(|source| ModelLibraryError::Io {
        path: tokenizer.clone(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(ModelLibraryError::InvalidArtifact(format!(
            "q27 tokenizer companion {} is not a regular file",
            tokenizer.display()
        )));
    }
    validate_q27_tokenizer_header(&tokenizer).map_err(|reason| {
        ModelLibraryError::InvalidArtifact(format!(
            "q27 tokenizer companion {} is invalid: {reason}",
            tokenizer.display()
        ))
    })?;
    Ok(tokenizer)
}

fn package_role_name(role: NortedPackageAcquisitionRole) -> &'static str {
    match role {
        NortedPackageAcquisitionRole::Manifest => "manifest",
        NortedPackageAcquisitionRole::Primary => "primary",
        NortedPackageAcquisitionRole::Tokenizer => "tokenizer",
        NortedPackageAcquisitionRole::Projector => "projector",
        NortedPackageAcquisitionRole::Sharp => "Sharp metadata",
        NortedPackageAcquisitionRole::Other => "auxiliary",
    }
}

async fn local_package_root(source: &Path, format: ArtifactFormat) -> Result<Option<PathBuf>> {
    let manifest_name = norted_package_manifest_name(format);
    let mut directory = source.parent().ok_or_else(|| {
        ModelLibraryError::InvalidArtifact("artifact path has no parent directory".to_owned())
    })?;
    loop {
        let manifest = directory.join(manifest_name);
        if let Ok(metadata) = tokio::fs::metadata(&manifest).await
            && metadata.is_file()
            && metadata.len() > 0
            && metadata.len() <= MAX_PACKAGE_MANIFEST_BYTES
        {
            let bytes =
                tokio::fs::read(&manifest)
                    .await
                    .map_err(|source| ModelLibraryError::Io {
                        path: manifest.clone(),
                        source,
                    })?;
            let selected = source.strip_prefix(directory).map_err(|_| {
                ModelLibraryError::InvalidArtifact(
                    "local package candidate escapes its manifest directory".to_owned(),
                )
            })?;
            match plan_norted_package_acquisition(&bytes, format) {
                Ok(plan) => {
                    if let Some(selected_member) =
                        plan.files.iter().find(|file| file.path == selected)
                    {
                        if selected_member.role != NortedPackageAcquisitionRole::Primary {
                            return Err(ModelLibraryError::InvalidArtifact(format!(
                                "Norted package manifest {} declares the selected artifact as a non-primary {} member; select a primary model artifact instead",
                                manifest.display(),
                                package_role_name(selected_member.role)
                            )));
                        }
                        return directory.canonicalize().map(Some).map_err(|source| {
                            ModelLibraryError::Io {
                                path: directory.to_path_buf(),
                                source,
                            }
                        });
                    }
                }
                Err(error)
                    if recover_norted_package_primary_paths(&bytes, format)
                        .is_some_and(|claims| claims.contains(selected)) =>
                {
                    return Err(ModelLibraryError::InvalidArtifact(format!(
                        "Norted package manifest {} claims the selected artifact but is invalid: {error}",
                        manifest.display()
                    )));
                }
                Err(_) => {}
            }
        }
        let Some(parent) = directory.parent() else {
            break;
        };
        if parent == directory {
            break;
        }
        directory = parent;
    }
    Ok(None)
}

async fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    tokio::fs::copy(source, destination)
        .await
        .map(|_| ())
        .map_err(|source_error| ModelLibraryError::Io {
            path: destination.to_path_buf(),
            source: source_error,
        })
}

async fn create_parent(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        ModelLibraryError::InvalidArtifact(format!(
            "artifact path {} has no parent",
            path.display()
        ))
    })?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|source| ModelLibraryError::Io {
            path: parent.to_path_buf(),
            source,
        })
}

async fn copy_relative_file(source_root: &Path, stage: &Path, relative: &Path) -> Result<()> {
    if !safe_relative(relative) {
        return Err(ModelLibraryError::InvalidArtifact(format!(
            "unsafe package-relative path {}",
            relative.display()
        )));
    }
    let source = source_root
        .join(relative)
        .canonicalize()
        .map_err(|source| ModelLibraryError::Io {
            path: source_root.join(relative),
            source,
        })?;
    if !source.starts_with(source_root) || !source.is_file() {
        return Err(ModelLibraryError::InvalidArtifact(format!(
            "package file {} escapes its source root or is not regular",
            relative.display()
        )));
    }
    let destination = stage.join(relative);
    create_parent(&destination).await?;
    copy_file(&source, &destination).await
}

fn validate_package_stage(
    stage: &Path,
    plan: &NortedPackageAcquisitionPlan,
) -> Result<Vec<ModelArtifact>> {
    let registry = ModelRegistry::discover(&[stage.to_path_buf()]);
    let expected = plan
        .primary_files()
        .map(|file| file.path.clone())
        .collect::<HashSet<_>>();
    let members = registry
        .artifacts()
        .iter()
        .filter(|artifact| artifact.norted_package.is_some())
        .filter_map(|artifact| {
            let relative = artifact.path.strip_prefix(stage).ok()?;
            expected.contains(relative).then_some(artifact.clone())
        })
        .collect::<Vec<_>>();
    if members.len() != expected.len() {
        return Err(ModelLibraryError::InvalidArtifact(
            if registry.warnings().is_empty() {
                "staged Norted package did not expose every declared primary member".to_owned()
            } else {
                registry.warnings().join("; ")
            },
        ));
    }
    Ok(members)
}

async fn verify_package_files(stage: &Path, plan: &NortedPackageAcquisitionPlan) -> Result<()> {
    for file in &plan.files {
        let path = stage.join(&file.path);
        let metadata =
            tokio::fs::metadata(&path)
                .await
                .map_err(|source| ModelLibraryError::Io {
                    path: path.clone(),
                    source,
                })?;
        if !metadata.is_file() {
            return Err(ModelLibraryError::InvalidArtifact(format!(
                "package member {} is not a regular file",
                file.path.display()
            )));
        }
        if file
            .size_bytes
            .is_some_and(|expected| expected != metadata.len())
        {
            return Err(ModelLibraryError::InvalidArtifact(format!(
                "package member {} has {} bytes; expected {}",
                file.path.display(),
                metadata.len(),
                file.size_bytes.expect("checked expected size")
            )));
        }
        if let Some(expected) = &file.sha256 {
            let observed = sha256_file(&path).await?;
            if observed != *expected {
                return Err(ModelLibraryError::DigestMismatch {
                    filename: slash_path(&file.path),
                    expected: expected.clone(),
                    observed,
                });
            }
        }
    }
    Ok(())
}

fn remote_provenance(
    resolved: &ResolvedArtifact,
    file: &ResolvedFile,
    acquisition_id: &str,
    size_bytes: u64,
) -> ModelArtifactProvenance {
    ModelArtifactProvenance {
        acquisition_id: acquisition_id.to_owned(),
        provider: resolved.provider.clone(),
        repository: Some(resolved.repository.clone()),
        logical_id: Some(format!(
            "{}:{}@{}:{}",
            resolved.provider, resolved.repository, resolved.revision, file.filename
        )),
        source: Some(file.url.to_string()),
        revision: Some(resolved.revision.clone()),
        remote_filename: Some(file.filename.clone()),
        acquired_at_unix: unix_timestamp(),
        size_bytes,
        digest: file.sha256.clone(),
    }
}

fn slash_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

async fn file_len(path: &Path) -> Result<u64> {
    tokio::fs::metadata(path)
        .await
        .map(|metadata| metadata.len())
        .map_err(|source| ModelLibraryError::Io {
            path: path.to_path_buf(),
            source,
        })
}

async fn write_receipt(
    acquisition_root: &Path,
    acquisition_id: &str,
    members: Vec<ModelLibraryReceiptMember>,
) -> Result<()> {
    let receipt = ModelLibraryReceipt {
        schema_version: RECEIPT_SCHEMA_VERSION,
        acquisition_id: acquisition_id.to_owned(),
        members,
    };
    let bytes = serde_json::to_vec_pretty(&receipt)
        .map_err(|error| ModelLibraryError::InvalidArtifact(error.to_string()))?;
    let path = model_library_receipt_path(acquisition_root);
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|source| ModelLibraryError::Io { path, source })
}

async fn activate_stage(stage: &Path, destination: &Path) -> Result<()> {
    let parent = destination.parent().ok_or_else(|| {
        ModelLibraryError::InvalidCatalog("managed destination has no parent".to_owned())
    })?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|source| ModelLibraryError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    tokio::fs::rename(stage, destination)
        .await
        .map_err(|source| ModelLibraryError::Io {
            path: destination.to_path_buf(),
            source,
        })
}

fn discover_exact(root: &Path, relative: &Path) -> Result<ModelArtifact> {
    let expected = root
        .join(relative)
        .canonicalize()
        .map_err(|source| ModelLibraryError::Io {
            path: root.join(relative),
            source,
        })?;
    let registry = ModelRegistry::discover(&[root.to_path_buf()]);
    registry
        .artifacts()
        .iter()
        .find(|artifact| artifact.path == expected)
        .cloned()
        .ok_or_else(|| ModelLibraryError::InvalidArtifact(registry.warnings().join("; ")))
}

async fn sha256_file(path: &Path) -> Result<String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|source| ModelLibraryError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|source| ModelLibraryError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn sha256_text(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}
