use std::io::Read;
use std::path::{Path, PathBuf};

use norted_core::{
    AppPaths, InstalledRuntime, RUNTIME_SELECTIONS_SCHEMA_VERSION, RuntimeAcquisitionMethod,
    RuntimeId, RuntimeManifest, RuntimeSelections,
};
use sha2::{Digest, Sha256};

use crate::catalog::atomic_write;

pub const RUNTIME_MANIFEST_FILE: &str = "runtime.json";

#[derive(Debug, thiserror::Error)]
pub enum RuntimeStoreError {
    #[error("runtime store I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("runtime manifest at {path} is invalid: {message}")]
    InvalidManifest { path: PathBuf, message: String },
    #[error("runtime `{0}` is not installed")]
    NotInstalled(RuntimeId),
    #[error("runtime `{0}` is leased by a loading or running backend")]
    RuntimeInUse(RuntimeId),
    #[error("runtime `{runtime_id}` is selected for {selection}")]
    RuntimeSelected {
        runtime_id: RuntimeId,
        selection: String,
    },
    #[error("external runtime `{0}` is unmanaged and cannot be removed by Norted")]
    ExternalRuntime(RuntimeId),
    #[error("runtime path failed the store containment check: {0}")]
    UnsafePath(PathBuf),
    #[error("runtime `{runtime_id}` has a provenance conflict: {detail}")]
    ProvenanceConflict {
        runtime_id: RuntimeId,
        detail: String,
    },
    #[error("runtime selections are invalid: {0}")]
    InvalidSelections(String),
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeStoreSnapshot {
    pub runtimes: Vec<InstalledRuntime>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RuntimeStore {
    root: PathBuf,
    selections_file: PathBuf,
}

#[derive(Debug)]
pub struct RuntimeLease {
    _file: std::fs::File,
}

#[derive(Debug)]
pub struct RuntimeStoreTransaction {
    _file: std::fs::File,
}

impl RuntimeStore {
    pub fn new(paths: &AppPaths) -> Self {
        Self {
            root: paths.runtimes_dir.clone(),
            selections_file: paths.runtime_selections_file.clone(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub async fn ensure(&self) -> Result<(), RuntimeStoreError> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || ensure_store_layout_blocking(&root))
            .await
            .map_err(|source| RuntimeStoreError::Io {
                path: self.root.clone(),
                source: std::io::Error::other(source),
            })?
    }

    pub async fn acquire_transaction(&self) -> Result<RuntimeStoreTransaction, RuntimeStoreError> {
        self.ensure().await?;
        let lock_path = self.root.join(".locks").join("runtime-store.lock");
        tokio::task::spawn_blocking(move || {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&lock_path)
                .map_err(|source| RuntimeStoreError::Io {
                    path: lock_path.clone(),
                    source,
                })?;
            fs2::FileExt::lock_exclusive(&file).map_err(|source| RuntimeStoreError::Io {
                path: lock_path,
                source,
            })?;
            Ok(RuntimeStoreTransaction { _file: file })
        })
        .await
        .map_err(|source| RuntimeStoreError::Io {
            path: self.root.clone(),
            source: std::io::Error::other(source),
        })?
    }

    pub async fn scan(&self) -> Result<RuntimeStoreSnapshot, RuntimeStoreError> {
        self.ensure().await?;
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || scan_blocking(&root))
            .await
            .map_err(|source| RuntimeStoreError::Io {
                path: self.root.clone(),
                source: std::io::Error::other(source),
            })?
    }

    pub async fn get(
        &self,
        runtime_id: &RuntimeId,
    ) -> Result<Option<InstalledRuntime>, RuntimeStoreError> {
        Ok(self
            .scan()
            .await?
            .runtimes
            .into_iter()
            .find(|runtime| &runtime.manifest.runtime_id == runtime_id))
    }

    pub fn installation_path(&self, manifest: &RuntimeManifest) -> PathBuf {
        installation_path_for(&self.root, manifest)
    }

    pub async fn create_staging(&self) -> Result<PathBuf, RuntimeStoreError> {
        self.ensure().await?;
        let path = self
            .root
            .join(".staging")
            .join(uuid::Uuid::new_v4().to_string());
        tokio::fs::create_dir(&path)
            .await
            .map_err(|source| RuntimeStoreError::Io {
                path: path.clone(),
                source,
            })?;
        Ok(path)
    }

    pub async fn activate(
        &self,
        staging: &Path,
        manifest: &RuntimeManifest,
    ) -> Result<InstalledRuntime, RuntimeStoreError> {
        manifest
            .validate()
            .map_err(|error| RuntimeStoreError::InvalidManifest {
                path: staging.join(RUNTIME_MANIFEST_FILE),
                message: error.to_string(),
            })?;
        let _transaction = self.acquire_transaction().await?;
        let _lease = self.acquire_lease(&manifest.runtime_id, true).await?;
        let destination = self.installation_path(manifest);
        let installed = self.scan().await?.runtimes;
        if let Some(existing) = installed
            .iter()
            .find(|runtime| runtime.manifest.runtime_id == manifest.runtime_id)
        {
            if existing.manifest == *manifest {
                return Ok(existing.clone());
            }
            return Err(RuntimeStoreError::ProvenanceConflict {
                runtime_id: manifest.runtime_id.clone(),
                detail: provenance_difference(&existing.manifest, manifest),
            });
        }
        if let Some(existing) = installed
            .iter()
            .find(|runtime| same_claimed_release_identity(&runtime.manifest, manifest))
        {
            return Err(RuntimeStoreError::ProvenanceConflict {
                runtime_id: manifest.runtime_id.clone(),
                detail: format!(
                    "upstream release/asset identity matches installed runtime `{}`, but its asset identity or digest changed: {}",
                    existing.manifest.runtime_id,
                    provenance_difference(&existing.manifest, manifest)
                ),
            });
        }
        let root = self.root.clone();
        let staging = staging.to_path_buf();
        let activation_destination = destination.clone();
        let activation_manifest = manifest.clone();
        tokio::task::spawn_blocking(move || {
            activate_blocking(
                &root,
                &staging,
                &activation_destination,
                &activation_manifest,
            )
        })
        .await
        .map_err(|source| RuntimeStoreError::Io {
            path: destination.clone(),
            source: std::io::Error::other(source),
        })??;
        Ok(InstalledRuntime {
            manifest: manifest.clone(),
            installation_root: destination,
        })
    }

    pub async fn remove(&self, runtime_id: &RuntimeId) -> Result<(), RuntimeStoreError> {
        let _transaction = self.acquire_transaction().await?;
        let _lease = self.acquire_lease(runtime_id, true).await?;
        let selections = self.selections().await?;
        if let Some(selection) = selection_for_runtime(&selections, runtime_id) {
            return Err(RuntimeStoreError::RuntimeSelected {
                runtime_id: runtime_id.clone(),
                selection,
            });
        }
        let root = self.root.clone();
        let requested = runtime_id.clone();
        let runtime =
            tokio::task::spawn_blocking(move || find_removal_runtime_blocking(&root, &requested))
                .await
                .map_err(|source| RuntimeStoreError::Io {
                    path: self.root.clone(),
                    source: std::io::Error::other(source),
                })??;
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || {
            if runtime.manifest.acquisition_method == RuntimeAcquisitionMethod::ExternalBinary {
                return Err(RuntimeStoreError::ExternalRuntime(
                    runtime.manifest.runtime_id,
                ));
            }
            let expected = installation_path_for(&root, &runtime.manifest);
            if expected != runtime.installation_root {
                return Err(RuntimeStoreError::UnsafePath(runtime.installation_root));
            }
            remove_blocking(&root, &expected, &runtime.manifest)
        })
        .await
        .map_err(|source| RuntimeStoreError::Io {
            path: self.root.clone(),
            source: std::io::Error::other(source),
        })?
    }

