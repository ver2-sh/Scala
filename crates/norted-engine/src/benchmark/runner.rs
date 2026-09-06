use super::*;
use crate::benchmark::{self as bench, *};
use crate::{InferenceEvent, InferenceMessage, InferenceRole, InferenceToolCall};
use futures_util::{FutureExt, StreamExt};
use serde_json::{Value, json};
use std::time::Instant;
use tokio::sync::{OwnedRwLockReadGuard, watch};

impl RuntimeManager {
    /// Called only after a server has bound its listeners, never by offline
    /// runtime probes. Hold a process-owned file lock before recovering records.
    pub async fn initialize_benchmark_server(&self) -> Result<(), String> {
        let mut owner = self.benchmark.owner.lock().await;
        if owner.is_some() {
            return Ok(());
        }
        let claimed = self.benchmark.store.claim_server().await?;
        self.benchmark.store.recover().await?;
        *owner = Some(claimed);
        Ok(())
    }

    pub(super) fn benchmark_admission(
        &self,
    ) -> Result<Option<OwnedRwLockReadGuard<()>>, RuntimeError> {
        if self.benchmark.quarantined.load(Ordering::Acquire) {
            return Err(RuntimeError::BenchmarkReserved);
        }
        if bench::EXECUTOR.try_with(|()| ()).is_ok() {
            return Ok(None);
        }
        self.benchmark
            .gate
            .clone()
            .try_read_owned()
            .map(Some)
            .map_err(|_| RuntimeError::BenchmarkReserved)
    }
    pub async fn benchmark_control(
        self: &Arc<Self>,
        request: BenchmarkRequest,
    ) -> Result<Value, String> {
        match request {
            BenchmarkRequest::Start { profile_id } => self.start_benchmark(profile_id).await,
            BenchmarkRequest::Cancel => {
                let active = self.benchmark.active.lock().await;
                if let Some(a) = active.as_ref() {
                    a.cancel.send_replace(true);
                    Ok(
                        json!({"run_id":a.run_id,"status":"cancelling","message":"Stopping managed work before releasing inference reservation"}),
                    )
                } else {
                    Ok(json!({"status":"idle"}))
                }
            }
            BenchmarkRequest::Status => self.benchmark_overview().await,
            BenchmarkRequest::History { profile_id } => Ok(
                json!({"profile_id":profile_id,"history":self.benchmark.store.summaries().await?.into_iter().filter(|s|s.profile_id==profile_id).take(200).collect::<Vec<_>>(),"limit":200}),
            ),
            BenchmarkRequest::Result { run_id } => {
                let run = self.benchmark.store.result(&run_id).await?;
                Ok(json!({"summary":run.summary(),"record":run}))
            }
            BenchmarkRequest::Compare { left, right } => Ok(bench::compare(
                &self.benchmark.store.result(&left).await?,
                &self.benchmark.store.result(&right).await?,
            )),
        }
    }
    async fn saved_benchmark_configuration(
        &self,
        profile: &norted_core::ModelProfile,
    ) -> Result<Value, String> {
        let settings = self.settings.read().await.map_err(|e| e.to_string())?;
        let model = self
            .core
            .model(&profile.model_id)
            .await
            .ok_or("bound model missing")?;
        let resolved = settings
            .resolve(
                &profile.id,
                profile.engine_id.as_str(),
                &profile.overrides,
                &norted_core::SettingsPatch::default(),
                &self.core.paths.data_dir,
            )
            .map_err(|e| e.to_string())?;
        let setting_files = setting_file_observations(&resolved).await;
        let selections = match tokio::fs::read(&self.core.paths.runtime_selections_file).await {
            Ok(bytes) if bytes.len() <= 1024 * 1024 => {
                serde_json::from_slice::<norted_core::RuntimeSelections>(&bytes)
                    .map_err(|e| e.to_string())?
            }
            Ok(_) => return Err("runtime selections exceed inspection bound".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
            Err(e) => return Err(e.to_string()),
        };
        let mut files =
            vec![json!({"path":model.path,"observation":file_observation(&model.path).await})];
        for auxiliary in &model.auxiliary_artifacts {
            files.push(json!({"path":auxiliary.path,"observation":file_observation(&auxiliary.path).await}));
        }
        Ok(
            json!({"profile_hash":profile.content_hash(),"inherited_settings":settings.runtime_defaults.get(profile.engine_id.as_str()),
            "model_runtime_selection":selections.model_overrides.get(&profile.model_id),
            "format_runtime_selection":selections.format_defaults.get(&model.format),
            "model":semantic(json!(model)),"files":files,"setting_files":setting_files}),
        )
    }

    async fn served_setting_files(&self, id: &ModelProfileId) -> Value {
        let settings = self
            .state
            .read()
            .await
            .backends
            .get(id)
            .and_then(|b| b.running.as_ref())
            .map(|r| r.settings.clone());
        match settings {
            Some(settings) => setting_file_observations(&settings).await,
            None => Value::Null,
        }
    }

    async fn benchmark_environment(&self) -> Value {
        json!({"host":self.packs.host_capabilities().await,"os":std::env::consts::OS,"architecture":std::env::consts::ARCH,
            "logical_cpus":std::thread::available_parallelism().ok().map(usize::from),
            "cpu_model":read_host_line("/proc/cpuinfo","model name").await,
            "ram_total":read_host_line("/proc/meminfo","MemTotal").await,
            "cache":"run-unique nonce prefix; backend cache occupancy and isolation unverified",
            "external_workloads":"unverified"})
    }
    async fn benchmark_overview(&self) -> Result<Value, String> {
        let history = self.benchmark.store.summaries().await?;
        let profiles = self
            .model_profiles
            .read()
            .await
            .map_err(|e| e.to_string())?;
        let env = self.benchmark_environment().await;
        let status = self.status().await;
        let pack = bench::digest(bench::manifest());
        let running_profile = self
            .benchmark
            .active
            .lock()
            .await
            .as_ref()
            .map(|a| a.profile_id.clone());
        let mut rows = Vec::new();
        for profile in profiles.profiles.values() {
            let saved = self.saved_benchmark_configuration(profile).await.ok();
            let backend = status.backend(&profile.id);
            let mut saved = saved;
            if let (Some(s), Some(p)) =
                (saved.as_mut(), backend.and_then(|b| b.provenance.as_ref()))
            {
                s["served_files"] = served_files(p).await;
                s["served_setting_files"] = self.served_setting_files(&profile.id).await;
            }
            let key = backend
                .filter(|b| b.lifecycle == BackendLifecycle::Running)
                .and_then(|b| b.provenance.as_ref())
                .zip(saved.as_ref())
                .and_then(|(p, s)| verified_key(p, s, &env));
            let h = history
                .iter()
                .filter(|s| s.profile_id == profile.id)
                .cloned()
                .collect::<Vec<_>>();
            let (result, state) = bench::select(&h, key.as_deref(), &pack);
            let state = if state.starts_with("Current")
                && result.is_some_and(|r| r.saved_equals_served_requested_settings == Some(false))
            {
                "Current served session — differs from saved"
            } else {
                state
            };
            let reasons = if let Some(result) = result {
                let mut reasons = Vec::new();
                if profile.content_hash() != result.profile_hash {
                    reasons.push("Model Profile configuration changed");
                }
                if saved.as_ref().is_some_and(|s| {
                    semantic(s.clone()) != semantic(result.saved_configuration.clone())
                }) {
                    reasons
                        .push("Inherited settings, runtime selection or referenced files changed");
                }
                if key.is_none() {
                    reasons.push("Current runtime/environment identity is unverified");
                }
                reasons
            } else {
                Vec::new()
            };
            let state = if running_profile.as_ref() == Some(&profile.id) {
                "Running"
            } else if result.is_some_and(|r| !bench::finished(&r.status)) {
                if result.is_some_and(|r| r.status == "failed") {
                    "Failed"
                } else {
                    "Incomplete"
                }
            } else if result.is_none() {
                match h.first().map(|s| s.status.as_str()) {
                    Some("failed") => "Failed",
                    Some(_) => "Incomplete",
                    None => "Never benchmarked",
                }
            } else if reasons.iter().any(|s| s.contains("changed")) {
                "Configuration changed"
            } else {
                state
            };
            rows.push(json!({"profile_id":profile.id,"display_name":profile.display_name,"state":state,"configuration_notes":reasons,
                "result":result,"last_benchmark_unix_ms":result.filter(|s|bench::finished(&s.status)).and_then(|s|s.ended_unix_ms),"latest_attempt":h.first(),
                "identity_note":"Current requires a running observed runtime. Hardware cache and external workloads remain unverified."}));
        }
        let active = self.benchmark.active.lock().await;
        Ok(
            json!({"suite":suite::SUITE,"methodology":suite::METHOD,"rows":rows,
            "active":active.as_ref().map(|a|json!({"run_id":a.run_id,"profile_id":a.profile_id,"phase":a.phase,"completed_tasks":a.done,"total_tasks":40,"elapsed_seconds":a.started.elapsed().as_secs_f64(),"remaining_seconds":(600.0-a.started.elapsed().as_secs_f64()).max(0.0),"cancelling":*a.cancel.borrow()})),
            "history_summary_limit":4096,"timing_boundary":"internal managed streaming inference","speed_method":"visible Unicode characters/s after first chunk; first chunk excluded","latency_method":"time to first nonempty visible text (ms); first answer unavailable"}),
        )
    }
    async fn start_benchmark(
        self: &Arc<Self>,
        profile_id: ModelProfileId,
    ) -> Result<Value, String> {
        if self.benchmark.owner.lock().await.is_none() {
            return Err("benchmark execution requires an owning private-control server".into());
        }
        if self.benchmark.quarantined.load(Ordering::Acquire) {
            return Err(
                "Busy: benchmark cleanup could not establish stopped work; restart the server"
                    .into(),
            );
        }
        let reservation = self
            .benchmark
            .gate
            .clone()
            .try_write_owned()
            .map_err(|_| "Busy: benchmark or inference admission in progress")?;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err("server shutting down".into());
        }
        let operation = self
            .operation
            .try_lock()
            .map_err(|_| "Busy: runtime operation in progress")?;
        let runtime_reservation = self.packs.try_reserve_benchmark()?;
        let status = self.status().await;
        if status.backends.iter().any(|b| {
            b.active_request_count > 0
                || matches!(
                    b.lifecycle,
                    BackendLifecycle::Loading | BackendLifecycle::Stopping
                )
        }) {
            return Err("Busy: active inference or conflicting runtime operation".into());
        }
        let profile = self
            .model_profiles
            .read()
            .await
            .map_err(|e| e.to_string())?
            .profiles
            .get(&profile_id)
            .cloned()
            .ok_or("Model Profile not found")?;
        // Nothing is resolved or loaded before this monotonic admission timestamp.
        let started = Instant::now();
        let started_unix_ms = bench::now_ms();
        let run_id = uuid::Uuid::new_v4().to_string();
        let (cancel, receiver) = watch::channel(false);
        *self.benchmark.active.lock().await = Some(Active {
            run_id: run_id.clone(),
            profile_id: profile_id.clone(),
            started,
            phase: "preparation".into(),
            done: 0,
            cancel,
        });
        drop(operation);
        let run = Run {
            record_version: 1,
            run_id: run_id.clone(),
            profile_hash: profile.content_hash(),
            profile,
            started_unix_ms,
            ended_unix_ms: None,
            duration_seconds: 0.0,
            status: "running".into(),
            diagnostic: None,
            suite: suite::SUITE.into(),
            pack_hash: bench::digest(bench::manifest()),
            methodology: suite::METHOD.into(),
            policy: suite::POLICY.into(),
            manifest: bench::manifest(),
            server_version: env!("CARGO_PKG_VERSION").into(),
            source_revision: option_env!("NORTED_SOURCE_REVISION").map(str::to_owned),
            provenance: None,
            saved_configuration: Value::Null,
            environment: Value::Null,
            configuration_key: None,
            loaded_before: status.backend(&profile_id).is_some_and(|b| b.lifecycle == BackendLifecycle::Running),
            load_seconds: None,
            phases: BTreeMap::new(),
            missing: vec![
                "Native decode rate and first-answer latency unavailable: shared event contract does not separate reasoning".into(),
                "External workloads, cache occupancy and cold-cache conditions unverified".into(),
                "Source revision unavailable unless NORTED_SOURCE_REVISION was set at build time".into(),
                "Resource peaks are unavailable; accelerator identity and total VRAM are host observations".into(),
            ],
            evidence: Vec::new(),
        };
        tokio::spawn(bench::EXECUTOR.scope(
            (),
            Arc::clone(self).run_benchmark(
                run,
                started,
                receiver,
                reservation,
                runtime_reservation,
            ),
        ));
        Ok(
            json!({"run_id":run_id,"profile_id":profile_id,"status":"running","execution_budget_seconds":600,"reservation":"Normal inference and load/unload are temporarily rejected"}),
        )
    }
    async fn run_benchmark(
        self: Arc<Self>,
        mut run: Run,
        started: Instant,
        mut receiver: watch::Receiver<bool>,
        _reservation: tokio::sync::OwnedRwLockWriteGuard<()>,
        _runtime_reservation: tokio::sync::OwnedMutexGuard<()>,
    ) {
        let execution_deadline = tokio::time::Instant::from_std(started + Duration::from_secs(584));
        let outcome = tokio::select! {
            biased;
            _ = receiver.changed() => Err("cancelled".to_owned()),
            r = tokio::time::timeout_at(execution_deadline, std::panic::AssertUnwindSafe(self.execute_benchmark(&mut run, started)).catch_unwind()) => match r {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err("benchmark execution panicked; stopping owned work".into()),
                Err(_) => Err("global execution deadline".into()),
            },
        };
        if let Err(reason) = outcome {
            run.status = if self.shutting_down.load(Ordering::Acquire) {
                "interrupted"
            } else if reason == "cancelled" {
                "cancelled"
            } else if run.provenance.is_none() {
                "failed"
            } else {
                "incomplete"
            }
            .into();
            run.diagnostic = Some(reason);
            // Stream cancellation is followed by owned load cancellation and
            // managed process termination. Keep the reservation through cleanup.
            let deadline = tokio::time::Instant::from_std(started + Duration::from_secs(599))
                .min(tokio::time::Instant::now() + Duration::from_secs(4));
            match tokio::time::timeout_at(deadline, self.stop_benchmark_work(&run.profile.id)).await
            {
                Ok(Ok(())) => {}
                result => {
                    self.benchmark.quarantined.store(true, Ordering::Release);
                    run.diagnostic = Some(format!(
                        "{}; stopped work could not be verified: {result:?}; inference remains quarantined",
                        run.diagnostic.as_deref().unwrap_or("interrupted")
                    ));
                }
            }
        } else {
            let summary = run.summary();
            run.status = if summary.speed["combined"]["native_end_to_end_output_tokens_per_second"]
                ["median"]
                .is_null()
                || summary.intelligence.is_none()
                || summary.agentic.is_none()
                || summary.speed["combined"]["visible_delivery_characters_per_second"]["median"]
                    .is_null()
            {
                "completed_unavailable"
            } else {
                "completed"
            }
            .into();
        }
        for evidence in &mut run.evidence {
            if evidence.status == "running" {
                evidence.status = "interrupted".into();
                evidence.explanation =
                    "Attempt interrupted before a complete response was available".into();
            }
        }
        run.duration_seconds = started.elapsed().as_secs_f64();
        run.ended_unix_ms = Some(bench::now_ms());
        let final_deadline = started + Duration::from_secs(600);
        match tokio::time::timeout_at(
            tokio::time::Instant::from_std(final_deadline),
            self.benchmark.store.save_before(&run, Some(final_deadline)),
        )
        .await
        {
            Ok(Ok(())) => {}
            result => tracing::error!(
                ?result,
                "benchmark finalization failed; checkpoint will recover as interrupted"
            ),
        }
        *self.benchmark.active.lock().await = None;
    }

