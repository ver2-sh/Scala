//! Synthetic acquisition/lifecycle fixtures. No Python packages, GPU or model loads.
use crate::python_runtime::{ManagedPythonAsset, python_recipe_digest};
use crate::*;
use async_trait::async_trait;
use scala_core::*;
use sha2::{Digest, Sha256};
use std::result::Result;
use std::sync::Arc;

fn assets() -> Vec<ManagedPythonAsset> {
    vec![ManagedPythonAsset {
        name: "provision.py",
        bytes: b"synthetic fixture; real Python is never invoked",
    }]
}
struct Adapter {
    reject: bool,
}
#[async_trait]
impl EngineAdapter for Adapter {
    fn identity(&self) -> EngineIdentity {
        EngineIdentity {
            id: "pytorch".into(),
            display_name: "fixture".into(),
            upstream_repository: "owner/fixture".into(),
        }
    }
    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            artifact_formats: vec![ArtifactFormat::DecisionBundle],
            api: vec![],
            features: vec![],
        }
    }
    fn native_options(&self) -> Vec<NativeOption> {
        vec![]
    }
    fn managed_python_assets(&self) -> Vec<ManagedPythonAsset> {
        assets()
    }
    async fn managed_python_observation_key(&self, runtime: &InstalledRuntime) -> Option<String> {
        crate::local_runtime_observation_key(vec![runtime.entrypoint_path()]).await
    }
    async fn admit_managed_python(
        &self,
        runtime: &InstalledRuntime,
    ) -> Result<RuntimeProbeObservation, EngineError> {
        if self.reject {
            return Err(EngineError::InvalidConfiguration(
                "synthetic defective wheel RECORD".into(),
            ));
        }
        if !runtime.entrypoint_path().is_file() {
            return Err(EngineError::NotInstalled);
        }
        Ok(RuntimeProbeObservation {
            compatible: true,
            observed_engine_id: "pytorch".into(),
            observed_version: Some(runtime.manifest.identity.version.clone()),
            observed_revision: Some("b".repeat(64)),
            detail: "synthetic admission".into(),
            observed_at_unix: 1,
        })
    }
    async fn probe_runtime(
        &self,
        runtime: &InstalledRuntime,
    ) -> Result<RuntimeProbeObservation, EngineError> {
        self.admit_managed_python(runtime).await
    }
    async fn probe(&self) -> Result<EngineProbe, EngineError> {
        Ok(EngineProbe {
            installation: InstallationState::NotInstalled,
            update: UpdateState::Unknown,
            healthy: false,
            detail: "fixture".into(),
        })
    }
    async fn effective_generation_settings(
        &self,
        _: &ProcessDescriptor,
    ) -> Result<EffectiveGenerationSettings, EngineError> {
        unreachable!()
    }
    async fn build_launch_spec(&self, _: LaunchRequest) -> Result<LaunchSpec, EngineError> {
        unreachable!()
    }
    async fn health(&self, _: &ProcessDescriptor) -> Result<bool, EngineError> {
        unreachable!()
    }
    async fn infer(&self, _: &str, _: InferenceRequest) -> Result<InferenceOutput, EngineError> {
        unreachable!()
    }
    async fn infer_stream(
        &self,
        _: &str,
        _: InferenceRequest,
        _: InferenceActivityReporter,
    ) -> Result<InferenceStream, EngineError> {
        unreachable!()
    }
}
struct Provider(Vec<AvailableRuntime>);
#[async_trait]
impl RuntimeCatalogProvider for Provider {
    fn id(&self) -> &'static str {
        "python-fixture"
    }
    fn engine_id(&self) -> &'static str {
        "pytorch"
    }
    fn repository(&self) -> &'static str {
        "owner/fixture"
    }
    async fn fetch(&self, _: &GitHubReleaseClient) -> Result<Vec<AvailableRuntime>, CatalogError> {
        Ok(self.0.clone())
    }
    async fn fetch_reference(
        &self,
        _: &GitHubReleaseClient,
        reference: &str,
    ) -> Result<Vec<AvailableRuntime>, CatalogError> {
        Ok(self
            .0
            .iter()
            .filter(|c| c.runtime_id.as_str() == reference)
            .cloned()
            .collect())
    }
}
fn download(body: &[u8], name: &str, archive_format: RuntimeArchiveFormat) -> RuntimeDownload {
    RuntimeDownload {
        url: format!("https://github.com/owner/fixture/{name}"),
        size_bytes: body.len() as u64,
        digest: Some(RuntimeDigest::sha256(format!("{:x}", Sha256::digest(body))).unwrap()),
        archive_format,
        entrypoint_names: vec!["uv".into()],
    }
}
fn candidate(python: &[u8], uv: &[u8], version: &str) -> AvailableRuntime {
    let recipe_id = format!("fixture-{version}");
    let recipe_sha256 = python_recipe_digest(&recipe_id, &assets());
    let identity = RuntimeIdentity {
        engine_id: "pytorch".into(),
        package_family: "pytorch-managed-python".into(),
        version: version.into(),
        upstream_revision: Some(recipe_sha256.clone()),
        platform: "linux".into(),
        architecture: "x86_64".into(),
        accelerator: "cpu".into(),
        variant: "torch-readout".into(),
        package: RuntimePackageIdentity {
            provider_id: "python-fixture".into(),
            repository: Some("owner/fixture".into()),
            release_tag: None,
            asset_id: Some(recipe_sha256.clone()),
            asset_name: Some(recipe_id.clone()),
            additional_assets: vec![],
        },
    };
    AvailableRuntime {
        runtime_id: RuntimeId::from_identity(&identity),
        identity,
        display_name: "fixture".into(),
        supported_formats: vec![ArtifactFormat::DecisionBundle],
        source_url: "https://github.com/owner/fixture".into(),
        published_at_unix: None,
        channels: vec![RuntimeReleaseChannel::Latest, RuntimeReleaseChannel::Stable],
        prerelease: false,
        supported_native_identities: vec![],
        requirements: RuntimeRequirements::default(),
        acquisition: RuntimeAcquisitionPlan::ManagedPython(Box::new(RuntimePythonPlan {
            recipe_id,
            recipe_sha256,
            backend: "torch-readout".into(),
            dependency_lock_sha256: "c".repeat(64),
            dependencies: Default::default(),
            python: download(python, "python", RuntimeArchiveFormat::TarGz),
            package_tool: download(uv, "uv", RuntimeArchiveFormat::Zip),
            source: None,
            source_revision: None,
            minimum_glibc: None,
            estimated_installed_bytes: None,
        })),
    }
}
fn archives(cancel: bool) -> (Vec<u8>, Vec<u8>) {
    use std::io::Write;
    let script = if cancel {
        "#!/bin/sh\n/bin/mkdir -p \"$5/runner\"\n/bin/sleep 30 &\necho acquiring-fixture-wheel >&2\nwait\n"
    } else {
        "#!/bin/sh\n/bin/mkdir -p \"$5/runner\"\necho synthetic > \"$5/runner/pytorch-server\"\necho acquiring-fixture-wheel >&2\n"
    };
    let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut archive = tar::Builder::new(gzip);
    let mut header = tar::Header::new_gnu();
    header.set_size(script.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    archive
        .append_data(&mut header, "python/bin/python3.12", script.as_bytes())
        .unwrap();
    let python = archive.into_inner().unwrap().finish().unwrap();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file("uv", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
    (python, zip.finish().unwrap().into_inner())
}
async fn fixture(
    cancel: bool,
    reject: bool,
) -> (
    tempfile::TempDir,
    crate::test_support::HttpFixture,
    Arc<RuntimePackManager>,
    Vec<AvailableRuntime>,
) {
    let dir = tempfile::tempdir().unwrap();
    let paths = crate::test_support::paths(dir.path());
    let (python, uv) = archives(cancel);
    let candidates = vec![
        candidate(&python, &uv, "1.0.0"),
        candidate(&python, &uv, "2.0.0"),
    ];
    let server = crate::test_support::HttpFixture::new(move |path| {
        (
            200,
            vec![],
            if path.ends_with("/python") {
                python.clone()
            } else {
                uv.clone()
            },
        )
    })
    .await;
    let mut registry = EngineRegistry::default();
    registry.register(Arc::new(Adapter { reject })).unwrap();
    let mut manager = RuntimePackManager::new(
        &paths,
        registry,
        vec![Arc::new(Provider(candidates.clone())) as Arc<dyn RuntimeCatalogProvider>],
    )
    .unwrap();
    Arc::get_mut(&mut manager)
        .unwrap()
        .set_test_download_origin(server.origin.clone());
    (dir, server, manager, candidates)
}
#[tokio::test]
async fn managed_acquisition_activation_side_by_side_update_leases_and_cache() {
    let (dir, server, manager, candidates) = fixture(false, false).await;
    let first = manager.install(&candidates[0].runtime_id).await.unwrap();
    first.validate().unwrap();
    assert_eq!(
        first
            .manifest
            .python_environment
            .as_ref()
            .unwrap()
            .environment_revision,
        "b".repeat(64)
    );
    let updates = manager.check_updates().await.unwrap();
    assert!(matches!(
        &updates[0].state,
        RuntimeUpdateState::NewerCompatibleVersion { .. }
    ));
    let second = manager.update(&first.manifest.runtime_id).await.unwrap();
    assert_ne!(first.installation_root, second.installation_root);
    assert!(first.entrypoint_path().exists());
    assert_eq!(
        server.paths().len(),
        2,
        "verified bootstrap cache must be reused"
    );
    let lease = manager
        .acquire_runtime_lease(&first.manifest.runtime_id)
        .await
        .unwrap();
    assert!(
        manager
            .remove(&first.manifest.runtime_id, None)
            .await
            .is_err()
    );
    drop(lease);
    let user_model = dir.path().join("model.safetensors");
    std::fs::write(&user_model, b"owned by user").unwrap();
    manager.clean_python_cache().await.unwrap();
    assert!(first.entrypoint_path().exists());
    assert!(second.entrypoint_path().exists());
    assert!(user_model.exists());
    manager
        .remove(&first.manifest.runtime_id, None)
        .await
        .unwrap();
    assert!(!first.installation_root.exists());
    assert!(second.entrypoint_path().exists());
    assert!(user_model.exists());
}
#[tokio::test]
async fn cancellation_never_registers_partial_python_and_retry_recovers() {
    let (_dir, _server, manager, candidates) = fixture(true, false).await;
    let mut progress = manager.progress();
    let task = {
        let manager = manager.clone();
        let id = candidates[0].runtime_id.clone();
        tokio::spawn(async move { manager.install(&id).await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if progress
                .recv()
                .await
                .unwrap()
                .detail
                .contains("acquiring-fixture-wheel")
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    task.abort();
    let _ = task.await;
    assert!(manager.list().await.unwrap().installed.is_empty());
    let paths = crate::test_support::paths(_dir.path());
    assert!(
        !walkdir::WalkDir::new(paths.runtimes_dir)
            .into_iter()
            .filter_map(Result::ok)
            .any(|e| e.file_name() == candidates[0].runtime_id.as_str())
    );
}

#[tokio::test]
async fn failed_admission_never_registers_an_environment() {
    let (dir, _server, manager, candidates) = fixture(false, true).await;
    let error = manager
        .install(&candidates[0].runtime_id)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("defective wheel RECORD"));
    assert!(manager.list().await.unwrap().installed.is_empty());
    let paths = crate::test_support::paths(dir.path());
    assert!(
        !walkdir::WalkDir::new(paths.runtimes_dir)
            .into_iter()
            .filter_map(Result::ok)
            .any(|e| e.file_name() == candidates[0].runtime_id.as_str())
    );
}
#[tokio::test]
async fn interrupted_python_acquisition_is_recovered_only_with_matching_ownership() {
    let (dir, _server, manager, candidates) = fixture(false, false).await;
    let first = manager.install(&candidates[0].runtime_id).await.unwrap();
    manager
        .remove(&first.manifest.runtime_id, None)
        .await
        .unwrap();
    let store = RuntimeStore::new(&crate::test_support::paths(dir.path()));
    let staging = store.create_python_staging(&first.manifest).await.unwrap();
    let path = staging.path().to_owned();
    std::fs::write(path.join("unfinished"), "disposable").unwrap();
    std::mem::forget(staging); // Simulate process interruption, not an installed runtime.
    assert!(manager.list().await.unwrap().installed.is_empty());
    let recovered = manager.install(&candidates[0].runtime_id).await.unwrap();
    assert!(!recovered.installation_root.join("unfinished").exists());
    manager
        .remove(&recovered.manifest.runtime_id, None)
        .await
        .unwrap();
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("user-file"), "must retain").unwrap();
    assert!(manager.install(&candidates[0].runtime_id).await.is_err());
    assert!(path.join("user-file").exists());
}

struct UnrelatedProvider;
#[async_trait]
impl RuntimeCatalogProvider for UnrelatedProvider {
    fn id(&self) -> &'static str {
        "unrelated-fixture"
    }
    fn engine_id(&self) -> &'static str {
        "unrelated-engine"
    }
    fn repository(&self) -> &'static str {
        "owner/unrelated"
    }
    async fn fetch(&self, _: &GitHubReleaseClient) -> Result<Vec<AvailableRuntime>, CatalogError> {
        panic!("engine-specific and model-first searches must not contact unrelated providers")
    }
}
#[tokio::test]
async fn python_searches_skip_unrelated_network_providers() {
    let dir = tempfile::tempdir().unwrap();
    let (python, uv) = archives(false);
    let available = candidate(&python, &uv, "1.0.0");
    let catalog = RuntimeCatalog::new(
        dir.path().join("catalog"),
        vec![
            Arc::new(Provider(vec![available])) as Arc<dyn RuntimeCatalogProvider>,
            Arc::new(UnrelatedProvider),
        ],
    )
    .unwrap();
    let host = HostCapabilities::current_without_accelerator_probe();
    assert_eq!(
        catalog.search("pytorch", &host, true).await.entries.len(),
        1
    );
    let engines = ["pytorch".into()].into_iter().collect();
    assert_eq!(
        catalog
            .search_for_engines("decisionbundle", &host, true, Some(&engines))
            .await
            .entries
            .len(),
        1
    );
}
