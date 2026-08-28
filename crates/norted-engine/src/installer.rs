use std::collections::BTreeMap;
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::read::GzDecoder;
use futures_util::StreamExt;
use norted_core::{
    AvailableRuntime, InstalledRuntime, RUNTIME_MANIFEST_SCHEMA_VERSION, RuntimeAcquisitionMethod,
    RuntimeArchiveFormat, RuntimeManifest, RuntimeOperationPhase, RuntimeOperationProgress,
    RuntimeProbeObservation, is_safe_relative_path,
};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::broadcast;

use crate::catalog::{
    GitHubReleaseAsset, GitHubReleaseClient, RuntimeProviderAuthority, is_allowed_github_host,
};
use crate::store::{RUNTIME_MANIFEST_FILE, RuntimeStore, RuntimeStoreError};
use crate::{EngineError, EngineRegistry};

const MAX_ARCHIVE_ENTRIES: usize = 100_000;
const MAX_EXTRACTED_BYTES: u64 = 32 * 1024 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum RuntimeInstallError {
    #[error("runtime metadata is invalid: {0}")]
    InvalidMetadata(String),
    #[error(
        "runtime `{0}` has no trustworthy SHA-256 package digest; managed installation is refused"
    )]
    MissingDigest(String),
    #[error("runtime download URL is not an approved GitHub release host: {0}")]
    UntrustedUrl(String),
    #[error("runtime download failed: {0}")]
    Download(String),
    #[error("runtime release provenance changed or could not be verified: {0}")]
    SourceChanged(String),
    #[error("runtime package checksum mismatch: expected {expected}, observed {observed}")]
    ChecksumMismatch { expected: String, observed: String },
    #[error("runtime archive is unsafe or invalid: {0}")]
    UnsafeArchive(String),
    #[error("runtime archive did not contain exactly one expected entrypoint: {0}")]
    Entrypoint(String),
    #[error("runtime engine adapter is unavailable: {0}")]
    Adapter(String),
    #[error("runtime probe failed: {0}")]
    Probe(#[from] EngineError),
    #[error(transparent)]
    Store(#[from] RuntimeStoreError),
    #[error("runtime installation task failed: {0}")]
    Task(String),
}

#[derive(Clone)]
pub struct RuntimeInstaller {
    store: Arc<RuntimeStore>,
    registry: EngineRegistry,
    cache_root: PathBuf,
    github: GitHubReleaseClient,
    authorities: BTreeMap<String, RuntimeProviderAuthority>,
    progress: broadcast::Sender<RuntimeOperationProgress>,
}

impl RuntimeInstaller {
    pub fn new(
        store: Arc<RuntimeStore>,
        registry: EngineRegistry,
        cache_root: PathBuf,
        authorities: impl IntoIterator<Item = RuntimeProviderAuthority>,
    ) -> Result<Self, RuntimeInstallError> {
        let github = GitHubReleaseClient::new()
            .map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
        let authorities = authorities
            .into_iter()
            .map(|authority| (authority.provider_id.clone(), authority))
            .collect();
        let (progress, _) = broadcast::channel(64);
        Ok(Self {
            store,
            registry,
            cache_root,
            github,
            authorities,
            progress,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<RuntimeOperationProgress> {
        self.progress.subscribe()
    }

    pub async fn install(
        &self,
        available: &AvailableRuntime,
    ) -> Result<InstalledRuntime, RuntimeInstallError> {
        available
            .validate()
            .map_err(|error| RuntimeInstallError::InvalidMetadata(error.to_string()))?;
        let authority = self
            .authorities
            .get(&available.identity.package.provider_id)
            .ok_or_else(|| {
                RuntimeInstallError::InvalidMetadata(format!(
                    "runtime provider `{}` is not registered as an installation authority",
                    available.identity.package.provider_id
                ))
            })?;
        authority
            .validate(available)
            .map_err(|error| RuntimeInstallError::InvalidMetadata(error.to_string()))?;
        let repository = authority.repository.clone();
        let expected =
            available.download.digest.as_ref().ok_or_else(|| {
                RuntimeInstallError::MissingDigest(available.runtime_id.to_string())
            })?;
        if !expected.algorithm.eq_ignore_ascii_case("sha256") {
            return Err(RuntimeInstallError::MissingDigest(
                available.runtime_id.to_string(),
            ));
        }
        validate_release_source(available)?;
        self.verify_published_assets(available).await?;
        let primary_asset_name = available
            .identity
            .package
            .asset_name
            .as_deref()
            .ok_or_else(|| {
                RuntimeInstallError::InvalidMetadata("primary asset name is missing".to_owned())
            })?;
        validate_release_asset_url(available, primary_asset_name, &available.download)?;
        let primary_asset_id = parse_asset_id(
            available.identity.package.asset_id.as_deref(),
            primary_asset_name,
        )?;
        let adapter = self
            .registry
            .get(&available.identity.engine_id)
            .ok_or_else(|| RuntimeInstallError::Adapter(available.identity.engine_id.clone()))?;
        let archive = self
            .download(
                available,
                &available.download,
                &repository,
                primary_asset_id,
                &expected.value,
                available.download_size_bytes(),
            )
            .await?;
        let mut additional_archives = Vec::new();
        let mut additional_digests = Vec::new();
        for (asset, download) in available
            .identity
            .package
            .additional_assets
            .iter()
            .zip(&available.additional_downloads)
        {
            let digest = download.digest.as_ref().ok_or_else(|| {
                RuntimeInstallError::MissingDigest(available.runtime_id.to_string())
            })?;
            if !digest.algorithm.eq_ignore_ascii_case("sha256") {
                return Err(RuntimeInstallError::MissingDigest(
                    available.runtime_id.to_string(),
                ));
            }
            validate_release_asset_url(available, &asset.asset_name, download)?;
            let asset_id = parse_asset_id(Some(&asset.asset_id), &asset.asset_name)?;
            additional_archives.push(
                self.download(
                    available,
                    download,
                    &repository,
                    asset_id,
                    &digest.value,
                    available.download_size_bytes(),
                )
                .await?,
            );
            additional_digests.push(digest.value.clone());
        }
        self.emit(
            available,
            RuntimeOperationPhase::Extracting,
            None,
            Some(available.download_size_bytes()),
            "Extracting verified package",
        );
        let staging = self.store.create_staging().await?;
        let result = async {
            let archive_path = archive.clone();
            let staging_path = staging.clone();
            let format = available.download.archive_format;
            tokio::task::spawn_blocking(move || {
                extract_archive(&archive_path, &staging_path, format)
            })
            .await
            .map_err(|error| RuntimeInstallError::Task(error.to_string()))??;
            for (download, archive) in available
                .additional_downloads
                .iter()
                .zip(&additional_archives)
            {
                let archive_path = archive.clone();
                let staging_path = staging.clone();
                let format = download.archive_format;
                tokio::task::spawn_blocking(move || {
                    extract_archive(&archive_path, &staging_path, format)
                })
                .await
                .map_err(|error| RuntimeInstallError::Task(error.to_string()))??;
            }
            let entrypoint = locate_entrypoint(&staging, &available.download.entrypoint_names)
                .map_err(RuntimeInstallError::Entrypoint)?;
            let relative_entrypoint = entrypoint
                .strip_prefix(&staging)
                .map_err(|_| {
                    RuntimeInstallError::Entrypoint(
                        "entrypoint escaped the staging directory".to_owned(),
                    )
                })?
                .to_path_buf();
            if !is_safe_relative_path(&relative_entrypoint) {
                return Err(RuntimeInstallError::Entrypoint(
                    "entrypoint is not a contained relative path".to_owned(),
                ));
            }
            let entrypoint_sha256 = hash_file(&entrypoint)
                .await
                .map_err(|error| RuntimeInstallError::Entrypoint(error.to_string()))?;
            let mut manifest = RuntimeManifest {
                schema_version: RUNTIME_MANIFEST_SCHEMA_VERSION,
                runtime_id: available.runtime_id.clone(),
                identity: available.identity.clone(),
                supported_formats: available.supported_formats.clone(),
                requirements: available.requirements.clone(),
                acquisition_method: RuntimeAcquisitionMethod::OfficialReleaseAsset,
                source_url: Some(available.source_url.clone()),
                downloaded_archive_sha256: Some(expected.value.clone()),
                additional_downloaded_archive_sha256: additional_digests.clone(),
                entrypoint: relative_entrypoint,
                entrypoint_sha256,
                installed_at_unix: Some(unix_timestamp()),
                probe: RuntimeProbeObservation {
                    compatible: false,
                    observed_engine_id: available.identity.engine_id.clone(),
                    observed_version: None,
                    observed_revision: None,
                    detail: "probe pending".to_owned(),
                    observed_at_unix: unix_timestamp(),
                },
            };
            let candidate = InstalledRuntime {
                manifest: manifest.clone(),
                installation_root: staging.clone(),
            };
            self.emit(
                available,
                RuntimeOperationPhase::Probing,
                None,
                None,
                "Validating executable through the engine adapter",
            );
            let observation = adapter.probe_runtime(&candidate).await?;
            if !observation.compatible
                || observation.observed_engine_id != available.identity.engine_id
            {
                return Err(RuntimeInstallError::Probe(
                    EngineError::InvalidConfiguration(format!(
                        "runtime probe reported engine `{}` with compatible={}",
                        observation.observed_engine_id, observation.compatible
                    )),
                ));
            }
            let post_probe_sha256 = hash_file(&entrypoint)
                .await
                .map_err(|error| RuntimeInstallError::Entrypoint(error.to_string()))?;
            if post_probe_sha256 != manifest.entrypoint_sha256 {
                return Err(RuntimeInstallError::Probe(
                    EngineError::InvalidConfiguration(
                        "runtime entrypoint changed while its adapter probe was executing"
                            .to_owned(),
                    ),
                ));
            }
            manifest.probe = observation;
            manifest
                .validate()
                .map_err(|error| RuntimeInstallError::InvalidMetadata(error.to_string()))?;
            write_manifest(&staging.join(RUNTIME_MANIFEST_FILE), &manifest).await?;
            self.emit(
                available,
                RuntimeOperationPhase::Installing,
                None,
                None,
                "Atomically activating immutable runtime",
            );
            self.store
                .activate(&staging, &manifest)
                .await
                .map_err(Into::into)
        }
        .await;
        match result {
            Ok(runtime) => {
                if tokio::fs::try_exists(&staging).await.unwrap_or(false) {
                    let _ = tokio::fs::remove_dir_all(&staging).await;
                }
                self.emit(
                    available,
                    RuntimeOperationPhase::Installed,
                    Some(available.download_size_bytes()),
                    Some(available.download_size_bytes()),
                    "Runtime installed",
                );
                Ok(runtime)
            }
            Err(error) => {
                if tokio::fs::try_exists(&staging).await.unwrap_or(false) {
                    let _ = tokio::fs::remove_dir_all(&staging).await;
                }
                self.emit(
                    available,
                    RuntimeOperationPhase::Failed,
                    None,
                    None,
                    &error.to_string(),
                );
                Err(error)
            }
        }
    }

    async fn verify_published_assets(
        &self,
        available: &AvailableRuntime,
    ) -> Result<(), RuntimeInstallError> {
        let package = &available.identity.package;
        let repository = package.repository.as_deref().ok_or_else(|| {
            RuntimeInstallError::InvalidMetadata(
                "official repository identity is missing".to_owned(),
            )
        })?;
        let release_tag = package.release_tag.as_deref().ok_or_else(|| {
            RuntimeInstallError::InvalidMetadata("official release tag is missing".to_owned())
        })?;
        let release = self
            .github
            .release_by_tag(repository, release_tag)
            .await
            .map_err(|error| RuntimeInstallError::SourceChanged(error.to_string()))?
            .ok_or_else(|| {
                RuntimeInstallError::SourceChanged(format!(
                    "release `{repository}@{release_tag}` is no longer published"
                ))
            })?;
        if release.draft
            || release.tag_name != release_tag
            || release.html_url != available.source_url
            || available
                .identity
                .upstream_revision
                .as_deref()
                .is_some_and(|revision| revision != release.target_commitish)
        {
            return Err(RuntimeInstallError::SourceChanged(format!(
                "release `{repository}@{release_tag}` no longer matches the catalog identity"
            )));
        }

        let primary_name = package.asset_name.as_deref().ok_or_else(|| {
            RuntimeInstallError::InvalidMetadata("primary asset name is missing".to_owned())
        })?;
        verify_release_asset(
            &release.assets,
            package.asset_id.as_deref(),
            primary_name,
            &available.download,
        )?;
        for (asset, download) in package
            .additional_assets
            .iter()
            .zip(&available.additional_downloads)
        {
            verify_release_asset(
                &release.assets,
                Some(&asset.asset_id),
                &asset.asset_name,
                download,
            )?;
        }
        Ok(())
    }

    async fn download(
        &self,
        available: &AvailableRuntime,
        download: &norted_core::RuntimeDownload,
        repository: &str,
        asset_id: u64,
        expected: &str,
        total_package_size: u64,
    ) -> Result<PathBuf, RuntimeInstallError> {
        let downloads = self.cache_root.join("downloads");
        tokio::fs::create_dir_all(&downloads)
            .await
            .map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
        let cached = downloads.join(format!(
            "{}.{}",
            expected,
            download.archive_format.extension()
        ));
        if tokio::fs::try_exists(&cached)
            .await
            .map_err(|error| RuntimeInstallError::Download(error.to_string()))?
        {
            let cached_size = tokio::fs::metadata(&cached)
                .await
                .map_err(|error| RuntimeInstallError::Download(error.to_string()))?
                .len();
            if cached_size != download.size_bytes {
                tokio::fs::remove_file(&cached)
                    .await
                    .map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
            } else {
                self.emit(
                    available,
                    RuntimeOperationPhase::Verifying,
                    None,
                    Some(total_package_size),
                    "Verifying cached package",
                );
                let observed = hash_file(&cached)
                    .await
                    .map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
                if observed == expected {
                    return Ok(cached);
                }
                tokio::fs::remove_file(&cached)
                    .await
                    .map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
            }
        }
        let temporary = downloads.join(format!("{}.part", uuid::Uuid::new_v4()));
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .await
            .map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
        let result = async {
            let response = self
                .github
                .release_asset_request(repository, asset_id)
                .map_err(|error| RuntimeInstallError::Download(error.to_string()))?
                .send()
                .await
                .map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
            if !response.status().is_success() {
                return Err(RuntimeInstallError::Download(format!(
                    "GitHub asset returned HTTP {}",
                    response.status()
                )));
            }
            let total = response.content_length().or(Some(download.size_bytes));
            let mut stream = response.bytes_stream();
            let mut hash = Sha256::new();
            let mut completed = 0_u64;
            while let Some(chunk) = stream.next().await {
                let chunk =
                    chunk.map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
                let next_completed =
                    completed.checked_add(chunk.len() as u64).ok_or_else(|| {
                        RuntimeInstallError::Download("downloaded byte count overflowed".to_owned())
                    })?;
                if next_completed > download.size_bytes {
                    return Err(RuntimeInstallError::Download(format!(
                        "GitHub asset exceeded its advertised size of {} bytes",
                        download.size_bytes
                    )));
                }
                file.write_all(&chunk)
                    .await
                    .map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
                hash.update(&chunk);
                completed = next_completed;
                self.emit(
                    available,
                    RuntimeOperationPhase::Downloading,
                    Some(completed),
                    total,
                    "Downloading official release asset",
                );
            }
            if completed != download.size_bytes {
                return Err(RuntimeInstallError::Download(format!(
                    "GitHub asset size mismatch: expected {} bytes, received {completed}",
                    download.size_bytes
                )));
            }
            file.flush()
                .await
                .map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
            file.sync_all()
                .await
                .map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
            self.emit(
                available,
                RuntimeOperationPhase::Verifying,
                Some(completed),
                total,
                "Verifying package SHA-256",
            );
            let observed = hex_digest(hash.finalize());
            verify_package_digest(expected, &observed)?;
            drop(file);
            match tokio::fs::rename(&temporary, &cached).await {
                Ok(()) => Ok(cached.clone()),
                Err(_) if tokio::fs::try_exists(&cached).await.unwrap_or(false) => {
                    let cached_hash = hash_file(&cached)
                        .await
                        .map_err(|error| RuntimeInstallError::Download(error.to_string()))?;
                    if cached_hash == expected {
                        Ok(cached.clone())
                    } else {
                        Err(RuntimeInstallError::ChecksumMismatch {
                            expected: expected.to_owned(),
                            observed: cached_hash,
                        })
                    }
                }
                Err(error) => Err(RuntimeInstallError::Download(error.to_string())),
            }
        }
        .await;
        if result.is_err() && tokio::fs::try_exists(&temporary).await.unwrap_or(false) {
            let _ = tokio::fs::remove_file(&temporary).await;
        }
        result
    }

    fn emit(
        &self,
        available: &AvailableRuntime,
        phase: RuntimeOperationPhase,
        bytes_completed: Option<u64>,
        bytes_total: Option<u64>,
        detail: &str,
    ) {
        let _ = self.progress.send(RuntimeOperationProgress {
            runtime_id: available.runtime_id.clone(),
            phase,
            bytes_completed,
            bytes_total,
            detail: detail.to_owned(),
        });
    }
}

fn extract_archive(
    archive_path: &Path,
    staging: &Path,
    format: RuntimeArchiveFormat,
) -> Result<(), RuntimeInstallError> {
    match format {
        RuntimeArchiveFormat::Zip => extract_zip(archive_path, staging),
        RuntimeArchiveFormat::TarGz => extract_tar_gz(archive_path, staging),
    }?;
    validate_staging_limits(staging)
}

fn validate_staging_limits(staging: &Path) -> Result<(), RuntimeInstallError> {
    let mut entries = 0_usize;
    let mut bytes = 0_u64;
    for entry in walkdir::WalkDir::new(staging).follow_links(false) {
        let entry = entry.map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        if entry.path() == staging {
            continue;
        }
        entries = entries.checked_add(1).ok_or_else(|| {
            RuntimeInstallError::UnsafeArchive("extracted entry count overflow".to_owned())
        })?;
        if entries > MAX_ARCHIVE_ENTRIES {
            return Err(RuntimeInstallError::UnsafeArchive(format!(
                "combined package contains more than {MAX_ARCHIVE_ENTRIES} entries"
            )));
        }
        if entry.file_type().is_file() {
            bytes = bytes
                .checked_add(
                    entry
                        .metadata()
                        .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?
                        .len(),
                )
                .ok_or_else(|| {
                    RuntimeInstallError::UnsafeArchive("extracted size overflow".to_owned())
                })?;
            if bytes > MAX_EXTRACTED_BYTES {
                return Err(RuntimeInstallError::UnsafeArchive(format!(
                    "combined package expands beyond {MAX_EXTRACTED_BYTES} bytes"
                )));
            }
        }
    }
    Ok(())
}

fn validate_release_source(available: &AvailableRuntime) -> Result<(), RuntimeInstallError> {
    let package = &available.identity.package;
    let repository = package.repository.as_deref().ok_or_else(|| {
        RuntimeInstallError::InvalidMetadata("official repository identity is missing".to_owned())
    })?;
    let release_tag = package.release_tag.as_deref().ok_or_else(|| {
        RuntimeInstallError::InvalidMetadata("official release tag is missing".to_owned())
    })?;
    let expected = github_url(&[repository, "releases", "tag", release_tag])?;
    let observed = available
        .source_url
        .parse::<reqwest::Url>()
        .map_err(|error| RuntimeInstallError::InvalidMetadata(error.to_string()))?;
    if observed != expected {
        return Err(RuntimeInstallError::UntrustedUrl(observed.to_string()));
    }
    Ok(())
}

fn parse_asset_id(asset_id: Option<&str>, asset_name: &str) -> Result<u64, RuntimeInstallError> {
    asset_id
        .and_then(|asset_id| asset_id.parse::<u64>().ok())
        .filter(|asset_id| *asset_id != 0)
        .ok_or_else(|| {
            RuntimeInstallError::InvalidMetadata(format!(
                "release asset `{asset_name}` has an invalid immutable GitHub asset ID"
            ))
        })
}

fn verify_release_asset(
    live_assets: &[GitHubReleaseAsset],
    expected_id: Option<&str>,
    expected_name: &str,
    expected_download: &norted_core::RuntimeDownload,
) -> Result<(), RuntimeInstallError> {
    let expected_id = parse_asset_id(expected_id, expected_name)?;
    let live = live_assets
        .iter()
        .find(|asset| asset.id == expected_id)
        .ok_or_else(|| {
            RuntimeInstallError::SourceChanged(format!(
                "release asset `{expected_name}` no longer has GitHub asset ID {expected_id}"
            ))
        })?;
    let live_digest = live
        .digest
        .as_deref()
        .and_then(|digest| norted_core::RuntimeDigest::parse_github(digest).ok());
    if live.state != "uploaded"
        || live.name != expected_name
        || live.size != expected_download.size_bytes
        || live.browser_download_url != expected_download.url
        || live_digest.as_ref() != expected_download.digest.as_ref()
    {
        return Err(RuntimeInstallError::SourceChanged(format!(
            "release asset `{expected_name}` metadata changed after catalog discovery"
        )));
    }
    Ok(())
}

fn validate_release_asset_url(
    available: &AvailableRuntime,
    asset_name: &str,
    download: &norted_core::RuntimeDownload,
) -> Result<reqwest::Url, RuntimeInstallError> {
    let package = &available.identity.package;
    let repository = package.repository.as_deref().ok_or_else(|| {
        RuntimeInstallError::InvalidMetadata("official repository identity is missing".to_owned())
    })?;
    let release_tag = package.release_tag.as_deref().ok_or_else(|| {
        RuntimeInstallError::InvalidMetadata("official release tag is missing".to_owned())
    })?;
    let expected = github_url(&[repository, "releases", "download", release_tag, asset_name])?;
    let observed = download
        .url
        .parse::<reqwest::Url>()
        .map_err(|error| RuntimeInstallError::InvalidMetadata(error.to_string()))?;
    if !is_allowed_github_host(&observed) || observed != expected {
        return Err(RuntimeInstallError::UntrustedUrl(observed.to_string()));
    }
    Ok(observed)
}

fn github_url(path: &[&str]) -> Result<reqwest::Url, RuntimeInstallError> {
    let Some((owner, repository)) = path[0].split_once('/') else {
        return Err(RuntimeInstallError::InvalidMetadata(
            "official repository must be an owner/name pair".to_owned(),
        ));
    };
    if owner.is_empty()
        || repository.is_empty()
        || repository.contains('/')
        || !path.iter().skip(1).all(|segment| !segment.is_empty())
    {
        return Err(RuntimeInstallError::InvalidMetadata(
            "official release path contains an empty or invalid component".to_owned(),
        ));
    }
    let mut url = reqwest::Url::parse("https://github.com")
        .expect("the built-in GitHub origin is a valid URL");
    url.path_segments_mut()
        .expect("the built-in GitHub origin supports path segments")
        .push(owner)
        .push(repository)
        .extend(path.iter().skip(1).copied());
    Ok(url)
}

fn extract_zip(archive_path: &Path, staging: &Path) -> Result<(), RuntimeInstallError> {
    let file = std::fs::File::open(archive_path)
        .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(RuntimeInstallError::UnsafeArchive(format!(
            "archive contains more than {MAX_ARCHIVE_ENTRIES} entries"
        )));
    }
    let mut seen = HashSet::new();
    let mut extracted = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        let name = entry.name().to_owned();
        let relative = checked_archive_path(&name)?;
        if !seen.insert(normalized_archive_key(&relative)) {
            return Err(RuntimeInstallError::UnsafeArchive(format!(
                "archive contains duplicate path `{name}`"
            )));
        }
        if let Some(mode) = entry.unix_mode() {
            let file_type = mode & 0o170000;
            if file_type == 0o120000 {
                return Err(RuntimeInstallError::UnsafeArchive(format!(
                    "ZIP symlink entry is not allowed: `{name}`"
                )));
            }
            if file_type != 0 && file_type != 0o100000 && file_type != 0o040000 {
                return Err(RuntimeInstallError::UnsafeArchive(format!(
                    "ZIP contains a special filesystem entry: `{name}`"
                )));
            }
        }
        let destination = staging.join(&relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&destination)
                .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
            continue;
        }
        extracted = extracted.checked_add(entry.size()).ok_or_else(|| {
            RuntimeInstallError::UnsafeArchive("extracted size overflow".to_owned())
        })?;
        if extracted > MAX_EXTRACTED_BYTES {
            return Err(RuntimeInstallError::UnsafeArchive(format!(
                "archive expands beyond {MAX_EXTRACTED_BYTES} bytes"
            )));
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        std::io::copy(&mut entry, &mut output)
            .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        output
            .sync_all()
            .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        set_unix_mode(&destination, entry.unix_mode())?;
    }
    Ok(())
}

fn extract_tar_gz(archive_path: &Path, staging: &Path) -> Result<(), RuntimeInstallError> {
    let file = std::fs::File::open(archive_path)
        .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    let mut seen = HashSet::new();
    let mut count = 0_usize;
    let mut extracted = 0_u64;
    let mut symlinks = Vec::new();
    let mut hardlinks = Vec::new();
    let entries = archive
        .entries()
        .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
    for entry in entries {
        count += 1;
        if count > MAX_ARCHIVE_ENTRIES {
            return Err(RuntimeInstallError::UnsafeArchive(format!(
                "archive contains more than {MAX_ARCHIVE_ENTRIES} entries"
            )));
        }
        let mut entry =
            entry.map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        let path = entry
            .path()
            .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        let path_text = path.to_string_lossy();
        let relative = checked_archive_path(&path_text)?;
        if !seen.insert(normalized_archive_key(&relative)) {
            return Err(RuntimeInstallError::UnsafeArchive(format!(
                "archive contains duplicate path `{}`",
                path.display()
            )));
        }
        let entry_type = entry.header().entry_type();
        let destination = staging.join(&relative);
        if entry_type.is_dir() {
            std::fs::create_dir_all(&destination)
                .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
            continue;
        }
        if entry_type.is_symlink() {
            let target = entry
                .link_name()
                .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?
                .ok_or_else(|| {
                    RuntimeInstallError::UnsafeArchive(format!(
                        "symlink `{}` has no target",
                        path.display()
                    ))
                })?;
            validate_relative_link_target(&relative, &target)?;
            symlinks.push((destination, target.into_owned()));
            continue;
        }
        if entry_type.is_hard_link() {
            let target = entry
                .link_name()
                .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?
                .ok_or_else(|| {
                    RuntimeInstallError::UnsafeArchive(format!(
                        "hardlink `{}` has no target",
                        path.display()
                    ))
                })?;
            let target = checked_archive_path(&target.to_string_lossy())?;
            hardlinks.push((destination, staging.join(target)));
            continue;
        }
        if !entry_type.is_file() {
            return Err(RuntimeInstallError::UnsafeArchive(format!(
                "archive contains an unsupported entry: `{}`",
                path.display()
            )));
        }
        let size = entry
            .header()
            .size()
            .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        extracted = extracted.checked_add(size).ok_or_else(|| {
            RuntimeInstallError::UnsafeArchive("extracted size overflow".to_owned())
        })?;
        if extracted > MAX_EXTRACTED_BYTES {
            return Err(RuntimeInstallError::UnsafeArchive(format!(
                "archive expands beyond {MAX_EXTRACTED_BYTES} bytes"
            )));
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        std::io::copy(&mut entry, &mut output)
            .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        output
            .sync_all()
            .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        let mode = entry.header().mode().ok();
        set_unix_mode(&destination, mode)?;
    }
    for (destination, target) in hardlinks {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        }
        if !target.is_file() {
            return Err(RuntimeInstallError::UnsafeArchive(format!(
                "hardlink target is missing or not a regular file: {}",
                target.display()
            )));
        }
        std::fs::hard_link(&target, &destination)
            .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
    }
    for (destination, target) in &symlinks {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
        }
        create_symlink(target, destination)?;
    }
    let canonical_staging = std::fs::canonicalize(staging)
        .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
    for (destination, _) in symlinks {
        let resolved = std::fs::canonicalize(&destination).map_err(|error| {
            RuntimeInstallError::UnsafeArchive(format!(
                "archive symlink is broken or cyclic at {}: {error}",
                destination.display()
            ))
        })?;
        if !resolved.starts_with(&canonical_staging) {
            return Err(RuntimeInstallError::UnsafeArchive(format!(
                "archive symlink escapes staging: {}",
                destination.display()
            )));
        }
    }
    Ok(())
}

fn checked_archive_path(value: &str) -> Result<PathBuf, RuntimeInstallError> {
    if value.is_empty()
        || value.contains('\\')
        || value.contains(':')
        || value.starts_with('/')
        || value.starts_with('~')
    {
        return Err(RuntimeInstallError::UnsafeArchive(format!(
            "unsafe archive path `{value}`"
        )));
    }
    let path = PathBuf::from(value);
    if !is_safe_relative_path(&path)
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(RuntimeInstallError::UnsafeArchive(format!(
            "unsafe archive path `{value}`"
        )));
    }
    Ok(path)
}

