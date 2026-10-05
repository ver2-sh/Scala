//! Local HTTP fixtures; these are absent from production builds.
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use scala_core::{AppPaths, AvailableRuntime, RuntimeId};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{CatalogError, GitHubRelease, GitHubReleaseClient, RuntimeCatalogProvider};

pub(crate) struct HttpFixture {
    pub origin: reqwest::Url,
    pub requests: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl HttpFixture {
    pub async fn new(
        respond: impl Fn(&str) -> (u16, Vec<(&'static str, String)>, Vec<u8>) + Send + 'static,
    ) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    if socket.read_exact(&mut byte).await.is_err() {
                        break;
                    }
                    bytes.push(byte[0]);
                    assert!(bytes.len() < 16 * 1024);
                }
                let request = String::from_utf8(bytes).unwrap();
                let path = request.split_whitespace().nth(1).unwrap();
                recorded.lock().unwrap().push(request.clone());
                let (status, headers, body) = respond(path);
                let mut head = format!(
                    "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                for (name, value) in headers {
                    head.push_str(&format!("{name}: {value}\r\n"));
                }
                head.push_str("\r\n");
                socket.write_all(head.as_bytes()).await.unwrap();
                socket.write_all(&body).await.unwrap();
            }
        });
        Self {
            origin,
            requests,
            task,
        }
    }

    pub fn paths(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.split_whitespace().nth(1).unwrap().to_owned())
            .collect()
    }
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(crate) fn paths(root: &std::path::Path) -> AppPaths {
    AppPaths {
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
    }
}

pub(crate) fn release(body: &[u8]) -> serde_json::Value {
    let digest = format!("{:x}", sha2::Sha256::digest(body));
    json!({
        "id": 1, "tag_name": "v1", "name": "fixture", "draft": false, "prerelease": false,
        "html_url": "https://github.com/owner/repository/releases/tag/v1",
        "target_commitish": "1111111111111111111111111111111111111111",
        "published_at": null,
        "assets": [{"id": 7, "name": "fixture.zip", "size": body.len(), "state": "uploaded",
        "digest": format!("sha256:{digest}"),
        "browser_download_url": "https://github.com/owner/repository/releases/download/v1/fixture.zip"}]
    })
}

use sha2::Digest;

pub(crate) fn runtime(release: &GitHubRelease) -> AvailableRuntime {
    let asset = &release.assets[0];
    use scala_core::{
        ArtifactFormat, RuntimeAcquisitionPlan, RuntimeArchiveFormat, RuntimeDigest,
        RuntimeDownload, RuntimeIdentity, RuntimePackageIdentity, RuntimeRequirements,
    };
    let identity = RuntimeIdentity {
        engine_id: "fixture-release".into(),
        package_family: "fixture-release".into(),
        version: "v1".into(),
        upstream_revision: Some(release.target_commitish.clone()),
        platform: "linux".into(),
        architecture: "x86_64".into(),
        accelerator: "cpu".into(),
        variant: "default".into(),
        package: RuntimePackageIdentity {
            provider_id: "fixture-provider".into(),
            repository: Some("owner/repository".into()),
            release_tag: Some(release.tag_name.clone()),
            asset_id: Some(asset.id.to_string()),
            asset_name: Some(asset.name.clone()),
            additional_assets: Vec::new(),
        },
    };
    let runtime = AvailableRuntime {
        runtime_id: RuntimeId::from_identity(&identity),
        identity,
        display_name: "Fixture".into(),
        supported_formats: vec![ArtifactFormat::Gguf],
        source_url: release.html_url.clone(),
        published_at_unix: None,
        channels: Vec::new(),
        prerelease: false,
        acquisition: RuntimeAcquisitionPlan::ReleaseAsset {
            download: RuntimeDownload {
                url: asset.browser_download_url.clone(),
                size_bytes: asset.size,
                digest: Some(
                    RuntimeDigest::parse_github(asset.digest.as_deref().unwrap()).unwrap(),
                ),
                archive_format: RuntimeArchiveFormat::Zip,
                entrypoint_names: vec!["server".into()],
            },
            additional_downloads: Vec::new(),
        },
        supported_native_identities: Vec::new(),
        requirements: RuntimeRequirements::default(),
    };
    runtime.validate().unwrap();
    runtime
}

pub(crate) struct ReleaseProvider;

#[async_trait]
impl RuntimeCatalogProvider for ReleaseProvider {
    fn id(&self) -> &'static str {
        "fixture-provider"
    }
    fn engine_id(&self) -> &'static str {
        "fixture-release"
    }
    fn repository(&self) -> &'static str {
        "owner/repository"
    }
    async fn fetch(
        &self,
        github: &GitHubReleaseClient,
    ) -> Result<Vec<AvailableRuntime>, CatalogError> {
        Ok(github
            .releases(self.repository())
            .await?
            .iter()
            .map(runtime)
            .collect())
    }
    async fn fetch_reference(
        &self,
        github: &GitHubReleaseClient,
        _: &str,
    ) -> Result<Vec<AvailableRuntime>, CatalogError> {
        Ok(github
            .release_by_tag(self.repository(), "v1")
            .await?
            .iter()
            .map(runtime)
            .collect())
    }
}