    pub async fn acquire_runtime_lease(
        &self,
        runtime_id: &RuntimeId,
    ) -> Result<RuntimeLease, RuntimeStoreError> {
        self.acquire_lease(runtime_id, false).await
    }

    async fn acquire_lease(
        &self,
        runtime_id: &RuntimeId,
        exclusive: bool,
    ) -> Result<RuntimeLease, RuntimeStoreError> {
        self.ensure().await?;
        let lock_path = self
            .root
            .join(".locks")
            .join(format!("{}.lock", runtime_id.as_str()));
        let runtime_id = runtime_id.clone();
        tokio::task::spawn_blocking(move || {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&lock_path)
                .map_err(|source| RuntimeStoreError::Io {
                    path: lock_path.clone(),
                    source,
                })?;
            let lock_result = if exclusive {
                fs2::FileExt::try_lock_exclusive(&file)
            } else {
                fs2::FileExt::try_lock_shared(&file)
            };
            match lock_result {
                Ok(()) => Ok(RuntimeLease { _file: file }),
                // Windows reports lock contention as a platform-specific I/O
                // kind rather than WouldBlock. Fail closed for any lock error:
                // neither launch nor removal is safe without the lease.
                Err(_) => Err(RuntimeStoreError::RuntimeInUse(runtime_id)),
            }
        })
        .await
        .map_err(|source| RuntimeStoreError::Io {
            path: self.root.clone(),
            source: std::io::Error::other(source),
        })?
    }

    pub async fn selections(&self) -> Result<RuntimeSelections, RuntimeStoreError> {
        let bytes = match tokio::fs::read(&self.selections_file).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RuntimeSelections::default());
            }
            Err(source) => {
                return Err(RuntimeStoreError::Io {
                    path: self.selections_file.clone(),
                    source,
                });
            }
        };
        let selections: RuntimeSelections = serde_json::from_slice(&bytes)
            .map_err(|error| RuntimeStoreError::InvalidSelections(error.to_string()))?;
        if selections.schema_version != RUNTIME_SELECTIONS_SCHEMA_VERSION {
            return Err(RuntimeStoreError::InvalidSelections(format!(
                "schema {} is unsupported; expected {}",
                selections.schema_version, RUNTIME_SELECTIONS_SCHEMA_VERSION
            )));
        }
        Ok(selections)
    }

    pub async fn write_selections(
        &self,
        selections: &RuntimeSelections,
        _transaction: &RuntimeStoreTransaction,
    ) -> Result<(), RuntimeStoreError> {
        if selections.schema_version != RUNTIME_SELECTIONS_SCHEMA_VERSION {
            return Err(RuntimeStoreError::InvalidSelections(format!(
                "schema {} is unsupported",
                selections.schema_version
            )));
        }
        let bytes = serde_json::to_vec_pretty(selections)
            .map_err(|error| RuntimeStoreError::InvalidSelections(error.to_string()))?;
        atomic_write(self.selections_file.clone(), bytes)
            .await
            .map_err(|source| RuntimeStoreError::Io {
                path: self.selections_file.clone(),
                source,
            })
    }
}

