//! Shared acquisition metadata for engine-owned, embedded Python recipes.
use scala_core::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub struct ManagedPythonAsset {
    pub name: &'static str,
    pub bytes: &'static [u8],
}

pub fn common_python_assets() -> Vec<ManagedPythonAsset> {
    vec![
        ManagedPythonAsset {
            name: "bootstrap.json",
            bytes: include_bytes!("../runtime/managed/bootstrap.json"),
        },
        ManagedPythonAsset {
            name: "provision.py",
            bytes: include_bytes!("../runtime/managed/provision.py"),
        },
        ManagedPythonAsset {
            name: "crates/scala-engine/runtime/python_integrity.py",
            bytes: include_bytes!("../runtime/python_integrity.py"),
        },
    ]
}

pub fn python_recipe_digest(recipe_id: &str, assets: &[ManagedPythonAsset]) -> String {
    let mut hash = Sha256::new();
    hash.update(recipe_id.as_bytes());
    for asset in assets {
        hash.update((asset.name.len() as u64).to_le_bytes());
        hash.update(asset.name.as_bytes());
        hash.update((asset.bytes.len() as u64).to_le_bytes());
        hash.update(asset.bytes);
    }
    format!("{:x}", hash.finalize())
}

pub fn python_candidates(
    engine: &str,
    provider: &str,
    repository: &str,
    assets: &[ManagedPythonAsset],
) -> Vec<AvailableRuntime> {
    let bootstrap: serde_json::Value = serde_json::from_slice(
        assets
            .iter()
            .find(|a| a.name == "bootstrap.json")
            .expect("embedded bootstrap")
            .bytes,
    )
    .expect("reviewed JSON");
    let mut candidates = Vec::new();
    for platform in ["linux", "windows"] {
        let backends: &[&str] = if engine == "vllm" {
            &["vllm-openai", "vllm-labels"]
        } else {
            &["torch-readout"]
        };
        for backend in backends {
            let recipe_id = format!("{engine}-{backend}-{platform}-x86_64-v1");
            let digest = python_recipe_digest(&recipe_id, assets);
            let name = format!(
                "crates/scala-engine-{engine}/runtime/{}/{}-requirements.lock",
                if platform == "linux" {
                    "linux"
                } else {
                    "windows"
                },
                if engine == "vllm" { "h2o" } else { "imajev" }
            );
            let lock = assets
                .iter()
                .find(|a| a.name == name)
                .expect("embedded dependency lock");
            let version = if engine == "vllm" {
                if platform == "linux" {
                    "0.30.0"
                } else {
                    "0.29.0+cu132"
                }
            } else if platform == "linux" {
                "2.14.1"
            } else {
                "2.14.1+cu130"
            };
            let plan = RuntimePythonPlan {
                recipe_id,
                recipe_sha256: digest.clone(),
                backend: backend.to_string(),
                dependencies: std::str::from_utf8(lock.bytes)
                    .expect("UTF8 lock")
                    .lines()
                    .filter_map(|line| {
                        if line.starts_with(char::is_whitespace) || line.starts_with('#') {
                            return None;
                        }
                        if let Some((package, version)) = line.split_once("==") {
                            Some((package.into(), version.split_whitespace().next()?.into()))
                        } else {
                            let (package, url) = line.split_once(" @ ")?;
                            let filename = url.split_whitespace().next()?.rsplit('/').next()?;
                            Some((
                                package.into(),
                                filename.split('-').nth(1)?.replace("%2B", "+"),
                            ))
                        }
                    })
                    .collect(),
                dependency_lock_sha256: format!("{:x}", Sha256::digest(lock.bytes)),
                python: serde_json::from_value(bootstrap["python"][platform].clone())
                    .expect("Python archive"),
                package_tool: serde_json::from_value(bootstrap["uv"][platform].clone())
                    .expect("uv archive"),
                source: (engine == "pytorch").then(|| {
                    serde_json::from_value(bootstrap["imajev_source"].clone())
                        .expect("source archive")
                }),
                source_revision: if engine == "pytorch" {
                    Some("91729bff7a806187324e287fd0d730839dc32026".into())
                } else if *backend == "vllm-labels" {
                    Some("193ad740925b176a3b70a5a13a7cff2f2fadd01e".into())
                } else {
                    None
                },
                minimum_glibc: (platform == "linux")
                    .then(|| if engine == "vllm" { "2.31" } else { "2.28" }.into()),
                estimated_installed_bytes: None,
            };
            let identity = RuntimeIdentity {
                engine_id: engine.into(),
                package_family: format!("{engine}-managed-python"),
                version: version.into(),
                upstream_revision: None,
                platform: platform.into(),
                architecture: "x86_64".into(),
                accelerator: "cuda".into(),
                variant: backend.to_string(),
                package: RuntimePackageIdentity {
                    provider_id: provider.into(),
                    repository: Some(repository.into()),
                    release_tag: None,
                    asset_id: Some(digest),
                    asset_name: Some(plan.recipe_id.clone()),
                    additional_assets: Vec::new(),
                },
            };
            candidates.push(AvailableRuntime {
                runtime_id: RuntimeId::from_identity(&identity), identity,
                display_name: format!("{engine} {version} · {backend} · managed Python 3.12.12"),
                supported_formats: vec![if *backend == "vllm-openai" { ArtifactFormat::HuggingFace } else { ArtifactFormat::DecisionBundle }],
                supported_native_identities: Vec::new(),
                source_url: format!("https://github.com/{repository}"), published_at_unix: None,
                channels: vec![RuntimeReleaseChannel::Stable, RuntimeReleaseChannel::Latest], prerelease: false,
                acquisition: RuntimeAcquisitionPlan::ManagedPython(Box::new(plan)),
                requirements: RuntimeRequirements {
                    requires_nvidia_gpu: true, minimum_nvidia_driver: Some(if platform == "windows" { "610.62" } else { "580.65.06" }.into()),
                    supported_cuda_compute_capabilities: vec![ComputeCapability::new(8,0), ComputeCapability::new(8,6), ComputeCapability::new(8,9), ComputeCapability::new(12,0)],
                    advisories: vec!["Exact hashed wheel closure; isolated CPython and private uv; no model weights acquired or loaded. Model memory suitability is checked separately.".into(),
                        if platform == "windows" && engine == "vllm" { "Native Windows pinned compatibility port; private CUDA 13.3 SDK; existing Visual Studio 2019+ x64 C++ tools required for native JIT.".into() } else { "CUDA 13 wheel libraries are owned by this runtime; the NVIDIA driver remains a host prerequisite.".into() }],
                    ..RuntimeRequirements::default()
                },
            });
        }
    }
    candidates
}

pub fn python_install_environment(
    root: &std::path::Path,
    cache: &std::path::Path,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for key in ["SystemRoot", "WINDIR", "COMSPEC"] {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.into(), value);
        }
    }
    env.insert(
        "UV_CACHE_DIR".into(),
        cache.join("uv").display().to_string(),
    );
    env.insert("UV_NO_CONFIG".into(), "1".into());
    env.insert("UV_PYTHON_DOWNLOADS".into(), "never".into());
    env.insert("UV_LINK_MODE".into(), "copy".into());
    env.insert("PYTHONDONTWRITEBYTECODE".into(), "1".into());
    for key in ["HOME", "USERPROFILE", "TEMP", "TMP", "TMPDIR"] {
        env.insert(key.into(), root.join("work").display().to_string());
    }
    env
}
