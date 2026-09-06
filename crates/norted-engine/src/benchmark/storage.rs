use super::{Run, Summary, now_ms};
use fs2::FileExt;
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;

const MAX_RECORD_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SUMMARIES: usize = 4096;
pub(crate) struct Store {
    path: PathBuf,
    serial: Arc<Mutex<()>>,
    cache: std::sync::Mutex<Option<Vec<Summary>>>,
}
impl Store {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            serial: Arc::new(Mutex::new(())),
            cache: std::sync::Mutex::new(None),
        }
    }
    pub async fn claim_server(&self) -> Result<std::fs::File, String> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let _lock = lock(&path)?;
            let mut options = std::fs::OpenOptions::new();
            options.create(true).truncate(false).read(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let owner = options
                .open(path.join(".owner"))
                .map_err(|e| e.to_string())?;
            owner.try_lock_exclusive().map_err(|_| {
                "another server owns benchmark history; recovery was not attempted".to_owned()
            })?;
            Ok(owner)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    pub async fn save(&self, run: &Run) -> Result<(), String> {
        self.save_before(run, None).await
    }
    pub async fn save_before(
        &self,
        run: &Run,
        deadline: Option<std::time::Instant>,
    ) -> Result<(), String> {
        let path = self.path.clone();
        let run = run.clone();
        let _serial = self.serial.lock().await;
        *self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        tokio::task::spawn_blocking(move || {
            let _lock = lock(&path)?;
            let terminal = path.join(format!("{}.json", run.run_id));
            if terminal.exists() {
                return Err("terminal benchmark records are immutable".into());
            }
            let active = path.join(format!("{}.active", run.run_id));
            let finished = run.status != "running";
            atomic_before(if finished { &terminal } else { &active }, &run, deadline)?;
            atomic(
                &path.join(format!("{}.summary", run.run_id)),
                &run.summary(),
            )?;
            if finished && active.exists() {
                std::fs::remove_file(active).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }
    pub async fn recover(&self) -> Result<(), String> {
        let path = self.path.clone();
        let _serial = self.serial.lock().await;
        tokio::task::spawn_blocking(move|| {
            let _lock=lock(&path)?;
            for entry in std::fs::read_dir(&path).map_err(|e|e.to_string())? {
                let file=entry.map_err(|e|e.to_string())?.path();
                if file.extension().is_some_and(|s|s=="active") {
                    let mut run:Run=read(&file,MAX_RECORD_BYTES)?;
                    let terminal=path.join(format!("{}.json",run.run_id));
                    if !terminal.exists() {
                        run.status="interrupted".into();run.ended_unix_ms=Some(now_ms());
                        run.diagnostic=Some("Server stopped before finalization; preserved evidence, no automatic resume".into());
                        atomic(&terminal,&run)?;
                        atomic(&path.join(format!("{}.summary",run.run_id)),&run.summary())?;
                    } else {
                        let completed:Run=read(&terminal,MAX_RECORD_BYTES)?;
                        atomic(&path.join(format!("{}.summary",run.run_id)),&completed.summary())?;
                    }
                    std::fs::remove_file(file).map_err(|e|e.to_string())?;
                }
            }
            Ok(())
        }).await.map_err(|e|e.to_string())?
    }
    pub async fn summaries(&self) -> Result<Vec<Summary>, String> {
        let path = self.path.clone();
        let _serial = self.serial.lock().await;
        if let Some(rows) = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            return Ok(rows.clone());
        }
        let result = tokio::task::spawn_blocking(move || {
            if !path.exists() {
                return Ok(Vec::new());
            }
            let mut files = Vec::new();
            for e in std::fs::read_dir(path).map_err(|e| e.to_string())? {
                let e = e.map_err(|e| e.to_string())?;
                if e.path().extension().is_some_and(|s| s == "summary") {
                    files.push((e.metadata().and_then(|m| m.modified()).ok(), e.path()));
                }
            }
            files.sort_by(|a, b| b.0.cmp(&a.0));
            let mut rows = files
                .into_iter()
                .take(MAX_SUMMARIES)
                .map(|(_, p)| read::<Summary>(&p, 256 * 1024))
                .collect::<Result<Vec<_>, _>>()?;
            rows.sort_by(|a, b| b.started_unix_ms.cmp(&a.started_unix_ms));
            Ok(rows)
        })
        .await
        .map_err(|e| e.to_string())?;
        if let Ok(rows) = &result {
            *self
                .cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(rows.clone());
        }
        result
    }
    pub async fn result(&self, id: &str) -> Result<Run, String> {
        let id = uuid::Uuid::parse_str(id)
            .map_err(|_| "invalid run ID")?
            .to_string();
        let path = self.path.clone();
        let _serial = self.serial.lock().await;
        tokio::task::spawn_blocking(move || {
            let terminal = path.join(format!("{id}.json"));
            read(
                if terminal.exists() {
                    terminal
                } else {
                    path.join(format!("{id}.active"))
                }
                .as_path(),
                MAX_RECORD_BYTES,
            )
        })
        .await
        .map_err(|e| e.to_string())?
    }
}
fn read<T: serde::de::DeserializeOwned>(path: &Path, limit: u64) -> Result<T, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() > limit {
        return Err("benchmark file exceeds size limit".into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("benchmark file exceeds size limit".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}
fn lock(path: &Path) -> Result<std::fs::File, String> {
    std::fs::create_dir_all(path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.join(".lock"))
        .map_err(|e| e.to_string())?;
    file.lock_exclusive().map_err(|e| e.to_string())?;
    Ok(file)
}
fn atomic<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    atomic_before(path, value, None)
}
fn atomic_before<T: serde::Serialize>(
    path: &Path,
    value: &T,
    deadline: Option<std::time::Instant>,
) -> Result<(), String> {
    let parent = path.parent().ok_or("missing benchmark parent")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err("benchmark record exceeds bound".into());
    }
    temporary
        .write_all(&bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
        return Err("benchmark finalization deadline; checkpoint retained".into());
    }
    temporary.persist(path).map_err(|e| e.to_string())?;
    std::fs::File::open(parent)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}