fn ensure_store_layout_blocking(root: &Path) -> Result<(), RuntimeStoreError> {
    std::fs::create_dir_all(root).map_err(|source| RuntimeStoreError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    verify_real_directory(root)?;
    let canonical_root = canonical_directory(root)?;
    for name in [".staging", ".locks", ".trash"] {
        let path = root.join(name);
        match std::fs::create_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(RuntimeStoreError::Io {
                    path: path.clone(),
                    source,
                });
            }
        }
        verify_real_directory(&path)?;
        let canonical = canonical_directory(&path)?;
        if canonical == canonical_root || !canonical.starts_with(&canonical_root) {
            return Err(RuntimeStoreError::UnsafePath(canonical));
        }
    }
    Ok(())
}

fn activate_blocking(
    root: &Path,
    staging: &Path,
    destination: &Path,
    manifest: &RuntimeManifest,
) -> Result<(), RuntimeStoreError> {
    ensure_store_layout_blocking(root)?;
    validate_staging_directory(root, staging)?;
    verify_exact_manifest(staging, manifest)?;
    verify_entrypoint_integrity(staging, manifest)?;
    prepare_managed_parent(root, destination)?;
    let previous = match std::fs::symlink_metadata(destination) {
        Ok(_) => {
            validate_managed_runtime_directory(root, destination)?;
            let existing = read_valid_manifest(destination)?;
            if let Some(existing) = &existing
                && !repairable_runtime_manifest(existing, manifest)
            {
                return Err(RuntimeStoreError::ProvenanceConflict {
                    runtime_id: manifest.runtime_id.clone(),
                    detail: format!(
                        "activation directory contains a different immutable runtime manifest: {}",
                        destination.display()
                    ),
                });
            }
            let trash_root = root.join(".trash");
            let kind = if existing.is_some() {
                "repair"
            } else {
                "orphan"
            };
            let quarantine = trash_root.join(format!("{kind}-{}", uuid::Uuid::new_v4()));
            std::fs::rename(destination, &quarantine).map_err(|source| RuntimeStoreError::Io {
                path: destination.to_path_buf(),
                source,
            })?;
            if let Err(error) = validate_internal_directory(root, &trash_root, &quarantine) {
                let _ = std::fs::rename(&quarantine, destination);
                return Err(error);
            }
            Some((quarantine, existing.is_some()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(RuntimeStoreError::Io {
                path: destination.to_path_buf(),
                source,
            });
        }
    };
    if let Err(source) = std::fs::rename(staging, destination) {
        if let Some((quarantine, _)) = &previous {
            let _ = std::fs::rename(quarantine, destination);
        }
        return Err(RuntimeStoreError::Io {
            path: destination.to_path_buf(),
            source,
        });
    }
    let activated = validate_managed_runtime_directory(root, destination)
        .and_then(|()| verify_exact_manifest(destination, manifest))
        .and_then(|()| verify_entrypoint_integrity(destination, manifest));
    if let Err(error) = activated {
        let _ = std::fs::rename(destination, staging);
        if let Some((quarantine, _)) = &previous {
            let _ = std::fs::rename(quarantine, destination);
        }
        return Err(error);
    }
    if let Some((quarantine, true)) = previous {
        let _ = std::fs::remove_dir_all(quarantine);
    }
    Ok(())
}

fn remove_blocking(
    root: &Path,
    expected: &Path,
    expected_manifest: &RuntimeManifest,
) -> Result<(), RuntimeStoreError> {
    ensure_store_layout_blocking(root)?;
    validate_managed_runtime_directory(root, expected)?;
    verify_exact_manifest(expected, expected_manifest)?;

    let trash_root = root.join(".trash");
    let quarantine = trash_root.join(uuid::Uuid::new_v4().to_string());
    std::fs::rename(expected, &quarantine).map_err(|source| RuntimeStoreError::Io {
        path: expected.to_path_buf(),
        source,
    })?;

    let quarantined_is_exact = validate_internal_directory(root, &trash_root, &quarantine)
        .and_then(|()| verify_exact_manifest(&quarantine, expected_manifest));
    if let Err(error) = quarantined_is_exact {
        let _ = std::fs::rename(&quarantine, expected);
        return Err(error);
    }

    std::fs::remove_dir_all(&quarantine).map_err(|source| RuntimeStoreError::Io {
        path: quarantine,
        source,
    })
}

fn find_removal_runtime_blocking(
    root: &Path,
    runtime_id: &RuntimeId,
) -> Result<InstalledRuntime, RuntimeStoreError> {
    ensure_store_layout_blocking(root)?;
    let mut matches = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .min_depth(1)
        .max_depth(4)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            !entry
                .path()
                .components()
                .any(|component| is_internal_store_segment(component.as_os_str()))
        })
    {
        let entry = entry.map_err(|error| RuntimeStoreError::Io {
            path: error
                .path()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| root.to_path_buf()),
            source: std::io::Error::other(error),
        })?;
        let path = entry.path();
        let depth = path
            .strip_prefix(root)
            .map_err(|_| RuntimeStoreError::UnsafePath(path.to_path_buf()))?
            .components()
            .count();
        if depth == 4 && entry.file_type().is_dir() && entry.file_name() == runtime_id.as_str() {
            validate_managed_runtime_directory(root, path)?;
            matches.push(path.to_path_buf());
        }
    }
    let target = match matches.as_slice() {
        [] => return Err(RuntimeStoreError::NotInstalled(runtime_id.clone())),
        [target] => target.clone(),
        _ => return Err(RuntimeStoreError::UnsafePath(root.to_path_buf())),
    };
    let manifest_path = target.join(RUNTIME_MANIFEST_FILE);
    let bytes = std::fs::read(&manifest_path).map_err(|source| RuntimeStoreError::Io {
        path: manifest_path.clone(),
        source,
    })?;
    let manifest: RuntimeManifest =
        serde_json::from_slice(&bytes).map_err(|error| RuntimeStoreError::InvalidManifest {
            path: manifest_path.clone(),
            message: error.to_string(),
        })?;
    manifest
        .validate()
        .map_err(|error| RuntimeStoreError::InvalidManifest {
            path: manifest_path.clone(),
            message: error.to_string(),
        })?;
    if &manifest.runtime_id != runtime_id || installation_path_for(root, &manifest) != target {
        return Err(RuntimeStoreError::InvalidManifest {
            path: manifest_path,
            message: "manifest does not prove ownership of the requested exact runtime path"
                .to_owned(),
        });
    }
    Ok(InstalledRuntime {
        manifest,
        installation_root: target,
    })
}

