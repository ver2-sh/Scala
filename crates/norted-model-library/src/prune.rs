//! Complete acquisitions are durable user artifacts; only reset removes them.
use norted_core::{AppPaths, prune::PrunePlan};
use std::io;
use std::path::Path;

pub fn plan_model_prune(
    paths: &AppPaths,
    downloads_root: &Path,
    plan: &mut PrunePlan,
) -> io::Result<()> {
    // Staging always lives under Norted's own data directory, independent of
    // the configured download destination.
    let staging = paths.data_dir.join("models").join(".norted-staging");
    if plan.mode == "all" {
        // Only namespaces created by ModelLibrary are reclaimable. A user may
        // also have placed independent model files under the download root or
        // any configured `models.paths` entry.
        for name in ["huggingface", "imports"] {
            plan.children(&downloads_root.join(name), "managed model acquisitions")?;
        }
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