fn validate_relative_link_target(
    link_path: &Path,
    target: &Path,
) -> Result<(), RuntimeInstallError> {
    let target_text = target.to_string_lossy();
    if target_text.is_empty()
        || target_text.contains('\\')
        || target_text.contains(':')
        || target.is_absolute()
    {
        return Err(RuntimeInstallError::UnsafeArchive(format!(
            "unsafe archive link target `{target_text}`"
        )));
    }
    let mut depth = link_path.parent().map_or(0, |parent| {
        parent
            .components()
            .filter(|component| matches!(component, Component::Normal(_)))
            .count()
    });
    for component in target.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(RuntimeInstallError::UnsafeArchive(format!(
                    "archive link target escapes staging: `{target_text}`"
                )));
            }
        }
    }
    Ok(())
}

fn normalized_archive_key(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        value.to_ascii_lowercase()
    } else {
        value
    }
}

fn locate_entrypoint(staging: &Path, expected: &[String]) -> Result<PathBuf, String> {
    let mut matches = Vec::new();
    for entry in walkdir::WalkDir::new(staging).follow_links(false) {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str() else {
            continue;
        };
        if expected
            .iter()
            .any(|expected| name.eq_ignore_ascii_case(expected))
        {
            matches.push(entry.path().to_path_buf());
        }
    }
    match matches.as_slice() {
        [entrypoint] => Ok(entrypoint.clone()),
        [] => Err(format!(
            "expected one of [{}], but none was present",
            expected.join(", ")
        )),
        _ => Err(format!(
            "expected one entrypoint, found {} matching files",
            matches.len()
        )),
    }
}