fn prepare_managed_parent(root: &Path, destination: &Path) -> Result<(), RuntimeStoreError> {
    let parent = destination
        .parent()
        .ok_or_else(|| RuntimeStoreError::UnsafePath(destination.to_path_buf()))?;
    let relative = parent
        .strip_prefix(root)
        .map_err(|_| RuntimeStoreError::UnsafePath(parent.to_path_buf()))?;
    let canonical_root = canonical_directory(root)?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(RuntimeStoreError::UnsafePath(parent.to_path_buf()));
        };
        if is_internal_store_segment(component) {
            return Err(RuntimeStoreError::UnsafePath(parent.to_path_buf()));
        }
        current.push(component);
        match std::fs::create_dir(&current) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(RuntimeStoreError::Io {
                    path: current.clone(),
                    source,
                });
            }
        }
        verify_real_directory(&current)?;
        let canonical = canonical_directory(&current)?;
        if canonical == canonical_root || !canonical.starts_with(&canonical_root) {
            return Err(RuntimeStoreError::UnsafePath(canonical));
        }
    }
    Ok(())
}

fn validate_managed_runtime_directory(root: &Path, target: &Path) -> Result<(), RuntimeStoreError> {
    let relative = target
        .strip_prefix(root)
        .map_err(|_| RuntimeStoreError::UnsafePath(target.to_path_buf()))?;
    let canonical_root = canonical_directory(root)?;
    let mut current = root.to_path_buf();
    let mut depth = 0usize;
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(RuntimeStoreError::UnsafePath(target.to_path_buf()));
        };
        if is_internal_store_segment(component) {
            return Err(RuntimeStoreError::UnsafePath(target.to_path_buf()));
        }
        current.push(component);
        verify_real_directory(&current)?;
        depth += 1;
    }
    if depth != 4 {
        return Err(RuntimeStoreError::UnsafePath(target.to_path_buf()));
    }
    let canonical_target = canonical_directory(target)?;
    if canonical_target == canonical_root || !canonical_target.starts_with(&canonical_root) {
        return Err(RuntimeStoreError::UnsafePath(canonical_target));
    }
    Ok(())
}

