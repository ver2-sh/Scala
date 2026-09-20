use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use scala_core::{
    AcceleratorDevice, AvailableRuntime, ComputeCapability, HostCapabilities, RuntimeCompatibility,
    RuntimeId, RuntimeRequirements, RuntimeSourceSnapshot,
};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

const CATALOG_CACHE_SCHEMA_VERSION: u32 = 1;
const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(15 * 60);
const NVIDIA_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const GITHUB_API_VERSION_HEADER: &str = "X-GitHub-Api-Version";
const GITHUB_API_VERSION: &str = "2022-11-28";

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("runtime provider `{provider}` failed: {message}")]
    Provider { provider: String, message: String },
    #[error("could not create runtime catalog HTTP client: {0}")]
    Client(String),
    #[error("GitHub release request failed: {0}")]
    Request(String),
    #[error("GitHub release API returned HTTP {status}: {detail}")]
    Http { status: u16, detail: String },
    #[error("runtime catalog cache operation failed: {0}")]
    Cache(String),
}

#[async_trait]
pub trait RuntimeCatalogProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn engine_id(&self) -> &'static str;
    fn repository(&self) -> &'static str;
    async fn fetch(
        &self,
        github: &GitHubReleaseClient,
    ) -> Result<Vec<AvailableRuntime>, CatalogError>;

    async fn fetch_reference(
        &self,
        _github: &GitHubReleaseClient,
        _reference: &str,
    ) -> Result<Vec<AvailableRuntime>, CatalogError> {
        Ok(Vec::new())
    }

    /// Revalidates one selected candidate against its live provider authority.
    /// Release providers retain exact-reference lookup; source providers can
    /// instead prove the immutable commit/tree carried by the candidate.
    async fn verify_candidate(
        &self,
        github: &GitHubReleaseClient,
        candidate: &AvailableRuntime,
    ) -> Result<Option<AvailableRuntime>, CatalogError> {
        Ok(self
            .fetch_reference(github, candidate.runtime_id.as_str())
            .await?
            .into_iter()
            .find(|runtime| runtime.runtime_id == candidate.runtime_id))
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RuntimeProviderAuthority {
    pub provider_id: String,
    pub engine_id: String,
    pub repository: String,
}

impl RuntimeProviderAuthority {
    fn from_provider(provider: &dyn RuntimeCatalogProvider) -> Self {
        Self {
            provider_id: provider.id().to_owned(),
            engine_id: provider.engine_id().to_owned(),
            repository: provider.repository().to_owned(),
        }
    }

    pub fn validate(&self, runtime: &AvailableRuntime) -> Result<(), CatalogError> {
        runtime.validate().map_err(|error| CatalogError::Provider {
            provider: self.provider_id.clone(),
            message: error.to_string(),
        })?;
        if runtime.identity.package.provider_id != self.provider_id
            || runtime.identity.engine_id != self.engine_id
            || runtime.identity.package.repository.as_deref() != Some(self.repository.as_str())
        {
            return Err(CatalogError::Provider {
                provider: self.provider_id.clone(),
                message: format!(
                    "runtime `{}` does not match the provider's authoritative engine/repository identity",
                    runtime.runtime_id
                ),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeCatalogEntry {
    pub available: AvailableRuntime,
    pub compatibility: RuntimeCompatibility,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeProviderError {
    pub provider_id: String,
    pub message: String,
    pub using_stale_cache: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeCatalogSnapshot {
    pub entries: Vec<RuntimeCatalogEntry>,
    pub provider_errors: Vec<RuntimeProviderError>,
    pub fetched_at_unix: Option<i64>,
}

#[derive(Clone)]
pub struct RuntimeCatalog {
    providers: BTreeMap<String, Arc<dyn RuntimeCatalogProvider>>,
    cache_root: PathBuf,
    cache_ttl: Duration,
    github: GitHubReleaseClient,
}

impl RuntimeCatalog {
    pub fn new(
        cache_root: PathBuf,
        providers: impl IntoIterator<Item = Arc<dyn RuntimeCatalogProvider>>,
    ) -> Result<Self, CatalogError> {
        let providers = providers
            .into_iter()
            .map(|provider| (provider.id().to_owned(), provider))
            .collect();
        Ok(Self {
            providers,
            cache_root,
            cache_ttl: DEFAULT_CACHE_TTL,
            github: GitHubReleaseClient::new()?,
        })
    }

    pub fn provider_ids(&self) -> impl Iterator<Item = &str> {
        self.providers.keys().map(String::as_str)
    }

    pub fn authorities(&self) -> Vec<RuntimeProviderAuthority> {
        self.providers
            .values()
            .map(|provider| RuntimeProviderAuthority::from_provider(provider.as_ref()))
            .collect()
    }

    pub async fn search(
        &self,
        query: &str,
        host: &HostCapabilities,
        force_refresh: bool,
    ) -> RuntimeCatalogSnapshot {
        let mut all = Vec::new();
        let mut errors = Vec::new();
        let mut newest_fetch = None;
        for (provider_id, provider) in &self.providers {
            let authority = RuntimeProviderAuthority::from_provider(provider.as_ref());
            let cached = self
                .read_cache(provider_id, &authority)
                .await
                .ok()
                .flatten();
            let fresh = cached
                .as_ref()
                .is_some_and(|cache| cache_age(cache.fetched_at_unix) <= self.cache_ttl);
            let (runtimes, fetched_at) = if fresh && !force_refresh {
                let cache = cached.expect("fresh cache was present");
                (cache.runtimes, cache.fetched_at_unix)
            } else {
                match provider
                    .fetch(&self.github)
                    .await
                    .and_then(|runtimes| validate_provider_runtimes(&authority, runtimes))
                {
                    Ok(runtimes) => {
                        let fetched_at = unix_timestamp();
                        let cache = ProviderCache {
                            schema_version: CATALOG_CACHE_SCHEMA_VERSION,
                            provider_id: provider_id.clone(),
                            fetched_at_unix: fetched_at,
                            historical_source_runtimes: historical_source_candidates(
                                cached.as_ref(),
                                &runtimes,
                            ),
                            runtimes: runtimes.clone(),
                        };
                        if let Err(error) = self.write_cache(provider_id, &cache).await {
                            errors.push(RuntimeProviderError {
                                provider_id: provider_id.clone(),
                                message: error.to_string(),
                                using_stale_cache: false,
                            });
                        }
                        (runtimes, fetched_at)
                    }
                    Err(error) => {
                        if let Some(cache) = cached {
                            errors.push(RuntimeProviderError {
                                provider_id: provider_id.clone(),
                                message: error.to_string(),
                                using_stale_cache: true,
                            });
                            (cache.runtimes, cache.fetched_at_unix)
                        } else {
                            errors.push(RuntimeProviderError {
                                provider_id: provider_id.clone(),
                                message: error.to_string(),
                                using_stale_cache: false,
                            });
                            (Vec::new(), unix_timestamp())
                        }
                    }
                }
            };
            newest_fetch =
                Some(newest_fetch.map_or(fetched_at, |value: i64| value.max(fetched_at)));
            all.extend(runtimes);
        }
        let normalized_query = query.trim().to_ascii_lowercase();
        let mut entries = all
            .into_iter()
            .filter(|runtime| runtime_matches(runtime, &normalized_query))
            .map(|available| RuntimeCatalogEntry {
                compatibility: compatibility_for(
                    &available.identity.platform,
                    &available.identity.architecture,
                    &available.identity.accelerator,
                    &available.requirements,
                    host,
                ),
                available,
            })
            .collect::<Vec<_>>();
        if entries.is_empty() && !normalized_query.is_empty() {
            for (provider_id, provider) in &self.providers {
                let authority = RuntimeProviderAuthority::from_provider(provider.as_ref());
                match provider
                    .fetch_reference(&self.github, query.trim())
                    .await
                    .and_then(|runtimes| validate_provider_runtimes(&authority, runtimes))
                {
                    Ok(runtimes) => entries.extend(
                        runtimes
                            .into_iter()
                            .filter(|runtime| runtime_matches(runtime, &normalized_query))
                            .map(|available| RuntimeCatalogEntry {
                                compatibility: compatibility_for(
                                    &available.identity.platform,
                                    &available.identity.architecture,
                                    &available.identity.accelerator,
                                    &available.requirements,
                                    host,
                                ),
                                available,
                            }),
                    ),
                    Err(error) => errors.push(RuntimeProviderError {
                        provider_id: provider_id.clone(),
                        message: error.to_string(),
                        using_stale_cache: false,
                    }),
                }
            }
        }
        entries.sort_by(|left, right| {
            left.compatibility
                .preference_rank()
                .cmp(&right.compatibility.preference_rank())
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
        RuntimeCatalogSnapshot {
            entries,
            provider_errors: errors,
            fetched_at_unix: newest_fetch,
        }
    }

    pub async fn find(
        &self,
        runtime_id: &RuntimeId,
        host: &HostCapabilities,
        force_refresh: bool,
    ) -> Result<RuntimeCatalogEntry, CatalogError> {
        if !force_refresh {
            for (provider_id, provider) in &self.providers {
                let authority = RuntimeProviderAuthority::from_provider(provider.as_ref());
                if let Some(cache) = self
                    .read_cache(provider_id, &authority)
                    .await
                    .ok()
                    .flatten()
                    && let Some(available) = cache
                        .runtimes
                        .into_iter()
                        .chain(cache.historical_source_runtimes)
                        .find(|available| &available.runtime_id == runtime_id)
                {
                    return Ok(RuntimeCatalogEntry {
                        compatibility: compatibility_for(
                            &available.identity.platform,
                            &available.identity.architecture,
                            &available.identity.accelerator,
                            &available.requirements,
                            host,
                        ),
                        available,
                    });
                }
            }
        }
        let snapshot = self.search(runtime_id.as_str(), host, force_refresh).await;
        snapshot
            .entries
            .into_iter()
            .find(|entry| &entry.available.runtime_id == runtime_id)
            .ok_or_else(|| CatalogError::Provider {
                provider: "catalog".to_owned(),
                message: format!("runtime `{runtime_id}` was not found"),
            })
    }

    pub async fn verify_install_candidate(
        &self,
        candidate: &AvailableRuntime,
    ) -> Result<AvailableRuntime, CatalogError> {
        let provider_id = &candidate.identity.package.provider_id;
        let provider = self
            .providers
            .get(provider_id)
            .ok_or_else(|| CatalogError::Provider {
                provider: provider_id.clone(),
                message: "catalog entry names an unregistered provider authority".to_owned(),
            })?;
        let authority = RuntimeProviderAuthority::from_provider(provider.as_ref());
        let live = provider
            .verify_candidate(&self.github, candidate)
            .await?
            .ok_or_else(|| CatalogError::Provider {
                provider: provider_id.clone(),
                message: format!(
                    "runtime `{}` is not admitted by the provider's current exact-release policy",
                    candidate.runtime_id
                ),
            })?;
        authority.validate(&live)?;
        if !same_install_candidate(&live, candidate) {
            return Err(CatalogError::Provider {
                provider: provider_id.clone(),
                message: format!(
                    "runtime `{}` changed since catalog discovery; refresh search before installing",
                    candidate.runtime_id
                ),
            });
        }
        Ok(live)
    }

    pub async fn compare_source_history(
        &self,
        installed: &RuntimeSourceSnapshot,
        candidate: &RuntimeSourceSnapshot,
    ) -> Result<GitHubCompare, CatalogError> {
        if installed.repository != candidate.repository
            || installed.source_provider != candidate.source_provider
        {
            return Err(CatalogError::Provider {
                provider: candidate.source_provider.clone(),
                message: "source update candidates do not share one repository/provider history"
                    .to_owned(),
            });
        }
        let provider = self
            .providers
            .get(&candidate.source_provider)
            .ok_or_else(|| CatalogError::Provider {
                provider: candidate.source_provider.clone(),
                message: "source update names an unregistered provider authority".to_owned(),
            })?;
        let authority = RuntimeProviderAuthority::from_provider(provider.as_ref());
        if authority.repository != candidate.repository {
            return Err(CatalogError::Provider {
                provider: candidate.source_provider.clone(),
                message: "source update repository does not match provider authority".to_owned(),
            });
        }
        self.github
            .compare_commits(
                &candidate.repository,
                &installed.commit_sha,
                &candidate.commit_sha,
            )
            .await
    }

    pub async fn find_reference(
        &self,
        runtime_id: &RuntimeId,
        host: &HostCapabilities,
    ) -> Result<Option<RuntimeCatalogEntry>, CatalogError> {
        let mut first_error = None;
        for provider in self.providers.values() {
            let authority = RuntimeProviderAuthority::from_provider(provider.as_ref());
            match provider
                .fetch_reference(&self.github, runtime_id.as_str())
                .await
                .and_then(|runtimes| validate_provider_runtimes(&authority, runtimes))
            {
                Ok(runtimes) => {
                    if let Some(available) = runtimes
                        .into_iter()
                        .find(|available| &available.runtime_id == runtime_id)
                    {
                        return Ok(Some(RuntimeCatalogEntry {
                            compatibility: compatibility_for(
                                &available.identity.platform,
                                &available.identity.architecture,
                                &available.identity.accelerator,
                                &available.requirements,
                                host,
                            ),
                            available,
                        }));
                    }
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        if let Some(error) = first_error {
            Err(error)
        } else {
            Ok(None)
        }
    }

    async fn read_cache(
        &self,
        provider_id: &str,
        authority: &RuntimeProviderAuthority,
    ) -> Result<Option<ProviderCache>, CatalogError> {
        let path = self.cache_path(provider_id);
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(CatalogError::Cache(error.to_string())),
        };
        let cache: ProviderCache = serde_json::from_slice(&bytes)
            .map_err(|error| CatalogError::Cache(error.to_string()))?;
        if cache.schema_version != CATALOG_CACHE_SCHEMA_VERSION || cache.provider_id != provider_id
        {
            return Ok(None);
        }
        if validate_provider_runtimes(authority, cache.runtimes.clone()).is_err() {
            return Ok(None);
        }
        if validate_provider_runtimes(authority, cache.historical_source_runtimes.clone()).is_err()
        {
            return Ok(None);
        }
        Ok(Some(cache))
    }

    async fn write_cache(
        &self,
        provider_id: &str,
        cache: &ProviderCache,
    ) -> Result<(), CatalogError> {
        let path = self.cache_path(provider_id);
        let bytes = serde_json::to_vec_pretty(cache)
            .map_err(|error| CatalogError::Cache(error.to_string()))?;
        atomic_write(path, bytes)
            .await
            .map_err(|error| CatalogError::Cache(error.to_string()))
    }

    fn cache_path(&self, provider_id: &str) -> PathBuf {
        self.cache_root
            .join("catalog")
            .join(format!("{}.json", safe_name(provider_id)))
    }
}

#[derive(Debug, Clone)]
pub struct GitHubReleaseClient {
    api_client: reqwest::Client,
    asset_client: reqwest::Client,
}

impl GitHubReleaseClient {
    pub fn new() -> Result<Self, CatalogError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static("scala-runtime-catalog"),
        );
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            GITHUB_API_VERSION_HEADER,
            HeaderValue::from_static(GITHUB_API_VERSION),
        );
        if let Some(token) = std::env::var("GITHUB_TOKEN")
            .ok()
            .or_else(|| std::env::var("GH_TOKEN").ok())
            .filter(|token| !token.trim().is_empty())
        {
            // Optional authentication must never make local startup fail.
            // Invalid header bytes are ignored without logging the token.
            if let Ok(value) = HeaderValue::from_str(&format!("Bearer {token}")) {
                headers.insert(AUTHORIZATION, value);
            }
        }
        let redirect = || {
            reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() > 5 {
                    return attempt.error("too many GitHub redirects");
                }
                if is_allowed_github_host(attempt.url()) {
                    attempt.follow()
                } else {
                    attempt.error("GitHub request redirected to an untrusted host")
                }
            })
        };
        let api_client = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(redirect())
            .build()
            .map_err(|error| CatalogError::Client(error.to_string()))?;
        let asset_client = reqwest::Client::builder()
            .user_agent("scala-runtime-catalog")
            .connect_timeout(Duration::from_secs(10))
            // Runtime archives can be hundreds of MiB. Bound a stalled read,
            // not the duration of a healthy streaming transfer.
            .read_timeout(Duration::from_secs(60))
            .redirect(redirect())
            .build()
            .map_err(|error| CatalogError::Client(error.to_string()))?;
        Ok(Self {
            api_client,
            asset_client,
        })
    }

    pub async fn releases(&self, repository: &str) -> Result<Vec<GitHubRelease>, CatalogError> {
        if !valid_repository(repository) {
            return Err(CatalogError::Request(format!(
                "invalid configured GitHub repository `{repository}`"
            )));
        }
        let url = format!("https://api.github.com/repos/{repository}/releases?per_page=100");
        let response = self
            .api_client
            .get(url)
            .send()
            .await
            .map_err(|error| CatalogError::Request(error.to_string()))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|error| CatalogError::Request(error.to_string()))?;
        if !status.is_success() {
            let detail = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| {
                    value
                        .get("message")
                        .and_then(|value| value.as_str())
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| {
                    status
                        .canonical_reason()
                        .unwrap_or("request failed")
                        .to_owned()
                });
            return Err(CatalogError::Http {
                status: status.as_u16(),
                detail,
            });
        }
        serde_json::from_slice(&body).map_err(|error| CatalogError::Request(error.to_string()))
    }

    pub async fn repository(&self, repository: &str) -> Result<GitHubRepository, CatalogError> {
        self.get_api_json(repository, &["repos", repository]).await
    }

    pub async fn commit(
        &self,
        repository: &str,
        reference: &str,
    ) -> Result<GitHubCommit, CatalogError> {
        if reference.is_empty() || reference.contains('\0') {
            return Err(CatalogError::Request(
                "invalid configured GitHub commit reference".to_owned(),
            ));
        }
        self.get_api_json(repository, &["repos", repository, "commits", reference])
            .await
    }

    pub async fn compare_commits(
        &self,
        repository: &str,
        base: &str,
        head: &str,
    ) -> Result<GitHubCompare, CatalogError> {
        if !scala_core::is_full_git_sha(base) || !scala_core::is_full_git_sha(head) {
            return Err(CatalogError::Request(
                "GitHub comparison requires full hexadecimal commit SHAs".to_owned(),
            ));
        }
        let comparison = format!("{base}...{head}");
        self.get_api_json(repository, &["repos", repository, "compare", &comparison])
            .await
    }

    async fn get_api_json<T>(&self, repository: &str, path: &[&str]) -> Result<T, CatalogError>
    where
        T: serde::de::DeserializeOwned,
    {
        if !valid_repository(repository) {
            return Err(CatalogError::Request(format!(
                "invalid configured GitHub repository `{repository}`"
            )));
        }
        let mut url = reqwest::Url::parse("https://api.github.com/")
            .map_err(|error| CatalogError::Request(error.to_string()))?;
        {
            let mut segments = url.path_segments_mut().map_err(|_| {
                CatalogError::Request("GitHub API URL cannot contain path segments".to_owned())
            })?;
            segments.clear();
            for component in path {
                if *component == repository {
                    for repository_component in repository.split('/') {
                        segments.push(repository_component);
                    }
                } else {
                    segments.push(component);
                }
            }
        }
        let response = self
            .api_client
            .get(url)
            .send()
            .await
            .map_err(|error| CatalogError::Request(error.to_string()))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|error| CatalogError::Request(error.to_string()))?;
        if !status.is_success() {
            let detail = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| value.get("message")?.as_str().map(str::to_owned))
                .unwrap_or_else(|| {
                    status
                        .canonical_reason()
                        .unwrap_or("request failed")
                        .to_owned()
                });
            return Err(CatalogError::Http {
                status: status.as_u16(),
                detail,
            });
        }
        serde_json::from_slice(&body).map_err(|error| CatalogError::Request(error.to_string()))
    }

    pub async fn release_by_tag(
        &self,
        repository: &str,
        tag: &str,
    ) -> Result<Option<GitHubRelease>, CatalogError> {
        if !valid_repository(repository) || !valid_release_tag(tag) {
            return Err(CatalogError::Request(
                "invalid configured GitHub repository or release tag".to_owned(),
            ));
        }
        let url = format!("https://api.github.com/repos/{repository}/releases/tags/{tag}");
        let response = self
            .api_client
            .get(url)
            .send()
            .await
            .map_err(|error| CatalogError::Request(error.to_string()))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|error| CatalogError::Request(error.to_string()))?;
        if !status.is_success() {
            let detail = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| value.get("message")?.as_str().map(str::to_owned))
                .unwrap_or_else(|| {
                    status
                        .canonical_reason()
                        .unwrap_or("request failed")
                        .to_owned()
                });
            return Err(CatalogError::Http {
                status: status.as_u16(),
                detail,
            });
        }
        serde_json::from_slice(&body)
            .map(Some)
            .map_err(|error| CatalogError::Request(error.to_string()))
    }

    pub fn release_asset_request(
        &self,
        repository: &str,
        asset_id: u64,
    ) -> Result<reqwest::RequestBuilder, CatalogError> {
        if !valid_repository(repository) || asset_id == 0 {
            return Err(CatalogError::Request(
                "invalid configured GitHub repository or release asset ID".to_owned(),
            ));
        }
        let url = format!("https://api.github.com/repos/{repository}/releases/assets/{asset_id}");
        Ok(self
            .asset_client
            .get(url)
            .header(ACCEPT, "application/octet-stream")
            .header(GITHUB_API_VERSION_HEADER, GITHUB_API_VERSION))
    }

    pub async fn fetch_small_text(
        &self,
        url: &str,
        maximum_bytes: usize,
    ) -> Result<String, CatalogError> {
        let url = url
            .parse::<reqwest::Url>()
            .map_err(|error| CatalogError::Request(error.to_string()))?;
        if !is_allowed_github_host(&url) {
            return Err(CatalogError::Request(
                "asset URL is not hosted on an approved GitHub domain".to_owned(),
            ));
        }
        let response = self
            .asset_client
            .get(url)
            .send()
            .await
            .map_err(|error| CatalogError::Request(error.to_string()))?;
        if !response.status().is_success() {
            return Err(CatalogError::Http {
                status: response.status().as_u16(),
                detail: "small release metadata asset request failed".to_owned(),
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > maximum_bytes as u64)
        {
            return Err(CatalogError::Request(
                "release metadata asset exceeds the local size limit".to_owned(),
            ));
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| CatalogError::Request(error.to_string()))?;
            if bytes
                .len()
                .checked_add(chunk.len())
                .is_none_or(|length| length > maximum_bytes)
            {
                return Err(CatalogError::Request(
                    "release metadata asset exceeds the local size limit".to_owned(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        String::from_utf8(bytes).map_err(|error| CatalogError::Request(error.to_string()))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubRelease {
    pub id: u64,
    pub tag_name: String,
    pub name: Option<String>,
    pub html_url: String,
    pub target_commitish: String,
    pub draft: bool,
    pub prerelease: bool,
    pub published_at: Option<String>,
    pub assets: Vec<GitHubReleaseAsset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubReleaseAsset {
    pub id: u64,
    pub name: String,
    pub size: u64,
    pub browser_download_url: String,
    pub digest: Option<String>,
    #[serde(default)]
    pub state: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubRepository {
    pub full_name: String,
    pub default_branch: String,
    pub html_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubCommit {
    pub sha: String,
    pub html_url: String,
    pub commit: GitHubCommitDetails,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubCommitDetails {
    pub committer: GitHubCommitter,
    pub tree: GitHubGitTree,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubCommitter {
    pub date: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubGitTree {
    pub sha: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GitHubComparisonStatus {
    Identical,
    Ahead,
    Behind,
    Diverged,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubCompare {
    pub status: GitHubComparisonStatus,
    pub ahead_by: u64,
    pub behind_by: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProviderCache {
    schema_version: u32,
    provider_id: String,
    fetched_at_unix: i64,
    runtimes: Vec<AvailableRuntime>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    historical_source_runtimes: Vec<AvailableRuntime>,
}

fn historical_source_candidates(
    previous: Option<&ProviderCache>,
    current: &[AvailableRuntime],
) -> Vec<AvailableRuntime> {
    const MAX_HISTORICAL_SOURCE_CANDIDATES: usize = 64;

    let current_ids = current
        .iter()
        .map(|runtime| runtime.runtime_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut unique = BTreeMap::new();
    if let Some(previous) = previous {
        for runtime in previous
            .runtimes
            .iter()
            .chain(&previous.historical_source_runtimes)
            .filter(|runtime| runtime.source_build().is_some())
            .filter(|runtime| !current_ids.contains(&runtime.runtime_id))
        {
            unique
                .entry(runtime.runtime_id.clone())
                .or_insert_with(|| runtime.clone());
        }
    }
    let mut historical = unique.into_values().collect::<Vec<_>>();
    historical.sort_by(|left, right| {
        right
            .published_at_unix
            .cmp(&left.published_at_unix)
            .then_with(|| left.runtime_id.cmp(&right.runtime_id))
    });
    historical.truncate(MAX_HISTORICAL_SOURCE_CANDIDATES);
    historical
}

fn validate_provider_runtimes(
    authority: &RuntimeProviderAuthority,
    runtimes: Vec<AvailableRuntime>,
) -> Result<Vec<AvailableRuntime>, CatalogError> {
    for runtime in &runtimes {
        authority.validate(runtime)?;
    }
    Ok(runtimes)
}

fn same_install_candidate(left: &AvailableRuntime, right: &AvailableRuntime) -> bool {
    left.runtime_id == right.runtime_id
        && left.identity == right.identity
        && left.display_name == right.display_name
        && left.supported_formats == right.supported_formats
        && left.source_url == right.source_url
        && left.published_at_unix == right.published_at_unix
        && left.prerelease == right.prerelease
        && left.acquisition == right.acquisition
        && left.supported_native_identities == right.supported_native_identities
        && left.requirements == right.requirements
}

pub async fn detect_host_capabilities() -> HostCapabilities {
    let mut host = HostCapabilities::current_without_accelerator_probe();
    let output = tokio::time::timeout(NVIDIA_PROBE_TIMEOUT, query_nvidia_gpus()).await;
    match output {
        Ok(Ok((output, includes_compute_capability))) if output.status.success() => {
            let observed = parse_nvidia_smi_devices(
                &String::from_utf8_lossy(&output.stdout),
                includes_compute_capability,
            );
            if !observed.is_empty() {
                let count = observed.len();
                host.accelerators = observed;
                host.observations.push(if includes_compute_capability {
                    format!(
                        "observed {count} NVIDIA GPU device(s) with stable UUID and compute-capability queries through nvidia-smi"
                    )
                } else {
                    format!(
                        "observed {count} NVIDIA GPU device(s) with stable UUID queries through nvidia-smi; compute capability is unavailable from this nvidia-smi"
                    )
                });
            } else {
                host.nvidia_gpu_absence_confirmed = true;
                host.observations
                    .push("nvidia-smi confirmed that no NVIDIA GPU is available".to_owned());
            }
        }
        Ok(Ok(_)) => host
            .observations
            .push("nvidia-smi did not report an available NVIDIA GPU".to_owned()),
        Ok(Err(_)) => host
            .observations
            .push("nvidia-smi is unavailable; NVIDIA capability is unknown".to_owned()),
        Err(_) => host
            .observations
            .push("nvidia-smi timed out; NVIDIA capability is unknown".to_owned()),
    }
    host
}

async fn query_nvidia_gpus() -> std::io::Result<(std::process::Output, bool)> {
    let mut command = Command::new("nvidia-smi");
    command
        .args([
            "--query-gpu=uuid,name,memory.total,driver_version,compute_cap",
            "--format=csv,noheader,nounits",
        ])
        .kill_on_drop(true);
    let rich = command.output().await?;
    if rich.status.success() || !compute_cap_query_is_unsupported(&rich.stderr) {
        return Ok((rich, true));
    }

    let mut fallback = Command::new("nvidia-smi");
    fallback
        .args([
            "--query-gpu=uuid,name,memory.total,driver_version",
            "--format=csv,noheader,nounits",
        ])
        .kill_on_drop(true);
    fallback.output().await.map(|output| (output, false))
}

fn compute_cap_query_is_unsupported(stderr: &[u8]) -> bool {
    let detail = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    detail.contains("compute_cap")
        && (detail.contains("not a valid field")
            || detail.contains("unknown field")
            || detail.contains("not supported"))
}

fn parse_nvidia_smi_devices(
    output: &str,
    includes_compute_capability: bool,
) -> Vec<AcceleratorDevice> {
    output
        .lines()
        .filter_map(|line| {
            let fields = line.split(',').map(str::trim).collect::<Vec<_>>();
            (fields.len() >= 4).then(|| AcceleratorDevice {
                accelerator: "cuda".to_owned(),
                stable_id: nonempty(fields[0]).filter(|uuid| uuid.starts_with("GPU-")),
                name: nonempty(fields[1]),
                vram_bytes: fields[2]
                    .parse::<u64>()
                    .ok()
                    .and_then(|mib| mib.checked_mul(1024 * 1024)),
                driver_version: nonempty(fields[3]),
                compute_capability: includes_compute_capability
                    .then(|| {
                        fields
                            .get(4)
                            .and_then(|value| parse_compute_capability(value))
                    })
                    .flatten(),
            })
        })
        .collect()
}

fn parse_compute_capability(value: &str) -> Option<ComputeCapability> {
    let (major, minor) = value.trim().split_once('.')?;
    if major.is_empty()
        || minor.is_empty()
        || !major.bytes().all(|byte| byte.is_ascii_digit())
        || !minor.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some(ComputeCapability::new(
        major.parse().ok()?,
        minor.parse().ok()?,
    ))
}

pub fn compatibility_for(
    platform: &str,
    architecture: &str,
    accelerator: &str,
    requirements: &RuntimeRequirements,
    host: &HostCapabilities,
) -> RuntimeCompatibility {
    if platform != host.platform {
        return RuntimeCompatibility::Incompatible(format!(
            "requires {platform}; host is {}",
            host.platform
        ));
    }
    if architecture != host.architecture {
        return RuntimeCompatibility::Incompatible(format!(
            "requires {architecture}; host is {}",
            host.architecture
        ));
    }
    let accelerator = accelerator.to_ascii_lowercase();
    if requirements.requires_nvidia_gpu || accelerator.starts_with("cuda") {
        let nvidia = host
            .accelerators
            .iter()
            .filter(|device| device.accelerator.eq_ignore_ascii_case("cuda"))
            .collect::<Vec<_>>();
        if nvidia.is_empty() {
            return if host.nvidia_gpu_absence_confirmed {
                RuntimeCompatibility::Incompatible(
                    "requires an NVIDIA GPU; nvidia-smi confirmed none is available".to_owned(),
                )
            } else {
                RuntimeCompatibility::NeedsAttention(
                    "requires an NVIDIA GPU; nvidia-smi did not confirm one".to_owned(),
                )
            };
        }
        return nvidia
            .into_iter()
            .map(|device| compatibility_for_nvidia_device(requirements, device))
            .min_by_key(RuntimeCompatibility::preference_rank)
            .expect("non-empty NVIDIA device list");
    }
    if accelerator == "vulkan" {
        return RuntimeCompatibility::NeedsAttention(
            "Vulkan loader/device availability was not probed".to_owned(),
        );
    }
    if accelerator == "cpu" {
        RuntimeCompatibility::Recommended
    } else {
        RuntimeCompatibility::Compatible
    }
}

pub fn compatibility_for_nvidia_device(
    requirements: &RuntimeRequirements,
    nvidia: &AcceleratorDevice,
) -> RuntimeCompatibility {
    if !requirements.supported_cuda_compute_capabilities.is_empty() {
        let Some(observed) = nvidia.compute_capability else {
            return RuntimeCompatibility::NeedsAttention(format!(
                "runtime CUDA targets are known ({}), but nvidia-smi did not report this GPU's compute capability",
                format_compute_capabilities(&requirements.supported_cuda_compute_capabilities)
            ));
        };
        if !requirements
            .supported_cuda_compute_capabilities
            .contains(&observed)
        {
            return RuntimeCompatibility::Incompatible(format!(
                "runtime CUDA targets are {}; observed GPU compute capability is {observed}",
                format_compute_capabilities(&requirements.supported_cuda_compute_capabilities)
            ));
        }
    }
    if !requirements.required_nvidia_device_names.is_empty() {
        let Some(observed) = nvidia.name.as_deref() else {
            return RuntimeCompatibility::NeedsAttention(format!(
                "runtime requires one of these exact NVIDIA products: {}; nvidia-smi did not report this GPU's name",
                requirements.required_nvidia_device_names.join(", ")
            ));
        };
        if !requirements
            .required_nvidia_device_names
            .iter()
            .any(|required| required.eq_ignore_ascii_case(observed))
        {
            return RuntimeCompatibility::Incompatible(format!(
                "runtime requires one of these exact NVIDIA products: {}; observed {observed}",
                requirements.required_nvidia_device_names.join(", ")
            ));
        }
    }
    if (requirements.minimum_vram_bytes.is_some()
        || requirements.minimum_vram_class_gib.is_some()
        || requirements.minimum_vram_exclusive_class_gib.is_some())
        && nvidia.vram_bytes.is_none()
    {
        return RuntimeCompatibility::NeedsAttention(
            "minimum VRAM is known, but nvidia-smi did not report usable VRAM".to_owned(),
        );
    }
    if requirements.minimum_nvidia_driver.is_some() && nvidia.driver_version.is_none() {
        return RuntimeCompatibility::NeedsAttention(
            "minimum NVIDIA driver is known, but its installed version could not be read"
                .to_owned(),
        );
    }
    if let (Some(minimum), Some(observed)) = (requirements.minimum_vram_bytes, nvidia.vram_bytes)
        && observed < minimum
    {
        return RuntimeCompatibility::Incompatible(format!(
            "requires at least {} MiB VRAM; observed {} MiB",
            minimum / (1024 * 1024),
            observed / (1024 * 1024)
        ));
    }
    if let (Some(class_gib), Some(observed)) = (
        requirements.minimum_vram_exclusive_class_gib,
        nvidia.vram_bytes,
    ) && observed <= u64::from(class_gib).saturating_mul(GIB)
    {
        return RuntimeCompatibility::Incompatible(format!(
            "requires more than a {class_gib} GiB-class GPU; observed {} MiB, while the exact larger requirement is not published",
            observed / (1024 * 1024)
        ));
    }
    if let (Some(class_gib), Some(observed)) =
        (requirements.minimum_vram_class_gib, nvidia.vram_bytes)
    {
        match vram_class_compatibility(class_gib, observed) {
            VramClassCompatibility::Meets => {}
            VramClassCompatibility::Near => {
                return RuntimeCompatibility::NeedsAttention(format!(
                    "requires a {class_gib} GiB-class GPU; observed {} MiB, which is close but below the normal reporting allowance",
                    observed / (1024 * 1024)
                ));
            }
            VramClassCompatibility::Below => {
                return RuntimeCompatibility::Incompatible(format!(
                    "requires a {class_gib} GiB-class GPU; observed {} MiB, clearly below that class",
                    observed / (1024 * 1024)
                ));
            }
        }
    }
    if let (Some(minimum), Some(observed)) = (
        requirements.minimum_nvidia_driver.as_deref(),
        nvidia.driver_version.as_deref(),
    ) && version_components(observed) < version_components(minimum)
    {
        return RuntimeCompatibility::Incompatible(format!(
            "requires NVIDIA driver {minimum} or newer; observed {observed}"
        ));
    }
    if let Some(requirement) = requirements
        .unverified_requirements
        .first()
        .or_else(|| requirements.notes.first())
    {
        return RuntimeCompatibility::NeedsAttention(format!(
            "additional upstream requirement was not fully probed: {requirement}"
        ));
    }
    RuntimeCompatibility::Recommended
}

pub fn visible_nvidia_devices<'a>(
    host: &'a HostCapabilities,
    consumer: &str,
) -> Result<Vec<&'a AcceleratorDevice>, RuntimeCompatibility> {
    let devices = visible_nvidia_device_set(host, consumer)?;
    if host.cuda_visible_devices.is_some() && devices.len() != 1 {
        return Err(RuntimeCompatibility::Incompatible(format!(
            "existing CUDA_VISIBLE_DEVICES is empty or selects multiple devices; {consumer} requires exactly one resolvable GPU UUID"
        )));
    }
    Ok(devices)
}

/// Resolves the NVIDIA devices visible to an engine without assuming a
/// numeric CUDA/NVML index mapping. An inherited UUID list is preserved in
/// order and may contain more than one device; single-device adapters apply
/// their stricter arity contract through `visible_nvidia_devices`.
pub fn visible_nvidia_device_set<'a>(
    host: &'a HostCapabilities,
    consumer: &str,
) -> Result<Vec<&'a AcceleratorDevice>, RuntimeCompatibility> {
    let devices = host
        .accelerators
        .iter()
        .filter(|device| device.accelerator.eq_ignore_ascii_case("cuda"))
        .filter(|device| {
            device
                .stable_id
                .as_deref()
                .is_some_and(is_exact_nvidia_gpu_uuid)
        })
        .collect::<Vec<_>>();
    let Some(visibility) = host.cuda_visible_devices.as_deref() else {
        return if devices.is_empty() {
            Err(RuntimeCompatibility::NeedsAttention(format!(
                "{consumer} requires a stable NVIDIA GPU UUID, but none was observed"
            )))
        } else {
            Ok(devices)
        };
    };
    let identifiers = visibility.split(',').map(str::trim).collect::<Vec<_>>();
    if identifiers.is_empty() || identifiers.iter().any(|value| value.is_empty()) {
        return Err(RuntimeCompatibility::Incompatible(format!(
            "existing CUDA_VISIBLE_DEVICES is empty or contains an empty device; {consumer} requires resolvable GPU UUIDs"
        )));
    }
    let mut resolved = Vec::with_capacity(identifiers.len());
    for identifier in identifiers {
        if !identifier.starts_with("GPU-") {
            return Err(RuntimeCompatibility::Incompatible(format!(
                "existing CUDA_VISIBLE_DEVICES `{identifier}` is not a GPU UUID; Scala never assumes a CUDA/nvidia-smi numeric index mapping"
            )));
        }
        let matching = devices
            .iter()
            .copied()
            .filter(|device| {
                device
                    .stable_id
                    .as_deref()
                    .is_some_and(|stable_id| stable_id.starts_with(identifier))
            })
            .collect::<Vec<_>>();
        let device = match matching.as_slice() {
            [device] => *device,
            [] => {
                return Err(RuntimeCompatibility::Incompatible(format!(
                    "existing CUDA_VISIBLE_DEVICES `{identifier}` does not resolve to an observed NVIDIA GPU UUID"
                )));
            }
            _ => {
                return Err(RuntimeCompatibility::Incompatible(format!(
                    "existing CUDA_VISIBLE_DEVICES `{identifier}` is an ambiguous GPU UUID prefix"
                )));
            }
        };
        if resolved.contains(&device) {
            return Err(RuntimeCompatibility::Incompatible(format!(
                "existing CUDA_VISIBLE_DEVICES selects NVIDIA GPU `{identifier}` more than once"
            )));
        }
        resolved.push(device);
    }
    Ok(resolved)
}

pub fn is_exact_nvidia_gpu_uuid(value: &str) -> bool {
    let Some(uuid) = value.strip_prefix("GPU-") else {
        return false;
    };
    uuid.len() == 36
        && uuid.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

pub fn isolated_cuda_environment(
    configured: &BTreeMap<String, String>,
    accelerator: &AcceleratorDevice,
    consumer: &str,
) -> Result<BTreeMap<String, String>, String> {
    if !accelerator.accelerator.eq_ignore_ascii_case("cuda") {
        return Err(format!(
            "{consumer} selected accelerator is not a CUDA device"
        ));
    }
    let uuid = accelerator
        .stable_id
        .as_deref()
        .ok_or_else(|| format!("{consumer} selected CUDA device has no stable GPU UUID"))?;
    if !is_exact_nvidia_gpu_uuid(uuid) {
        return Err(format!(
            "{consumer} selected CUDA device identity is not an exact NVIDIA GPU UUID"
        ));
    }
    let mut environment = configured.clone();
    environment.insert("CUDA_VISIBLE_DEVICES".to_owned(), uuid.to_owned());
    Ok(environment)
}

pub fn isolated_cuda_environment_for_binding(
    configured: &BTreeMap<String, String>,
    binding: &scala_core::AcceleratorBinding,
    consumer: &str,
) -> Result<BTreeMap<String, String>, String> {
    if binding.devices.is_empty() {
        return Err(format!(
            "{consumer} selected accelerator binding contains no devices"
        ));
    }
    let mut uuids = Vec::with_capacity(binding.devices.len());
    for accelerator in &binding.devices {
        if !accelerator.accelerator.eq_ignore_ascii_case("cuda") {
            return Err(format!(
                "{consumer} selected accelerator is not a CUDA device"
            ));
        }
        let uuid = accelerator
            .stable_id
            .as_deref()
            .ok_or_else(|| format!("{consumer} selected CUDA device has no stable GPU UUID"))?;
        if !is_exact_nvidia_gpu_uuid(uuid) {
            return Err(format!(
                "{consumer} selected CUDA device identity is not an exact NVIDIA GPU UUID"
            ));
        }
        if uuids
            .iter()
            .any(|selected: &&str| selected.eq_ignore_ascii_case(uuid))
        {
            return Err(format!(
                "{consumer} selected CUDA device `{uuid}` more than once"
            ));
        }
        uuids.push(uuid);
    }
    let mut environment = configured.clone();
    environment.insert("CUDA_VISIBLE_DEVICES".to_owned(), uuids.join(","));
    Ok(environment)
}

fn format_compute_capabilities(capabilities: &[ComputeCapability]) -> String {
    capabilities
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

const GIB: u64 = 1024 * 1024 * 1024;
const VRAM_CLASS_REPORTING_ALLOWANCE: u64 = 2 * GIB;
const VRAM_CLASS_NEAR_BAND: u64 = GIB;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum VramClassCompatibility {
    Meets,
    Near,
    Below,
}

/// Upstream GPU sizes are nominal classes, while `nvidia-smi` may report less
/// after vendor rounding or reserved/ECC memory. q27 documents a working 22.6
/// GiB A10 as 24 GiB-class hardware, so up to 2 GiB of reporting/reservation
/// shortfall still counts as the class. The next 1 GiB remains uncertain;
/// larger shortfalls are clearly a lower hardware class.
fn vram_class_compatibility(class_gib: u16, observed: u64) -> VramClassCompatibility {
    let nominal = u64::from(class_gib).saturating_mul(GIB);
    let meets_floor = nominal.saturating_sub(VRAM_CLASS_REPORTING_ALLOWANCE);
    let near_floor = meets_floor.saturating_sub(VRAM_CLASS_NEAR_BAND);
    if observed >= meets_floor {
        VramClassCompatibility::Meets
    } else if observed >= near_floor {
        VramClassCompatibility::Near
    } else {
        VramClassCompatibility::Below
    }
}

pub fn is_allowed_github_host(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("github.com")
                || host.eq_ignore_ascii_case("api.github.com")
                || host.eq_ignore_ascii_case("raw.githubusercontent.com")
                || host.eq_ignore_ascii_case("release-assets.githubusercontent.com")
        })
}

fn runtime_matches(runtime: &AvailableRuntime, query: &str) -> bool {
    if query.is_empty() || matches!(query, "all" | "all runtimes" | "runtimes") {
        return true;
    }
    let searchable = [
        runtime.runtime_id.as_str(),
        runtime.identity.engine_id.as_str(),
        runtime.identity.package_family.as_str(),
        runtime.identity.version.as_str(),
        runtime.identity.platform.as_str(),
        runtime.identity.architecture.as_str(),
        runtime.identity.accelerator.as_str(),
        runtime.identity.variant.as_str(),
        runtime.display_name.as_str(),
        runtime.identity.package.repository.as_deref().unwrap_or(""),
    ]
    .join(" ")
    .to_ascii_lowercase();
    let format_query = query.strip_prefix('.').unwrap_or(query);
    searchable.contains(query)
        || runtime
            .supported_formats
            .iter()
            .any(|format| format.as_str().contains(format_query))
}

fn cache_age(timestamp: i64) -> Duration {
    let now = unix_timestamp();
    Duration::from_secs(now.saturating_sub(timestamp) as u64)
}

fn safe_name(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '-'
            }
        })
        .collect()
}

fn valid_repository(value: &str) -> bool {
    let Some((owner, repository)) = value.split_once('/') else {
        return false;
    };
    !owner.is_empty()
        && !repository.is_empty()
        && !repository.contains('/')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-'))
}

fn valid_release_tag(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn version_components(value: &str) -> Vec<u64> {
    value
        .split(|character: char| !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect()
}

pub(crate) async fn atomic_write(path: PathBuf, bytes: Vec<u8>) -> std::io::Result<()> {
    tokio::task::spawn_blocking(move || {
        use std::io::Write;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut temporary =
            tempfile::NamedTempFile::new_in(path.parent().unwrap_or_else(|| Path::new(".")))?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist(&path).map_err(|error| error.error)?;
        if let Some(parent) = path.parent()
            && let Ok(directory) = std::fs::File::open(parent)
        {
            let _ = directory.sync_all();
        }
        Ok(())
    })
    .await
    .map_err(std::io::Error::other)?
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
    use std::collections::BTreeMap;

    use scala_core::{
        AcceleratorBinding, AcceleratorDevice, ArtifactFormat, AvailableRuntime, ComputeCapability,
        HostCapabilities, RuntimeAcquisitionPlan, RuntimeArchiveFormat, RuntimeCompatibility,
        RuntimeDigest, RuntimeDownload, RuntimeIdentity, RuntimePackageIdentity,
        RuntimeReleaseChannel, RuntimeRequirements, RuntimeSourceBuildPlan,
        RuntimeSourceBuildPrerequisites, RuntimeSourceBuildRecipe, RuntimeSourceBuildSystem,
        RuntimeSourceSnapshot,
    };

    use super::{
        ProviderCache, RuntimeProviderAuthority, compatibility_for,
        compute_cap_query_is_unsupported, historical_source_candidates, is_allowed_github_host,
        isolated_cuda_environment_for_binding, parse_nvidia_smi_devices, same_install_candidate,
        visible_nvidia_device_set, visible_nvidia_devices,
    };

    #[test]
    fn parses_nvidia_compute_capability_without_guessing_malformed_values() {
        let devices = parse_nvidia_smi_devices(
            "GPU-one, NVIDIA One, 24576, 580.1, 8.6\nGPU-two, NVIDIA Two, 32768, 580.1, malformed\n",
            true,
        );
        assert_eq!(devices.len(), 2);
        assert_eq!(
            devices[0].compute_capability,
            Some(ComputeCapability::new(8, 6))
        );
        assert_eq!(devices[1].compute_capability, None);
    }

    #[test]
    fn unsupported_compute_query_preserves_legacy_gpu_observation() {
        assert!(compute_cap_query_is_unsupported(
            b"Field 'compute_cap' is not a valid field to query."
        ));
        let devices = parse_nvidia_smi_devices("GPU-one, NVIDIA One, 24576, 550.54.14\n", false);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].stable_id.as_deref(), Some("GPU-one"));
        assert_eq!(devices[0].compute_capability, None);
    }

    #[test]
    fn ordered_uuid_visibility_is_preserved_and_single_device_consumers_stay_strict() {
        let first_uuid = "GPU-11111111-1111-1111-1111-111111111111";
        let second_uuid = "GPU-22222222-2222-2222-2222-222222222222";
        let device = |uuid: &str, name: &str| AcceleratorDevice {
            accelerator: "cuda".to_owned(),
            stable_id: Some(uuid.to_owned()),
            name: Some(name.to_owned()),
            vram_bytes: Some(24 * 1024 * 1024 * 1024),
            driver_version: Some("580.1".to_owned()),
            compute_capability: Some(ComputeCapability::new(8, 9)),
        };
        let host = HostCapabilities {
            platform: "linux".to_owned(),
            architecture: "x86_64".to_owned(),
            accelerators: vec![device(first_uuid, "first"), device(second_uuid, "second")],
            nvidia_gpu_absence_confirmed: false,
            cuda_visible_devices: Some(format!("{second_uuid},{first_uuid}")),
            observations: Vec::new(),
        };

        let visible = visible_nvidia_device_set(&host, "fixture").expect("ordered devices");
        assert_eq!(visible[0].stable_id.as_deref(), Some(second_uuid));
        assert_eq!(visible[1].stable_id.as_deref(), Some(first_uuid));
        assert!(visible_nvidia_devices(&host, "single-device fixture").is_err());

        let mut unconstrained = host.clone();
        unconstrained.cuda_visible_devices = None;
        assert_eq!(
            visible_nvidia_devices(&unconstrained, "single-device fixture")
                .expect("candidates for automatic single-device selection")
                .len(),
            2
        );

        let binding = AcceleratorBinding {
            devices: visible.into_iter().cloned().collect(),
        };
        let environment = isolated_cuda_environment_for_binding(
            &BTreeMap::from([("ORDINARY".to_owned(), "kept".to_owned())]),
            &binding,
            "fixture",
        )
        .expect("isolated environment");
        assert_eq!(
            environment["CUDA_VISIBLE_DEVICES"],
            format!("{second_uuid},{first_uuid}")
        );
        assert_eq!(environment["ORDINARY"], "kept");

        let mut duplicate = host;
        duplicate.cuda_visible_devices = Some(format!("{first_uuid},{first_uuid}"));
        assert!(visible_nvidia_device_set(&duplicate, "fixture").is_err());
    }

    #[test]
    fn advisory_notes_do_not_downgrade_verified_compatibility() {
        let requirements = RuntimeRequirements {
            advisories: vec!["informational guidance".to_owned()],
            ..RuntimeRequirements::default()
        };
        let host = HostCapabilities {
            platform: "linux".to_owned(),
            architecture: "x86_64".to_owned(),
            accelerators: vec![AcceleratorDevice {
                accelerator: "cuda".to_owned(),
                stable_id: Some("GPU-fixture".to_owned()),
                name: None,
                vram_bytes: Some(24 * 1024 * 1024 * 1024),
                driver_version: None,
                compute_capability: None,
            }],
            nvidia_gpu_absence_confirmed: false,
            cuda_visible_devices: None,
            observations: Vec::new(),
        };
        assert_eq!(
            compatibility_for("linux", "x86_64", "cuda", &requirements, &host),
            RuntimeCompatibility::Recommended
        );

        let legacy = RuntimeRequirements {
            notes: vec!["legacy unverified condition".to_owned()],
            ..RuntimeRequirements::default()
        };
        assert!(matches!(
            compatibility_for("linux", "x86_64", "cuda", &legacy, &host),
            RuntimeCompatibility::NeedsAttention(_)
        ));
    }

    #[test]
    fn nominal_vram_classes_allow_reporting_shortfall_but_reject_lower_classes() {
        let requirements = RuntimeRequirements {
            requires_nvidia_gpu: true,
            minimum_vram_class_gib: Some(32),
            ..RuntimeRequirements::default()
        };
        let host = |gib: u64| HostCapabilities {
            platform: "linux".to_owned(),
            architecture: "x86_64".to_owned(),
            accelerators: vec![AcceleratorDevice {
                accelerator: "cuda".to_owned(),
                stable_id: Some("GPU-fixture".to_owned()),
                name: None,
                vram_bytes: Some(gib * 1024 * 1024 * 1024),
                driver_version: None,
                compute_capability: None,
            }],
            nvidia_gpu_absence_confirmed: false,
            cuda_visible_devices: None,
            observations: Vec::new(),
        };
        assert!(matches!(
            compatibility_for("linux", "x86_64", "cuda", &requirements, &host(30)),
            RuntimeCompatibility::Recommended
        ));
        assert!(matches!(
            compatibility_for("linux", "x86_64", "cuda", &requirements, &host(29)),
            RuntimeCompatibility::NeedsAttention(_)
        ));
        assert!(matches!(
            compatibility_for("linux", "x86_64", "cuda", &requirements, &host(24)),
            RuntimeCompatibility::Incompatible(_)
        ));
    }

    #[test]
    fn github_redirect_hosts_are_restricted() {
        for allowed in [
            "https://github.com/owner/repo/releases/download/v1/a.zip",
            "https://api.github.com/repos/owner/repo/releases",
            "https://raw.githubusercontent.com/owner/repo/commit/Makefile",
            "https://release-assets.githubusercontent.com/file",
        ] {
            assert!(is_allowed_github_host(&allowed.parse().expect("URL")));
        }
        for rejected in [
            "http://github.com/file",
            "https://user:password@github.com/file",
            "https://github.com:444/file",
            "https://github.com.evil.example/file",
            "https://example.com/file",
        ] {
            assert!(!is_allowed_github_host(&rejected.parse().expect("URL")));
        }
    }

    #[test]
    fn provider_authority_is_bound_while_derived_channels_are_not_install_identity() {
        let identity = RuntimeIdentity {
            engine_id: "fixture".to_owned(),
            package_family: "fixture-pack".to_owned(),
            version: "v1".to_owned(),
            upstream_revision: Some("revision".to_owned()),
            platform: "linux".to_owned(),
            architecture: "x86_64".to_owned(),
            accelerator: "cpu".to_owned(),
            variant: "default".to_owned(),
            package: RuntimePackageIdentity {
                provider_id: "fixture-provider".to_owned(),
                repository: Some("owner/repository".to_owned()),
                release_tag: Some("v1".to_owned()),
                asset_id: Some("1".to_owned()),
                asset_name: Some("fixture.zip".to_owned()),
                additional_assets: Vec::new(),
            },
        };
        let broad = AvailableRuntime {
            runtime_id: scala_core::RuntimeId::from_identity(&identity),
            identity,
            display_name: "Fixture".to_owned(),
            supported_formats: vec![ArtifactFormat::Gguf],
            source_url: "https://github.com/owner/repository/releases/tag/v1".to_owned(),
            published_at_unix: Some(1),
            channels: vec![RuntimeReleaseChannel::Stable, RuntimeReleaseChannel::Latest],
            prerelease: false,
            acquisition: scala_core::RuntimeAcquisitionPlan::ReleaseAsset {
                download: RuntimeDownload {
                    url: "https://github.com/owner/repository/releases/download/v1/fixture.zip"
                        .to_owned(),
                    size_bytes: 1,
                    digest: Some(RuntimeDigest::sha256("a".repeat(64)).expect("digest")),
                    archive_format: RuntimeArchiveFormat::Zip,
                    entrypoint_names: vec!["server".to_owned()],
                },
                additional_downloads: Vec::new(),
            },
            supported_native_identities: Vec::new(),
            requirements: RuntimeRequirements::default(),
        };
        let authority = RuntimeProviderAuthority {
            provider_id: "fixture-provider".to_owned(),
            engine_id: "fixture".to_owned(),
            repository: "owner/repository".to_owned(),
        };
        authority.validate(&broad).expect("authoritative runtime");
        let mut exact = broad.clone();
        exact.channels.clear();
        assert!(same_install_candidate(&broad, &exact));
        exact.identity.package.repository = Some("attacker/repository".to_owned());
        assert!(authority.validate(&exact).is_err());
    }

    #[test]
    fn refreshed_catalog_retains_the_previously_selected_source_candidate() {
        let previous_runtime = source_candidate('a', 1);
        let current_runtime = source_candidate('b', 2);
        let previous = ProviderCache {
            schema_version: 1,
            provider_id: "fixture-source".to_owned(),
            fetched_at_unix: 1,
            runtimes: vec![previous_runtime.clone()],
            historical_source_runtimes: Vec::new(),
        };
        let historical = historical_source_candidates(Some(&previous), &[current_runtime]);
        assert_eq!(historical, [previous_runtime]);
    }

    fn source_candidate(revision: char, published_at_unix: i64) -> AvailableRuntime {
        let commit_sha = revision.to_string().repeat(40);
        let tree_sha = if revision == 'a' { 'c' } else { 'd' }
            .to_string()
            .repeat(40);
        let identity = RuntimeIdentity {
            engine_id: "fixture".to_owned(),
            package_family: "fixture-source".to_owned(),
            version: format!("git-{revision}"),
            upstream_revision: Some(commit_sha.clone()),
            platform: "linux".to_owned(),
            architecture: "x86_64".to_owned(),
            accelerator: "cuda".to_owned(),
            variant: "recipe-v1".to_owned(),
            package: RuntimePackageIdentity {
                provider_id: "fixture-source".to_owned(),
                repository: Some("owner/repository".to_owned()),
                release_tag: None,
                asset_id: None,
                asset_name: None,
                additional_assets: Vec::new(),
            },
        };
        AvailableRuntime {
            runtime_id: scala_core::RuntimeId::from_identity(&identity),
            identity,
            display_name: "Fixture source".to_owned(),
            supported_formats: vec![ArtifactFormat::Ninfer],
            source_url: format!("https://github.com/owner/repository/commit/{commit_sha}"),
            published_at_unix: Some(published_at_unix),
            channels: vec![RuntimeReleaseChannel::Latest],
            prerelease: false,
            acquisition: RuntimeAcquisitionPlan::SourceBuild(Box::new(RuntimeSourceBuildPlan {
                source: RuntimeSourceSnapshot {
                    repository: "owner/repository".to_owned(),
                    repository_url: "https://github.com/owner/repository.git".to_owned(),
                    source_branch: "master".to_owned(),
                    commit_sha,
                    tree_sha,
                    commit_timestamp_unix: published_at_unix,
                    source_provider: "fixture-source".to_owned(),
                },
                recipe: RuntimeSourceBuildRecipe {
                    recipe_version: "recipe-v1".to_owned(),
                    build_system: RuntimeSourceBuildSystem::Cmake,
                    build_definition_sha256: None,
                    cmake_configuration_arguments: vec!["-G".to_owned(), "Ninja".to_owned()],
                    build_target: "server".to_owned(),
                    entrypoint: "build/server".into(),
                    accelerator_target: "sm_test".to_owned(),
                    rejected_build_environment: Vec::new(),
                },
                prerequisites: RuntimeSourceBuildPrerequisites {
                    minimum_cmake_version: "3.28".to_owned(),
                    minimum_cuda_version: Some("13.1".to_owned()),
                    maximum_cuda_version_exclusive: None,
                    requires_ninja: true,
                    requires_cpp20_compiler: true,
                    requires_make: false,
                    minimum_cpp_standard: Some(20),
                    cpp_compiler: None,
                    cuda_compiler: None,
                    requires_pkg_config: true,
                    pkg_config_modules: BTreeMap::new(),
                },
            })),
            supported_native_identities: Vec::new(),
            requirements: RuntimeRequirements::default(),
        }
    }
}
