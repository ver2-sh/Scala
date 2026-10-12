//! Independent v3 contracts. Linux schema 25 includes constraints; Windows
//! v0.9.1 schema 21 does not. Neither borrows the v2 reader/startup evidence.
use super::*;
use serde_json::Value;
mod owners;
pub(super) const CMAKE_SHA256: &str =
    "e6878298480e49528bbeedeaeb3d53f645b6d8052862adb14f6271df8be6ecbc";

pub(super) fn native_identities() -> Vec<ArtifactNativeIdentity> {
    ["Qwen3_5ForCausalLM", "Qwen3_5MoeForCausalLM"]
        .into_iter()
        .map(|model_id| {
            ArtifactNativeIdentity::Ninfer(NinferArtifactIdentity {
                container_version: 3,
                model_id: model_id.into(),
                weights_id: "v3-schema".into(),
            })
        })
        .collect()
}
pub(super) fn compatible(
    supported: &[ArtifactNativeIdentity],
    model: &ArtifactNativeIdentity,
) -> bool {
    matches!(model, ArtifactNativeIdentity::Ninfer(m) if m.container_version == 3 && supported.iter().any(|s|matches!(s,ArtifactNativeIdentity::Ninfer(r) if r.container_version==3 && r.weights_id=="v3-schema" && r.model_id==m.model_id)))
}
pub(super) fn source_registry_matches(root: &Path) -> bool {
    owners::SOURCE_OWNERS.iter().all(|(path, digest)| {
        std::fs::read(root.join(path))
            .is_ok_and(|bytes| format!("{:x}", Sha256::digest(bytes)) == *digest)
    })
}
pub(super) fn reviewed_source(
    identity: &scala_core::RuntimeIdentity,
    source: &scala_core::RuntimeSourceSnapshot,
) -> bool {
    canonical_source_identity(identity, source)
        && identity.variant == "ninfer-serve-v3-sm120a"
        && source.commit_sha == catalog::V3_REVISION
        && source.tree_sha == catalog::V3_TREE
}
fn capabilities(windows: bool) -> NinferRuntimeCapabilities {
    let mut c = ninfer_reviewed_capabilities(true, NinferReviewedDomains::all(true));
    c.request_log_schema = Some(if windows { 21 } else { 25 });
    c
}
pub(super) fn is_v3(c: NinferRuntimeCapabilities) -> bool {
    matches!(c.request_log_schema, Some(21 | 25))
}
pub(super) fn installed_capabilities(
    runtime: &InstalledRuntime,
) -> Option<NinferRuntimeCapabilities> {
    let m = &runtime.manifest;
    if windows_catalog::is_reviewed_installed_release(m)
        && m.identity.variant == windows_catalog::V3_VARIANT
    {
        return Some(capabilities(true));
    }
    (m.acquisition_method == RuntimeAcquisitionMethod::SourceBuild
        && m.supported_native_identities == native_identities()
        && m.source_build.as_ref().is_some_and(|p| {
            reviewed_source(&m.identity, &p.source)
                && p.build_definition_sha256.as_deref() == Some(CMAKE_SHA256)
                && p.build_platform == "linux"
                && p.build_architecture == "x86_64"
                && p.recipe_version == catalog::V3_RECIPE_VERSION
                && p.build_target == "ninfer-serve"
                && p.accelerator_target == "sm_120a"
                && p.entrypoint == Path::new("source/build/apps/ninfer-serve")
        }))
    .then(|| capabilities(false))
}
pub(super) fn available_capabilities(r: &AvailableRuntime) -> Option<NinferRuntimeCapabilities> {
    if windows_catalog::is_reviewed_release_candidate(r)
        && r.identity.variant == windows_catalog::V3_VARIANT
    {
        return Some(capabilities(true));
    }
    matches!(&r.acquisition,scala_core::RuntimeAcquisitionPlan::SourceBuild(p) if reviewed_source(&r.identity,&p.source) && p.recipe.recipe_version==catalog::V3_RECIPE_VERSION && p.recipe.build_target=="ninfer-serve" && p.recipe.accelerator_target=="sm_120a").then(||capabilities(false))
}
pub(super) fn component_present(model: &ModelArtifact, component: &str) -> bool {
    inspect_ninfer_container(&model.path).is_ok_and(|m| {
        m.v3_directory
            .as_ref()
            .and_then(|d| d["components"].get(component))
            .is_some()
    })
}
pub(super) fn validate_model_settings(
    settings: &ResolvedSettings,
    model: &ModelArtifact,
) -> Result<(), String> {
    let m = inspect_ninfer_container(&model.path).map_err(|e| e.to_string())?;
    if model.native_identity.as_ref() != Some(&ArtifactNativeIdentity::Ninfer(m.identity.clone())) {
        return Err("native artifact identity changed".into());
    }
    let directory = m
        .v3_directory
        .as_ref()
        .ok_or("native v3 directory is absent")?;
    let components = &directory["components"];
    if settings.value("ninfer.vision") == Some(&SettingValue::Toggle(true))
        && components.get("vision").is_none()
    {
        return Err(
            "ninfer.vision requires a native vision component and frontend resources".into(),
        );
    }
    if let Some(backend) = ninfer_startup_requirements(settings)
        .map_err(|e| e.to_string())?
        .speculative_backend
        && backend != "none"
    {
        if components.get(&backend).is_none() {
            return Err(format!(
                "ninfer.speculative_backend={backend} requires the corresponding native component"
            ));
        }
        let proposal = settings.value("ninfer.lm_head_draft") == Some(&SettingValue::Toggle(true));
        if proposal && components["text"].get("proposal").is_none() {
            return Err("ninfer.lm_head_draft requires native proposal resources".into());
        }
        let head = if proposal {
            "proposal/head"
        } else {
            "text/output_head"
        };
        let input = format!("{backend}/final_hidden");
        if !directory["uses"].as_array().unwrap().iter().any(|u| {
            u["parameter"] == head
                && u["input"] == input
                && matches!(
                    u["activation_policy"].as_str(),
                    Some("A16Only" | "AllowA8" | "AllowA4")
                )
        }) {
            return Err(format!(
                "ninfer.speculative_backend={backend} requires native Use {head}@{input} with an activation policy"
            ));
        }
    }
    Ok(())
}
fn failure() -> EngineError {
    EngineError::Operation("NInfer v3 startup does not prove the selected artifact, device, schema or explicit settings".into())
}
fn number(v: &Value) -> Result<f64, EngineError> {
    v.as_f64().filter(|v| v.is_finite()).ok_or_else(failure)
}