fn validate_staging_directory(root: &Path, staging: &Path) -> Result<(), RuntimeStoreError> {
    validate_internal_directory(root, &root.join(".staging"), staging)
}

fn validate_internal_directory(
    root: &Path,
    expected_parent: &Path,
    directory: &Path,
) -> Result<(), RuntimeStoreError> {
    if directory.parent() != Some(expected_parent) {
        return Err(RuntimeStoreError::UnsafePath(directory.to_path_buf()));
    }
    verify_real_directory(root)?;
    verify_real_directory(expected_parent)?;
    verify_real_directory(directory)?;
    let canonical_root = canonical_directory(root)?;
    let canonical_parent = canonical_directory(expected_parent)?;
    let canonical_directory = canonical_directory(directory)?;
    if canonical_parent == canonical_root
        || !canonical_parent.starts_with(&canonical_root)
        || canonical_directory == canonical_parent
        || !canonical_directory.starts_with(&canonical_parent)
    {
        return Err(RuntimeStoreError::UnsafePath(canonical_directory));
    }
    Ok(())
}

fn verify_exact_manifest(
    installation_root: &Path,
    expected: &RuntimeManifest,
) -> Result<(), RuntimeStoreError> {
    let path = installation_root.join(RUNTIME_MANIFEST_FILE);
    let bytes = std::fs::read(&path).map_err(|source| RuntimeStoreError::Io {
        path: path.clone(),
        source,
    })?;
    let observed: RuntimeManifest =
        serde_json::from_slice(&bytes).map_err(|error| RuntimeStoreError::InvalidManifest {
            path: path.clone(),
            message: error.to_string(),
        })?;
    if observed != *expected {
        return Err(RuntimeStoreError::InvalidManifest {
            path,
            message: "manifest changed between discovery and the exact removal transaction"
                .to_owned(),
        });
    }
    Ok(())
}

fn read_valid_manifest(
    installation_root: &Path,
) -> Result<Option<RuntimeManifest>, RuntimeStoreError> {
    let path = installation_root.join(RUNTIME_MANIFEST_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(RuntimeStoreError::Io { path, source }),
    };
    let Ok(manifest) = serde_json::from_slice::<RuntimeManifest>(&bytes) else {
        return Ok(None);
    };
    if manifest.validate().is_err() {
        return Ok(None);
    }
    Ok(Some(manifest))
}

fn repairable_runtime_manifest(existing: &RuntimeManifest, candidate: &RuntimeManifest) -> bool {
    existing.schema_version == candidate.schema_version
        && existing.runtime_id == candidate.runtime_id
        && existing.identity == candidate.identity
        && existing.supported_formats == candidate.supported_formats
        && existing.requirements == candidate.requirements
        && existing.acquisition_method == candidate.acquisition_method
        && existing.source_url == candidate.source_url
        && existing.downloaded_archive_sha256 == candidate.downloaded_archive_sha256
        && existing.additional_downloaded_archive_sha256
            == candidate.additional_downloaded_archive_sha256
        && existing.entrypoint == candidate.entrypoint
        && existing.entrypoint_sha256 == candidate.entrypoint_sha256
}

fn verify_entrypoint_integrity(
    installation_root: &Path,
    manifest: &RuntimeManifest,
) -> Result<(), RuntimeStoreError> {
    let entrypoint = installation_root.join(&manifest.entrypoint);
    let metadata =
        std::fs::symlink_metadata(&entrypoint).map_err(|source| RuntimeStoreError::Io {
            path: entrypoint.clone(),
            source,
        })?;
    if !metadata.is_file() || is_link_or_reparse(&metadata) {
        return Err(RuntimeStoreError::InvalidManifest {
            path: installation_root.join(RUNTIME_MANIFEST_FILE),
            message: format!(
                "entrypoint is missing, not a regular file, or is a filesystem link: {}",
                entrypoint.display()
            ),
        });
    }
    let canonical_root = canonical_directory(installation_root)?;
    let canonical_entrypoint =
        std::fs::canonicalize(&entrypoint).map_err(|source| RuntimeStoreError::Io {
            path: entrypoint.clone(),
            source,
        })?;
    if !canonical_entrypoint.starts_with(&canonical_root) {
        return Err(RuntimeStoreError::UnsafePath(canonical_entrypoint));
    }
    let observed = hash_file_blocking(&entrypoint)?;
    if !observed.eq_ignore_ascii_case(&manifest.entrypoint_sha256) {
        return Err(RuntimeStoreError::InvalidManifest {
            path: installation_root.join(RUNTIME_MANIFEST_FILE),
            message: format!(
                "entrypoint SHA-256 mismatch: expected {}, observed {observed}",
                manifest.entrypoint_sha256
            ),
        });
    }
    Ok(())
}

