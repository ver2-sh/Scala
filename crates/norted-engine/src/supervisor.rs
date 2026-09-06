use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::{RwLock, mpsc, oneshot, watch};
use uuid::Uuid;

use crate::{
    EngineError, EngineRevision, LaunchSpec, ProcessDescriptor, ProcessExit, ProcessSupervisor,
};

const DEFAULT_TERMINATION_TIMEOUT: Duration = Duration::from_secs(8);
const LOG_TAIL_LINES: usize = 80;
const LOG_LINE_LIMIT: usize = 4_096;

#[derive(Debug)]
pub struct CapturedCommand {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Runs a short-lived engine command while keeping generic process mechanics
/// out of individual adapters.
pub async fn capture_command(
    executable: &Path,
    arguments: &[&str],
    environment: &BTreeMap<String, String>,
    environment_remove: &[OsString],
    timeout: Duration,
) -> Result<CapturedCommand, EngineError> {
    let mut command = Command::new(executable);
    for name in environment_remove {
        command.env_remove(name);
    }
    command
        .args(arguments)
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = command.spawn().map_err(|error| {
        EngineError::Operation(format!("could not start engine probe: {error}"))
    })?;
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| EngineError::TimedOut("engine probe did not finish in time".to_owned()))?
        .map_err(|error| EngineError::Operation(format!("engine probe failed: {error}")))?;
    Ok(CapturedCommand {
        success: output.status.success(),
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

#[derive(Clone)]
pub struct TokioProcessSupervisor {
    inner: Arc<SupervisorInner>,
    termination_timeout: Duration,
}

struct SupervisorInner {
    processes: RwLock<HashMap<String, ManagedProcess>>,
}

#[derive(Clone)]
struct ManagedProcess {
    descriptor: ProcessDescriptor,
    commands: mpsc::Sender<ProcessCommand>,
    exit: watch::Receiver<Option<ProcessExit>>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
}

enum ProcessCommand {
    Terminate {
        immediate: bool,
        response: oneshot::Sender<Result<(), String>>,
    },
}

struct ChildActor {
    child: Child,
    descriptor: ProcessDescriptor,
    commands: mpsc::Receiver<ProcessCommand>,
    exit_sender: watch::Sender<Option<ProcessExit>>,
    stdout_task: tokio::task::JoinHandle<()>,
    stderr_task: tokio::task::JoinHandle<()>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    temporary_files: Vec<std::path::PathBuf>,
    termination_timeout: Duration,
}

impl Default for TokioProcessSupervisor {
    fn default() -> Self {
        Self {
            inner: Arc::new(SupervisorInner {
                processes: RwLock::new(HashMap::new()),
            }),
            termination_timeout: DEFAULT_TERMINATION_TIMEOUT,
        }
    }
}

impl TokioProcessSupervisor {
    pub fn with_termination_timeout(timeout: Duration) -> Self {
        Self {
            termination_timeout: timeout,
            ..Self::default()
        }
    }
}

#[async_trait]
impl ProcessSupervisor for TokioProcessSupervisor {
    async fn spawn(
        &self,
        spec: LaunchSpec,
        engine: EngineRevision,
        model_id: norted_core::ModelId,
    ) -> Result<ProcessDescriptor, EngineError> {
        self.inner
            .processes
            .write()
            .await
            .retain(|_, process| process.exit.borrow().is_none());
        let mut command = Command::new(&spec.executable);
        if !spec.inherits_parent_environment {
            command.env_clear();
        }
        for name in &spec.environment_remove {
            command.env_remove(name);
        }
        command
            .args(&spec.arguments)
            .envs(&spec.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(directory) = &spec.working_directory {
            command.current_dir(directory);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                cleanup_temporary_files(&spec.temporary_files).await;
                return Err(EngineError::Operation(format!(
                    "could not start managed engine process: {error}"
                )));
            }
        };
        let Some(process_id) = child.id() else {
            let _ = child.start_kill();
            let _ = child.wait().await;
            cleanup_temporary_files(&spec.temporary_files).await;
            return Err(EngineError::Operation(
                "managed child did not expose a process ID".to_owned(),
            ));
        };
        let descriptor = ProcessDescriptor {
            supervisor_id: Uuid::new_v4().to_string(),
            process_id,
            engine,
            runtime_id: spec.runtime.manifest.runtime_id.clone(),
            runtime_version: spec.runtime.manifest.identity.version.clone(),
            runtime_variant: spec.runtime.manifest.identity.variant.clone(),
            runtime_executable_sha256: spec.runtime.manifest.entrypoint_sha256.clone(),
            model_id,
            endpoint: spec.endpoint,
            launched_at_unix: unix_timestamp(),
        };
        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            let _ = child.start_kill();
            let _ = child.wait().await;
            cleanup_temporary_files(&spec.temporary_files).await;
            return Err(EngineError::Operation(
                "managed child output streams were not captured".to_owned(),
            ));
        };
        let tail = Arc::new(Mutex::new(VecDeque::with_capacity(LOG_TAIL_LINES)));
        let stdout_task = tokio::spawn(drain_stream(stdout, descriptor.clone(), "stdout", None));
        let stderr_task = tokio::spawn(drain_stream(
            stderr,
            descriptor.clone(),
            "stderr",
            Some(Arc::clone(&tail)),
        ));
        let (commands, command_receiver) = mpsc::channel(2);
        let (exit_sender, exit) = watch::channel(None);
        self.inner.processes.write().await.insert(
            descriptor.supervisor_id.clone(),
            ManagedProcess {
                descriptor: descriptor.clone(),
                commands,
                exit,
                stderr_tail: Arc::clone(&tail),
            },
        );
        tokio::spawn(run_child_actor(ChildActor {
            child,
            descriptor: descriptor.clone(),
            commands: command_receiver,
            exit_sender,
            stdout_task,
            stderr_task,
            stderr_tail: tail,
            temporary_files: spec.temporary_files,
            termination_timeout: self.termination_timeout,
        }));
        Ok(descriptor)
    }

    async fn terminate(&self, process: &ProcessDescriptor) -> Result<(), EngineError> {
        let managed = self
            .inner
            .processes
            .read()
            .await
            .get(&process.supervisor_id)
            .cloned()
            .ok_or_else(|| EngineError::Operation("managed process is not tracked".to_owned()))?;
        if managed.descriptor.process_id != process.process_id {
            return Err(EngineError::Operation(
                "managed process identity does not match the requested process".to_owned(),
            ));
        }
        let mut exit = managed.exit.clone();
        if exit.borrow().is_some() {
            return Ok(());
        }
        let (response, completed) = oneshot::channel();
        if managed
            .commands
            .send(ProcessCommand::Terminate {
                response,
                immediate: false,
            })
            .await
            .is_err()
        {
            return if observe_exit(&mut exit, Duration::from_millis(250)).await {
                Ok(())
            } else {
                Err(EngineError::Operation(
                    "managed process termination channel closed unexpectedly".to_owned(),
                ))
            };
        }
        let result =
            tokio::time::timeout(self.termination_timeout + Duration::from_secs(5), completed)
                .await;
        match result {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(detail))) => {
                if observe_exit(&mut exit, Duration::from_millis(250)).await {
                    Ok(())
                } else {
                    Err(EngineError::Operation(detail))
                }
            }
            Ok(Err(_)) => {
                if observe_exit(&mut exit, Duration::from_millis(250)).await {
                    Ok(())
                } else {
                    Err(EngineError::Operation(
                        "managed process termination task ended unexpectedly".to_owned(),
                    ))
                }
            }
            Err(_) => {
                if observe_exit(&mut exit, Duration::from_millis(250)).await {
                    Ok(())
                } else {
                    Err(EngineError::TimedOut(
                        "managed process termination did not complete".to_owned(),
                    ))
                }
            }
        }
    }

