use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::{ModelOperationPhase, ModelOperationProgress};

pub const DEFAULT_MAX_PARALLEL_DOWNLOADS: usize = 4;
pub const MAX_PARALLEL_DOWNLOADS_SETTING_ID: &str = "server.max_parallel_model_downloads";
const MAX_RECENT_TERMINAL_JOBS: usize = 24;

#[derive(Debug, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelDownloadJobId(String);

impl ModelDownloadJobId {
    pub(crate) fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

impl std::fmt::Display for ModelDownloadJobId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDownloadJob {
    pub id: ModelDownloadJobId,
    pub model_ref: String,
    pub provider: Option<String>,
    pub repository: Option<String>,
    pub filename: Option<String>,
    pub phase: ModelOperationPhase,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub progress_percent: Option<f64>,
    pub transfer_bytes_per_second: Option<f64>,
    pub elapsed: Duration,
    pub estimated_remaining: Option<Duration>,
    pub queue_position: Option<usize>,
    pub message: String,
}

impl ModelDownloadJob {
    pub fn is_terminal(&self) -> bool {
        self.phase.is_terminal()
    }
}

#[derive(Debug, Clone)]
pub enum DownloadAdmission {
    Started(ModelDownloadJob),
    Queued(ModelDownloadJob),
    Duplicate(ModelDownloadJob),
}

#[derive(Debug)]
struct JobRecord {
    id: ModelDownloadJobId,
    model_ref: String,
    provider: Option<String>,
    repository: Option<String>,
    filename: Option<String>,
    phase: ModelOperationPhase,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    message: String,
    enqueued_at: Instant,
    started_at: Option<Instant>,
    finished_at: Option<Instant>,
    transfer_started_at: Option<Instant>,
    transfer_baseline_bytes: u64,
    generation: u64,
    active_control: Option<ActiveControl>,
    resume_after_stop: bool,
    partial_paths: HashSet<std::path::PathBuf>,
}

#[derive(Debug)]
struct ActiveControl {
    generation: u64,
    stop: watch::Sender<bool>,
}

#[derive(Debug)]
pub(crate) struct DownloadStart {
    pub(crate) id: ModelDownloadJobId,
    pub(crate) generation: u64,
    pub(crate) stop: watch::Receiver<bool>,
}

#[derive(Debug, Default)]
pub(crate) struct DownloadTransition {
    pub(crate) starts: Vec<DownloadStart>,
    pub(crate) cleanup: Vec<std::path::PathBuf>,
}

#[derive(Debug)]
struct State {
    maximum_parallel: usize,
    jobs: HashMap<ModelDownloadJobId, JobRecord>,
    order: Vec<ModelDownloadJobId>,
    queue: VecDeque<ModelDownloadJobId>,
    active: HashSet<ModelDownloadJobId>,
}

#[derive(Clone, Debug)]
pub(crate) struct DownloadManager {
    state: Arc<Mutex<State>>,
}

impl DownloadManager {
    pub(crate) fn new(maximum_parallel: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                maximum_parallel: maximum_parallel.max(1),
                jobs: HashMap::new(),
                order: Vec::new(),
                queue: VecDeque::new(),
                active: HashSet::new(),
            })),
        }
    }

    pub(crate) fn admit(&self, model_ref: String) -> (DownloadAdmission, Vec<DownloadStart>) {
        let mut state = self.state.lock().expect("download manager lock poisoned");
        if let Some(record) = state
            .jobs
            .values()
            .find(|record| record.model_ref == model_ref && !record.phase.is_terminal())
        {
            return (
                DownloadAdmission::Duplicate(snapshot(&state, record)),
                Vec::new(),
            );
        }

        let id = ModelDownloadJobId::new();
        let (provider, repository, filename) = identity_from_reference(&model_ref);
        let record = JobRecord {
            id: id.clone(),
            model_ref,
            provider,
            repository,
            filename,
            phase: ModelOperationPhase::Queued,
            downloaded_bytes: 0,
            total_bytes: None,
            message: "Waiting for an available download slot".to_owned(),
            enqueued_at: Instant::now(),
            started_at: None,
            finished_at: None,
            transfer_started_at: None,
            transfer_baseline_bytes: 0,
            generation: 0,
            active_control: None,
            resume_after_stop: false,
            partial_paths: HashSet::new(),
        };
        state.order.push(id.clone());
        state.queue.push_back(id.clone());
        state.jobs.insert(id.clone(), record);
        let starts = schedule_locked(&mut state);
        let job = snapshot(
            &state,
            state.jobs.get(&id).expect("new download job is present"),
        );
        let admission = if state.active.contains(&id) {
            DownloadAdmission::Started(job)
        } else {
            DownloadAdmission::Queued(job)
        };
        (admission, starts)
    }

    pub(crate) fn set_maximum_parallel(&self, maximum_parallel: usize) -> Vec<DownloadStart> {
        let mut state = self.state.lock().expect("download manager lock poisoned");
        state.maximum_parallel = maximum_parallel.max(1);
        schedule_locked(&mut state)
    }

    pub(crate) fn maximum_parallel(&self) -> usize {
        self.state
            .lock()
            .expect("download manager lock poisoned")
            .maximum_parallel
    }

    pub(crate) fn model_ref(&self, id: &ModelDownloadJobId) -> Option<String> {
        self.state
            .lock()
            .expect("download manager lock poisoned")
            .jobs
            .get(id)
            .map(|record| record.model_ref.clone())
    }

    pub(crate) fn update(&self, progress: &ModelOperationProgress, generation: Option<u64>) {
        let mut state = self.state.lock().expect("download manager lock poisoned");
        let Some(record) = state.jobs.get_mut(&progress.job_id) else {
            return;
        };
        if generation.is_some_and(|generation| {
            record
                .active_control
                .as_ref()
                .is_none_or(|active| active.generation != generation)
        }) || matches!(
            record.phase,
            ModelOperationPhase::Paused | ModelOperationPhase::Cancelled
        ) {
            return;
        }
        if progress.phase == ModelOperationPhase::Downloading
            && record.phase != ModelOperationPhase::Downloading
        {
            record.transfer_started_at = Some(Instant::now());
            record.transfer_baseline_bytes = progress.downloaded_bytes;
        }
        record.provider.clone_from(&progress.provider);
        record.repository.clone_from(&progress.repository);
        if !progress.filename.is_empty() {
            record.filename = Some(progress.filename.clone());
        }
        record.phase = progress.phase;
        if progress.phase.is_terminal() && record.finished_at.is_none() {
            record.finished_at = Some(Instant::now());
        }
        record.downloaded_bytes = progress.downloaded_bytes;
        record.total_bytes = progress.total_bytes;
        record.message.clone_from(&progress.message);
    }

    pub(crate) fn register_partial_paths(
        &self,
        id: &ModelDownloadJobId,
        generation: Option<u64>,
        paths: impl IntoIterator<Item = std::path::PathBuf>,
    ) {
        let mut state = self.state.lock().expect("download manager lock poisoned");
        let Some(record) = state.jobs.get_mut(id) else {
            return;
        };
        if generation.is_some_and(|generation| {
            record
                .active_control
                .as_ref()
                .is_none_or(|active| active.generation != generation)
        }) {
            return;
        }
        record.partial_paths.extend(paths);
    }

    pub(crate) fn pause(
        &self,
        id: &ModelDownloadJobId,
    ) -> Result<(ModelDownloadJob, Vec<DownloadStart>), String> {
        let mut state = self.state.lock().expect("download manager lock poisoned");
        let Some(record) = state.jobs.get(id) else {
            return Err("Download job no longer exists".to_owned());
        };
        match record.phase {
            ModelOperationPhase::Queued => {
                state.queue.retain(|queued| queued != id);
            }
            ModelOperationPhase::Resolving | ModelOperationPhase::Downloading => {
                if let Some(active) = &record.active_control {
                    active.stop.send_replace(true);
                }
                state.active.remove(id);
            }
            ModelOperationPhase::Paused => return Err("Download is already paused".to_owned()),
            _ => return Err("Download can no longer be paused safely".to_owned()),
        }
        let record = state
            .jobs
            .get_mut(id)
            .expect("download job remains present");
        record.phase = ModelOperationPhase::Paused;
        record.message = "Paused".to_owned();
        record.transfer_started_at = None;
        record.transfer_baseline_bytes = record.downloaded_bytes;
        record.resume_after_stop = false;
        let starts = schedule_locked(&mut state);
        let job = snapshot(
            &state,
            state.jobs.get(id).expect("download job remains present"),
        );
        Ok((job, starts))
    }

    pub(crate) fn resume(
        &self,
        id: &ModelDownloadJobId,
    ) -> Result<(ModelDownloadJob, Vec<DownloadStart>), String> {
        let mut state = self.state.lock().expect("download manager lock poisoned");
        let Some(record) = state.jobs.get_mut(id) else {
            return Err("Download job no longer exists".to_owned());
        };
        if record.phase != ModelOperationPhase::Paused {
            return Err("Only a paused download can be resumed".to_owned());
        }
        record.transfer_started_at = None;
        record.transfer_baseline_bytes = record.downloaded_bytes;
        if record.active_control.is_some() {
            record.resume_after_stop = true;
            record.message = "Resuming after the paused transfer stops".to_owned();
        } else {
            record.phase = ModelOperationPhase::Queued;
            record.message = "Waiting for an available download slot".to_owned();
            state.queue.push_back(id.clone());
        }
        let starts = schedule_locked(&mut state);
        let job = snapshot(
            &state,
            state.jobs.get(id).expect("download job remains present"),
        );
        Ok((job, starts))
    }

    pub(crate) fn cancel(
        &self,
        id: &ModelDownloadJobId,
    ) -> Result<(ModelDownloadJob, DownloadTransition), String> {
        let mut state = self.state.lock().expect("download manager lock poisoned");
        let Some(record) = state.jobs.get(id) else {
            return Err("Download job no longer exists".to_owned());
        };
        if record.phase.is_terminal() {
            return Err("Download has already finished".to_owned());
        }
        if matches!(
            record.phase,
            ModelOperationPhase::Verifying
                | ModelOperationPhase::Validating
                | ModelOperationPhase::Installing
        ) {
            return Err("Download is completing an atomic post-download stage".to_owned());
        }
        if let Some(active) = &record.active_control {
            active.stop.send_replace(true);
        }
        state.queue.retain(|queued| queued != id);
        state.active.remove(id);
        let record = state
            .jobs
            .get_mut(id)
            .expect("download job remains present");
        record.phase = ModelOperationPhase::Cancelled;
        record.message = "Download cancelled".to_owned();
        record.finished_at = Some(Instant::now());
        record.resume_after_stop = false;
        record.transfer_started_at = None;
        let cleanup = if record.active_control.is_none() {
            record.partial_paths.drain().collect()
        } else {
            Vec::new()
        };
        let starts = schedule_locked(&mut state);
        prune_locked(&mut state);
        let job = snapshot(
            &state,
            state.jobs.get(id).expect("download job remains present"),
        );
        Ok((job, DownloadTransition { starts, cleanup }))
    }

    pub(crate) fn finish_attempt(
        &self,
        id: &ModelDownloadJobId,
        generation: u64,
        completion: Option<(ModelOperationPhase, String)>,
    ) -> DownloadTransition {
        let mut state = self.state.lock().expect("download manager lock poisoned");
        let Some(record) = state.jobs.get_mut(id) else {
            return DownloadTransition::default();
        };
        if record
            .active_control
            .as_ref()
            .is_none_or(|active| active.generation != generation)
        {
            return DownloadTransition::default();
        }
        record.active_control = None;
        state.active.remove(id);

        let mut cleanup = Vec::new();
        let phase = state
            .jobs
            .get(id)
            .expect("download job remains present")
            .phase;
        if phase == ModelOperationPhase::Paused {
            let record = state
                .jobs
                .get_mut(id)
                .expect("download job remains present");
            if record.resume_after_stop {
                record.resume_after_stop = false;
                record.phase = ModelOperationPhase::Queued;
                record.message = "Waiting for an available download slot".to_owned();
                state.queue.push_back(id.clone());
            }
        } else if phase == ModelOperationPhase::Cancelled {
            cleanup = state
                .jobs
                .get_mut(id)
                .expect("download job remains present")
                .partial_paths
                .drain()
                .collect();
        } else if let Some((phase, message)) = completion {
            let record = state
                .jobs
                .get_mut(id)
                .expect("download job remains present");
            record.phase = phase;
            record.message = message;
            if phase.is_terminal() && record.finished_at.is_none() {
                record.finished_at = Some(Instant::now());
            }
            if phase == ModelOperationPhase::Installed {
                cleanup = record.partial_paths.drain().collect();
            }
        }
        let starts = schedule_locked(&mut state);
        prune_locked(&mut state);
        DownloadTransition { starts, cleanup }
    }

    pub(crate) fn snapshots(&self) -> Vec<ModelDownloadJob> {
        let state = self.state.lock().expect("download manager lock poisoned");
        state
            .order
            .iter()
            .filter_map(|id| state.jobs.get(id))
            .map(|record| snapshot(&state, record))
            .collect()
    }
}