fn hash_file_blocking(path: &Path) -> Result<String, RuntimeStoreError> {
    let mut file = std::fs::File::open(path).map_err(|source| RuntimeStoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| RuntimeStoreError::Io {
                path: path.to_path_buf(),
                source,
            })?;
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

fn verify_real_directory(path: &Path) -> Result<(), RuntimeStoreError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| RuntimeStoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_dir() || is_link_or_reparse(&metadata) {
        return Err(RuntimeStoreError::UnsafePath(path.to_path_buf()));
    }
    Ok(())
}

fn canonical_directory(path: &Path) -> Result<PathBuf, RuntimeStoreError> {
    std::fs::canonicalize(path).map_err(|source| RuntimeStoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(windows)]
fn is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn is_internal_store_segment(segment: &std::ffi::OsStr) -> bool {
    matches!(segment.to_str(), Some(".staging" | ".locks" | ".trash"))
}

fn selection_for_runtime(selections: &RuntimeSelections, runtime_id: &RuntimeId) -> Option<String> {
    selections
        .format_defaults
        .iter()
        .find(|(_, selected)| *selected == runtime_id)
        .map(|(format, _)| format!("{} default", format.as_str().to_ascii_uppercase()))
        .or_else(|| {
            selections
                .model_overrides
                .iter()
                .find(|(_, selected)| *selected == runtime_id)
                .map(|(model, _)| format!("model {model}"))
        })
}

fn installation_path_for(root: &Path, manifest: &RuntimeManifest) -> PathBuf {
    let identity = &manifest.identity;
    let platform_variant = safe_segment(&format!(
        "{}-{}-{}-{}",
        identity.platform, identity.architecture, identity.accelerator, identity.variant
    ));
    root.join(safe_segment(&identity.engine_id))
        .join(platform_variant)
        .join(safe_segment(&identity.version))
        .join(manifest.runtime_id.as_str())
}

fn scan_blocking(root: &Path) -> Result<RuntimeStoreSnapshot, RuntimeStoreError> {
    let mut snapshot = RuntimeStoreSnapshot::default();
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                snapshot.warnings.push(error.to_string());
                continue;
            }
        };
        if !entry.file_type().is_file()
            || entry.file_name() != RUNTIME_MANIFEST_FILE
            || entry
                .path()
                .components()
                .any(|component| is_internal_store_segment(component.as_os_str()))
        {
            continue;
        }
        let path = entry.path().to_path_buf();
        let parsed = (|| {
            let bytes = std::fs::read(&path).map_err(|source| RuntimeStoreError::Io {
                path: path.clone(),
                source,
            })?;
            let manifest: RuntimeManifest = serde_json::from_slice(&bytes).map_err(|error| {
                RuntimeStoreError::InvalidManifest {
                    path: path.clone(),
                    message: error.to_string(),
                }
            })?;
            manifest
                .validate()
                .map_err(|error| RuntimeStoreError::InvalidManifest {
                    path: path.clone(),
                    message: error.to_string(),
                })?;
            if manifest.acquisition_method == RuntimeAcquisitionMethod::ExternalBinary {
                return Err(RuntimeStoreError::InvalidManifest {
                    path: path.clone(),
                    message: "external runtime manifests do not belong in the managed store"
                        .to_owned(),
                });
            }
            let installation_root = path
                .parent()
                .ok_or_else(|| RuntimeStoreError::UnsafePath(path.clone()))?
                .to_path_buf();
            let expected_installation = installation_path_for(root, &manifest);
            if installation_root != expected_installation {
                return Err(RuntimeStoreError::InvalidManifest {
                    path: path.clone(),
                    message: format!(
                        "manifest is outside its exact identity-derived installation path {}",
                        expected_installation.display()
                    ),
                });
            }
            validate_managed_runtime_directory(root, &installation_root)?;
            let runtime = InstalledRuntime {
                manifest,
                installation_root,
            };
            verify_entrypoint_integrity(&runtime.installation_root, &runtime.manifest)?;
            Ok(runtime)
        })();
        match parsed {
            Ok(runtime) => snapshot.runtimes.push(runtime),
            Err(error) => snapshot.warnings.push(error.to_string()),
        }
    }
    snapshot.runtimes.sort_by(|left, right| {
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
                right
                    .manifest
                    .identity
                    .version
                    .cmp(&left.manifest.identity.version)
            })
    });
    Ok(snapshot)
}

fn safe_segment(value: &str) -> String {
    let segment = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches(['.', '-'])
        .to_owned();
    if segment.is_empty() {
        "unknown".to_owned()
    } else {
        segment
    }
}

fn provenance_difference(existing: &RuntimeManifest, candidate: &RuntimeManifest) -> String {
    match (
        existing.downloaded_archive_sha256.as_deref(),
        candidate.downloaded_archive_sha256.as_deref(),
    ) {
        (Some(existing), Some(candidate)) if existing != candidate => format!(
            "the same immutable release asset now advertises archive digest {candidate}, but the installed digest is {existing}"
        ),
        _ if existing.additional_downloaded_archive_sha256
            != candidate.additional_downloaded_archive_sha256 =>
        {
            "one or more required component archive digests changed".to_owned()
        }
        _ if existing.entrypoint_sha256 != candidate.entrypoint_sha256 => format!(
            "the entrypoint digest changed from {} to {}",
            existing.entrypoint_sha256, candidate.entrypoint_sha256
        ),
        _ => "the immutable runtime manifest differs from the installed record".to_owned(),
    }
}

