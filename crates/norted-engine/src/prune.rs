//! Runtime-store policy, using the same validated installation scan as serving.
use std::cmp::Ordering;
use std::io;

use norted_core::{AppPaths, RuntimeManifest, prune::PrunePlan};

use crate::RuntimeStore;

pub async fn plan_runtime_prune(paths: &AppPaths, plan: &mut PrunePlan) -> io::Result<()> {
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
                    same_group(&runtime.manifest, &newer.manifest)
                        && comparable_versions(&runtime.manifest, &newer.manifest)
                        && crate::packs::compare_installed_recency(newer, runtime, None)
                            == Ordering::Greater
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

fn same_group(old: &RuntimeManifest, new: &RuntimeManifest) -> bool {
    let a = &old.identity;
    let b = &new.identity;
    a.engine_id == b.engine_id
        && a.package_family == b.package_family
        && a.platform == b.platform
        && a.architecture == b.architecture
        && a.accelerator == b.accelerator
        && a.variant == b.variant
        && a.package.provider_id == b.package.provider_id
        && a.package.repository == b.package.repository
        && old.supported_formats == new.supported_formats
        && old.supported_native_identities == new.supported_native_identities
        && old.requirements == new.requirements
        && old.acquisition_method == new.acquisition_method
}

fn comparable_versions(old: &RuntimeManifest, new: &RuntimeManifest) -> bool {
    if old.source_build.is_some() && new.source_build.is_some() {
        return true;
    }
    // Unknown version labels cannot prove supersession. Numeric releases and
    // llama.cpp's bNNNN releases have a well-defined existing ordering.
    [&old.identity.version, &new.identity.version]
        .iter()
        .all(|version| {
            let version = version.trim_start_matches(['v', 'b']);
            !version.is_empty()
                && version
                    .split('.')
                    .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        })
}
