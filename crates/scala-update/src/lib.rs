//! Application release checks and explicitly approved cargo-dist replacement.
//! Never reads application configuration, Link credentials or inference state.
use std::{
    fs::{self, File, OpenOptions},
    future::Future,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axoupdater::{AxoUpdater, ReleaseSource, ReleaseSourceType};
use color_eyre::eyre::{Context, Result, bail, ensure};
use scala_core::{
    AppPaths,
    prune::{StorageLease, ensure_no_server_process},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const DAY: u64 = 86400;
const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
const CHANNEL_ERROR: &str =
    "release channel inaccessible (private/unpublished repository, network failure or rate limit)";
const STOP: &str = "Stop existing Scala serving/TUI and control instances before updating; afterward use your original startup method. Scala will not stop or restart them";
const OWNERSHIP: &str = "No matching cargo-dist direct installation. Use the package manager, source scala-service.sh update, or manual installation method that owns this executable";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub checked: u64,
    pub latest: Option<String>,
    pub error: Option<String>,
}
impl State {
    pub fn available(&self) -> bool {
        self.error.is_none()
            && self
                .latest
                .as_deref()
                .is_some_and(|v| newer_stable(CURRENT, v))
    }
    fn fresh(&self, now: u64) -> bool {
        // A future timestamp must never pin a cached result after clock rollback.
        now.checked_sub(self.checked).is_some_and(|age| age < DAY)
    }
    pub fn message(&self) -> String {
        if let Some(error) = &self.error {
            format!("Scala {CURRENT}; update check unavailable: {error}")
        } else {
            format!(
                "Scala {CURRENT}; latest stable: {}{}",
                self.latest.as_deref().unwrap_or("unknown"),
                if self.available() {
                    "; update available — stop Scala, then run scala update"
                } else {
                    ""
                }
            )
        }
    }
    pub fn badge(&self) -> &'static str {
        if self.error.is_some() {
            "Update check failed /update"
        } else if self.available() {
            "Update available /update"
        } else {
            "/update: check Scala"
        }
    }
}
fn newer_stable(current: &str, latest: &str) -> bool {
    match (
        semver::Version::parse(current),
        semver::Version::parse(latest),
    ) {
        (Ok(current), Ok(latest)) => {
            latest.pre.is_empty() && latest.cmp_precedence(&current).is_gt()
        }
        _ => false,
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn reject_source_overrides() -> Result<()> {
    // The child cargo-dist installer inherits the environment. Discovery-only
    // guards are insufficient: these variables can redirect artifact downloads,
    // change installation/receipt behavior, or send credentials during install.
    for key in [
        "SCALA_INSTALLER_GITHUB_BASE_URL",
        "SCALA_INSTALLER_GHE_BASE_URL",
        "SCALA_DOWNLOAD_URL",
        "INSTALLER_DOWNLOAD_URL",
        "SCALA_UNMANAGED_INSTALL",
        "SCALA_DISABLE_UPDATE",
        "SCALA_GITHUB_TOKEN",
        "AXOUPDATER_CONFIG_WORKING_DIR",
        "AXOUPDATER_CONFIG_PATH",
    ] {
        ensure!(
            std::env::var_os(key).is_none(),
            "Unset {key} to use the official ver2-sh/Scala update channel"
        );
    }
    Ok(())
}
fn updater() -> Result<AxoUpdater> {
    reject_source_overrides()?;
    let mut updater = AxoUpdater::new_for("scala");
    updater.set_client(
        update_http::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?,
    );
    updater.disable_installer_output();
    updater.set_release_source(ReleaseSource {
        release_type: ReleaseSourceType::GitHub,
        owner: "ver2-sh".into(),
        name: "Scala".into(),
        app_name: "scala".into(),
    });
    updater.set_current_version(CURRENT.parse()?)?;
    Ok(updater)
}
async fn discover(updater: &mut AxoUpdater) -> Result<String> {
    let version = updater
        .query_new_version()
        .await?
        .ok_or_else(|| color_eyre::eyre::eyre!(CHANNEL_ERROR))?
        .to_string();
    ensure!(
        semver::Version::parse(&version)?.pre.is_empty(),
        "channel returned a prerelease"
    );
    Ok(version)
}

/// Retains the exact discovered release for subsequent explicit approval.
/// `run` reuses axoupdater's requested release, never rediscovers after approval.
pub struct Candidate {
    updater: AxoUpdater,
    pub state: State,
}
impl Candidate {
    pub async fn discover(cache: &Path) -> Result<Self> {
        let mut updater = updater()?;
        let state = check_with(cache, true, CHECK_TIMEOUT, || discover(&mut updater)).await?;
        ensure!(state.error.is_none(), "{}", state.message());
        Ok(Self { updater, state })
    }
    pub async fn install(mut self, paths: &AppPaths) -> Result<()> {
        ensure!(self.state.available(), "No strictly newer stable release");
        let receipt_path = receipt_path()?;
        let receipt: Receipt =
            serde_json::from_slice(&fs::read(&receipt_path).wrap_err(OWNERSHIP)?)
                .wrap_err(OWNERSHIP)?;
        let executable = std::env::current_exe()?.canonicalize()?;
        receipt.validate(&executable)?;
        let locked_prefix = receipt.install_prefix.canonicalize()?;
        let _guard =
            ReplacementGuard::acquire(paths, &executable, &receipt_path, &receipt.install_prefix)?;
        // Re-read ownership under every replacement lock, including the receipt lock.
        let receipt: Receipt = serde_json::from_slice(&fs::read(&receipt_path)?)?;
        receipt.validate(&executable)?;
        ensure!(
            receipt.install_prefix.canonicalize()? == locked_prefix,
            "Installation receipt changed during update; retry"
        );
        self.updater.load_receipt().wrap_err(OWNERSHIP)?;
        ensure!(
            self.updater.check_receipt_is_for_this_executable()?,
            "{OWNERSHIP}"
        );
        ensure!(
            matches!(self.updater.source.as_ref(), Some(source) if official_source(source)),
            "{OWNERSHIP}"
        );
        self.updater.set_current_version(CURRENT.parse()?)?;
        // No timeout around replacement: dropping upstream during Windows rename/restore
        // would abandon its recovery. Discovery/download HTTP calls are bounded.
        let result = self.updater.run().await.wrap_err(if cfg!(windows) {
            "Update failed. PowerShell uses process-only Bypass; MachinePolicy/UserPolicy remain authoritative. If blocked by organizational policy, contact your administrator. Use your original startup method afterward"
        } else { "Update failed; use your original startup method afterward" })?;
        ensure!(result.is_some(), "No binary replacement was performed");
        Ok(())
    }
}
fn official_source(source: &ReleaseSource) -> bool {
    matches!(source.release_type, ReleaseSourceType::GitHub)
        && source.owner == "ver2-sh"
        && source.name == "Scala"
        && source.app_name == "scala"
}

/// Only the UI uses cached checks. Explicit checks never return stale success on contention.
pub async fn check(cache: &Path, force: bool) -> Result<State> {
    check_with(cache, force, CHECK_TIMEOUT, || async {
        discover(&mut updater()?).await
    })
    .await
}
async fn check_with<F, Fut>(cache: &Path, force: bool, timeout: Duration, query: F) -> Result<State>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<String>>,
{
    let path = cache.join("app-update.json");
    let lock = open_lock(&cache.join("app-update.lock"))?;
    if let Err(error) = fs2::FileExt::try_lock_exclusive(&lock) {
        if error.raw_os_error() != fs2::lock_contended_error().raw_os_error() {
            return Err(error.into());
        }
        // Another process owns the fresh attempt. Return without blocking UI/inference;
        // do not use the old result even if a previous successful cache exists.
        return Ok(State {
            checked: now(),
            latest: None,
            error: Some("another update check is in progress; retry shortly".into()),
        });
    }
    // Explicit unlock avoids briefly inheriting a held lock across an unrelated
    // concurrent fork before its child execs (inference/runtime tools may spawn).
    let _lock = FileLock(lock);
    if !force
        && let Ok(bytes) = fs::read(&path)
        && let Ok(state) = serde_json::from_slice::<State>(&bytes)
        && state.fresh(now())
    {
        return Ok(state);
    }
    let mut state = State {
        checked: now(),
        latest: None,
        error: Some("check pending or interrupted; /update retries".into()),
    };
    atomic_write(&path, &state)?;
    match tokio::time::timeout(timeout, query()).await {
        Ok(Ok(latest)) if semver::Version::parse(&latest).is_ok_and(|v| v.pre.is_empty()) => {
            state.latest = Some(latest);
            state.error = None;
        }
        _ => state.error = Some(CHANNEL_ERROR.into()),
    }
    atomic_write(&path, &state)?;
    Ok(state)
}
fn atomic_write(path: &Path, state: &State) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().expect("cache parent"))?;
    serde_json::to_writer(&mut file, state)?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}
