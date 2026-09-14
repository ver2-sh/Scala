//! UI-only projections of owner reports. Never passed to local stores/resolvers.
use super::*;
use norted_engine::link::{LinkAction, LinkControlRequest, LinkPeer, LinkProfile, LinkSnapshot};

impl App {
    pub fn replace_link(&mut self, result: Result<LinkSnapshot, String>) {
        match result {
            Ok(snapshot) => self.link = snapshot,
            Err(error) => {
                self.link.error = Some(error);
                for peer in &mut self.link.peers {
                    peer.reachable = false;
                }
            }
        }
        self.project_link();
        self.reconcile_settings_selection();
    }
    pub(super) fn project_link(&mut self) {
        let selected = self
            .snapshot
            .models
            .get(self.selected_model.unwrap_or(usize::MAX))
            .map(|m| m.id.clone());
        let selected_profile = self
            .selected_model_profile_value()
            .map(|p| (p.model_id.clone(), p.id.clone()));
        let selected_backend = self
            .overview_selected
            .and_then(|index| self.resident_backends().get(index).copied())
            .map(|b| (b.model_id.clone(), b.model_profile_id.clone(), b.generation));
        self.snapshot
            .models
            .retain(|m| !self.remote_models.contains_key(&m.id));
        self.remote_models.clear();
        self.remote_profiles.clear();
        self.remote_backends.clear();
        for peer in &self.link.peers {
            let Some(state) = &peer.state else {
                continue;
            };
            for model in &state.models {
                let id = ModelId(format!("{}@{}", model.id, peer.node_id));
                self.remote_models
                    .insert(id.clone(), (peer.node_id.clone(), model.id.clone()));
                self.snapshot.models.push(ModelArtifact {
                    id,
                    display_name: model.display_name.clone(),
                    path: Default::default(),
                    format: model.format,
                    size_bytes: model.size_bytes,
                    created: model.created,
                    hash: model.hash.clone(),
                    architecture: model.architecture.clone(),
                    context_length: model.context_length,
                    provenance: model.provenance.clone(),
                    native_identity: model.native_identity.clone(),
                    auxiliary_artifacts: Vec::new(),
                    norted_package: None,
                });
            }
            for profile in &state.profiles {
                let Ok(engine_id) = EngineId::new(profile.engine_id.clone()) else {
                    continue;
                };
                let model_id = ModelId(format!("{}@{}", profile.model_id, peer.node_id));
                self.remote_profiles.push((
                    peer.node_id.clone(),
                    ModelProfile {
                        id: profile.id.clone(),
                        display_name: profile.display_name.clone(),
                        model_id: model_id.clone(),
                        engine_id,
                        role: profile.role,
                        overrides: Default::default(),
                    },
                ));
                if let Some(backend) = &profile.backend {
                    self.remote_backends.push(BackendStatus {
                        generation: backend.generation,
                        lifecycle: backend.lifecycle,
                        model_profile_id: profile.id.clone(),
                        model_id,
                        role: profile.role,
                        residency: backend.residency,
                        engine_id: backend.engine_id.clone(),
                        runtime_id: backend.runtime_id.clone(),
                        runtime_version: backend.runtime_version.clone(),
                        runtime_variant: backend.runtime_variant.clone(),
                        runtime_executable_sha256: None,
                        accelerator_binding: None,
                        process_id: None,
                        private_endpoint: None,
                        load_progress: backend.load_progress.clone(),
                        failure: backend.failure.clone(),
                        provenance: None,
                        parallel_requests: backend.parallel_requests.clone(),
                        activities: backend.activities.clone(),
                        active_request_count: backend.active_requests,
                        primary_lease_count: backend.primary_lease_count,
                        last_used_unix: backend.last_used_unix,
                        retiring: backend.retiring,
                    });
                }
            }
        }
        self.selected_model =
            selected.and_then(|id| self.snapshot.models.iter().position(|m| m.id == id));
        if let Some((model, profile)) = selected_profile {
            self.selected_model_profile = self
                .model_profile_values()
                .iter()
                .position(|p| p.model_id == model && p.id == profile);
        }
        if let Some((model, profile, generation)) = selected_backend {
            self.overview_selected = self.resident_backends().iter().position(|b| {
                b.model_id == model && b.model_profile_id == profile && b.generation == generation
            });
        }
    }
    pub fn local_host_label(&self) -> String {
        self.link
            .node_name
            .as_ref()
            .map_or_else(|| "local".into(), |name| format!("{name} (local)"))
    }
    pub fn model_peer(&self, model: &ModelId) -> Option<&LinkPeer> {
        let (node, _) = self.remote_models.get(model)?;
        self.link.peers.iter().find(|p| &p.node_id == node)
    }
    pub fn model_host_label(&self, model: &ModelId) -> String {
        self.model_peer(model).map_or_else(
            || self.local_host_label(),
            |peer| {
                format!(
                    "{}{}",
                    peer.name,
                    if peer.reachable { "" } else { " [stale]" }
                )
            },
        )
    }
    pub fn selected_model_is_remote(&self) -> bool {
        self.snapshot
            .models
            .get(self.selected_model.unwrap_or(usize::MAX))
            .is_some_and(|m| self.remote_models.contains_key(&m.id))
    }
    pub fn selected_remote_profile(&self) -> Option<(&LinkPeer, &LinkProfile)> {
        let local_count = self.model_profiles.as_ref().map_or(0, |p| p.profiles.len());
        let index = self.selected_model_profile?.checked_sub(local_count)?;
        let (node, profile) = self.remote_profiles.get(index)?;
        let peer = self.link.peers.iter().find(|p| &p.node_id == node)?;
        Some((
            peer,
            peer.state
                .as_ref()?
                .profiles
                .iter()
                .find(|p| p.id == profile.id)?,
        ))
    }
    pub fn profile_label(&self, index: usize) -> String {
        let profiles = self.model_profile_values();
        let Some(profile) = profiles.get(index) else {
            return String::new();
        };
        format!(
            "{} · {}",
            profile.id,
            self.model_host_label(&profile.model_id)
        )
    }
    pub fn link_summary(&self) -> String {
        if !self.link.enabled {
            return "Your local model runtime, from artifacts to API".into();
        }
        let mut hosts = vec![self.local_host_label()];
        hosts.extend(self.link.peers.iter().map(|peer| {
            format!(
                "{} ({})",
                peer.name,
                if peer.reachable { "online" } else { "stale" }
            )
        }));
        let mut text = format!("Norted Link: {}", hosts.join(" · "));
        if let Some(error) = &self.link.error {
            text.push_str(&format!(" · {error}"));
        }
        text
    }
    pub(super) fn request_remote_control(&mut self, action: LinkAction) -> Update {
        if self.link_control_busy {
            return Update::None;
        }
        let Some((peer, profile)) = self.selected_remote_profile() else {
            return Update::None;
        };
        if !peer.reachable {
            self.notice = Some("Owner is unavailable; wait for reconnect".into());
            return Update::Render;
        }
        let request = LinkControlRequest {
            node_id: peer.node_id.clone(),
            profile_id: profile.id.clone(),
            action,
        };
        self.notice = Some(format!("{:?} {} on {}…", action, profile.id, peer.name));
        self.pending_link_action = Some(request);
        self.link_control_busy = true;
        Update::Render
    }
    pub fn take_link_action(&mut self) -> Option<LinkControlRequest> {
        self.pending_link_action.take()
    }
    pub fn handle_link_result(&mut self, result: Result<LinkSnapshot, String>) {
        self.link_control_busy = false;
        match result {
            Ok(snapshot) => {
                self.replace_link(Ok(snapshot));
                self.notice =
                    Some("Owner accepted the operation; loaded state updates automatically".into());
            }
            Err(error) => self.notice = Some(error),
        }
    }
    pub fn remote_profile_detail(&self) -> Option<String> {
        let (peer, profile) = self.selected_remote_profile()?;
        let state = peer.state.as_ref()?;
        let backend = profile.backend.as_ref();
        let benchmark = state.benchmarks.iter().find(|b| b.profile_id == profile.id);
        Some(format!(
            "Profile: {}\nHost: {} ({})\nNode: {}\nReachability: {}\nModel: {} · installed on owner: {}\nEngine: {}\nState: {}\nRuntime: {}\nHardware: {}\n{}\nLoaded context: {}\nLatest benchmark: {}\n\nl: Load on host    u: Unload on host    ←/→: Profile\nProfile settings and runtimes are managed on {}.",
            profile.display_name,
            peer.name,
            state.server_version,
            peer.node_id,
            if peer.reachable {
                "online"
            } else {
                "stale / unavailable"
            },
            profile.model_id,
            profile.installed,
            profile.engine_id,
            backend.map_or_else(|| "Unloaded".into(), |b| format!("{:?}", b.lifecycle)),
            backend.map_or_else(
                || "Not loaded; resolved by owner on load".into(),
                |b| format!(
                    "{} · {} · {}",
                    b.runtime_id
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "pending".into()),
                    b.runtime_version.as_deref().unwrap_or("unknown"),
                    b.runtime_variant.as_deref().unwrap_or("unknown")
                )
            ),
            state.hardware,
            backend
                .and_then(|b| b.failure.as_deref())
                .or(peer.error.as_deref())
                .unwrap_or(""),
            backend
                .and_then(|b| b.context_length.as_deref())
                .unwrap_or("not reported"),
            benchmark.map_or_else(
                || state
                    .benchmark_error
                    .clone()
                    .unwrap_or_else(|| "No benchmark recorded".into()),
                |b| format!(
                    "{} · {} · intelligence {}",
                    b.run_id,
                    b.status,
                    b.intelligence
                        .map_or_else(|| "not measured".into(), |v| format!("{v:.2}"))
                )
            ),
            peer.name
        ))
    }
}