fn same_claimed_release_identity(left: &RuntimeManifest, right: &RuntimeManifest) -> bool {
    let left_identity = &left.identity;
    let right_identity = &right.identity;
    left_identity.engine_id == right_identity.engine_id
        && left_identity.package_family == right_identity.package_family
        && left_identity.version == right_identity.version
        && left_identity.platform == right_identity.platform
        && left_identity.architecture == right_identity.architecture
        && left_identity.accelerator == right_identity.accelerator
        && left_identity.variant == right_identity.variant
        && left_identity.package.provider_id == right_identity.package.provider_id
        && left_identity.package.repository == right_identity.package.repository
        && left_identity.package.release_tag == right_identity.package.release_tag
        && left_identity.package.asset_name == right_identity.package.asset_name
        && left_identity
            .package
            .additional_assets
            .iter()
            .map(|asset| (&asset.asset_name, &asset.role))
            .eq(right_identity
                .package
                .additional_assets
                .iter()
                .map(|asset| (&asset.asset_name, &asset.role)))
}

#[cfg(test)]
mod tests {
    use norted_core::{
        AppPaths, ArtifactFormat, RUNTIME_MANIFEST_SCHEMA_VERSION, RuntimeAcquisitionMethod,
        RuntimeIdentity, RuntimePackageIdentity, RuntimeProbeObservation, RuntimeRequirements,
    };

    use super::{RuntimeStore, RuntimeStoreError};

    async fn stage_runtime(
        store: &RuntimeStore,
        manifest: &norted_core::RuntimeManifest,
        entrypoint: &[u8],
    ) -> std::path::PathBuf {
        let staging = store.create_staging().await.expect("staging directory");
        tokio::fs::write(staging.join("server"), entrypoint)
            .await
            .expect("fixture entrypoint");
        tokio::fs::write(
            staging.join(super::RUNTIME_MANIFEST_FILE),
            serde_json::to_vec_pretty(manifest).expect("fixture manifest"),
        )
        .await
        .expect("write fixture manifest");
        staging
    }