fn open_lock(path: &Path) -> Result<File> {
    fs::create_dir_all(path.parent().expect("lock parent"))?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}
/// An OS lock released explicitly before closing, including during unwinding.
pub struct FileLock(File);
impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}
fn lock(path: &Path, exclusive: bool) -> Result<FileLock> {
    lock_file(open_lock(path)?, exclusive)
}
fn lock_file(file: File, exclusive: bool) -> Result<FileLock> {
    let result = if exclusive {
        fs2::FileExt::try_lock_exclusive(&file)
    } else {
        fs2::FileExt::try_lock_shared(&file)
    };
    result.wrap_err("Another Scala instance or update owns this installation; stop existing instances and retry")?;
    Ok(FileLock(file))
}

// This anchor deliberately ignores XDG overrides: multiple state/cache roots using
// the same executable must still share its lock. No lock file is ever unlinked.
fn installation_lock_root() -> Result<PathBuf> {
    let home = directories::UserDirs::new()
        .ok_or_else(|| color_eyre::eyre::eyre!("Home directory unavailable"))?
        .home_dir()
        .to_path_buf();
    let root = if cfg!(windows) {
        home.join("AppData/Local/Scala/update-locks")
    } else if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Scala/update-locks")
    } else {
        home.join(".local/state/scala/update-locks")
    };
    Ok(root)
}
fn installation_lock_at(root: &Path, path: &Path) -> PathBuf {
    let digest = Sha256::digest(path.as_os_str().as_encoded_bytes());
    root.join(format!("{digest:x}.lock"))
}
fn installation_lock(path: &Path) -> Result<PathBuf> {
    Ok(installation_lock_at(&installation_lock_root()?, path))
}
/// Hold before loading application configuration through the entire app session.
/// Independent state directories still share the canonical executable lock.
pub struct SessionGuard {
    _path: FileLock,
    _executable: FileLock,
}
pub fn session_guard() -> Result<SessionGuard> {
    let executable = std::env::current_exe()?.canonicalize()?;
    Ok(SessionGuard {
        _path: lock(&installation_lock(&executable)?, false)?,
        // Also cover hard-link aliases and sessions under another OS account.
        _executable: lock_file(File::open(executable)?, false)?,
    })
}
struct ReplacementGuard {
    _locks: Vec<FileLock>,
    _storage: Option<StorageLease>,
}
impl ReplacementGuard {
    fn acquire(paths: &AppPaths, executable: &Path, receipt: &Path, prefix: &Path) -> Result<Self> {
        Self::acquire_at(
            paths,
            executable,
            receipt,
            prefix,
            &installation_lock_root()?,
        )
    }
    fn acquire_at(
        paths: &AppPaths,
        executable: &Path,
        receipt: &Path,
        prefix: &Path,
        root: &Path,
    ) -> Result<Self> {
        let mut locks = Vec::new();
        // Canonical parent + leaf remains stable even as installer replaces files.
        for path in [
            executable.to_path_buf(),
            receipt
                .parent()
                .unwrap()
                .canonicalize()?
                .join(receipt.file_name().unwrap()),
            prefix.canonicalize()?,
        ] {
            locks.push(lock(&installation_lock_at(root, &path), true)?);
        }
        locks.push(lock_file(File::open(executable)?, true).wrap_err(STOP)?);
        let storage = StorageLease::acquire(paths, true, false).wrap_err(STOP)?;
        locks.push(lock(&paths.state_dir.join("runtime/server-start.lock"), true).wrap_err(STOP)?);
        ensure_no_server_process(paths).wrap_err(STOP)?;
        Ok(Self {
            _locks: locks,
            _storage: storage,
        })
    }
}