    async fn terminate_immediately(&self, process: &ProcessDescriptor) -> Result<(), EngineError> {
        let managed = self
            .inner
            .processes
            .read()
            .await
            .get(&process.supervisor_id)
            .cloned()
            .ok_or_else(|| EngineError::Operation("managed process is not tracked".into()))?;
        if managed.descriptor.process_id != process.process_id {
            return Err(EngineError::Operation(
                "managed process identity mismatch".into(),
            ));
        }
        if managed.exit.borrow().is_some() {
            return Ok(());
        }
        let (response, completed) = oneshot::channel();
        let sent = managed
            .commands
            .send(ProcessCommand::Terminate {
                response,
                immediate: true,
            })
            .await;
        if sent.is_err() {
            return if managed.exit.borrow().is_some() {
                Ok(())
            } else {
                Err(EngineError::Operation(
                    "managed cancellation channel closed".into(),
                ))
            };
        }
        let result = tokio::time::timeout(Duration::from_secs(4), completed)
            .await
            .map_err(|_| EngineError::TimedOut("managed immediate termination".into()))?
            .map_err(|_| EngineError::Operation("managed cancellation response closed".into()))?
            .map_err(EngineError::Operation);
        if result.is_err() && managed.exit.borrow().is_some() {
            Ok(())
        } else {
            result
        }
    }

