//! Managed model acquisition without model transformation or runtime ownership.

use std::ops::Deref;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures_util::StreamExt;
use norted_core::{
    AppPaths, ArtifactFormat, ModelArtifact, ModelArtifactProvenance, ModelId, ModelLibraryReceipt,
    ModelRegistry, inspect_gguf_metadata, inspect_ninfer_container, model_library_receipt_path,
    q27_tokenizer_candidate, select_q27_tokenizer_filename,
};
use reqwest::{Client, StatusCode, Url, header};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::broadcast;

const RECEIPT_SCHEMA_VERSION: u32 = 1;

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
    Resolving,
    Downloading,
    Verifying,
    Validating,
    Installing,
    Installed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelOperationProgress {
    pub model_ref: String,
    pub filename: String,
    pub phase: ModelOperationPhase,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub message: String,
}

pub struct ModelLibrary {
    root: PathBuf,
    staging: PathBuf,
    download_cache: PathBuf,
    providers: Vec<Arc<dyn ModelCatalogProvider>>,
    progress: broadcast::Sender<ModelOperationProgress>,
}

impl ModelLibrary {
    pub fn new(paths: &AppPaths) -> Self {
        let (progress, _) = broadcast::channel(256);
        Self {
            root: paths.data_dir.join("models"),
            staging: paths.data_dir.join("models").join(".norted-staging"),
            download_cache: paths.cache_dir.join("model-downloads"),
            providers: vec![Arc::new(HuggingFaceCatalogProvider::new())],
            progress,
        }
    }

