use super::*;
use scala_core::{
    ModelId, ResolvedSetting, SettingId, SettingSource, UnsignedIntegerOrChoiceValue,
};

fn settings(values: &[(&str, SettingValue)]) -> ResolvedSettings {
    ResolvedSettings {
        engine_id: ENGINE_ID.into(),
        model_profile_id: None,
        configured: values
            .iter()
            .map(|(id, v)| {
                (
                    SettingId::new(*id).unwrap(),
                    ResolvedSetting {
                        value: v.clone(),
                        source: SettingSource::Invocation,
                    },
                )
            })
            .collect(),
        effective: BTreeMap::new(),
    }
}
fn extended() -> ResolvedSettings {
    settings(&[
        (
            "ninfer.context_length",
            SettingValue::UnsignedInteger(500_000),
        ),
        (
            "ninfer.kv_capacity",
            SettingValue::UnsignedIntegerOrChoice(UnsignedIntegerOrChoiceValue::UnsignedInteger(
                500_000,
            )),
        ),
        ("ninfer.rope_yarn", SettingValue::Toggle(true)),
        ("ninfer.gdn_state_fp16", SettingValue::Toggle(true)),
        ("ninfer.kv_dtype", SettingValue::Choice("rk4v4".into())),
    ])
}
fn model() -> ModelArtifact {
    ModelArtifact {
        id: ModelId("fixture".into()),
        display_name: "fixture".into(),
        path: "fixture.ninfer".into(),
        format: ArtifactFormat::Ninfer,
        size_bytes: 4096,
        created: 1,
        hash: None,
        architecture: None,
        context_length: None,
        provenance: None,
        native_identity: Some(ArtifactNativeIdentity::Ninfer(NinferArtifactIdentity {
            container_version: 3,
            model_id: "Qwen3_5ForCausalLM".into(),
            weights_id: "v3-schema".into(),
        })),
        auxiliary_artifacts: vec![],
        norted_package: None,
    }
}
fn fixture(requested: Option<&ResolvedSettings>) -> (PendingStartup, Value) {
    let p = PendingStartup {
        request_log_path: "unused".into(),
        artifact_file: Some(("model.ninfer".into(), 4096)),
        native_identity: NinferArtifactIdentity {
            container_version: 3,
            model_id: "Qwen3_5ForCausalLM".into(),
            weights_id: "v3-schema".into(),
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
        settings_requirements: requested.map(|v| ninfer_startup_requirements(v).unwrap()),
        capabilities: capabilities(),
    };
    let preset = json!({"temperature":0.7,"top_p":0.9,"top_k":20,"min_p":0.0,"presence_penalty":0.0,"frequency_penalty":0.0});
    let s = json!({"schema_version":28,"artifact_type":"ninfer_serve_request_log","event":"server_start",
        "server":{"public_model_id":"fixture","default_thinking":null,"default_preserve_thinking":null,"default_output_tokens":null,"default_output_policy":"concurrent_lane_budget"},
        "artifact":{"architecture":"Qwen3_5ForCausalLM","path":"model.ninfer","size_bytes":4096},
        "environment":{"gpu_uuid":"GPU-fixture","gpu_name":"NVIDIA GeForce RTX 5090","compute_capability_major":12,"compute_capability_minor":0},
        "engine":{"kv_cache":"rotated-lloyd4g64-v4g32","max_context":500000,"kv_capacity":500032,"kv_capacity_mode":"explicit","max_concurrency":1,"prefill_chunk":128,"max_pending_requests":8,"pending_timeout_ms":0,"log_stats_interval_ms":0,"cuda_graph":true,"prefix_reuse":true,"vision":false,"speculative_backend":"none","speculative_draft_window":0,"proposal_head":"auto","rope_yarn":true,"rope_yarn_factor":1.0,"gdn_state_fp16":true,
            "context_cache":{"device_state_slots":0,"host_kv_capacity_bytes":0,"host_state_slots":0,"max_private_continuations":1,"max_shared_prefixes":0,"max_long_anchors_per_continuation":0}},
        "sampling_defaults":{"greedy":false,"server_overrides":{},"thinking":preset,"non_thinking":preset}});
    (p, s)
}
#[test]
fn launch_500k_uses_native_flags_without_changing_settings_or_weights() {
    let s = extended();
    let before = format!("{s:?}");
    validate_ninfer_settings_prelaunch(&s, capabilities()).unwrap();
    let args = settings::translate(&s, &model(), &[], true).unwrap();
    let args: Vec<_> = args.iter().map(|a| a.to_str().unwrap()).collect();
    for pair in [
        ["--max-context", "500000"],
        ["--kv-capacity", "500000"],
        ["--kv-dtype", "rk4v4"],
    ] {
        assert!(args.windows(2).any(|w| w == pair));
    }
    assert!(args.contains(&"--rope-yarn") && args.contains(&"--gdn-state-fp16"));
    assert!(!args.contains(&"--rope-yarn-factor"));
    assert_eq!(before, format!("{s:?}"));
}
#[test]
fn inherit_and_false_are_distinct_and_do_not_emit_positive_flags() {
    let empty = settings(&[]);
    assert!(
        settings::translate(&empty, &model(), &[], true)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        ExtendedContext::from_settings(&empty).unwrap(),
        ExtendedContext::default()
    );
    let off = settings(&[
        ("ninfer.rope_yarn", SettingValue::Toggle(false)),
        ("ninfer.gdn_state_fp16", SettingValue::Toggle(false)),
    ]);
    assert!(
        settings::translate(&off, &model(), &[], true)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        ExtendedContext::from_settings(&off).unwrap().rope_yarn,
        Some(false)
    );
    assert!(validate_settings(&off, NinferRuntimeCapabilities::default()).is_err());
}
#[test]
fn full_proposal_head_is_explicit_and_none_disables_speculation() {
    let s = settings(&[
        (
            "ninfer.speculative_backend",
            SettingValue::Choice("none".into()),
        ),
        ("ninfer.lm_head_draft", SettingValue::Toggle(false)),
    ]);
    let mut args = settings::translate(&s, &model(), &[], true).unwrap();
    finish_launch_arguments(&s, &mut args);
    assert_eq!(args, vec![OsString::from("--full-draft-head")]);
    assert_eq!(
        ninfer_startup_requirements(&s)
            .unwrap()
            .speculative_backend
            .as_deref(),
        Some("none")
    );
}
#[test]
fn fixed_yarn_factors_are_finite_and_bounded() {
    for factor in [0.0, 0.99, 4.01, f64::NAN, f64::INFINITY] {
        assert!(
            validate_settings(
                &settings(&[("ninfer.rope_yarn_factor", SettingValue::Float(factor))]),
                capabilities()
            )
            .is_err()
        );
    }
    for factor in [1.0, 1.9073486328125, 2.0, 4.0] {
        validate_settings(
            &settings(&[("ninfer.rope_yarn_factor", SettingValue::Float(factor))]),
            capabilities(),
        )
        .unwrap();
    }
}
#[test]
fn old_runtime_generations_do_not_gain_rotated_kv_or_yarn() {
    for schema in [20, 21, 25] {
        let mut c = capabilities();
        c.request_log_schema = Some(schema);
        assert!(validate_ninfer_settings_prelaunch(&extended(), c).is_err());
        for kv in ROTATED_KV {
            assert!(
                validate_settings(
                    &settings(&[("ninfer.kv_dtype", SettingValue::Choice((*kv).into()))]),
                    c
                )
                .is_err()
            );
        }
        assert!(
            validate_settings(
                &settings(&[("ninfer.kv_dtype", SettingValue::Choice("fp8".into()))]),
                c
            )
            .is_ok()
        );
    }
}
#[test]
fn artifact_and_draft_position_limits_are_independent_of_labels() {
    let mut directory = json!({"components":{"text":{"config":{"max_position_embeddings":262144}},"dflash2":{"config":{"max_position_embeddings":32768}}}});
    let mut s = extended();
    validate_position_capacity(&s, &directory).unwrap();
    s.configured.extend(
        settings(&[(
            "ninfer.speculative_backend",
            SettingValue::Choice("dflash2".into()),
        )])
        .configured,
    );
    assert!(validate_position_capacity(&s, &directory).is_err());
    directory["components"]["dflash2"]["config"]["max_position_embeddings"] = json!(262144);
    validate_position_capacity(&s, &directory).unwrap();
    directory["components"]["text"]["config"]["max_position_embeddings"] = json!(32768);
    assert!(validate_position_capacity(&s, &directory).is_err());
}
#[test]
fn startup_500k_keeps_requested_and_page_rounded_capacity_separate() {
    let s = extended();
    let before = format!("{s:?}");
    let (p, record) = fixture(Some(&s));
    let observed = v3::validate_startup(&p, &record).unwrap();
    assert_eq!(
        observed.resolved_settings["ninfer.context_length"],
        json!(500000)
    );
    assert_eq!(
        observed.resolved_settings["ninfer.kv_capacity"],
        json!(500032)
    );
    assert_eq!(
        observed.resolved_settings["ninfer.kv_dtype"],
        json!("rk4v4")
    );
    assert_eq!(observed.resolved_settings["ninfer.rope_yarn"], json!(true));
    assert_eq!(before, format!("{s:?}"));
}
#[test]
fn conditional_output_and_sampler_defaults_remain_unknown() {
    let (p, s) = fixture(None);
    let o = v3::validate_startup(&p, &s).unwrap();
    assert_eq!(o.generation_settings.temperature, None);
    assert_eq!(o.generation_settings.top_p, None);
    assert!(!o.resolved_settings.contains_key("ninfer.max_output_tokens"));
    assert!(!o.resolved_settings.contains_key("ninfer.lm_head_draft"));
    assert_eq!(
        o.resolved_settings["native_output_policy"],
        json!("concurrent_lane_budget")
    );
    assert!(o.resolved_settings.contains_key("native_sampling_presets"));
}
#[test]
fn missing_or_inconsistent_output_policy_is_rejected() {
    let (p, s) = fixture(None);
    for policy in [Value::Null, json!("fixed"), json!("unknown")] {
        let mut bad = s.clone();
        bad["server"]["default_output_policy"] = policy;
        assert!(v3::validate_startup(&p, &bad).is_err());
    }
    let mut fixed = s;
    fixed["server"]["default_output_policy"] = json!("fixed");
    fixed["server"]["default_output_tokens"] = json!(32768);
    assert_eq!(
        v3::validate_startup(&p, &fixed).unwrap().resolved_settings["ninfer.max_output_tokens"],
        json!(32768)
    );
}
#[test]
fn startup_rejects_wrong_artifact_gpu_schema_settings_and_capacity() {
    let (p, s) = fixture(Some(&extended()));
    for (pointer, value) in [
        ("/schema_version", json!(25)),
        ("/artifact/path", json!("other.ninfer")),
        ("/artifact/size_bytes", json!(4097)),
        ("/environment/gpu_uuid", json!("GPU-other")),
        ("/engine/rope_yarn", json!(false)),
        ("/engine/gdn_state_fp16", json!(false)),
        ("/engine/kv_cache", json!("bf16")),
        ("/engine/max_context", json!(524288)),
        ("/engine/kv_capacity", json!(499968)),
        ("/engine/kv_capacity", json!(500096)),
        ("/engine/rope_yarn_factor", Value::Null),
    ] {
        let mut bad = s.clone();
        *bad.pointer_mut(pointer).unwrap() = value;
        assert!(v3::validate_startup(&p, &bad).is_err(), "{pointer}");
    }
}
#[test]
fn all_rotated_formats_require_matching_native_observations() {
    for (cli, log) in [
        ("rk8v4", "rotated-k8g64-v4g32"),
        ("rk4v4", "rotated-lloyd4g64-v4g32"),
        ("rk4v4-e8", "rotated-k4e8g64-v4g32"),
        ("rk2v4-e8", "rotated-k2e8g64-v4g32"),
    ] {
        let (p, mut s) = fixture(Some(&settings(&[(
            "ninfer.kv_dtype",
            SettingValue::Choice(cli.into()),
        )])));
        s["engine"]["kv_cache"] = json!(log);
        assert_eq!(
            v3::validate_startup(&p, &s).unwrap().resolved_settings["ninfer.kv_dtype"],
            json!(cli)
        );
    }
}
#[test]
fn automatic_kv_and_fixed_yarn_policy_are_verified_not_persisted() {
    let r = settings(&[
        (
            "ninfer.kv_capacity",
            SettingValue::UnsignedIntegerOrChoice(UnsignedIntegerOrChoiceValue::Choice(
                "auto".into(),
            )),
        ),
        ("ninfer.rope_yarn_factor", SettingValue::Float(2.0)),
    ]);
    let (p, mut s) = fixture(Some(&r));
    s["engine"]["kv_capacity_mode"] = json!("auto");
    s["engine"]["kv_capacity"] = json!(524288);
    s["engine"]["rope_yarn_factor"] = json!(2.0);
    v3::validate_startup(&p, &s).unwrap();
    s["engine"]["rope_yarn_factor"] = json!(4.0);
    assert!(v3::validate_startup(&p, &s).is_err());
}
#[test]
fn dflash_and_vision_startup_require_exact_requested_components() {
    let r = settings(&[
        (
            "ninfer.speculative_backend",
            SettingValue::Choice("dflash2".into()),
        ),
        ("ninfer.draft_tokens", SettingValue::UnsignedInteger(7)),
        ("ninfer.lm_head_draft", SettingValue::Toggle(false)),
        ("ninfer.vision", SettingValue::Toggle(true)),
    ]);
    let (p, mut s) = fixture(Some(&r));
    s["engine"]["speculative_backend"] = json!("dflash2");
    s["engine"]["speculative_draft_window"] = json!(7);
    s["engine"]["proposal_head"] = json!("full");
    s["engine"]["vision"] = json!(true);
    v3::validate_startup(&p, &s).unwrap();
    s["engine"]["proposal_head"] = json!("auto");
    assert!(v3::validate_startup(&p, &s).is_err());
}
#[test]
fn schema_filters_extensions_without_borrowing_old_defaults() {
    let help = "--max-context --kv-dtype bf16 int8 fp8 nvfp4 k8v4 rk8v4 rk4v4 rk4v4-e8 rk2v4-e8 --rope-yarn --rope-yarn-factor --gdn-state-fp16";
    for schema in [20, 21, 25, 28] {
        let mut c = capabilities();
        c.request_log_schema = Some(schema);
        let mut defs = settings::definitions();
        apply_ninfer_runtime_contract(&mut defs, help, c);
        for id in [
            "ninfer.rope_yarn",
            "ninfer.rope_yarn_factor",
            "ninfer.gdn_state_fp16",
        ] {
            let def = defs.iter().find(|d| d.id.as_str() == id).unwrap();
            assert_eq!(def.supported, schema == 28);
            assert!(def.default_preview.is_none());
        }
        let def = defs
            .iter()
            .find(|d| d.id.as_str() == "ninfer.kv_dtype")
            .unwrap();
        let scala_core::SettingKind::Choice { choices } = &def.kind else {
            panic!()
        };
        assert_eq!(choices.contains(&"rk4v4".into()), schema == 28);
    }
}
#[test]
fn partial_source_evidence_cannot_grant_serving_capabilities() {
    let root = tempfile::tempdir().unwrap();
    assert!(!source_matches(root.path()));
    assert!(owners::FILES.len() > 50);
    assert!(
        owners::FILES
            .iter()
            .any(|(p, _)| *p == "src/product/rope_yarn_options.h")
    );
}

fn request() -> scala_engine::InferenceRequest {
    scala_engine::InferenceRequest {
        api: None,
        model_profile_id: ModelProfileId::new("fixture").unwrap(),
        messages: vec![scala_engine::InferenceMessage::text(
            scala_engine::InferenceRole::User,
            "Explain this code",
        )],
        generation_settings: Default::default(),
        tools: vec![],
        tool_choice: None,
        parallel_tool_calls: None,
        output_format: None,
        max_output_tokens: None,
        stream: false,
    }
}
fn execution() -> NinferExecution {
    NinferExecution {
        capabilities: capabilities(),
        vision: true,
        greedy: false,
    }
}
#[test]
fn native_requests_use_qualified_contracts_and_only_explicit_request_overrides() {
    let mut r = request();
    let admission = execution().request_admission();
    let inherited = protocol::backend_request(&r, false, admission).unwrap();
    for key in [
        "temperature",
        "max_tokens",
        "rope_yarn",
        "max_context",
        "kv_dtype",
    ] {
        assert!(inherited.get(key).is_none());
    }
    r.generation_settings.temperature = Some(0.3);
    r.generation_settings.top_p = Some(0.8);
    r.generation_settings.reasoning_enabled = Some(true);
    r.max_output_tokens = Some(2048);
    for stream in [false, true] {
        let body = protocol::backend_request(&r, stream, admission).unwrap();
        assert_eq!(body["model"], json!("fixture"));
        assert_eq!(body["temperature"], json!(0.3));
        assert_eq!(body["max_tokens"], json!(2048));
        assert_eq!(body["enable_thinking"], json!(true));
        assert_eq!(body["stream"], json!(stream));
    }
    r.tools=serde_json::from_value(json!([{"name":"inspect","description":"Read code","parameters":{"type":"object"},"strict":false}])).unwrap();
    assert_eq!(
        protocol::backend_request(&r, false, admission).unwrap()["tools"][0]["function"]["name"],
        json!("inspect")
    );
    r.tools[0].strict = Some(true);
    assert!(protocol::backend_request(&r, false, admission).is_err()); // no schema-25 constraint proof is borrowed
    let unqualified = NinferExecution {
        capabilities: NinferRuntimeCapabilities::default(),
        ..execution()
    };
    assert!(protocol::backend_request(&request(), false, unqualified.request_admission()).is_err());
}
async fn response(status: &str, content_type: &str, body: String) -> reqwest::Response {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0u8; 4096];
        socket.read(&mut buffer).await.unwrap();
        socket.write_all(reply.as_bytes()).await.unwrap();
        socket.shutdown().await.unwrap();
    });
    let reply = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!("http://{address}/fixture"))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();
    task.await.unwrap();
    reply
}
#[tokio::test]
async fn schema28_http_completion_translates_tools_usage_and_native_errors() {
    let body=json!({"object":"chat.completion","choices":[{"message":{"content":null,"reasoning_content":"private","tool_calls":[{"id":"call1","type":"function","function":{"name":"inspect","arguments":"{}"}}]},"finish_reason":"tool_calls"}],
        "usage":{"prompt_tokens":500000,"completion_tokens":10,"total_tokens":500010,"prompt_tokens_details":{"cached_tokens":490000}},"timings":{"predicted_per_second":1}}).to_string();
    let reply = response("200 OK", "application/json", body).await;
    let result =
        protocol::parse_completion_response(reply, execution().request_admission().constrained)
            .await
            .unwrap();
    assert_eq!(result.tool_calls[0].name, "inspect");
    assert_eq!(result.usage.unwrap().cached_input_tokens, Some(490000));
    let bad=response("400 Bad Request","application/json",json!({"error":{"message":"fixture native error","type":"invalid_request_error","param":"messages","code":"invalid_request"}}).to_string()).await;
    assert!(
        protocol::parse_completion_response(bad, false)
            .await
            .unwrap_err()
            .to_string()
            .contains("fixture native error")
    );
}
#[tokio::test]
async fn schema28_http_stream_preserves_reasoning_and_terminal_usage() {
    use futures_util::StreamExt;
    let mut body = String::new();
    for frame in [
        json!({"choices":[{"delta":{"reasoning_content":"check"},"finish_reason":null}]}),
        json!({"choices":[{"delta":{"content":"answer"},"finish_reason":null}]}),
        json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
        json!({"choices":[],"usage":{"prompt_tokens":500000,"completion_tokens":2,"total_tokens":500002}}),
    ] {
        body.push_str(&format!("data: {frame}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    let reply = response("200 OK", "text/event-stream", body).await;
    let stream =
        protocol::parse_stream_response(reply, execution().request_admission().constrained)
            .await
            .unwrap();
    let events = stream.collect::<Vec<_>>().await;
    assert!(events.iter().all(Result::is_ok));
    assert!(events.iter().any(
        |e| matches!(e,Ok(scala_engine::InferenceEvent::ReasoningDelta{delta}) if delta=="check")
    ));
    assert!(events.iter().any(|e| matches!(e,Ok(scala_engine::InferenceEvent::Completed{usage:Some(u),..}) if u.input_tokens==500000)));
}