    async fn subscribe(
        &self,
        process: &ProcessDescriptor,
    ) -> Result<watch::Receiver<Option<ProcessExit>>, EngineError> {
        let processes = self.inner.processes.read().await;
        let managed = processes
            .get(&process.supervisor_id)
            .ok_or_else(|| EngineError::Operation("managed process is not tracked".to_owned()))?;
        if managed.descriptor.process_id != process.process_id {
            return Err(EngineError::Operation(
                "managed process identity does not match the requested process".to_owned(),
            ));
        }
        Ok(managed.exit.clone())
    }

    async fn stderr_tail(&self, process: &ProcessDescriptor) -> Result<Vec<String>, EngineError> {
        let tail = self
            .inner
            .processes
            .read()
            .await
            .get(&process.supervisor_id)
            .filter(|managed| managed.descriptor.process_id == process.process_id)
            .map(|managed| Arc::clone(&managed.stderr_tail))
            .ok_or_else(|| EngineError::Operation("managed process is not tracked".to_owned()))?;
        let result = tail
            .lock()
            .map_err(|_| EngineError::Operation("managed process log tail is poisoned".to_owned()))?
            .iter()
            .cloned()
            .collect();
        Ok(result)
    }

    async fn shutdown(&self) {
        let processes = self
            .inner
            .processes
            .read()
            .await
            .values()
            .map(|process| process.descriptor.clone())
            .collect::<Vec<_>>();
        for process in processes {
            if let Err(error) = self.terminate(&process).await {
                tracing::warn!(
                    process_id = process.process_id,
                    %error,
                    "could not terminate managed process during shutdown"
                );
            }
        }
        self.inner.processes.write().await.clear();
    }
}

async fn run_child_actor(actor: ChildActor) {
    let ChildActor {
        mut child,
        descriptor,
        mut commands,
        exit_sender,
        mut stdout_task,
        mut stderr_task,
        stderr_tail,
        temporary_files,
        termination_timeout,
    } = actor;
    let mut termination_response = None;
    let mut expected = false;
    let result = tokio::select! {
        result = child.wait() => result,
        command = commands.recv() => {
            if let Some(ProcessCommand::Terminate { response, immediate }) = command {
                expected = true;
                termination_response = Some(response);
                terminate_child(&mut child, descriptor.process_id, if immediate { Duration::ZERO } else { termination_timeout }).await
            } else {
                child.wait().await
            }
        }
    };

    if tokio::time::timeout(Duration::from_secs(1), &mut stdout_task)
        .await
        .is_err()
    {
        stdout_task.abort();
    }
    if tokio::time::timeout(Duration::from_secs(1), &mut stderr_task)
        .await
        .is_err()
    {
        stderr_task.abort();
    }

    let stderr_tail = stderr_tail
        .lock()
        .map(|tail| tail.iter().cloned().collect())
        .unwrap_or_default();
    let wait_succeeded = result.is_ok();
    let (success, code, detail) = match result {
        Ok(status) => (
            status.success(),
            status.code(),
            status.code().map_or_else(
                || "process exited without an exit code".to_owned(),
                |code| format!("process exited with code {code}"),
            ),
        ),
        Err(error) => (
            false,
            None,
            format!("could not wait for process exit: {error}"),
        ),
    };
    let exit = ProcessExit {
        process_id: descriptor.process_id,
        success,
        code,
        expected,
        detail,
        stderr_tail,
    };
    tracing::info!(
        engine = %descriptor.engine.engine_id,
        model_id = %descriptor.model_id,
        process_id = descriptor.process_id,
        expected,
        success,
        code,
        "managed engine process exited"
    );
    let termination_result = if expected && wait_succeeded {
        Ok(())
    } else if expected {
        Err(exit.detail.clone())
    } else {
        Ok(())
    };
    exit_sender.send_replace(Some(exit));
    if let Some(response) = termination_response {
        let _ = response.send(termination_result);
    }
    cleanup_temporary_files(&temporary_files).await;
}