    pub fn managed_root(&self) -> &Path {
        &self.root
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ModelOperationProgress> {
        self.progress.subscribe()
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
        self.emit(
            model_ref,
            "",
            ModelOperationPhase::Resolving,
            0,
            None,
            "Resolving exact artifact",
        );
        let provider = self.provider_for(model_ref)?;
        let resolved = provider.resolve(model_ref).await?;
        let result = self.download_resolved(model_ref, &resolved).await;
        if let Err(error) = &result {
            self.emit(
                model_ref,
                &resolved.primary.filename,
                ModelOperationPhase::Failed,
                0,
                None,
                &error.to_string(),
            );
        }
        result
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
        validate_artifact(&source, format)?;
        let source_registry = ModelRegistry::discover(&[source
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()]);
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
        let mut companion = None;
        if format == ArtifactFormat::Q27 {
            let mut warnings = Vec::new();
            companion = q27_tokenizer_candidate(&source, &mut warnings);
            if companion.is_none() {
                let detail = warnings
                    .first()
                    .map_or("required tokenizer companion was not found", String::as_str);
                return Err(ModelLibraryError::InvalidArtifact(detail.to_owned()));
            }
        }
        let filename = safe_basename(&source)?;
        let logical = format!("local-import:{}", sha256_text(&source.to_string_lossy()));
        let destination = self.root.join("imports").join(&logical[13..25]);
        if destination.exists() {
            return Err(ModelLibraryError::AlreadyInstalled(destination));
        }
        let stage = self.create_stage().await?;
        if let Some(package) = &discovered_source.norted_package {
            let mut entries =
                tokio::fs::read_dir(&package.package_root)
                    .await
                    .map_err(|source| ModelLibraryError::Io {
                        path: package.package_root.clone(),
                        source,
                    })?;
            while let Some(entry) =
                entries
                    .next_entry()
                    .await
                    .map_err(|source| ModelLibraryError::Io {
                        path: package.package_root.clone(),
                        source,
                    })?
            {
                if entry
                    .file_type()
                    .await
                    .map_err(|source| ModelLibraryError::Io {
                        path: entry.path(),
                        source,
                    })?
                    .is_file()
                {
                    copy_file(&entry.path(), &stage.join(entry.file_name())).await?;
                }
            }
        } else {
            copy_file(&source, &stage.join(&filename)).await?;
            if let Some(companion) = companion {
                copy_file(&companion, &stage.join(safe_basename(&companion)?)).await?;
            }
        }
        let size_bytes = file_len(&source).await?;
        let provenance = ModelArtifactProvenance {
            provider: "local_import".to_owned(),
            repository: None,
            logical_id: Some(logical),
            source: Some(source.display().to_string()),
            revision: None,
            remote_filename: None,
            acquired_at_unix: unix_timestamp(),
            size_bytes,
            digest: None,
        };
        write_receipt(&stage.join(&filename), provenance).await?;
        let _ = discover_exact(&stage, &filename)?;
        activate_stage(&stage, &destination).await?;
        discover_exact(&destination, &filename)
    }

    pub async fn remove(&self, model: &ModelArtifact) -> Result<()> {
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
        if !path.starts_with(&root) || model.provenance.is_none() {
            return Err(ModelLibraryError::NotManaged(model.id.clone()));
        }
        let artifact_root = path
            .parent()
            .ok_or_else(|| ModelLibraryError::NotManaged(model.id.clone()))?;
        if artifact_root == root || !artifact_root.starts_with(&root) {
            return Err(ModelLibraryError::NotManaged(model.id.clone()));
        }
        tokio::fs::remove_dir_all(artifact_root)
            .await
            .map_err(|source| ModelLibraryError::Io {
                path: artifact_root.to_path_buf(),
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
        model_ref: &str,
        resolved: &ResolvedArtifact,
    ) -> Result<ModelArtifact> {
        tokio::fs::create_dir_all(&self.download_cache)
            .await
            .map_err(|source| ModelLibraryError::Io {
                path: self.download_cache.clone(),
                source,
            })?;
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
            for file in std::iter::once(&resolved.primary).chain(resolved.companions.iter()) {
                let basename = safe_remote_basename(&file.filename)?;
                let destination = stage.join(&basename);
                let bytes = self
                    .download_file(
                        model_ref,
                        file,
                        &destination,
                        completed,
                        total_known.then_some(total),
                    )
                    .await?;
                completed += bytes;
            }
            let primary_name = safe_remote_basename(&resolved.primary.filename)?;
            let primary_path = stage.join(&primary_name);
            self.emit(
                model_ref,
                &resolved.primary.filename,
                ModelOperationPhase::Validating,
                completed,
                total_known.then_some(total),
                "Inspecting downloaded artifact",
            );
            validate_artifact(&primary_path, resolved.format)?;
            if resolved.format == ArtifactFormat::Q27 {
                let mut warnings = Vec::new();
                if q27_tokenizer_candidate(&primary_path, &mut warnings).is_none() {
                    return Err(ModelLibraryError::InvalidArtifact(
                        warnings.first().cloned().unwrap_or_else(|| {
                            "downloaded q27 artifact is missing its tokenizer companion".to_owned()
                        }),
                    ));
                }
            }
            let logical_id = format!(
                "{}:{}/{}@{}:{}",
                resolved.provider,
                resolved.repository.split('/').next().unwrap_or("unknown"),
                resolved.repository.split('/').nth(1).unwrap_or("unknown"),
                resolved.revision,
                resolved.primary.filename
            );
            let provenance = ModelArtifactProvenance {
                provider: resolved.provider.clone(),
                repository: Some(resolved.repository.clone()),
                logical_id: Some(logical_id),
                source: Some(resolved.primary.url.to_string()),
                revision: Some(resolved.revision.clone()),
                remote_filename: Some(resolved.primary.filename.clone()),
                acquired_at_unix: unix_timestamp(),
                size_bytes: file_len(&primary_path).await?,
                digest: resolved.primary.sha256.clone(),
            };
            write_receipt(&primary_path, provenance).await?;
            let _ = discover_exact(&stage, &primary_name)?;
            let destination = managed_destination(&self.root, resolved)?;
            if destination.exists() {
                return Err(ModelLibraryError::AlreadyInstalled(destination));
            }
            self.emit(
                model_ref,
                &resolved.primary.filename,
                ModelOperationPhase::Installing,
                completed,
                total_known.then_some(total),
                "Atomically activating model",
            );
            activate_stage(&stage, &destination).await?;
            let artifact = discover_exact(&destination, &primary_name)?;
            self.emit(
                model_ref,
                &resolved.primary.filename,
                ModelOperationPhase::Installed,
                completed,
                total_known.then_some(total),
                "Model installed",
            );
            Ok(artifact)
        }
        .await
    }

    async fn download_file(
        &self,
        model_ref: &str,
        file: &ResolvedFile,
        destination: &Path,
        completed_before: u64,
        total: Option<u64>,
    ) -> Result<u64> {
        let partial = self
            .download_cache
            .join(format!("{}.part", sha256_text(file.url.as_str())));
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
            tokio::fs::rename(&partial, destination)
                .await
                .map_err(|source| ModelLibraryError::Io {
                    path: destination.to_path_buf(),
                    source,
                })?;
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
                model_ref,
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
            model_ref,
            &file.filename,
            ModelOperationPhase::Verifying,
            completed_before + downloaded,
            total,
            "Verifying downloaded data",
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
        tokio::fs::rename(&partial, destination)
            .await
            .map_err(|source| ModelLibraryError::Io {
                path: destination.to_path_buf(),
                source,
            })?;
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
        model_ref: &str,
        filename: &str,
        phase: ModelOperationPhase,
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
        message: &str,
    ) {
        let _ = self.progress.send(ModelOperationProgress {
            model_ref: model_ref.to_owned(),
            filename: filename.to_owned(),
            phase,
            downloaded_bytes,
            total_bytes,
            message: message.to_owned(),
        });
    }
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

    async fn package_companions(
        &self,
        repository: &HfRepository,
        repository_id: &str,
        revision: &str,
        primary_filename: &str,
        format: ArtifactFormat,
    ) -> Result<Vec<ResolvedFile>> {
        let manifest_name = match format {
            ArtifactFormat::Q27 => "q27-manifest.json",
            ArtifactFormat::Ninfer => "ninfer-manifest.json",
            ArtifactFormat::Gguf => return Ok(Vec::new()),
        };
        let Some(manifest) = repository.siblings.iter().find(|file| {
            file.rfilename
                .rsplit('/')
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case(manifest_name))
        }) else {
            return Ok(Vec::new());
        };
        let resolved_manifest = manifest.resolved(&self.api_base, repository_id, revision)?;
        let document: serde_json::Value = self
            .client
            .get(resolved_manifest.url.clone())
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let mut filenames = Vec::new();
        collect_manifest_filenames(&document, &mut filenames);
        if !filenames
            .iter()
            .any(|filename| filename == primary_filename)
        {
            return Ok(Vec::new());
        }
        let mut companions = vec![resolved_manifest];
        for filename in filenames {
            if filename == primary_filename
                || companions.iter().any(|file| file.filename == filename)
            {
                continue;
            }
            let sibling = repository
                .siblings
                .iter()
                .find(|file| file.rfilename == filename)
                .ok_or_else(|| {
                    ModelLibraryError::InvalidCatalog(format!(
                        "Norted package manifest references missing file `{filename}`"
                    ))
                })?;
            companions.push(sibling.resolved(&self.api_base, repository_id, revision)?);
        }
        Ok(companions)
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
        let mut companions = if format == ArtifactFormat::Q27 {
            let tokenizer_names = repository
                .siblings
                .iter()
                .filter(|file| file.rfilename.to_ascii_lowercase().ends_with(".tok"))
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
            vec![
                repository
                    .siblings
                    .iter()
                    .find(|file| file.rfilename == selected)
                    .expect("selected sibling exists")
                    .resolved(&self.api_base, &parsed.repository, &revision)?,
            ]
        } else {
            Vec::new()
        };
        for companion in self
            .package_companions(
                &repository,
                &parsed.repository,
                &revision,
                &parsed.filename,
                format,
            )
            .await?
        {
            if !companions
                .iter()
                .any(|existing| existing.filename == companion.filename)
            {
                companions.push(companion);
            }
        }
        Ok(ResolvedArtifact {
            provider: self.id().to_owned(),
            repository: parsed.repository.clone(),
            revision: revision.clone(),
            primary: primary.resolved(&self.api_base, &parsed.repository, &revision)?,
            companions,
            format,
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
        let tokenizers = self
            .siblings
            .iter()
            .filter(|file| file.rfilename.to_ascii_lowercase().ends_with(".tok"))
            .map(|file| file.rfilename.clone())
            .collect::<Vec<_>>();
        let mut artifacts = Vec::new();
        for sibling in &self.siblings {
            let Some(format) = ArtifactFormat::from_path(Path::new(&sibling.rfilename)) else {
                continue;
            };
            if format_filter.is_some_and(|filter| filter != format) {
                continue;
            }
            let required_companions = if format == ArtifactFormat::Q27 {
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

fn collect_manifest_filenames(value: &serde_json::Value, output: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                if key == "filename"
                    && let Some(filename) = value.as_str()
                {
                    output.push(filename.to_owned());
                }
                collect_manifest_filenames(value, output);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_manifest_filenames(value, output);
            }
        }
        _ => {}
    }
}

fn managed_destination(root: &Path, resolved: &ResolvedArtifact) -> Result<PathBuf> {
    let mut repo = resolved.repository.split('/');
    let publisher = safe_segment(repo.next().unwrap_or_default())?;
    let repository = safe_segment(repo.next().unwrap_or_default())?;
    let revision = safe_segment(&resolved.revision)?;
    let artifact = &sha256_text(&resolved.primary.filename)[..16];
    Ok(root
        .join("huggingface")
        .join(publisher)
        .join(repository)
        .join(revision)
        .join(artifact))
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
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn safe_remote_basename(filename: &str) -> Result<String> {
    if !safe_relative(Path::new(filename)) {
        return Err(ModelLibraryError::InvalidCatalog(format!(
            "unsafe remote filename `{filename}`"
        )));
    }
    Path::new(filename)
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            ModelLibraryError::InvalidCatalog(format!("invalid remote filename `{filename}`"))
        })
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

async fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    tokio::fs::copy(source, destination)
        .await
        .map(|_| ())
        .map_err(|source_error| ModelLibraryError::Io {
            path: destination.to_path_buf(),
            source: source_error,
        })
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

async fn write_receipt(primary: &Path, provenance: ModelArtifactProvenance) -> Result<()> {
    let receipt = ModelLibraryReceipt {
        schema_version: RECEIPT_SCHEMA_VERSION,
        primary_filename: safe_basename(primary)?,
        provenance,
    };
    let bytes = serde_json::to_vec_pretty(&receipt)
        .map_err(|error| ModelLibraryError::InvalidArtifact(error.to_string()))?;
    let path = model_library_receipt_path(primary);
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

fn discover_exact(root: &Path, filename: &str) -> Result<ModelArtifact> {
    let registry = ModelRegistry::discover(&[root.to_path_buf()]);
    registry
        .artifacts()
        .iter()
        .find(|artifact| {
            artifact.path.file_name().and_then(|value| value.to_str()) == Some(filename)
        })
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
