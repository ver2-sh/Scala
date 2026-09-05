use color_eyre::Result;
use norted_core::{
    AppPaths,
    prune::{
        PrunePlan, StorageLease, ensure_no_server_process, lock_existing_operations,
        validate_ancestors,
    },
};
use std::io::{self, IsTerminal, Write};

pub async fn run(paths: &AppPaths, args: &crate::cli::PruneArgs, json: bool) -> Result<()> {
    if args.all {
        eprintln!(
            "WARNING: prune --all removes all managed model acquisitions, runtime packs, downloads, staging, catalogs, logs and generated runtime selections. Configuration, settings, Model Profiles and API keys survive."
        );
        if !args.dry_run && !args.yes {
            if !io::stdin().is_terminal() {
                color_eyre::eyre::bail!("non-interactive prune --all requires --yes");
            }
            eprint!("Type yes to reset managed storage: ");
            io::stderr().flush()?;
            let mut answer = String::new();
            io::stdin().read_line(&mut answer)?;
            if !answer.trim().eq_ignore_ascii_case("yes") {
                color_eyre::eyre::bail!("prune cancelled");
            }
        }
    }
    for root in [
        &paths.data_dir,
        &paths.cache_dir,
        &paths.state_dir,
        &paths.log_dir,
        &paths.runtimes_dir,
    ] {
        validate_ancestors(&root.join("child"))?;
    }
    let _lease = StorageLease::acquire(paths, true, args.dry_run)?;
    let _operations = lock_existing_operations(paths)?;
    ensure_no_server_process(paths)?;
    let mut plan = PrunePlan::new(args.all, args.dry_run);
    norted_engine::plan_runtime_prune(paths, &mut plan).await?;
    norted_model_library::plan_model_prune(paths, &mut plan)?;
    plan.operational_storage(paths)?;
    plan.protected.push("config.toml, settings, Model Profiles, authentication, external files and stable operation lock files".into());
    plan.execute()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&plan)?);
    } else {
        println!(
            "Prune mode: {}{}",
            plan.mode,
            if args.dry_run { " (dry-run)" } else { "" }
        );
        println!("Categories: {}", plan.categories.join(", "));
        println!(
            "Plan: {} paths, {} files; expected reclaim {} bytes (logical)",
            plan.entries.len(),
            plan.entries.iter().map(|entry| entry.files).sum::<u64>(),
            plan.expected_bytes
        );
        if args.dry_run {
            for entry in &plan.entries {
                println!(
                    "  {}: {} ({} bytes)",
                    entry.category,
                    entry.path.display(),
                    entry.expected_bytes
                );
            }
        }
        println!(
            "Removed: {} paths, {} files; filesystem reclaim: {}",
            plan.removed_paths,
            plan.removed_files,
            plan.filesystem_reclaimed_bytes
                .map_or_else(|| "not measured".into(), |bytes| format!("{bytes} bytes"))
        );
        for protected in &plan.protected {
            println!("Protected: {protected}");
        }
        if plan.filesystem_reclaimed_bytes.is_none() && !args.dry_run {
            for (root, bytes) in &plan.filesystem_reclaimed_by_root {
                println!("Filesystem reclaim at {}: {bytes} bytes", root.display());
            }
        }
    }
    Ok(())
}