fn schedule_locked(state: &mut State) -> Vec<DownloadStart> {
    let mut starts = Vec::new();
    while state.active.len() < state.maximum_parallel {
        let Some(id) = state.queue.pop_front() else {
            break;
        };
        let Some(record) = state.jobs.get_mut(&id) else {
            continue;
        };
        if record.phase != ModelOperationPhase::Queued || record.active_control.is_some() {
            continue;
        }
        record.phase = ModelOperationPhase::Resolving;
        record.message = "Resolving exact artifact".to_owned();
        record.started_at.get_or_insert_with(Instant::now);
        record.generation = record.generation.wrapping_add(1).max(1);
        record.transfer_started_at = None;
        record.transfer_baseline_bytes = record.downloaded_bytes;
        let (stop, receiver) = watch::channel(false);
        record.active_control = Some(ActiveControl {
            generation: record.generation,
            stop,
        });
        state.active.insert(id.clone());
        starts.push(DownloadStart {
            id,
            generation: record.generation,
            stop: receiver,
        });
    }
    starts
}

fn prune_locked(state: &mut State) {
    let terminal = state
        .order
        .iter()
        .filter(|id| {
            state
                .jobs
                .get(*id)
                .is_some_and(|record| record.phase.is_terminal() && record.active_control.is_none())
        })
        .count();
    let mut remove = terminal.saturating_sub(MAX_RECENT_TERMINAL_JOBS);
    if remove == 0 {
        return;
    }
    state.order.retain(|id| {
        if remove > 0
            && state
                .jobs
                .get(id)
                .is_some_and(|record| record.phase.is_terminal() && record.active_control.is_none())
        {
            state.jobs.remove(id);
            remove -= 1;
            false
        } else {
            true
        }
    });
}

