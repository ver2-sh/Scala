//! Cache removal and collection of unreferenced durable environments.
use super::*;
use std::collections::BTreeSet;

pub(in crate::installer) fn trim_archives(
    cache: &Path,
    limit: u64,
) -> Result<(), RuntimeInstallError> {
    let pkgs = cache.join("pkgs");
    if !present(&pkgs)? {
        return Ok(());
    }
    directories(&pkgs, false)?;
    let mut files = Vec::new();
    let mut total = 0_u64;
    for entry in walkdir::WalkDir::new(&pkgs)
        .follow_links(false)
        .max_depth(6)
    {
        let entry = entry.map_err(io)?;
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name
            .strip_suffix(".conda")
            .or_else(|| name.strip_suffix(".tar.bz2"))
        else {
            continue;
        };
        let receipt = cache
            .join("package-inventories")
            .join(format!("{stem}.json"));
        if !present(&receipt)? {
            continue;
        }
        let saved: PackageInventory =
            serde_json::from_slice(&read_bounded(&receipt, MAX_INVENTORY_BYTES)?).map_err(io)?;
        if saved.package.filename() != name {
            return Err(invalid("archive cache identity changed"));
        }
        let metadata = regular(entry.path())?;
        total = total.saturating_add(metadata.len());
        files.push((
            metadata.modified().ok(),
            metadata.len(),
            entry.path().to_owned(),
        ));
    }
    files.sort_by_key(|f| f.0);
    for (_, size, path) in files {
        if total <= limit {
            break;
        }
        fs::remove_file(path).map_err(io)?;
        total = total.saturating_sub(size);
    }
    Ok(())
}
pub(in crate::installer) fn clean_locked(
    installer: &RuntimeInstaller,
) -> Result<(), RuntimeInstallError> {
    let cache = installer.cache_root.join("native/linux");
    let durable = installer
        .store
        .root()
        .parent()
        .ok_or_else(|| invalid("runtime store parent is unavailable"))?
        .join("runtime-dependencies/linux");
    let mut references = BTreeSet::new();
    if installer.store.root().exists() {
        for entry in walkdir::WalkDir::new(installer.store.root())
            .follow_links(false)
            .max_depth(5)
        {
            let entry = entry.map_err(io)?;
            if entry.file_name() != "linux-dependencies.json" {
                continue;
            }
            if entry.path().components().any(|c| {
                matches!(
                    c.as_os_str().to_str(),
                    Some(".staging" | ".trash" | ".locks")
                )
            }) {
                continue;
            }
            // A pending Python install cannot hold a reference after its acquisition lease ends.
            if entry.path().parent().is_some_and(|p| {
                p.join(".scala-python-pending").exists() && !p.join(RUNTIME_MANIFEST_FILE).exists()
            }) {
                continue;
            }
            let reference: LinuxDependencyReference =
                serde_json::from_slice(&read_bounded(entry.path(), 65536)?).map_err(io)?;
            let key = &reference.environment_sha256;
            if key.len() != 64
                || !key.bytes().all(|b| b.is_ascii_hexdigit())
                || reference.prefix != durable.join(key).join("prefix")
            {
                return Err(invalid(
                    "runtime dependency reference escapes the owned durable store",
                ));
            }
            references.insert(key.clone());
        }
    }
    if present(&durable)? {
        directories(&durable, false)?;
        if read_bounded(&durable.join(".scala-owner"), 1024)? != DURABLE_OWNER.as_bytes() {
            return Err(invalid("durable dependency ownership mismatch"));
        }
        for entry in fs::read_dir(&durable).map_err(io)? {
            let entry = entry.map_err(io)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".scala-owner" {
                continue;
            }
            if name.len() != 64 || !name.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(invalid("unexpected durable dependency member"));
            }
            directories(&entry.path(), false)?;
            if read_bounded(&entry.path().join(".scala-owner"), 1024)?
                != format!("{IMPLEMENTATION}:{name}").as_bytes()
            {
                return Err(invalid("durable environment ownership mismatch"));
            }
            if !references.contains(&name) {
                fs::remove_dir_all(entry.path()).map_err(io)?;
            }
        }
    }
    if present(&cache)? {
        directories(&cache, false)?;
        if read_bounded(&cache.join(".scala-owner"), 1024)? != CACHE_OWNER.as_bytes() {
            return Err(invalid("Linux dependency cache ownership mismatch"));
        }
        fs::remove_dir_all(cache).map_err(io)?;
    }
    Ok(())
}