    pub(super) async fn cancel_benchmark_shutdown(&self) {
        if let Some(active) = self.benchmark.active.lock().await.as_ref() {
            active.cancel.send_replace(true);
        }
        // Wait for the benchmark to preserve evidence before normal shutdown.
        let _reservation = self.benchmark.gate.write().await;
    }
    async fn stop_benchmark_work(
        self: &Arc<Self>,
        profile_id: &ModelProfileId,
    ) -> Result<(), String> {
        // Abort the owned load future as well as signalling its generation. This
        // prevents long preparation from retaining the lifecycle lock or launching late.
        let handoff = self.benchmark.load_handoff.lock().await;
        if let Some(task) = self
            .benchmark
            .load_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
        drop(handoff);
        let process = {
            let state = self.state.read().await;
            state.backends.get(profile_id).and_then(|b| {
                b.running
                    .as_ref()
                    .map(|r| r.process.clone())
                    .or_else(|| b.loading_process.clone())
            })
        };
        if let Some(process) = process {
            self.supervisor
                .terminate_immediately(&process)
                .await
                .map_err(|e| e.to_string())?;
        }
        // The process has already been killed; normal cancellation clears the
        // lifecycle and adapter bookkeeping without a graceful inference drain.
        self.cancel_loading(profile_id).await;
        self.unload(profile_id.clone())
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    async fn execute_benchmark(
        self: &Arc<Self>,
        run: &mut Run,
        started: Instant,
    ) -> Result<(), String> {
        self.benchmark.store.save(run).await?;
        let prep = Instant::now();
        tokio::time::timeout(Duration::from_secs(60).saturating_sub(started.elapsed()),async {
            run.saved_configuration=self.saved_benchmark_configuration(&run.profile).await?;
            run.environment=self.benchmark_environment().await;
            let before=self.status().await;
            run.environment["resident_profiles"]=json!(before.backends.iter().filter(|b|b.lifecycle==BackendLifecycle::Running).map(|b|json!({"profile_id":b.model_profile_id,"runtime_id":b.runtime_id,"accelerator_binding":b.accelerator_binding})).collect::<Vec<_>>());
            self.load(run.profile.id.clone()).await.map_err(|e|e.to_string())?;
            let residents=run.environment["resident_profiles"].clone();
            run.environment=self.benchmark_environment().await;
            run.environment["resident_profiles"]=residents;
            let after=self.status().await;
            let backend=after.backend(&run.profile.id).ok_or("loaded backend disappeared")?;
            let mut provenance=backend.provenance.clone().ok_or("runtime provenance unavailable")?;
            provenance.private_backend_endpoint="<redacted>".into();
            run.saved_configuration["served_files"]=served_files(&provenance).await;
            run.saved_configuration["served_setting_files"]=self.served_setting_files(&run.profile.id).await;
            run.configuration_key=verified_key(&provenance,&run.saved_configuration,&run.environment);
            if run.configuration_key.is_none() {run.missing.push("Current configuration equivalence unverified: missing identity, file observation, accelerator binding or fallback next-load runtime selection".into());}
            {
                let state=self.state.read().await;
                if let Some(running)=state.backends.get(&run.profile.id).and_then(|b|b.running.as_ref()) {
                    let settings=self.settings.read().await.map_err(|e|e.to_string())?;
                    let saved=settings.resolve(&run.profile.id,run.profile.engine_id.as_str(),&run.profile.overrides,&norted_core::SettingsPatch::default(),&self.core.paths.data_dir).map_err(|e|e.to_string())?;
                    run.environment["requested_saved_settings"]=json!(saved.configured);
                    let served_requested=running.settings.configured.iter().filter(|(_,setting)|!matches!(setting.source,norted_core::SettingSource::RuntimeDefault)).map(|(id,value)|(id.clone(),value.clone())).collect::<BTreeMap<_,_>>();
                    run.environment["requested_served_settings"]=json!(served_requested);
                    run.environment["saved_equals_served_requested_settings"]=json!(saved.configured==served_requested);
                    if saved.configured!=served_requested {run.missing.push("Served settings differ from saved next-load settings; this result evaluates the existing served session (see requested_saved_settings and requested_served_settings)".into());}
                }
            }
            run.provenance=Some(provenance);
            run.load_seconds=Some(prep.elapsed().as_secs_f64());
            Ok::<_,String>(())
        }).await.map_err(|_|"setup timeout (60 seconds, including integrity checks)")??;
        run.phases
            .insert("preparation".into(), prep.elapsed().as_secs_f64());
        self.checkpoint(run, started, "warmup").await?;
        let warm = self
            .request(run, "Reply with the word ready.", 32, false, Vec::new())
            .await?;
        let mut e = new_evidence(
            "warmup",
            "warmup",
            "Reply with the word ready.",
            Value::Null,
            10,
            warm.max_output_tokens,
        );
        self.one_request(run, &mut e, warm, 10).await?;
        self.checkpoint(run, started, "speed").await?;
        for (id, prompt) in suite::probes() {
            self.verify_pinned(run).await?;
            let request = self.request(run, &prompt, 512, false, Vec::new()).await?;
            let mut e = new_evidence(
                &id,
                "speed",
                &prompt,
                Value::Null,
                15,
                request.max_output_tokens,
            );
            self.one_request(run, &mut e, request, 15).await?;
            self.checkpoint(run, started, "speed").await?;
        }
        for q in suite::questions() {
            self.verify_pinned(run).await?;
            let request = self
                .request(run, &q.prompt, q.max_output_tokens, false, Vec::new())
                .await?;
            let mut e = new_evidence(
                &q.id,
                &q.category,
                &q.prompt,
                q.answer.clone(),
                q.seconds,
                request.max_output_tokens,
            );
            self.one_request(run, &mut e, request, q.seconds).await?;
            self.checkpoint(run, started, "intelligence").await?;
        }
        let probe_request = self
            .request(run, &suite::single_cases()[0].prompt, 384, true, Vec::new())
            .await?;
        let capability = {
            let state = self.state.read().await;
            let running = state
                .backends
                .get(&run.profile.id)
                .and_then(|b| b.running.as_ref())
                .ok_or("backend missing")?;
            if !running
                .adapter
                .capabilities()
                .features
                .contains(&crate::EngineFeature::ToolCalling)
            {
                Err("adapter does not advertise native tool calling".into())
            } else {
                running
                    .adapter
                    .validate_inference_request(
                        &probe_request,
                        &running.effective_generation_settings,
                        &running.settings_schema,
                    )
                    .map_err(|e| e.to_string())
            }
        };
        if let Err(reason) = capability {
            run.missing.push(format!("Agentic unavailable: {reason}"));
        } else {
            for case in suite::single_cases() {
                self.verify_pinned(run).await?;
                let request = self
                    .request(run, &case.prompt, 384, true, Vec::new())
                    .await?;
                let mut e = new_evidence(
                    &case.id,
                    "tool",
                    &case.prompt,
                    json!(case),
                    8,
                    request.max_output_tokens,
                );
                self.one_request(run, &mut e, request, 8).await?;
                self.checkpoint(run, started, "single-turn tools").await?;
            }
            for (case, prompt) in suite::AGENT_PROMPTS.iter().enumerate() {
                self.verify_pinned(run).await?;
                let request = self.request(run, prompt, 384, true, Vec::new()).await?;
                let mut e = new_evidence(
                    &format!("agent-{}", case + 1),
                    "agent",
                    prompt,
                    json!({"rubric":"fixture-observed-state-v2","case":case}),
                    30,
                    request.max_output_tokens,
                );
                if let Some(active) = self.benchmark.active.lock().await.as_mut() {
                    active.phase = format!("agent / {}", e.id);
                }
                e.status = "running".into();
                run.evidence.push(e.clone());
                self.benchmark.store.save(run).await?;
                let begin = Instant::now();
                let result = tokio::time::timeout(
                    Duration::from_secs(30),
                    self.agent_task(run, case, request, &mut e),
                )
                .await;
                run.phases
                    .entry("agent".into())
                    .and_modify(|v| *v += begin.elapsed().as_secs_f64())
                    .or_insert(begin.elapsed().as_secs_f64());
                match result {
                    Ok(Ok(passed)) => {
                        e.score = Some(f64::from(passed));
                        e.status = if passed { "passed" } else { "failed" }.into();
                        if e.explanation.is_empty() {
                            e.explanation = "Binary rubric: used delivered observations, changed and verified final virtual state, within 6 turns / 8 calls".into();
                        }
                    }
                    Ok(Err(error)) => {
                        if e.status != "failed" {
                            e.status = "infrastructure_error".into();
                        }
                        e.explanation = format!("{}; {error}", e.explanation);
                        if let Some(attempt) = run.evidence.last_mut() {
                            *attempt = e;
                        }
                        return Err(error);
                    }
                    Err(_) => {
                        e.status = "timeout".into();
                        e.score = Some(0.0);
                        e.explanation =
                            "30-second task deadline; unsuccessful under fixed budget".into();
                        let stopped = self.confirm_benchmark_request_stopped(run, &mut e).await;
                        if let Some(attempt) = run.evidence.last_mut() {
                            *attempt = e.clone();
                        }
                        stopped?;
                    }
                }
                if let Some(attempt) = run.evidence.last_mut() {
                    *attempt = e;
                }
                self.checkpoint(run, started, "multi-step agents").await?;
            }
        }
        self.verify_pinned(run).await?;
        if run
            .evidence
            .iter()
            .filter(|e| e.category == "speed")
            .any(|e| !speed_sample(e)["valid"].as_bool().unwrap_or(false))
        {
            run.missing.push(
                "Speed unavailable: invalid/insufficient probe samples; quality coverage retained"
                    .into(),
            );
        }
        self.checkpoint(run, started, "finalization").await?;
        Ok(())
    }
    async fn checkpoint(&self, run: &mut Run, started: Instant, phase: &str) -> Result<(), String> {
        run.duration_seconds = started.elapsed().as_secs_f64();
        if let Some(active) = self.benchmark.active.lock().await.as_mut() {
            active.phase = phase.into();
            active.done = run
                .evidence
                .iter()
                .filter(|e| e.category != "warmup")
                .count();
        }
        self.benchmark.store.save(run).await
    }
    async fn verify_pinned(&self, run: &Run) -> Result<(), String> {
        let profile = self
            .model_profiles
            .read()
            .await
            .map_err(|e| e.to_string())?
            .profiles
            .get(&run.profile.id)
            .cloned()
            .ok_or("profile removed during benchmark")?;
        if profile.content_hash() != run.profile_hash {
            return Err("profile configuration changed during benchmark".into());
        }
        let mut current = self.saved_benchmark_configuration(&profile).await?;
        if let Some(p) = &run.provenance {
            current["served_files"] = served_files(p).await;
            current["served_setting_files"] = self.served_setting_files(&profile.id).await;
        }
        if semantic(current) != semantic(run.saved_configuration.clone()) {
            return Err(
                "inherited settings, selected runtime or artifact changed during benchmark".into(),
            );
        }
        let status = self.status().await;
        let p = status
            .backend(&profile.id)
            .filter(|b| b.lifecycle == BackendLifecycle::Running)
            .and_then(|b| b.provenance.as_ref())
            .ok_or("pinned runtime is no longer running")?;
        if run.provenance.as_ref().is_none_or(|old| {
            old.process.process_id != p.process.process_id
                || old.launched_at_unix != p.launched_at_unix
        }) {
            return Err("pinned backend was replaced".into());
        }
        Ok(())
    }
    async fn request(
        &self,
        run: &Run,
        prompt: &str,
        cap: u32,
        tools: bool,
        messages: Vec<InferenceMessage>,
    ) -> Result<crate::InferenceRequest, String> {
        let state = self.state.read().await;
        let r = state
            .backends
            .get(&run.profile.id)
            .and_then(|b| b.running.as_ref())
            .ok_or("backend missing")?;
        let mut max = match r.settings.runtime_value("max_output_tokens") {
            Some(norted_core::SettingValue::UnsignedInteger(v)) => {
                Some(cap.min(u32::try_from(*v).unwrap_or(u32::MAX)))
            }
            _ => Some(cap),
        };
        if let Some(value) = r
            .settings
            .effective
            .iter()
            .find(|(id, _)| id.as_str() == format!("{}.max_output_tokens", run.profile.engine_id))
            .and_then(|(_, s)| s.value.parse::<u32>().ok())
        {
            max = Some(max.unwrap_or(cap).min(value));
        }
        Ok(crate::InferenceRequest {
            model_profile_id: run.profile.id.clone(),
            messages: if messages.is_empty() {
                vec![InferenceMessage::text(
                    InferenceRole::User,
                    format!(
                        "Benchmark isolation nonce: {}. Ignore this identifier when answering.\n{}",
                        run.run_id, prompt
                    ),
                )]
            } else {
                messages
            },
            generation_settings: Default::default(),
            tools: if tools { suite::tools() } else { Vec::new() },
            tool_choice: None,
            parallel_tool_calls: None,
            output_format: None,
            max_output_tokens: max,
            stream: true,
        })
    }
    async fn one_request(
        self: &Arc<Self>,
        run: &mut Run,
        e: &mut Evidence,
        request: crate::InferenceRequest,
        seconds: u64,
    ) -> Result<(), String> {
        if let Some(active) = self.benchmark.active.lock().await.as_mut() {
            active.phase = format!("{} / {}", e.category, e.id);
        }
        e.status = "running".into();
        e.request_overrides["benchmark_messages_before_profile_defaults"] = json!(request.messages);
        e.request_overrides["tool_definitions"] = json!(request.tools);
        run.evidence.push(e.clone());
        self.benchmark.store.save(run).await?;
        let begin = Instant::now();
        let result =
            tokio::time::timeout(Duration::from_secs(seconds), self.measure(request, e)).await;
        run.phases
            .entry(e.category.clone())
            .and_modify(|v| *v += begin.elapsed().as_secs_f64())
            .or_insert(begin.elapsed().as_secs_f64());
        let error = match result {
            Ok(Ok(response)) => {
                e.response = response.text;
                e.timing = response.timing;
                e.usage = response.usage;
                e.tools = response.calls.iter().map(|c| json!({"call":c})).collect();
                let passed = if e.category == "tool" {
                    let case: suite::ToolCase =
                        serde_json::from_value(e.expected.clone()).map_err(|e| e.to_string())?;
                    suite::grade_single(&case, &e.response, &response.calls)
                } else if e.category == "speed" || e.category == "warmup" {
                    response.calls.is_empty() && !e.response.trim().is_empty()
                } else {
                    response.calls.is_empty() && suite::grade_json(&e.response, &e.expected)
                };
                e.status = if passed { "passed" } else { "failed" }.into();
                if e.category != "speed" && e.category != "warmup" {
                    e.score = Some(f64::from(passed));
                }
                e.explanation = format!(
                    "{}; completion: {}",
                    if passed {
                        "rubric passed"
                    } else {
                        "response does not satisfy the fixed rubric"
                    },
                    response.finish
                );
                None
            }
            Ok(Err(MeasureError::Candidate(reason))) => {
                e.status = "failed".into();
                e.score = (!matches!(e.category.as_str(), "warmup" | "speed")).then_some(0.0);
                e.explanation = reason;
                self.confirm_benchmark_request_stopped(run, e).await.err()
            }
            Ok(Err(MeasureError::Infrastructure(error))) => {
                e.status = "infrastructure_error".into();
                e.explanation = error.clone();
                Some(error)
            }
            Err(_) => {
                e.status = "timeout".into();
                e.score = (!matches!(e.category.as_str(), "warmup" | "speed")).then_some(0.0);
                e.explanation =
                    format!("{seconds}-second task deadline; unsuccessful under fixed budget");
                self.confirm_benchmark_request_stopped(run, e).await.err()
            }
        };
        if let Some(attempt) = run.evidence.last_mut() {
            *attempt = e.clone();
        }
        if let Some(error) = error {
            Err(error)
        } else {
            Ok(())
        }
    }
    async fn agent_task(
        self: &Arc<Self>,
        run: &mut Run,
        case: usize,
        mut request: crate::InferenceRequest,
        e: &mut Evidence,
    ) -> Result<bool, String> {
        e.request_overrides["benchmark_messages_before_profile_defaults"] = json!(request.messages);
        e.request_overrides["tool_definitions"] = json!(request.tools);
        let mut fixture = suite::Fixture::new(case);
        let mut count = 0;
        let mut ids = std::collections::BTreeSet::new();
        for turn in 0..6 {
            fixture.begin_response();
            let r = match self.measure(request.clone(), e).await {
                Ok(r) => r,
                Err(MeasureError::Candidate(reason)) => {
                    e.status = "failed".into();
                    e.score = Some(0.0);
                    e.explanation = reason;
                    self.confirm_benchmark_request_stopped(run, e).await?;
                    return Ok(false);
                }
                Err(MeasureError::Infrastructure(reason)) => return Err(reason),
            };
            if e.response.len() > 65536 {
                return Ok(false);
            }
            e.timing = r.timing.clone();
            e.usage = r.usage.clone();
            e.tools.push(json!({"turn":turn+1,"observations_before_response":fixture,"text":r.text,"calls":r.calls,"timing":r.timing,"usage":r.usage}));
            if r.calls.is_empty() {
                break;
            }
            count += r.calls.len();
            if count > 8 {
                e.explanation = "Candidate exceeded 8 tool calls in this task".into();
                return Ok(false);
            }
            let mut assistant = InferenceMessage::text(InferenceRole::Assistant, r.text);
            assistant.tool_calls = r.calls.clone();
            request.messages.push(assistant);
            for call in r.calls {
                if call.id.is_empty() || !ids.insert(call.id.clone()) {
                    return Ok(false);
                }
                let result = fixture.apply(&call);
                let payload = match result {
                    Ok(v) => json!({"ok":v}),
                    Err(error) => json!({"error":error}),
                };
                e.tools
                    .push(json!({"tool_call_id":call.id,"result":payload}));
                let mut message = InferenceMessage::text(InferenceRole::Tool, payload.to_string());
                message.tool_call_id = Some(call.id);
                request.messages.push(message);
            }
            if let Some(attempt) = run.evidence.last_mut() {
                *attempt = e.clone();
            }
            self.benchmark.store.save(run).await?;
            if fixture.solved(case) {
                return Ok(true);
            }
            // Saved profile behavior is preserved across all turns.
            self.verify_pinned(run).await?;
        }
        Ok(fixture.solved(case))
    }
    async fn confirm_benchmark_request_stopped(
        &self,
        run: &mut Run,
        e: &mut Evidence,
    ) -> Result<(), String> {
        if let Some(attempt) = run.evidence.last_mut() {
            *attempt = e.clone();
        }
        // An event proves this request reached inference. Before that, an idle
        // snapshot cannot exclude a delayed HTTP submission; fail closed.
        if !e.stream_event_observed {
            return Err("Request admission/cancellation unverified; stopping owned backend".into());
        }
        let (adapter, process) = {
            let state = self.state.read().await;
            let running = state
                .backends
                .get(&run.profile.id)
                .and_then(|b| b.running.as_ref())
                .ok_or("backend missing during cancellation")?;
            (running.adapter.clone(), running.process.clone())
        };
        let stopped = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if adapter
                    .confirm_request_stopped(&process)
                    .await
                    .map_err(|e| e.to_string())?
                {
                    if !adapter.health(&process).await.map_err(|e| e.to_string())? {
                        return Err("backend unhealthy after cancellation".to_owned());
                    }
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|_| {
            "Request cancellation not confirmed within 1s; stopping owned backend".to_owned()
        })?;
        stopped?;
        self.verify_pinned(run).await?;
        e.explanation
            .push_str("; HTTP stream closed, engine idle and health confirmed");
        Ok(())
    }

    async fn measure(
        self: &Arc<Self>,
        request: crate::InferenceRequest,
        evidence: &mut Evidence,
    ) -> Result<bench::Response, MeasureError> {
        evidence.stream_event_observed = false;
        let tools_requested = !request.tools.is_empty();
        evidence.timing = Timing {
            request_start_unix_ms: bench::now_ms(),
            ..Default::default()
        };
        let timing = &mut evidence.timing;
        let begin = Instant::now();
        let mut stream = self
            .infer_stream(request)
            .await
            .map_err(|e| e.to_string())?
            .stream;
        let mut text = String::new();
        let mut calls: BTreeMap<u32, InferenceToolCall> = BTreeMap::new();
        let mut bytes = 0;
        let mut usage = None;
        let mut finish = None;
        while let Some(event) = stream.next().await {
            let elapsed = begin.elapsed().as_secs_f64() * 1000.0;
            let event = event.map_err(|e| e.to_string())?;
            evidence.stream_event_observed = true;
            match event {
                InferenceEvent::TextDelta { delta } => {
                    bytes += delta.len();
                    if bytes > 65536 {
                        return Err(MeasureError::Candidate(
                            "response evidence limit exceeded".into(),
                        ));
                    }
                    if !delta.is_empty() {
                        let chars = delta.chars().count();
                        if timing.first_text_ms.is_none() {
                            timing.first_text_ms = Some(elapsed);
                            timing.first_chunk_characters = chars;
                        }
                        if timing.first_visible_ms.is_none()
                            && delta.chars().any(|c| !c.is_whitespace() && !c.is_control())
                        {
                            timing.first_visible_ms = Some(elapsed);
                        }
                        timing.last_text_ms = Some(elapsed);
                        timing.visible_characters += chars;
                    }
                    if evidence.response.len() + delta.len() > 65536 {
                        return Err(MeasureError::Candidate(
                            "response evidence limit exceeded".into(),
                        ));
                    }
                    evidence.response.push_str(&delta);
                    text.push_str(&delta);
                }
                InferenceEvent::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments_delta,
                } => {
                    if calls.len() >= 8 && !calls.contains_key(&index) {
                        return Err(MeasureError::Candidate(
                            "tool-call evidence limit exceeded".into(),
                        ));
                    }
                    bytes += arguments_delta.len()
                        + id.as_ref().map_or(0, String::len)
                        + name.as_ref().map_or(0, String::len);
                    if bytes > 65536 {
                        return Err(MeasureError::Candidate(
                            "tool evidence limit exceeded".into(),
                        ));
                    }
                    let call = calls.entry(index).or_insert(InferenceToolCall {
                        id: String::new(),
                        name: String::new(),
                        arguments: String::new(),
                    });
                    if let Some(id) = id {
                        call.id = id;
                    }
                    if let Some(name) = name {
                        call.name = name;
                    }
                    call.arguments.push_str(&arguments_delta);
                }
                InferenceEvent::Completed {
                    usage: u,
                    finish_reason,
                } => {
                    evidence.usage = u.clone();
                    usage = u;
                    finish = Some(format!("{finish_reason:?}"));
                    timing.completion_ms = Some(elapsed);
                    break;
                }
            }
        }
        let finish = finish.ok_or("stream ended without completion")?;
        let calls = calls.into_values().collect::<Vec<_>>();
        // A parseable prefix is not a complete call. Only after completion can
        // a fixture action be validated and considered executable.
        if tools_requested
            && calls
                .iter()
                .any(|c| !c.id.is_empty() && suite::Fixture::new(0).apply(c).is_ok())
        {
            timing.first_executable_tool_ms = timing.completion_ms;
        }
        Ok(bench::Response {
            text,
            calls,
            timing: timing.clone(),
            usage,
            finish,
        })
    }
}
fn new_evidence(
    id: &str,
    category: &str,
    input: &str,
    expected: Value,
    seconds: u64,
    max: Option<u32>,
) -> Evidence {
    Evidence {
        id: id.into(),
        category: category.into(),
        status: "unattempted".into(),
        score: None,
        explanation: String::new(),
        input: input.into(),
        input_utf8_bytes: input.len(),
        input_unicode_characters: input.chars().count(),
        expected,
        request_overrides: json!({"max_output_tokens":max,"generation_settings":{},"output_format":null,"tools":matches!(category,"tool"|"agent"),"cache_policy":"run nonce prefix"}),
        stream_event_observed: false,
        seconds_limit: seconds,
        max_output_tokens: max,
        response: String::new(),
        tools: Vec::new(),
        timing: Timing::default(),
        usage: None,
    }
}
fn configuration_key(
    p: &norted_core::RuntimeProvenance,
    saved: &Value,
    environment: &Value,
) -> String {
    // Retain requested and observed settings separately; strip only incidental
    // process/time/display information, not runtime or artifact identities.
    bench::digest(semantic(
        json!({"model":p.model,"runtime":p.runtime,"executable_sha256":p.installation.binary_sha256,
        "accelerator":p.accelerator_binding,"settings":p.settings,"observed":p.normalized_settings,
        "saved":saved,"host":environment["host"],"os":environment["os"],"architecture":environment["architecture"],"logical_cpus":environment["logical_cpus"],"cpu_model":environment["cpu_model"],"ram_total":environment["ram_total"]}),
    ))
}