fn snapshot(state: &State, record: &JobRecord) -> ModelDownloadJob {
    let now = record.finished_at.unwrap_or_else(Instant::now);
    let elapsed = record.started_at.map_or_else(
        || now.duration_since(record.enqueued_at),
        |start| now.duration_since(start),
    );
    let transfer_bytes_per_second = (record.phase == ModelOperationPhase::Downloading)
        .then_some(record.transfer_started_at)
        .flatten()
        .and_then(|start| {
            let seconds = now.duration_since(start).as_secs_f64();
            let transferred = record
                .downloaded_bytes
                .saturating_sub(record.transfer_baseline_bytes);
            (seconds > 0.25 && transferred > 0).then_some(transferred as f64 / seconds)
        });
    let progress_percent = record.total_bytes.and_then(|total| {
        (total > 0).then_some((record.downloaded_bytes.min(total) as f64 / total as f64) * 100.0)
    });
    let estimated_remaining = match (record.total_bytes, transfer_bytes_per_second) {
        (Some(total), Some(rate)) if rate > 0.0 && record.downloaded_bytes < total => Some(
            Duration::from_secs_f64((total - record.downloaded_bytes) as f64 / rate),
        ),
        _ => None,
    };
    let queue_position = (record.phase == ModelOperationPhase::Queued).then(|| {
        state
            .queue
            .iter()
            .position(|candidate| candidate == &record.id)
            .map_or(0, |position| position + 1)
    });
    ModelDownloadJob {
        id: record.id.clone(),
        model_ref: record.model_ref.clone(),
        provider: record.provider.clone(),
        repository: record.repository.clone(),
        filename: record.filename.clone(),
        phase: record.phase,
        downloaded_bytes: record.downloaded_bytes,
        total_bytes: record.total_bytes,
        progress_percent,
        transfer_bytes_per_second,
        elapsed,
        estimated_remaining,
        queue_position,
        message: record.message.clone(),
    }
}

