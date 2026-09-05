//! Complete acquisitions are durable user artifacts; only reset removes them.
use norted_core::{AppPaths, prune::PrunePlan};
use std::io;

pub fn plan_model_prune(paths: &AppPaths, plan: &mut PrunePlan) -> io::Result<()> {
    let models = paths.data_dir.join("models");
    if plan.mode == "all" {
        // Only namespaces created by ModelLibrary are reclaimable. A user may
        // also have placed independent model files under the models directory.
        for name in ["huggingface", "imports", ".norted-staging"] {
            plan.children(&models.join(name), "managed model acquisitions")?;
        }
    } else {
        plan.children(&models.join(".norted-staging"), "abandoned model staging")?;
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