    #[tokio::test]
    async fn failed_probe_manifest_cannot_be_activated() {
        let temporary = tempfile::tempdir().expect("temporary runtime root");
        let paths = AppPaths {
            config_dir: temporary.path().join("config"),
            config_file: temporary.path().join("config/config.toml"),
            data_dir: temporary.path().join("data"),
            state_dir: temporary.path().join("state"),
            cache_dir: temporary.path().join("cache"),
            log_dir: temporary.path().join("logs"),
            runtimes_dir: temporary.path().join("data/runtimes"),
            runtime_cache_dir: temporary.path().join("cache/runtime-packs"),
            runtime_selections_file: temporary.path().join("data/runtime-selections.json"),
            load_profiles_file: temporary.path().join("data/load-profiles.json"),
            load_profiles_lock_file: temporary.path().join("data/.load-profiles.lock"),
        };
        let store = RuntimeStore::new(&paths);
        let staging = store.create_staging().await.expect("staging directory");
        tokio::fs::write(staging.join("server"), b"not executable")
            .await
            .expect("fixture entrypoint");
        let identity = RuntimeIdentity {
            engine_id: "fixture".to_owned(),
            package_family: "fixture-pack".to_owned(),
            version: "v1".to_owned(),
            upstream_revision: None,
            platform: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
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
        let manifest = norted_core::RuntimeManifest {
            schema_version: RUNTIME_MANIFEST_SCHEMA_VERSION,
            runtime_id: norted_core::RuntimeId::from_identity(&identity),
            identity,
            supported_formats: vec![ArtifactFormat::Gguf],
            requirements: RuntimeRequirements::default(),
            acquisition_method: RuntimeAcquisitionMethod::OfficialReleaseAsset,
            source_url: Some("https://github.com/owner/repository/releases/tag/v1".to_owned()),
            downloaded_archive_sha256: Some("a".repeat(64)),
            additional_downloaded_archive_sha256: Vec::new(),
            entrypoint: "server".into(),
            entrypoint_sha256: "b".repeat(64),
            installed_at_unix: Some(1),
            probe: RuntimeProbeObservation {
                compatible: false,
                observed_engine_id: "fixture".to_owned(),
                observed_version: None,
                observed_revision: None,
                detail: "probe failed".to_owned(),
                observed_at_unix: 1,
            },
        };
        let destination = store.installation_path(&manifest);
        let error = store
            .activate(&staging, &manifest)
            .await
            .expect_err("failed probe must prevent activation");
        assert!(matches!(error, RuntimeStoreError::InvalidManifest { .. }));
        assert!(!destination.exists());
        assert!(staging.exists());
    }

    #[tokio::test]
    async fn running_runtime_lease_blocks_exclusive_removal_lease() {
        let temporary = tempfile::tempdir().expect("temporary runtime root");
        let paths = AppPaths {
            config_dir: temporary.path().join("config"),
            config_file: temporary.path().join("config/config.toml"),
            data_dir: temporary.path().join("data"),
            state_dir: temporary.path().join("state"),
            cache_dir: temporary.path().join("cache"),
            log_dir: temporary.path().join("logs"),
            runtimes_dir: temporary.path().join("data/runtimes"),
            runtime_cache_dir: temporary.path().join("cache/runtime-packs"),
            runtime_selections_file: temporary.path().join("data/runtime-selections.json"),
            load_profiles_file: temporary.path().join("data/load-profiles.json"),
            load_profiles_lock_file: temporary.path().join("data/.load-profiles.lock"),
        };
        let store = RuntimeStore::new(&paths);
        let runtime_id = norted_core::RuntimeId::new("leased-runtime").expect("runtime ID");
        let _running = store
            .acquire_runtime_lease(&runtime_id)
            .await
            .expect("shared running lease");
        assert!(matches!(
            store.acquire_lease(&runtime_id, true).await,
            Err(RuntimeStoreError::RuntimeInUse(observed)) if observed == runtime_id
        ));
    }

    #[tokio::test]
    async fn corrupt_entrypoint_is_not_listed_and_can_be_repaired_or_removed() {
        use sha2::{Digest, Sha256};

        let temporary = tempfile::tempdir().expect("temporary runtime root");
        let paths = AppPaths {
            config_dir: temporary.path().join("config"),
            config_file: temporary.path().join("config/config.toml"),
            data_dir: temporary.path().join("data"),
            state_dir: temporary.path().join("state"),
            cache_dir: temporary.path().join("cache"),
            log_dir: temporary.path().join("logs"),
            runtimes_dir: temporary.path().join("data/runtimes"),
            runtime_cache_dir: temporary.path().join("cache/runtime-packs"),
            runtime_selections_file: temporary.path().join("data/runtime-selections.json"),
            load_profiles_file: temporary.path().join("data/load-profiles.json"),
            load_profiles_lock_file: temporary.path().join("data/.load-profiles.lock"),
        };
        let store = RuntimeStore::new(&paths);
        let identity = RuntimeIdentity {
            engine_id: "fixture".to_owned(),
            package_family: "fixture-pack".to_owned(),
            version: "v1".to_owned(),
            upstream_revision: Some("revision".to_owned()),
            platform: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
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
        let manifest = norted_core::RuntimeManifest {
            schema_version: RUNTIME_MANIFEST_SCHEMA_VERSION,
            runtime_id: norted_core::RuntimeId::from_identity(&identity),
            identity,
            supported_formats: vec![ArtifactFormat::Gguf],
            requirements: RuntimeRequirements::default(),
            acquisition_method: RuntimeAcquisitionMethod::OfficialReleaseAsset,
            source_url: Some("https://github.com/owner/repository/releases/tag/v1".to_owned()),
            downloaded_archive_sha256: Some("a".repeat(64)),
            additional_downloaded_archive_sha256: Vec::new(),
            entrypoint: "server".into(),
            entrypoint_sha256: super::hex_digest(Sha256::digest(b"original")),
            installed_at_unix: Some(1),
            probe: RuntimeProbeObservation {
                compatible: true,
                observed_engine_id: "fixture".to_owned(),
                observed_version: Some("v1".to_owned()),
                observed_revision: Some("revision".to_owned()),
                detail: "fixture probe".to_owned(),
                observed_at_unix: 1,
            },
        };

        let staging = stage_runtime(&store, &manifest, b"original").await;
        let installed = store
            .activate(&staging, &manifest)
            .await
            .expect("initial activation");
        tokio::fs::write(installed.entrypoint_path(), b"corrupt")
            .await
            .expect("corrupt entrypoint");
        let snapshot = store.scan().await.expect("scan corrupt runtime");
        assert!(snapshot.runtimes.is_empty());
        assert!(
            snapshot
                .warnings
                .iter()
                .any(|warning| warning.contains("SHA-256"))
        );

        let repair_staging = stage_runtime(&store, &manifest, b"original").await;
        store
            .activate(&repair_staging, &manifest)
            .await
            .expect("transactional repair");
        assert_eq!(store.scan().await.expect("scan repaired").runtimes.len(), 1);

        tokio::fs::write(installed.entrypoint_path(), b"corrupt again")
            .await
            .expect("corrupt repaired entrypoint");
        store
            .remove(&manifest.runtime_id)
            .await
            .expect("exact manifest permits corrupt runtime removal");
        assert!(!installed.installation_root.exists());
    }
}