fn identity_from_reference(model_ref: &str) -> (Option<String>, Option<String>, Option<String>) {
    let (provider, rest) = match model_ref.split_once(':') {
        Some((provider, rest)) => (Some(provider.to_owned()), rest),
        None => (None, model_ref),
    };
    let (repository, revision_and_filename) =
        rest.split_once('@')
            .map_or((None, None), |(repository, tail)| {
                (
                    (!repository.is_empty()).then(|| repository.to_owned()),
                    Some(tail),
                )
            });
    let filename = revision_and_filename
        .and_then(|tail| tail.split_once('/'))
        .map(|(_, filename)| filename.to_owned())
        .filter(|value| !value.is_empty());
    (provider, repository, filename)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::DownloadManager;
    use crate::{DownloadAdmission, ModelOperationPhase};

    #[test]
    fn paused_attempt_releases_capacity_and_resumes_the_same_job_after_stopping() {
        let manager = DownloadManager::new(1);
        let (first, first_starts) = manager.admit("hf:owner/first@main/model.gguf".to_owned());
        let first_id = match first {
            DownloadAdmission::Started(job) => job.id,
            _ => panic!("first download should start"),
        };
        let first_start = first_starts.into_iter().next().expect("first start");
        let (second, _) = manager.admit("hf:owner/second@main/model.gguf".to_owned());
        let second_id = match second {
            DownloadAdmission::Queued(job) => job.id,
            _ => panic!("second download should queue"),
        };

        let (paused, replacement) = manager.pause(&first_id).expect("pause succeeds");
        assert_eq!(paused.phase, ModelOperationPhase::Paused);
        assert_eq!(replacement.len(), 1);
        assert_eq!(replacement[0].id, second_id);

        let (resuming, starts) = manager.resume(&first_id).expect("resume succeeds");
        assert_eq!(resuming.phase, ModelOperationPhase::Paused);
        assert!(starts.is_empty(), "old attempt must stop before resuming");
        let stopped = manager.finish_attempt(&first_id, first_start.generation, None);
        assert!(
            stopped.starts.is_empty(),
            "the replacement still owns the slot"
        );

        let second_generation = replacement[0].generation;
        let resumed = manager.finish_attempt(
            &second_id,
            second_generation,
            Some((ModelOperationPhase::Installed, "installed".to_owned())),
        );
        assert_eq!(resumed.starts.len(), 1);
        assert_eq!(resumed.starts[0].id, first_id);
        assert_ne!(resumed.starts[0].generation, first_start.generation);
    }

    #[test]
    fn cancelled_attempt_cannot_be_overwritten_and_defers_cleanup_until_it_stops() {
        let manager = DownloadManager::new(1);
        let (admission, starts) = manager.admit("hf:owner/model@main/model.gguf".to_owned());
        let id = match admission {
            DownloadAdmission::Started(job) => job.id,
            _ => panic!("download should start"),
        };
        let start = starts.into_iter().next().expect("download start");
        let partial = PathBuf::from("model.part");
        manager.register_partial_paths(&id, Some(start.generation), [partial.clone()]);

        let (cancelled, transition) = manager.cancel(&id).expect("cancel succeeds");
        assert_eq!(cancelled.phase, ModelOperationPhase::Cancelled);
        assert!(transition.cleanup.is_empty());

        let finished = manager.finish_attempt(
            &id,
            start.generation,
            Some((ModelOperationPhase::Failed, "late failure".to_owned())),
        );
        assert_eq!(finished.cleanup, [partial]);
        let snapshot = manager
            .snapshots()
            .into_iter()
            .find(|job| job.id == id)
            .expect("cancelled job remains visible");
        assert_eq!(snapshot.phase, ModelOperationPhase::Cancelled);
        assert_eq!(snapshot.message, "Download cancelled");
    }

    #[test]
    fn pausing_a_queued_job_removes_it_from_scheduling_and_duplicate_admission() {
        let manager = DownloadManager::new(1);
        let (_, _) = manager.admit("hf:owner/active@main/model.gguf".to_owned());
        let model_ref = "hf:owner/queued@main/model.gguf".to_owned();
        let (queued, _) = manager.admit(model_ref.clone());
        let queued_id = match queued {
            DownloadAdmission::Queued(job) => job.id,
            _ => panic!("second download should queue"),
        };
        let (paused, starts) = manager.pause(&queued_id).expect("queued pause succeeds");
        assert_eq!(paused.phase, ModelOperationPhase::Paused);
        assert!(starts.is_empty());
        assert!(matches!(
            manager.admit(model_ref).0,
            DownloadAdmission::Duplicate(job) if job.id == queued_id && job.phase == ModelOperationPhase::Paused
        ));
    }
}