async fn write_manifest(
    path: &Path,
    manifest: &RuntimeManifest,
) -> Result<(), RuntimeInstallError> {
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| RuntimeInstallError::InvalidMetadata(error.to_string()))?;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
        .map_err(|error| RuntimeInstallError::InvalidMetadata(error.to_string()))?;
    file.write_all(&bytes)
        .await
        .map_err(|error| RuntimeInstallError::InvalidMetadata(error.to_string()))?;
    file.flush()
        .await
        .map_err(|error| RuntimeInstallError::InvalidMetadata(error.to_string()))?;
    file.sync_all()
        .await
        .map_err(|error| RuntimeInstallError::InvalidMetadata(error.to_string()))
}

async fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(hex_digest(hash.finalize()))
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn verify_package_digest(expected: &str, observed: &str) -> Result<(), RuntimeInstallError> {
    if observed == expected {
        Ok(())
    } else {
        Err(RuntimeInstallError::ChecksumMismatch {
            expected: expected.to_owned(),
            observed: observed.to_owned(),
        })
    }
}

#[cfg(unix)]
fn set_unix_mode(path: &Path, mode: Option<u32>) -> Result<(), RuntimeInstallError> {
    use std::os::unix::fs::PermissionsExt;

    if let Some(mode) = mode {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o777))
            .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))?;
    }
    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: &Path, destination: &Path) -> Result<(), RuntimeInstallError> {
    std::os::unix::fs::symlink(target, destination)
        .map_err(|error| RuntimeInstallError::UnsafeArchive(error.to_string()))
}