async fn cleanup_temporary_files(paths: &[std::path::PathBuf]) {
    for path in paths {
        if let Err(error) = tokio::fs::remove_file(path).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %path.display(), %error, "could not remove temporary engine process file");
        }
    }
}

async fn terminate_child(
    child: &mut Child,
    process_id: u32,
    timeout: Duration,
) -> std::io::Result<std::process::ExitStatus> {
    request_graceful_termination(child, process_id)?;
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(result) => result,
        Err(_) => {
            child.start_kill()?;
            tokio::time::timeout(Duration::from_secs(2), child.wait())
                .await
                .map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::TimedOut, "force termination timed out")
                })?
        }
    }
}

#[cfg(unix)]
fn request_graceful_termination(_child: &mut Child, process_id: u32) -> std::io::Result<()> {
    let process_id = i32::try_from(process_id)
        .map_err(|_| std::io::Error::other("process ID is outside the platform range"))?;
    // SAFETY: kill only observes the integer PID and sends SIGTERM; no pointers
    // or borrowed memory cross the FFI boundary.
    if unsafe { libc::kill(process_id, libc::SIGTERM) } == 0 {
        Ok(())
    } else {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }
}

#[cfg(not(unix))]
fn request_graceful_termination(child: &mut Child, _process_id: u32) -> std::io::Result<()> {
    // Windows does not expose a portable graceful signal for arbitrary child
    // processes. TerminateProcess is the best available child-owned fallback.
    child.start_kill()
}

async fn drain_stream<R>(
    mut reader: R,
    descriptor: ProcessDescriptor,
    stream: &'static str,
    tail: Option<Arc<Mutex<VecDeque<String>>>>,
) where
    R: AsyncRead + Unpin,
{
    let mut pending = Vec::with_capacity(LOG_LINE_LIMIT);
    let mut chunk = [0_u8; 1024];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => {
                for byte in &chunk[..read] {
                    if *byte == b'\n' {
                        record_log_line(&pending, &descriptor, stream, tail.as_ref());
                        pending.clear();
                    } else if pending.len() < LOG_LINE_LIMIT {
                        pending.push(*byte);
                    }
                }
            }
            Err(error) => {
                tracing::warn!(
                    engine = %descriptor.engine.engine_id,
                    model_id = %descriptor.model_id,
                    process_id = descriptor.process_id,
                    stream,
                    %error,
                    "could not drain engine process output"
                );
                break;
            }
        }
    }
    if !pending.is_empty() {
        record_log_line(&pending, &descriptor, stream, tail.as_ref());
    }
}

fn record_log_line(
    bytes: &[u8],
    descriptor: &ProcessDescriptor,
    stream: &'static str,
    tail: Option<&Arc<Mutex<VecDeque<String>>>>,
) {
    let line = String::from_utf8_lossy(bytes)
        .trim_end_matches('\r')
        .to_owned();
    if let Some(tail) = tail
        && let Ok(mut tail) = tail.lock()
    {
        if tail.len() == LOG_TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(line.clone());
    }
    if stream == "stderr" {
        tracing::warn!(
            engine = %descriptor.engine.engine_id,
            model_id = %descriptor.model_id,
            process_id = descriptor.process_id,
            stream,
            message = %line,
            "engine process output"
        );
    } else {
        tracing::info!(
            engine = %descriptor.engine.engine_id,
            model_id = %descriptor.model_id,
            process_id = descriptor.process_id,
            stream,
            message = %line,
            "engine process output"
        );
    }
}

async fn observe_exit(exit: &mut watch::Receiver<Option<ProcessExit>>, timeout: Duration) -> bool {
    if exit.borrow().is_some() {
        return true;
    }
    tokio::time::timeout(timeout, async {
        loop {
            if exit.changed().await.is_err() {
                return exit.borrow().is_some();
            }
            if exit.borrow().is_some() {
                return true;
            }
        }
    })
    .await
    .unwrap_or(false)
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}
