//! Complete acquisitions are durable user artifacts; only reset removes them.
use scala_core::{AppPaths, ModelLibraryReceipt, model_library_receipt_path, prune::PrunePlan};
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;

use crate::{RECEIPT_SCHEMA_VERSION, safe_relative};

pub fn plan_model_prune(
    paths: &AppPaths,
    downloads_root: &Path,
    plan: &mut PrunePlan,
) -> io::Result<()> {
    // Staging always lives under Scala's own data directory, independent of
    // the configured download destination.
    let staging = paths.data_dir.join("models").join(".scala-staging");
    if plan.mode == "all" {
        // The configured download root may be a user-owned/general model
        // directory (e.g. /mnt/models). Only Scala-owned managed acquisitions
        // beneath it are reclaimable: a directory is treated as a complete
        // managed acquisition only when it lives at a known Scala layout and
        // carries a valid `.scala-library.json` receipt. Unrelated content
        // under coincidentally named `huggingface` or `imports` directories is
        // preserved. Configured `models.paths` entries are discovery-only and
        // are never pruned as managed storage.
        plan_managed_acquisitions(downloads_root, plan)?;
        plan.children(&staging, "abandoned model staging")?;
    } else {
        plan.children(&staging, "abandoned model staging")?;
        plan.protected
            .push("complete managed model acquisitions and externally configured models".into());
    }
    let cache = paths.cache_dir.join("model-downloads");
    if cache.try_exists()? {
        for entry in std::fs::read_dir(&cache)? {
            let entry = entry?;
            if entry.file_name() != "locks" {
                plan.add(&cache, &entry.path(), "incomplete model downloads")?;
            }
        }
    }
    Ok(())
}

/// Schedule deletion of Scala-owned managed acquisitions beneath the
/// configured download root. Traversal is constrained to the known managed
/// acquisition layouts:
///
///   `<root>/huggingface/<publisher>/<repository>/<revision>/<acquisition>`
///   `<root>/imports/<acquisition>`
///
/// A candidate acquisition directory is scheduled only when its acquisition
/// root carries a valid `.scala-library.json` receipt. The download root
/// itself and any unrelated content beneath it are preserved. Invalid or
/// missing receipts fail safe: the questionable directory is left untouched.
fn plan_managed_acquisitions(root: &Path, plan: &mut PrunePlan) -> io::Result<()> {
    let huggingface = root.join("huggingface");
    if huggingface.try_exists()? {
        for publisher in fs::read_dir(&huggingface)? {
            let publisher = publisher?;
            if !publisher.file_type()?.is_dir() {
                continue;
            }
            for repository in fs::read_dir(publisher.path())? {
                let repository = repository?;
                if !repository.file_type()?.is_dir() {
                    continue;
                }
                for revision in fs::read_dir(repository.path())? {
                    let revision = revision?;
                    if !revision.file_type()?.is_dir() {
                        continue;
                    }
                    for acquisition in fs::read_dir(revision.path())? {
                        let acquisition = acquisition?;
                        if !acquisition.file_type()?.is_dir() {
                            continue;
                        }
                        let path = acquisition.path();
                        if is_managed_acquisition(&path)? {
                            plan.add(root, &path, "managed model acquisitions")?;
                        }
                    }
                }
            }
        }
    }
    let imports = root.join("imports");
    if imports.try_exists()? {
        for acquisition in fs::read_dir(&imports)? {
            let acquisition = acquisition?;
            if !acquisition.file_type()?.is_dir() {
                continue;
            }
            let path = acquisition.path();
            if is_managed_acquisition(&path)? {
                plan.add(root, &path, "managed model acquisitions")?;
            }
        }
    }
    Ok(())
}

/// Confirm that `path` is a genuine Scala-managed acquisition by validating
/// its `.scala-library.json` receipt against the existing model-library
/// ownership contract. Invalid, missing, or unparseable receipts fail safe:
/// the directory is treated as not Scala-owned and preserved rather than
/// scheduled for destructive pruning.
fn is_managed_acquisition(path: &Path) -> io::Result<bool> {
    let receipt_path = model_library_receipt_path(path);
    if !receipt_path.is_file() {
        return Ok(false);
    }
    let bytes = match fs::read(&receipt_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let receipt: ModelLibraryReceipt = match serde_json::from_slice(&bytes) {
        Ok(receipt) => receipt,
        Err(_) => return Ok(false),
    };
    if receipt.schema_version != RECEIPT_SCHEMA_VERSION
        || receipt.acquisition_id.is_empty()
        || receipt.members.is_empty()
    {
        return Ok(false);
    }
    if !receipt_members_consistent(&receipt) {
        return Ok(false);
    }
    Ok(true)
}

/// Verify receipt member paths are safe, unique, and bound to the same
/// acquisition id, matching the contract enforced by `ModelLibrary::remove`.
fn receipt_members_consistent(receipt: &ModelLibraryReceipt) -> bool {
    let mut member_paths = HashSet::new();
    for member in &receipt.members {
        if !safe_relative(&member.path)
            || !member_paths.insert(member.path.clone())
            || member.provenance.acquisition_id != receipt.acquisition_id
        {
            return false;
        }
    }
    true
}