#[cfg(not(unix))]
fn create_symlink(_target: &Path, _destination: &Path) -> Result<(), RuntimeInstallError> {
    Err(RuntimeInstallError::UnsafeArchive(
        "archive symlinks are unsupported on this host".to_owned(),
    ))
}

#[cfg(not(unix))]
fn set_unix_mode(_path: &Path, _mode: Option<u32>) -> Result<(), RuntimeInstallError> {
    Ok(())
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::Path;

    use norted_core::RuntimeArchiveFormat;

    use super::{
        RuntimeInstallError, extract_archive, validate_relative_link_target, verify_package_digest,
    };

    #[test]
    fn zip_and_tar_traversal_are_rejected() {
        let workspace = tempfile::tempdir().expect("temporary directory");
        let zip_path = workspace.path().join("bad.zip");
        {
            let file = std::fs::File::create(&zip_path).expect("zip file");
            let mut zip = zip::ZipWriter::new(file);
            zip.start_file("../escape", zip::write::SimpleFileOptions::default())
                .expect("zip entry");
            zip.write_all(b"bad").expect("zip contents");
            zip.finish().expect("finish zip");
        }
        let staging = workspace.path().join("zip-staging");
        std::fs::create_dir(&staging).expect("zip staging");
        assert!(matches!(
            extract_archive(&zip_path, &staging, RuntimeArchiveFormat::Zip),
            Err(RuntimeInstallError::UnsafeArchive(_))
        ));

        let tar_path = workspace.path().join("bad.tar.gz");
        {
            let file = std::fs::File::create(&tar_path).expect("tar file");
            let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut tar = tar::Builder::new(encoder);
            let mut header = tar::Header::new_gnu();
            header.set_size(3);
            header.set_mode(0o644);
            header.set_cksum();
            // Build the malicious path directly in the header because recent
            // tar builders correctly refuse traversal paths themselves.
            let bytes = header.as_mut_bytes();
            bytes[..9].copy_from_slice(b"../escape");
            header.set_cksum();
            tar.append(&header, &b"bad"[..]).expect("tar entry");
            tar.finish().expect("finish tar");
        }
        let staging = workspace.path().join("tar-staging");
        std::fs::create_dir(&staging).expect("tar staging");
        assert!(matches!(
            extract_archive(&tar_path, &staging, RuntimeArchiveFormat::TarGz),
            Err(RuntimeInstallError::UnsafeArchive(_))
        ));
    }

    #[test]
    fn checksum_mismatch_is_rejected() {
        let expected = "a".repeat(64);
        let observed = "b".repeat(64);
        let error = verify_package_digest(&expected, &observed)
            .expect_err("mismatched package digest must fail");
        assert!(error.to_string().contains(&expected));
        assert!(error.to_string().contains(&observed));
        assert!(verify_package_digest(&expected, &expected).is_ok());
    }

    #[test]
    fn archive_link_targets_must_remain_inside_staging() {
        assert!(
            validate_relative_link_target(Path::new("bin/link"), Path::new("../lib/file")).is_ok()
        );
        assert!(
            validate_relative_link_target(Path::new("bin/link"), Path::new("../../escape"))
                .is_err()
        );
    }
}