fn semantic(mut value: Value) -> Value {
    match &mut value {
        Value::Object(map) => {
            for key in [
                "display_name",
                "installed_at_unix",
                "acquired_at_unix",
                "observed_at_unix",
                "launched_at_unix",
                "process_id",
                "detail",
                "observations",
            ] {
                map.remove(key);
            }
            for child in map.values_mut() {
                *child = semantic(child.take());
            }
        }
        Value::Array(items) => {
            for child in items {
                *child = semantic(child.take());
            }
        }
        _ => {}
    }
    value
}
async fn file_observation(path: &std::path::Path) -> Value {
    match tokio::fs::metadata(path).await {
        Ok(m) => {
            #[cfg(unix)]
            let identity = {
                use std::os::unix::fs::MetadataExt;
                json!({"device":m.dev(),"inode":m.ino(),"ctime":m.ctime(),"ctime_nsec":m.ctime_nsec()})
            };
            #[cfg(not(unix))]
            let identity = Value::Null;
            json!({"size":m.len(),"modified_unix_ns":m.modified().ok().and_then(|t|t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d|d.as_nanos()),"identity":identity,"evidence":"metadata observation, not a fresh content digest"})
        }
        Err(e) => json!({"unverified":e.to_string()}),
    }
}
async fn served_files(p: &norted_core::RuntimeProvenance) -> Value {
    let mut files = vec![
        json!({"path":p.runtime_entrypoint,"observation":file_observation(&p.runtime_entrypoint).await}),
        json!({"path":p.model.artifact_path,"observation":file_observation(&p.model.artifact_path).await}),
    ];
    for a in &p.model.auxiliary {
        files.push(
            json!({"path":a.artifact_path,"observation":file_observation(&a.artifact_path).await}),
        );
    }
    json!(files)
}
async fn read_host_line(path: &str, key: &str) -> Option<String> {
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path).await.ok()?;
    let mut text = String::new();
    file.take(65536).read_to_string(&mut text).await.ok()?;
    text.lines().find_map(|line| {
        line.split_once(':')
            .filter(|(name, _)| name.trim() == key)
            .map(|(_, v)| v.trim().to_owned())
    })
}

