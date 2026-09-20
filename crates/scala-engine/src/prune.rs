//! Runtime-store policy, using the same validated installation scan as serving.
use std::io;

use scala_core::{AppPaths, prune::PrunePlan};

use crate::{EngineRegistry, RuntimeStore};

pub async fn plan_runtime_prune(
    paths: &AppPaths,
    registry: &EngineRegistry,
    plan: &mut PrunePlan,
) -> io::Result<()> {
    let root = &paths.runtimes_dir;
    plan.categories
        .push("managed runtimes (active/selected/newest protected)".into());
    if plan.mode == "all" {
        // Clear generated selections first: if interrupted, remaining packs
        // are still valid, and no selection references a deleted pack.
        plan.add(
            &paths.data_dir,
            &paths.runtime_selections_file,
            "runtime selections",
        )?;
        if root.try_exists()? {
            for child in std::fs::read_dir(root)? {
                let child = child?;
                if child.file_name() != ".locks" {
                    plan.add(root, &child.path(), "managed runtimes")?;
                }
            }
        }
    } else {
        let store = RuntimeStore::new(paths);
        let snapshot = store.inspect_existing().await.map_err(io::Error::other)?;
        let selections = match store.selections().await {
            Ok(selections) => Some(selections),
            Err(error) => {
                plan.protected.push(format!("runtime versions: {error}"));
                None
            }
        };
        for issue in &snapshot.issues {
            plan.protected
                .push(format!("unproven runtime: {}", issue.message));
        }
        if let Some(selections) = selections {
            for runtime in &snapshot.runtimes {
                let id = &runtime.manifest.runtime_id;
                let selected = selections
                    .format_defaults
                    .values()
                    .chain(selections.model_overrides.values())
                    .any(|value| value == id)
                    || selections.update_preferences.contains_key(id);
                let superseded = snapshot.runtimes.iter().any(|newer| {
                    crate::packs::is_proven_superseding_installed_runtime(registry, newer, runtime)
                });
                if !selected && superseded {
                    plan.add(
                        root,
                        &runtime.installation_root,
                        "superseded runtime versions",
                    )?;
                } else {
                    plan.protected.push(format!(
                        "runtime {id}: {}",
                        if selected {
                            "explicitly selected"
                        } else {
                            "newest or no proven replacement"
                        }
                    ));
                }
            }
        }
        for name in [".staging", ".trash"] {
            plan.children(&root.join(name), "interrupted runtime installations")?;
        }
    }
    // Downloaded packages and provider catalogs are reproducible. Installed
    // manifests carry their digests and do not depend on these cached bytes.
    plan.children(
        &paths.runtime_cache_dir,
        "runtime downloads and provider catalogs",
    )?;
    Ok(())
}