#[derive(Deserialize)]
struct Receipt {
    install_prefix: PathBuf,
    binaries: Vec<String>,
    #[serde(default)]
    cdylibs: Vec<String>,
    #[serde(default)]
    cstaticlibs: Vec<String>,
    #[serde(default)]
    install_layout: String,
    source: ReleaseSource,
    version: String,
    provider: Provider,
}
#[derive(Deserialize)]
struct Provider {
    source: String,
    version: String,
}
impl Receipt {
    fn validate(&self, executable: &Path) -> Result<()> {
        let name = if cfg!(windows) { "scala.exe" } else { "scala" };
        ensure!(
            official_source(&self.source)
                && self.provider.source == "cargo-dist"
                && semver::Version::parse(&self.provider.version).is_ok()
                && self.version == CURRENT
                && self.binaries == [name]
                && self.cdylibs.is_empty()
                && self.cstaticlibs.is_empty()
                && self.install_layout == "cargo-home",
            "{OWNERSHIP}"
        );
        // dist-workspace.toml uses CARGO_HOME; its generated installer forces
        // this layout on updates. A flat/manual receipt must not redirect the
        // replacement into a different bin directory and report false success.
        let owns = self
            .install_prefix
            .join("bin")
            .join(name)
            .canonicalize()
            .is_ok_and(|p| p == executable);
        ensure!(
            owns && executable.file_name().is_some_and(|n| n == name),
            "{OWNERSHIP}"
        );
        Ok(())
    }
}
fn receipt_path() -> Result<PathBuf> {
    reject_source_overrides()?;
    // Same ordered lookup as axoupdater 0.10.2; its own load and executable
    // ownership check are still required immediately before replacement.
    let mut roots = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let root = PathBuf::from(xdg).join("scala");
        if root.exists() {
            roots.push(root);
        }
    }
    if cfg!(windows) {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            roots.push(PathBuf::from(local).join("scala"));
        }
    } else if let Some(dirs) = directories::UserDirs::new() {
        roots.push(dirs.home_dir().join(".config/scala"));
    }
    for root in roots {
        let path = root.join("scala-receipt.json");
        if path.try_exists()? {
            return Ok(path);
        }
    }
    bail!(OWNERSHIP)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn state(latest: Option<&str>, error: Option<&str>, checked: u64) -> State {
        State {
            checked,
            latest: latest.map(str::to_owned),
            error: error.map(str::to_owned),
        }
    }
    #[test]
    fn stable_comparison_and_error_visibility() {
        for (version, expected) in [
            (CURRENT, false),
            ("0.0.0", false),
            ("999.0.0", true),
            ("999.0.0-beta.1", false),
            ("invalid", false),
            ("v999.0.0", false),
        ] {
            assert_eq!(state(Some(version), None, 0).available(), expected);
        }
        assert!(!newer_stable("1.0.0", "1.0.0+build.2"));
        assert!(newer_stable("1.0.0-rc.1", "1.0.0"));
        assert!(!newer_stable("invalid", "999.0.0"));
        let failure = state(Some("999.0.0"), Some("offline"), 0);
        assert!(!failure.available());
        assert!(failure.message().contains("offline"));
        assert!(failure.badge().contains("failed"));
        assert!(
            state(Some("999.0.0"), None, 0)
                .badge()
                .contains("available")
        );
    }
    #[test]
    fn freshness_is_bounded_in_both_directions() {
        let state = state(Some(CURRENT), None, DAY);
        assert!(state.fresh(DAY));
        assert!(state.fresh(DAY * 2 - 1));
        assert!(!state.fresh(DAY * 2));
        assert!(!state.fresh(DAY - 1));
    }
    #[tokio::test]
    async fn success_and_error_cache_skip_network_but_force_and_expiration_retry() {
        let dir = tempfile::tempdir().unwrap();
        for error in [None, Some("offline")] {
            atomic_write(
                &dir.path().join("app-update.json"),
                &state(Some(CURRENT), error, now()),
            )
            .unwrap();
            let result = check_with(dir.path(), false, CHECK_TIMEOUT, || async {
                panic!("cached check queried")
            })
            .await
            .unwrap();
            assert_eq!(result.error.as_deref(), error);
        }
        let count = AtomicUsize::new(0);
        let query = || async {
            count.fetch_add(1, Ordering::Relaxed);
            Ok("999.0.0".into())
        };
        assert!(
            check_with(dir.path(), true, CHECK_TIMEOUT, query)
                .await
                .unwrap()
                .available()
        );
        for checked in [now() - DAY, now() + DAY] {
            atomic_write(
                &dir.path().join("app-update.json"),
                &state(None, Some("offline"), checked),
            )
            .unwrap();
            assert!(
                check_with(dir.path(), false, CHECK_TIMEOUT, query)
                    .await
                    .unwrap()
                    .available()
            );
        }
        assert_eq!(count.load(Ordering::Relaxed), 3);
    }
    #[tokio::test]
    async fn failure_invalid_release_timeout_and_cancellation_never_keep_old_success() {
        let dir = tempfile::tempdir().unwrap();
        for value in ["invalid", "999.0.0-beta.1"] {
            let result = check_with(dir.path(), true, CHECK_TIMEOUT, || async {
                Ok(value.into())
            })
            .await
            .unwrap();
            assert!(result.error.is_some());
            assert!(result.latest.is_none());
        }
        let result = check_with(dir.path(), true, Duration::from_millis(1), || {
            std::future::pending()
        })
        .await
        .unwrap();
        assert!(result.error.is_some());
        let cached = check_with(dir.path(), false, CHECK_TIMEOUT, || async {
            panic!("timeout wasn't cached")
        })
        .await
        .unwrap();
        assert!(cached.error.is_some());
        atomic_write(
            &dir.path().join("app-update.json"),
            &state(Some("999.0.0"), None, now()),
        )
        .unwrap();
        let result = check_with(dir.path(), true, CHECK_TIMEOUT, || async {
            bail!("offline")
        })
        .await
        .unwrap();
        assert!(!result.available());
        assert!(result.latest.is_none());
        let pending = check_with(dir.path(), true, CHECK_TIMEOUT, std::future::pending);
        assert!(
            tokio::time::timeout(Duration::from_millis(1), pending)
                .await
                .is_err()
        );
        let cached = check_with(dir.path(), false, CHECK_TIMEOUT, || async {
            panic!("interruption wasn't cached")
        })
        .await
        .unwrap();
        assert!(cached.error.unwrap().contains("interrupted"));
    }
    #[tokio::test]
    async fn concurrent_checks_deduplicate_without_stale_success() {
        let dir = tempfile::tempdir().unwrap();
        atomic_write(
            &dir.path().join("app-update.json"),
            &state(Some("999.0.0"), None, now()),
        )
        .unwrap();
        let held = lock(&dir.path().join("app-update.lock"), true).unwrap();
        for force in [false, true] {
            let result = check_with(dir.path(), force, CHECK_TIMEOUT, || async {
                panic!("contending check queried")
            })
            .await
            .unwrap();
            assert!(!result.available());
            assert!(result.latest.is_none());
            assert!(result.error.unwrap().contains("in progress"));
        }
        drop(held);
        let count = Arc::new(AtomicUsize::new(0));
        let query = || async {
            count.fetch_add(1, Ordering::Relaxed);
            tokio::task::yield_now().await;
            Ok(CURRENT.into())
        };
        let (a, b) = tokio::join!(
            check_with(dir.path(), true, CHECK_TIMEOUT, query),
            check_with(dir.path(), true, CHECK_TIMEOUT, query)
        );
        assert!(a.unwrap().error.is_none());
        assert!(b.unwrap().error.is_some());
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }
    fn paths(root: &Path) -> AppPaths {
        AppPaths {
            config_dir: root.join("config"),
            config_file: root.join("config/config.toml"),
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            cache_dir: root.join("cache"),
            log_dir: root.join("logs"),
            runtimes_dir: root.join("data/runtimes"),
            runtime_cache_dir: root.join("cache/runtime-packs"),
            runtime_selections_file: root.join("data/runtime-selections.json"),
            settings_file: root.join("data/settings.json"),
            settings_lock_file: root.join("data/.settings.lock"),
            model_profiles_file: root.join("data/profiles.json"),
            model_profiles_lock_file: root.join("data/.profiles.lock"),
        }
    }
    #[test]
    fn replacement_excludes_other_roots_receipts_prefixes_sessions_and_startup() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join("install");
        fs::create_dir(&prefix).unwrap();
        let executable = prefix.join("scala");
        fs::write(&executable, "synthetic").unwrap();
        let receipt = dir.path().join("receipt.json");
        let root = dir.path().join("locks");
        let a = paths(&dir.path().join("a"));
        let b = paths(&dir.path().join("b"));
        let acquire = |paths: &AppPaths, exe: &Path, receipt: &Path| {
            ReplacementGuard::acquire_at(paths, exe, receipt, &prefix, &root)
        };
        let held = acquire(&a, &executable, &receipt).unwrap();
        assert!(acquire(&b, &executable, &receipt).is_err());
        // Different binary still shares the same receipt/prefix and cannot update.
        assert!(acquire(&b, &prefix.join("other"), &receipt).is_err());
        assert!(acquire(&b, &prefix.join("other"), &dir.path().join("other-receipt")).is_err());
        assert!(lock(&installation_lock_at(&root, &executable), false).is_err());
        assert!(StorageLease::acquire(&a, false, false).is_err());
        assert!(lock(&a.state_dir.join("runtime/server-start.lock"), true).is_err());
        drop(held);
        let session = lock(&installation_lock_at(&root, &executable), false).unwrap();
        assert!(acquire(&b, &executable, &receipt).is_err());
        drop(session);
        let startup = lock(&a.state_dir.join("runtime/server-start.lock"), true).unwrap();
        assert!(acquire(&a, &executable, &receipt).is_err());
        drop(startup);
        let storage = StorageLease::acquire(&a, false, false).unwrap();
        assert!(acquire(&a, &executable, &receipt).is_err());
        drop(storage);
        let binary_session = lock_file(File::open(&executable).unwrap(), false).unwrap();
        assert!(
            ReplacementGuard::acquire_at(
                &b,
                &executable,
                &receipt,
                &prefix,
                &dir.path().join("other-account-locks")
            )
            .is_err()
        );
        drop(binary_session);
        assert!(acquire(&a, &executable, &receipt).is_ok());
    }
    #[test]
    fn recorded_live_or_unknown_server_blocks_replacement_without_probing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let servers = paths.state_dir.join("runtime/servers");
        fs::create_dir_all(&servers).unwrap();
        fs::write(servers.join("unknown.json"), "{}").unwrap();
        assert!(ensure_no_server_process(&paths).is_err());
        fs::write(
            servers.join("unknown.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 2, "instance_id": "synthetic", "process_id": std::process::id(),
                "endpoint": "http://127.0.0.1:1", "address": "127.0.0.1:1",
                "control_endpoint": "http://127.0.0.1:2", "control_address": "127.0.0.1:2",
                "control_token": "synthetic", "started_at_unix": 0
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(ensure_no_server_process(&paths).is_err());
    }
    #[test]
    fn receipt_policy_requires_exact_official_direct_install() {
        let dir = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) { "scala.exe" } else { "scala" };
        fs::create_dir(dir.path().join("bin")).unwrap();
        let executable = dir.path().join("bin").join(name);
        fs::write(&executable, "synthetic").unwrap();
        let executable = executable.canonicalize().unwrap();
        let value = serde_json::json!({"install_prefix": dir.path(), "install_layout": "cargo-home", "binaries": [name], "version": CURRENT,
            "provider": {"source": "cargo-dist", "version": "0.33.0"},
            "source": {"release_type": "github", "owner": "ver2-sh", "name": "Scala", "app_name": "scala"}});
        let receipt: Receipt = serde_json::from_value(value.clone()).unwrap();
        receipt.validate(&executable).unwrap();
        assert!(
            receipt
                .validate(&dir.path().join("elsewhere/scala"))
                .is_err()
        );
        for pointer in [
            "/install_layout",
            "/provider/source",
            "/provider/version",
            "/source/owner",
            "/source/name",
            "/source/app_name",
            "/version",
            "/binaries/0",
        ] {
            let mut bad = value.clone();
            *bad.pointer_mut(pointer).unwrap() = "wrong".into();
            assert!(
                serde_json::from_value::<Receipt>(bad)
                    .unwrap()
                    .validate(&executable)
                    .is_err(),
                "{pointer}"
            );
        }
    }
    #[test]
    fn locks_are_cross_process_and_released_on_exit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cross-process.lock");
        let held = lock(&path, true).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::child_lock_probe", "--nocapture"])
            .env("SCALA_TEST_LOCK", &path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        drop(held);
        assert!(lock(&path, true).is_ok());
    }
    #[test]
    fn child_lock_probe() {
        if let Some(path) = std::env::var_os("SCALA_TEST_LOCK") {
            assert!(lock(Path::new(&path), true).is_err());
            assert!(lock(Path::new(&path), false).is_err());
        }
    }
}