fn verified_key(p: &norted_core::RuntimeProvenance, saved: &Value, env: &Value) -> Option<String> {
    fn missing(v: &Value) -> bool {
        match v {
            Value::Object(m) => m.contains_key("unverified") || m.values().any(missing),
            Value::Array(a) => a.iter().any(missing),
            _ => false,
        }
    }
    if missing(saved)
        || p.runtime.entrypoint_sha256.is_empty()
        || (p.runtime.identity.accelerator != "cpu"
            && p.accelerator_binding.as_ref().is_none_or(|b| {
                b.devices.is_empty() || b.devices.iter().any(|d| d.stable_id.is_none())
            }))
        || (p.model.content_sha256.is_none() && p.model.native_identity.is_none())
        || matches!(
            p.selection_source,
            norted_core::RuntimeSelectionSource::Fallback
        )
        || env["cpu_model"].is_null()
        || env["ram_total"].is_null()
    {
        None
    } else {
        Some(configuration_key(p, saved, env))
    }
}

async fn setting_file_observations(settings: &norted_core::ResolvedSettings) -> Value {
    let mut files = Vec::new();
    for (id, value) in &settings.configured {
        if let norted_core::SettingValue::Path(path) = &value.value {
            files.push(
                json!({"setting_id":id,"path":path,"observation":file_observation(path).await}),
            );
        }
    }
    json!(files)
}

#[derive(Debug)]
enum MeasureError {
    Candidate(String),
    Infrastructure(String),
}
impl From<String> for MeasureError {
    fn from(value: String) -> Self {
        Self::Infrastructure(value)
    }
}
impl From<&str> for MeasureError {
    fn from(value: &str) -> Self {
        Self::Infrastructure(value.into())
    }
}