pub(super) fn validate_startup(
    p: &PendingStartup,
    s: &Value,
) -> Result<ObservedNinferStartup, EngineError> {
    let schema = p.capabilities.request_log_schema.ok_or_else(failure)?;
    if !matches!(schema, 21 | 25)
        || s["schema_version"] != schema
        || s["artifact_type"] != "ninfer_serve_request_log"
        || s["event"] != "server_start"
        || s["server"]["public_model_id"] != p.public_model_id.as_str()
        || s["artifact"]["architecture"] != p.native_identity.model_id
    {
        return Err(failure());
    }
    let (path, size) = p.artifact_file.as_ref().ok_or_else(failure)?;
    if s["artifact"]["path"].as_str() != path.to_str()
        || s["artifact"]["size_bytes"].as_u64() != Some(*size)
    {
        return Err(failure());
    }
    let env = &s["environment"];
    if env["gpu_uuid"].as_str() != p.accelerator.stable_id.as_deref()
        || p.accelerator.stable_id.is_none()
        || p.accelerator
            .name
            .as_deref()
            .is_some_and(|n| env["gpu_name"] != n)
        || p.accelerator.compute_capability.is_some_and(|c| {
            env["compute_capability_major"] != c.major || env["compute_capability_minor"] != c.minor
        })
    {
        return Err(failure());
    }
    let e = &s["engine"];
    let server = &s["server"];
    let cache = &e["context_cache"];
    let sampling = &s["sampling_defaults"];
    for key in [
        "max_context",
        "max_concurrency",
        "kv_capacity",
        "prefill_chunk",
    ] {
        if e[key].as_u64().is_none_or(|n| n == 0) {
            return Err(failure());
        }
    }
    for key in [
        "max_pending_requests",
        "pending_timeout_ms",
        "log_stats_interval_ms",
        "speculative_draft_window",
    ] {
        if e[key].as_u64().is_none() {
            return Err(failure());
        }
    }
    for key in ["cuda_graph", "prefix_reuse", "vision"] {
        if e[key].as_bool().is_none() {
            return Err(failure());
        }
    }
    if cache["device_state_slots"].as_u64().is_none()
        || server["default_output_tokens"].as_u64().is_none()
        || !matches!(
            e["speculative_backend"].as_str(),
            Some("none" | "mtp" | "dflash" | "dflash2")
        )
    {
        return Err(failure());
    }
    let kv = e["kv_cache"].as_str().ok_or_else(failure)?;
    let kv_cli = match kv {
        "bf16" => "bf16",
        "int8-group64" => "int8",
        "fp8-e4m3-row256" => "fp8",
        "nvfp4" => "nvfp4",
        "k8v4" => "k8v4",
        _ => return Err(failure()),
    };
    let thinking = server["default_thinking"].as_bool();
    if !server["default_thinking"].is_null() && thinking.is_none() {
        return Err(failure());
    }
    let preset = thinking.map(|t| &sampling[if t { "thinking" } else { "non_thinking" }]);
    let override_value = |name: &str| -> Result<Option<f64>, EngineError> {
        if !sampling["server_overrides"][name].is_null() {
            Ok(Some(number(&sampling["server_overrides"][name])?))
        } else {
            preset.map(|v| number(&v[name])).transpose()
        }
    };
    // Both conditional presets remain observations. Never select a preset when
    // the startup contract leaves thinking under the native template's control.
    for mode in ["thinking", "non_thinking"] {
        for name in [
            "temperature",
            "top_p",
            "top_k",
            "min_p",
            "presence_penalty",
            "frequency_penalty",
        ] {
            number(&sampling[mode][name])?;
        }
    }
    let greedy = sampling["greedy"].as_bool().ok_or_else(failure)?;
    let defaults = EffectiveGenerationSettings {
        temperature: if greedy {
            Some(0.0)
        } else {
            override_value("temperature")?
        },
        top_p: override_value("top_p")?,
    };
    if defaults
        .temperature
        .is_some_and(|v| !(0.0..=2.0).contains(&v))
        || defaults.top_p.is_some_and(|v| !(0.0..=1.0).contains(&v))
    {
        return Err(failure());
    }
    let mut observed = BTreeMap::new();
    let fields = [
        ("context_length", &e["max_context"]),
        ("parallel_requests", &e["max_concurrency"]),
        ("kv_capacity", &e["kv_capacity"]),
        ("prefill_chunk", &e["prefill_chunk"]),
        ("cuda_graph", &e["cuda_graph"]),
        ("prefix_reuse", &e["prefix_reuse"]),
        ("vision", &e["vision"]),
        ("device_state_slots", &cache["device_state_slots"]),
        ("max_pending_requests", &e["max_pending_requests"]),
        ("pending_timeout_ms", &e["pending_timeout_ms"]),
        ("log_stats_interval_ms", &e["log_stats_interval_ms"]),
        ("max_output_tokens", &server["default_output_tokens"]),
        ("reasoning_budget", &server["default_thinking_budget"]),
        ("thinking", &server["default_thinking"]),
        ("preserve_thinking", &server["default_preserve_thinking"]),
    ];
    for (k, v) in fields {
        if !v.is_null() {
            observed.insert(format!("ninfer.{k}"), v.clone());
        }
    }
    observed.insert("ninfer.kv_dtype".into(), json!(kv_cli));
    let host = if schema == 25 {
        "host_capacity_bytes"
    } else {
        "host_kv_capacity_bytes"
    };
    let host_bytes = cache[host].as_u64().ok_or_else(failure)?;
    observed.insert(
        if schema == 25 {
            "ninfer.host_context_mib"
        } else {
            "ninfer.host_kv_mib"
        }
        .into(),
        json!(host_bytes >> 20),
    );
    if schema == 21 {
        for k in [
            "host_state_slots",
            "max_private_continuations",
            "max_shared_prefixes",
            "max_long_anchors_per_continuation",
        ] {
            observed.insert(format!("ninfer.{k}"), cache[k].clone());
        }
    }
    for name in [
        "temperature",
        "top_p",
        "top_k",
        "min_p",
        "presence_penalty",
        "frequency_penalty",
    ] {
        if let Some(v) = override_value(name)? {
            observed.insert(format!("ninfer.{name}"), json!(v));
        }
    }
    if !sampling["server_overrides"]["seed"].is_null() {
        observed.insert(
            "ninfer.seed".into(),
            sampling["server_overrides"]["seed"].clone(),
        );
    }
    observed.insert("native_sampling_presets".into(), sampling.clone());
    if let Some(r) = &p.settings_requirements {
        let check_u = |requested: Option<u64>, actual: &Value| -> Result<(), EngineError> {
            if requested.is_some_and(|v| actual.as_u64() != Some(v)) {
                Err(failure())
            } else {
                Ok(())
            }
        };
        let check_b = |requested: Option<bool>, actual: &Value| -> Result<(), EngineError> {
            if requested.is_some_and(|v| actual.as_bool() != Some(v)) {
                Err(failure())
            } else {
                Ok(())
            }
        };
        if r.minimum_context_tokens
            .is_some_and(|v| e["max_context"].as_u64().is_none_or(|n| n < v))
        {
            return Err(failure());
        }
        for (v, a) in [
            (r.max_concurrency, &e["max_concurrency"]),
            (r.kv_capacity, &e["kv_capacity"]),
            (r.prefill_chunk, &e["prefill_chunk"]),
            (r.device_state_slots, &cache["device_state_slots"]),
            (r.host_context_bytes, &cache["host_capacity_bytes"]),
            (r.host_state_slots, &cache["host_state_slots"]),
            (r.host_kv_bytes, &cache["host_kv_capacity_bytes"]),
            (
                r.max_private_continuations,
                &cache["max_private_continuations"],
            ),
            (r.max_shared_prefixes, &cache["max_shared_prefixes"]),
            (
                r.max_long_anchors_per_continuation,
                &cache["max_long_anchors_per_continuation"],
            ),
            (r.speculative_draft_window, &e["speculative_draft_window"]),
            (r.default_output_tokens, &server["default_output_tokens"]),
            (
                r.default_thinking_budget,
                &server["default_thinking_budget"],
            ),
            (r.max_pending_requests, &e["max_pending_requests"]),
            (r.pending_timeout_ms, &e["pending_timeout_ms"]),
            (r.log_stats_interval_ms, &e["log_stats_interval_ms"]),
            (r.max_request_bytes, &server["max_request_bytes"]),
            (r.media_cache_bytes, &server["media_cache_bytes"]),
            (r.media_live_bytes, &server["media_live_bytes"]),
            (
                r.media_preprocess_threads,
                &server["media_preprocess_threads"],
            ),
        ] {
            check_u(v, a)?;
        }
        for (v, a) in [
            (r.cuda_graph, &e["cuda_graph"]),
            (r.prefix_reuse, &e["prefix_reuse"]),
            (r.vision, &e["vision"]),
            (r.expected_thinking, &server["default_thinking"]),
            (r.preserve_thinking, &server["default_preserve_thinking"]),
            (r.greedy, &sampling["greedy"]),
        ] {
            check_b(v, a)?;
        }
        for (v, a) in [
            (&r.kv_capacity_mode, &e["kv_capacity_mode"]),
            (&r.speculative_backend, &e["speculative_backend"]),
            (&r.proposal_head, &e["proposal_head"]),
        ] {
            if v.as_ref().is_some_and(|v| a.as_str() != Some(v.as_str())) {
                return Err(failure());
            }
        }
        if r.kv_dtype.as_ref().is_some_and(|v| v != kv_cli) {
            return Err(failure());
        }
        for (requested, name) in [
            (r.temperature, "temperature"),
            (r.top_p, "top_p"),
            (r.min_p, "min_p"),
            (r.presence_penalty, "presence_penalty"),
            (r.frequency_penalty, "frequency_penalty"),
        ] {
            if requested.is_some_and(|v| {
                override_value(name)
                    .ok()
                    .flatten()
                    .is_none_or(|a| !matches_runtime_float(a, v))
            }) {
                return Err(failure());
            }
        }
        check_u(r.top_k, &sampling["server_overrides"]["top_k"])?;
        check_u(r.seed, &sampling["server_overrides"]["seed"])?;
    }
    Ok(ObservedNinferStartup {
        kv_cache_format: kv.into(),
        generation_settings: defaults,
        resolved_settings: observed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn v3_generations_observe_their_own_schema_and_conditional_defaults() {
        for windows in [false, true] {
            let p = PendingStartup {
                request_log_path: PathBuf::from("unused"),
                artifact_file: Some((PathBuf::from("model.ninfer"), 4096)),
                native_identity: NinferArtifactIdentity {
                    container_version: 3,
                    model_id: "Qwen3_5ForCausalLM".into(),
                    weights_id: "v3-fixture".into(),
                },
                public_model_id: ModelProfileId::new("fixture").unwrap(),
                accelerator: AcceleratorDevice {
                    accelerator: "cuda".into(),
                    stable_id: Some("GPU-fixture".into()),
                    name: Some("NVIDIA GeForce RTX 5090".into()),
                    vram_bytes: None,
                    driver_version: None,
                    compute_capability: Some(scala_core::ComputeCapability::new(12, 0)),
                },
                settings_requirements: None,
                capabilities: capabilities(windows),
            };
            let preset = json!({"temperature":0.7,"top_p":0.9,"top_k":20,"min_p":0.0,"presence_penalty":0.0,"frequency_penalty":0.0});
            let mut s = json!({"schema_version":if windows {21}else{25},"artifact_type":"ninfer_serve_request_log","event":"server_start",
                "server":{"public_model_id":"fixture","default_thinking":null,"default_output_tokens":2048},
                "artifact":{"architecture":"Qwen3_5ForCausalLM","path":"model.ninfer","size_bytes":4096},
                "environment":{"gpu_uuid":"GPU-fixture","gpu_name":"NVIDIA GeForce RTX 5090","compute_capability_major":12,"compute_capability_minor":0},
                "engine":{"kv_cache":"bf16","max_context":4096,"kv_capacity":4096,"max_concurrency":1,"prefill_chunk":128,"max_pending_requests":8,"pending_timeout_ms":0,"log_stats_interval_ms":0,"cuda_graph":true,"prefix_reuse":true,"vision":false,"speculative_backend":"none","speculative_draft_window":0,"context_cache":{"device_state_slots":0,"host_capacity_bytes":0,"host_kv_capacity_bytes":0}},
                "sampling_defaults":{"greedy":false,"server_overrides":{},"thinking":preset,"non_thinking":preset}});
            let observed = validate_startup(&p, &s).unwrap();
            assert_eq!(observed.generation_settings.temperature, None);
            assert_eq!(observed.generation_settings.top_p, None);
            assert!(
                observed
                    .resolved_settings
                    .contains_key("native_sampling_presets")
            );
            s["server"]["default_thinking"] = json!(true);
            assert_eq!(
                validate_startup(&p, &s)
                    .unwrap()
                    .generation_settings
                    .temperature,
                Some(0.7)
            );
            s["schema_version"] = json!(if windows { 25 } else { 21 });
            assert!(validate_startup(&p, &s).is_err());
            s["schema_version"] = json!(if windows { 21 } else { 25 });
            s["environment"]["gpu_uuid"] = json!("GPU-other");
            assert!(validate_startup(&p, &s).is_err());
        }
    }
    #[test]
    fn partial_source_owner_evidence_and_cross_generation_bindings_never_qualify() {
        let d = tempfile::tempdir().unwrap();
        assert!(!source_registry_matches(d.path()));
        let v3 = native_identities();
        assert!(compatible(&v3, &v3[0]));
        let mut v2 = v3[0].clone();
        if let ArtifactNativeIdentity::Ninfer(ref mut i) = v2 {
            i.container_version = 2;
        }
        assert!(!compatible(&v3, &v2));
        assert!(!compatible(&[v2], &v3[0]));
        assert_eq!(capabilities(false).request_log_schema, Some(25));
        assert_eq!(capabilities(true).request_log_schema, Some(21));
    }
}
