//! Qualified upstream OpenAI protocols. Templates and sampling stay in vLLM.
use super::*;
use bytes::Bytes;
use futures_util::{
    StreamExt,
    stream::{self, BoxStream},
};
use std::collections::VecDeque;

pub(super) fn bounded_json(path: &Path) -> Option<Value> {
    use std::io::Read;
    let mut bytes = vec![];
    std::fs::File::open(path)
        .ok()?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 1024 * 1024 {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}
pub(super) fn snapshot(
    model: &ModelArtifact,
) -> Result<scala_core::HuggingFaceSnapshot, EngineError> {
    if model.format != ArtifactFormat::HuggingFace {
        return Err(invalid("Expected original Hugging Face snapshot"));
    }
    let snapshot = scala_core::HuggingFaceSnapshot::read(&model.path).map_err(invalid)?;
    if !matches!(
        snapshot.architecture.as_str(),
        "LlamaForCausalLM" | "Qwen2ForCausalLM"
    ) {
        return Err(EngineError::Unsupported(
            "This vLLM version has no Scala-qualified text serving contract for that architecture"
                .into(),
        ));
    }
    let cfg =
        bounded_json(&model.path).ok_or_else(|| invalid("Invalid bounded Hugging Face config"))?;
    if cfg.get("auto_map").is_some()
        || snapshot
            .files
            .keys()
            .any(|n| n.ends_with(".py") || n.ends_with(".bin"))
    {
        return Err(EngineError::Unsupported(
            "Custom model code and pickle checkpoints are not qualified for vLLM".into(),
        ));
    }
    let tokenizer = bounded_json(&snapshot.root.join("tokenizer_config.json"))
        .ok_or_else(|| invalid("Missing original model-owned tokenizer configuration"))?;
    if !snapshot.files.contains_key("tokenizer.json")
        || tokenizer.get("auto_map").is_some()
        || tokenizer
            .get("chat_template")
            .and_then(Value::as_str)
            .is_none_or(|s| s.trim().is_empty())
            && !snapshot.files.contains_key("chat_template.jinja")
    {
        return Err(EngineError::Unsupported(
            "vLLM Chat requires a qualified original tokenizer and model-owned template".into(),
        ));
    }
    if cfg.get("quantization_config").is_some() {
        return Err(EngineError::Unsupported(
            "Quantized HF checkpoint tuple is not qualified".into(),
        ));
    }
    Ok(snapshot)
}
pub(super) async fn fingerprint(model: &ModelArtifact) -> Result<String, EngineError> {
    let model = model.clone();
    tokio::task::spawn_blocking(move || snapshot(&model)?.fingerprint().map_err(invalid))
        .await
        .map_err(|e| invalid(e.to_string()))?
}
pub(super) fn validate_settings(s: &GenerationSettingsPatch) -> Result<(), EngineError> {
    if s.reasoning_enabled.is_some() || s.reasoning_budget.is_some() || s.reasoning_effort.is_some()
    {
        return Err(EngineError::InvalidGenerationSettings(
            "This tuple has no qualified reasoning controls".into(),
        ));
    }
    let valid = |v: Option<f64>, min: f64, max: f64, exclusive: bool| {
        v.is_none_or(|v| {
            v.is_finite()
                && if exclusive {
                    v > min && v <= max
                } else {
                    v >= min && v <= max
                }
        })
    };
    if !valid(s.temperature, 0.0, 2.0, false)
        || !valid(s.top_p, 0.0, 1.0, true)
        || !valid(s.min_p, 0.0, 1.0, false)
        || !valid(s.repeat_penalty, 0.0, f64::MAX, true)
        || !valid(s.presence_penalty, -2.0, 2.0, false)
        || !valid(s.frequency_penalty, -2.0, 2.0, false)
        || s.seed.is_some_and(|n| n > i64::MAX as u64)
        || s.top_k.is_some_and(|n| n > i64::MAX as u64)
        || s.stop
            .as_ref()
            .is_some_and(|s| s.iter().any(String::is_empty))
    {
        return Err(EngineError::InvalidGenerationSettings(
            "Invalid upstream vLLM sampling value".into(),
        ));
    }
    Ok(())
}
fn body(
    request: &InferenceRequest,
    proof: &Proof,
    defaults: &Value,
    stream: bool,
) -> Result<(String, Value), EngineError> {
    if !proof.ready
        || proof.backend != "vllm-openai"
        || proof.profile.as_ref() != Some(&request.model_profile_id)
    {
        return Err(EngineError::Unsupported(
            "No qualified vLLM text-generation launch for this profile".into(),
        ));
    }
    validate_settings(&request.generation_settings)?;
    if !request.tools.is_empty()
        || request.tool_choice.is_some()
        || request.parallel_tool_calls.is_some()
        || request
            .output_format
            .as_ref()
            .is_some_and(|f| !matches!(f, OutputFormat::Text))
    {
        return Err(EngineError::Unsupported(
            "Tools and structured output require a separate tuple qualification".into(),
        ));
    }
    let responses = match request.api {
        None | Some(ApiCapability::ChatCompletions) => false,
        Some(ApiCapability::Responses) => true,
        _ => {
            return Err(EngineError::Unsupported(
                "Unqualified API for vLLM text serving".into(),
            ));
        }
    };
    if responses && request.generation_settings.min_p.is_some() {
        return Err(EngineError::InvalidGenerationSettings(
            "vLLM Responses does not support min_p".into(),
        ));
    }
    let messages=request.messages.iter().map(|m| {
        if !m.tool_calls.is_empty() || m.tool_call_id.is_some() || m.role == InferenceRole::Tool { return Err(EngineError::Unsupported("Tool messages".into())); }
        let text=m.text_only().ok_or_else(||EngineError::Unsupported("This text tuple does not qualify media input".into()))?;
        if responses && m.name.is_some() {return Err(EngineError::MessageNamesUnsupported);}
        let mut value=json!({"role":m.role,"content":if responses {json!([{"type":"input_text","text":text}])} else {json!(text)}});
        if let Some(name)=&m.name {value["name"]=json!(name);}
        Ok(value)
    }).collect::<Result<Vec<_>,EngineError>>()?;
    let mut value = json!({"model":format!("scala-{}",proof.nonce),"stream":stream});
    if responses {
        value["input"] = json!(messages);
        value["store"] = json!(false);
        value["background"] = json!(false);
    } else {
        value["messages"] = json!(messages);
        if stream {
            value["stream_options"] = json!({"include_usage":true});
        }
    }
    if let Some(n) = request.max_output_tokens {
        value[if responses {
            "max_output_tokens"
        } else {
            "max_tokens"
        }] = json!(n);
    }
    let g = &request.generation_settings;
    for (name, v) in [
        ("temperature", g.temperature),
        ("top_p", g.top_p),
        ("min_p", g.min_p),
        ("repetition_penalty", g.repeat_penalty),
        ("presence_penalty", g.presence_penalty),
        ("frequency_penalty", g.frequency_penalty),
    ] {
        if let Some(v) = v {
            value[name] = json!(v);
        }
    }
    if let Some(n) = g.top_k {
        value["top_k"] = json!(n);
    }
    if let Some(n) = g.seed {
        value["seed"] = json!(n);
    }
    if let Some(v) = &g.stop {
        value["stop"] = json!(v);
    }
    let names = [
        ("temperature", "temperature"),
        ("top_p", "top_p"),
        ("top_k", "top_k"),
        ("min_p", "min_p"),
        ("seed", "seed"),
        ("repeat_penalty", "repetition_penalty"),
        ("presence_penalty", "presence_penalty"),
        ("frequency_penalty", "frequency_penalty"),
        ("stop_strings", "stop"),
        (
            "max_output_tokens",
            if responses {
                "max_output_tokens"
            } else {
                "max_tokens"
            },
        ),
    ];
    for (setting, field) in names {
        if value.get(field).is_none()
            && let Some(default) = defaults.get(setting)
        {
            if setting == "seed" {
                if let Some(n) = default.get("value").and_then(Value::as_u64) {
                    value[field] = json!(n);
                }
            } else {
                value[field] = default.clone();
            }
        }
    }
    if responses && value.get("min_p").is_some() {
        return Err(EngineError::InvalidGenerationSettings(
            "vLLM Responses does not support min_p; use Chat Completions or Inherit".into(),
        ));
    }
    Ok((
        (if responses {
            "/v1/responses"
        } else {
            "/v1/chat/completions"
        })
        .into(),
        value,
    ))
}
fn usage(v: Option<&Value>, responses: bool) -> Result<Option<InferenceUsage>, EngineError> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let counter = |name| {
        v.get(name)
            .and_then(Value::as_u64)
            .ok_or_else(|| EngineError::Operation("Malformed upstream usage".into()))
    };
    let input = counter(if responses {
        "input_tokens"
    } else {
        "prompt_tokens"
    })?;
    let output = counter(if responses {
        "output_tokens"
    } else {
        "completion_tokens"
    })?;
    let total = counter("total_tokens")?;
    if input.checked_add(output) != Some(total) {
        return Err(EngineError::Operation("Inconsistent upstream usage".into()));
    }
    Ok(Some(InferenceUsage {
        input_tokens: input,
        output_tokens: output,
        total_tokens: total,
        ..Default::default()
    }))
}
fn finish(v: &Value, responses: bool) -> Result<InferenceFinishReason, EngineError> {
    let code = if responses {
        v["status"].as_str()
    } else {
        v.as_str()
    };
    match code {
        Some("stop" | "completed") => Ok(InferenceFinishReason::Stop),
        Some("length") => Ok(InferenceFinishReason::MaxOutputTokens),
        Some("incomplete") if v["incomplete_details"]["reason"] == "max_output_tokens" => {
            Ok(InferenceFinishReason::MaxOutputTokens)
        }
        _ => Err(EngineError::Operation(
            "Unqualified upstream finish reason".into(),
        )),
    }
}
fn output(v: &Value, responses: bool) -> Result<InferenceOutput, EngineError> {
    let (text, finish_reason) = if responses {
        let mut text = String::new();
        for item in v["output"]
            .as_array()
            .ok_or_else(|| invalid("Missing native Responses output"))?
        {
            if item["type"] != "message" || item["role"] != "assistant" {
                return Err(EngineError::Operation(
                    "Unqualified native Responses output item".into(),
                ));
            }
            for part in item["content"]
                .as_array()
                .ok_or_else(|| invalid("Missing native output content"))?
            {
                if part["type"] != "output_text" {
                    return Err(EngineError::Operation(
                        "Unqualified native output content".into(),
                    ));
                }
                text.push_str(
                    part["text"]
                        .as_str()
                        .ok_or_else(|| invalid("Missing native text"))?,
                );
            }
        }
        (text, finish(v, true)?)
    } else {
        let choices = v["choices"]
            .as_array()
            .filter(|a| a.len() == 1)
            .ok_or_else(|| invalid("Expected exactly one native choice"))?;
        if choices[0]["message"]
            .get("tool_calls")
            .is_some_and(|v| !v.is_null() && v != &json!([]))
        {
            return Err(EngineError::Operation(
                "Unqualified upstream tool calls".into(),
            ));
        }
        (
            choices[0]["message"]["content"]
                .as_str()
                .ok_or_else(|| invalid("Missing native text"))?
                .into(),
            finish(&choices[0]["finish_reason"], false)?,
        )
    };
    Ok(InferenceOutput {
        text,
        tool_calls: vec![],
        usage: usage(v.get("usage"), responses)?,
        finish_reason,
    })
}
impl VllmAdapter {
    pub(super) async fn qualify_generation(
        &self,
        endpoint: &str,
        proof: &Proof,
    ) -> Result<bool, EngineError> {
        let health = self
            .client
            .get(format!("{endpoint}/health"))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .map_err(|e| EngineError::BackendUnavailable(e.to_string()))?;
        if !health.status().is_success() {
            return Ok(false);
        }
        let (status, models) = self
            .read(
                self.client
                    .get(format!("{endpoint}/v1/models"))
                    .timeout(Duration::from_secs(2)),
            )
            .await?;
        if !status.is_success()
            || models["data"]
                .as_array()
                .is_none_or(|m| m.len() != 1 || m[0]["id"] != format!("scala-{}", proof.nonce))
        {
            return Err(invalid("vLLM served identity differs from launch"));
        }
        let (status, api) = self
            .read(
                self.client
                    .get(format!("{endpoint}/openapi.json"))
                    .timeout(Duration::from_secs(2)),
            )
            .await?;
        if !status.is_success()
            || ["/v1/chat/completions", "/v1/responses"]
                .iter()
                .any(|path| !api["paths"][path]["post"].is_object())
        {
            return Err(invalid("vLLM upstream APIs failed startup qualification"));
        }
        let mut proofs = self.proofs.write().unwrap();
        let current = proofs
            .get_mut(endpoint)
            .ok_or_else(|| invalid("Launch invalidated"))?;
        if current.nonce != proof.nonce {
            return Err(invalid("Launch changed during qualification"));
        }
        current.ready = true;
        Ok(true)
    }
    pub(super) async fn generate(
        &self,
        endpoint: &str,
        request: InferenceRequest,
    ) -> Result<InferenceOutput, EngineError> {
        let (path, body) = body(
            &request,
            &self.proof(endpoint)?,
            &self
                .generation_overrides
                .read()
                .unwrap()
                .get(endpoint)
                .cloned()
                .unwrap_or(json!({})),
            false,
        )?;
        let (status, value) = self
            .read(self.client.post(format!("{endpoint}{path}")).json(&body))
            .await?;
        if !status.is_success() {
            return Err(if matches!(status.as_u16(), 400 | 422) {
                EngineError::NativeRequest {message:"vLLM rejected the request; check the model-owned template, context limit and supported controls".into(),parameter:if path=="/v1/responses" {"input"} else {"messages"}.into(),code:"invalid_value"}
            } else {
                EngineError::BackendUnavailable(format!("vLLM HTTP {status}"))
            });
        }
        output(&value, path == "/v1/responses")
    }
    pub(super) async fn generate_stream(
        &self,
        endpoint: &str,
        request: InferenceRequest,
        _activity: InferenceActivityReporter,
    ) -> Result<InferenceStream, EngineError> {
        let (path, body) = body(
            &request,
            &self.proof(endpoint)?,
            &self
                .generation_overrides
                .read()
                .unwrap()
                .get(endpoint)
                .cloned()
                .unwrap_or(json!({})),
            true,
        )?;
        let response = self
            .client
            .post(format!("{endpoint}{path}"))
            .json(&body)
            .send()
            .await
            .map_err(|e| EngineError::BackendUnavailable(e.to_string()))?;
        if !response.status().is_success() {
            return Err(if matches!(response.status().as_u16(), 400 | 422) {
                EngineError::NativeRequest {message:"vLLM rejected the request; check the model-owned template, context limit and supported controls".into(),parameter:if path=="/v1/responses" {"input"} else {"messages"}.into(),code:"invalid_value"}
            } else {
                EngineError::BackendUnavailable(format!("vLLM HTTP {}", response.status()))
            });
        }
        Ok(sse(
            response.bytes_stream().boxed(),
            path == "/v1/responses",
        ))
    }
}
struct Sse {
    source: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    buffer: Vec<u8>,
    queued: VecDeque<Result<InferenceEvent, EngineError>>,
    responses: bool,
    usage: Option<InferenceUsage>,
    finish: Option<InferenceFinishReason>,
    done: bool,
}
fn sse(
    source: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    responses: bool,
) -> InferenceStream {
    Box::pin(stream::unfold(
        Sse {
            source,
            buffer: vec![],
            queued: VecDeque::new(),
            responses,
            usage: None,
            finish: None,
            done: false,
        },
        |mut state| async move {
            loop {
                if let Some(event) = state.queued.pop_front() {
                    return Some((event, state));
                }
                if state.done {
                    return None;
                }
                let result = match state.source.next().await {
                    Some(Ok(chunk)) => {
                        state.buffer.extend_from_slice(&chunk);
                        parse_frames(&mut state)
                    }
                    Some(Err(e)) => Err(EngineError::BackendUnavailable(e.to_string())),
                    None => Err(EngineError::BackendUnavailable(
                        "vLLM stream ended before completion".into(),
                    )),
                };
                if let Err(e) = result {
                    state.done = true;
                    state.queued.push_back(Err(e));
                }
            }
        },
    ))
}
fn parse_frames(s: &mut Sse) -> Result<(), EngineError> {
    while let Some((end, delimiter)) = s
        .buffer
        .windows(2)
        .position(|v| v == b"\n\n")
        .map(|n| (n, 2))
        .or_else(|| {
            s.buffer
                .windows(4)
                .position(|v| v == b"\r\n\r\n")
                .map(|n| (n, 4))
        })
    {
        if end > 1024 * 1024 {
            return Err(invalid("vLLM SSE frame exceeds bound"));
        }
        let frame = s.buffer.drain(..end + delimiter).collect::<Vec<_>>();
        let frame = std::str::from_utf8(&frame).map_err(|e| invalid(e.to_string()))?;
        let data = frame
            .lines()
            .filter_map(|l| l.strip_prefix("data:").map(str::trim))
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            if s.responses {
                return Err(invalid("Responses stream lacks native completion event"));
            }
            let finish_reason = s
                .finish
                .ok_or_else(|| invalid("Missing native stream finish reason"))?;
            s.queued.push_back(Ok(InferenceEvent::Completed {
                usage: s.usage.clone(),
                finish_reason,
            }));
            s.done = true;
            break;
        }
        let value: Value = serde_json::from_str(&data).map_err(|e| invalid(e.to_string()))?;
        if value.get("error").is_some() {
            return Err(EngineError::Operation("Upstream stream error".into()));
        }
        if s.responses {
            match value["type"].as_str() {
                Some("response.output_text.delta") => {
                    s.queued.push_back(Ok(InferenceEvent::TextDelta {
                        delta: value["delta"]
                            .as_str()
                            .ok_or_else(|| invalid("Invalid Responses text delta"))?
                            .into(),
                    }))
                }
                Some("response.completed" | "response.incomplete") => {
                    let output = output(&value["response"], true)?;
                    s.queued.push_back(Ok(InferenceEvent::Completed {
                        usage: output.usage,
                        finish_reason: output.finish_reason,
                    }));
                    s.done = true;
                    break;
                }
                Some(
                    "response.created"
                    | "response.in_progress"
                    | "response.content_part.added"
                    | "response.content_part.done"
                    | "response.output_text.done",
                ) => {}
                Some("response.output_item.added" | "response.output_item.done")
                    if value["item"]["type"] == "message" => {}
                _ => return Err(invalid("Unqualified native Responses event")),
            }
        } else {
            if let Some(usage) = usage(value.get("usage"), false)? {
                s.usage = Some(usage);
            }
            for choice in value["choices"]
                .as_array()
                .ok_or_else(|| invalid("Missing native stream choices"))?
            {
                if choice["index"] != 0 {
                    return Err(invalid("Unexpected native choice index"));
                }
                if choice["delta"]
                    .get("tool_calls")
                    .is_some_and(|v| !v.is_null())
                {
                    return Err(invalid("Unqualified stream tool calls"));
                }
                if let Some(delta) = choice["delta"]["content"].as_str() {
                    s.queued.push_back(Ok(InferenceEvent::TextDelta {
                        delta: delta.into(),
                    }));
                }
                if !choice["finish_reason"].is_null() {
                    s.finish = Some(finish(&choice["finish_reason"], false)?);
                }
            }
        }
    }
    if s.buffer.len() > 1024 * 1024 {
        return Err(invalid("vLLM SSE frame exceeds bound"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        routing::{get, post},
    };
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    fn proof() -> Proof {
        Proof {
            tuple: "tuple".into(),
            nonce: "nonce".into(),
            revision: "revision".into(),
            backend: "vllm-openai".into(),
            bundle_sha256: "digest".into(),
            profile: Some(ModelProfileId::new("text").unwrap()),
            ready: true,
        }
    }
    fn request(api: Option<ApiCapability>) -> InferenceRequest {
        InferenceRequest {
            api,
            model_profile_id: ModelProfileId::new("text").unwrap(),
            messages: vec![InferenceMessage::text(InferenceRole::User, "hello")],
            generation_settings: GenerationSettingsPatch::default(),
            tools: vec![],
            tool_choice: None,
            parallel_tool_calls: None,
            output_format: None,
            max_output_tokens: None,
            stream: false,
        }
    }
    fn native_response() -> Value {
        json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}],"usage":{"input_tokens":2,"output_tokens":1,"total_tokens":3}})
    }
    #[test]
    fn protocol_omission_defaults_and_overrides_preserve_native_shapes() {
        let (path, v) = body(&request(None), &proof(), &json!({}), true).unwrap();
        assert_eq!(path, "/v1/chat/completions");
        assert_eq!(v["messages"], json!([{"role":"user","content":"hello"}]));
        assert!(v.get("temperature").is_none());
        assert!(v.get("max_tokens").is_none());
        let mut r = request(Some(ApiCapability::Responses));
        r.generation_settings.temperature = Some(0.0);
        r.max_output_tokens = Some(7);
        let (path, v) = body(
            &r,
            &proof(),
            &json!({"temperature":1.0,"top_p":0.8,"max_output_tokens":99}),
            false,
        )
        .unwrap();
        assert_eq!(path, "/v1/responses");
        assert_eq!(v["temperature"], 0.0);
        assert_eq!(v["top_p"], 0.8);
        assert_eq!(v["max_output_tokens"], 7);
        assert_eq!(v["store"], false);
        assert_eq!(v["background"], false);
        assert_eq!(
            v["input"],
            json!([{"role":"user","content":[{"type":"input_text","text":"hello"}]}])
        );
        assert!(body(&r, &proof(), &json!({"min_p":0.1}), false).is_err());
        r.messages[0].name = Some("participant".into());
        assert!(matches!(
            body(&r, &proof(), &json!({}), false),
            Err(EngineError::MessageNamesUnsupported)
        ));
        assert!(output(&native_response(), true).unwrap().usage.is_some());
        let mut bad = native_response();
        bad["usage"]["total_tokens"] = json!(9);
        assert!(output(&bad, true).is_err());
        bad = native_response();
        bad.as_object_mut().unwrap().remove("usage");
        assert!(output(&bad, true).unwrap().usage.is_none());
    }
    #[test]
    fn unqualified_controls_and_finish_reasons_fail_closed() {
        for patch in [
            GenerationSettingsPatch {
                temperature: Some(2.1),
                ..Default::default()
            },
            GenerationSettingsPatch {
                top_p: Some(0.0),
                ..Default::default()
            },
            GenerationSettingsPatch {
                reasoning_enabled: Some(true),
                ..Default::default()
            },
            GenerationSettingsPatch {
                repeat_penalty: Some(0.0),
                ..Default::default()
            },
        ] {
            assert!(validate_settings(&patch).is_err());
        }
        assert!(finish(&json!("tool_calls"), false).is_err());
        let mut p = proof();
        p.ready = false;
        assert!(body(&request(None), &p, &json!({}), false).is_err());
        p = proof();
        p.backend = "vllm-labels".into();
        assert!(body(&request(None), &p, &json!({}), false).is_err());
    }
    #[tokio::test]
    async fn streams_translate_fragmented_native_chat_and_responses_without_fabricated_usage() {
        let frames = format!(
            "data: {}\r\n\r\ndata: {}\r\n\r\ndata: [DONE]\r\n\r\n",
            json!({"choices":[{"index":0,"delta":{"content":"hé"},"finish_reason":null}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
        );
        let source = stream::iter(
            frames
                .as_bytes()
                .chunks(3)
                .map(|c| Ok(Bytes::copy_from_slice(c)))
                .collect::<Vec<_>>(),
        )
        .boxed();
        let events = sse(source, false).collect::<Vec<_>>().await;
        assert!(matches!(&events[0],Ok(InferenceEvent::TextDelta{delta}) if delta=="hé"));
        assert!(matches!(
            &events[1],
            Ok(InferenceEvent::Completed {
                usage: None,
                finish_reason: InferenceFinishReason::Stop
            })
        ));
        let frames = format!(
            "event: response.output_text.delta\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
            json!({"type":"response.output_text.delta","delta":"hello"}),
            json!({"type":"response.completed","response":native_response()})
        );
        let events = sse(stream::iter(vec![Ok(Bytes::from(frames))]).boxed(), true)
            .collect::<Vec<_>>()
            .await;
        assert_eq!(events.len(), 2);
        assert!(
            matches!(&events[1],Ok(InferenceEvent::Completed{usage:Some(u),..}) if u.total_tokens==3)
        );
        assert!(
            sse(stream::empty().boxed(), false)
                .next()
                .await
                .unwrap()
                .is_err()
        );
    }
    struct OwnedSource(Arc<AtomicBool>);
    impl futures_util::Stream for OwnedSource {
        type Item = Result<Bytes, reqwest::Error>;
        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Pending
        }
    }
    impl Drop for OwnedSource {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    #[test]
    fn dropping_translation_cancels_owned_upstream_stream() {
        let dropped = Arc::new(AtomicBool::new(false));
        let stream = sse(Box::pin(OwnedSource(dropped.clone())), false);
        drop(stream);
        assert!(dropped.load(Ordering::SeqCst));
    }
    #[tokio::test]
    async fn fake_upstream_qualifies_native_apis_and_routes_both_surfaces() {
        let calls = Arc::new(Mutex::new(vec![]));
        async fn chat(
            State(c): State<Arc<Mutex<Vec<Value>>>>,
            Json(v): Json<Value>,
        ) -> Json<Value> {
            c.lock().unwrap().push(v);
            Json(json!({"choices":[{"message":{"content":"hello"},"finish_reason":"stop"}]}))
        }
        async fn responses(
            State(c): State<Arc<Mutex<Vec<Value>>>>,
            Json(v): Json<Value>,
        ) -> Json<Value> {
            c.lock().unwrap().push(v);
            Json(native_response())
        }
        let router=Router::new().route("/health",get(||async {""})).route("/v1/models",get(||async {Json(json!({"data":[{"id":"scala-nonce"}]}))})).route("/openapi.json",get(||async{Json(json!({"paths":{"/v1/chat/completions":{"post":{}},"/v1/responses":{"post":{}}}}))})).route("/v1/chat/completions",post(chat)).route("/v1/responses",post(responses)).with_state(calls.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let adapter = VllmAdapter::from_config(None, Path::new("."));
        let mut p = proof();
        p.ready = false;
        adapter
            .proofs
            .write()
            .unwrap()
            .insert(endpoint.clone(), p.clone());
        assert!(adapter.qualify_generation(&endpoint, &p).await.unwrap());
        adapter
            .generation_overrides
            .write()
            .unwrap()
            .insert(endpoint.clone(), json!({"temperature":0.7}));
        assert_eq!(
            adapter
                .generate(&endpoint, request(None))
                .await
                .unwrap()
                .text,
            "hello"
        );
        assert_eq!(
            adapter
                .generate(&endpoint, request(Some(ApiCapability::Responses)))
                .await
                .unwrap()
                .usage
                .unwrap()
                .input_tokens,
            2
        );
        assert_eq!(calls.lock().unwrap()[0]["temperature"], 0.7);
        assert!(calls.lock().unwrap()[0].get("messages").is_some());
        assert!(calls.lock().unwrap()[1].get("input").is_some());
        adapter.clear_launch_state(Some(&endpoint)).await;
        assert!(adapter.generate(&endpoint, request(None)).await.is_err());
        task.abort();
    }
    #[tokio::test]
    async fn original_hf_checkpoint_uses_ordinary_inventory_and_integrity() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (name, bytes) in [
            (
                "config.json",
                r#"{"architectures":["LlamaForCausalLM"],"max_position_embeddings":2048}"#,
            ),
            (
                "tokenizer_config.json",
                r#"{"chat_template":"{{ messages }}"}"#,
            ),
            ("tokenizer.json", "{}"),
            ("generation_config.json", r#"{"temperature":0.5}"#),
            ("model.safetensors", "synthetic weights"),
        ] {
            std::fs::write(root.join(name), bytes).unwrap();
        }
        let registry = scala_core::ModelRegistry::discover(&[root.into()]);
        assert_eq!(registry.artifacts().len(), 1);
        let model = &registry.artifacts()[0];
        assert_eq!(model.format, ArtifactFormat::HuggingFace);
        let adapter = VllmAdapter::from_config(None, root);
        assert!(adapter.supports_model_capability(model, ApiCapability::Responses));
        assert!(!adapter.supports_model_capability(model, ApiCapability::Decision));
        let prepared = adapter.prepare_model_input(model).await.unwrap();
        assert_eq!(prepared.primary.path, model.path);
        std::fs::write(root.join("model.safetensors"), "different weights").unwrap();
        assert_ne!(
            prepared.primary.hash.unwrap(),
            fingerprint(model).await.unwrap()
        );
        std::fs::write(root.join("custom.py"), "unqualified").unwrap();
        assert!(snapshot(model).is_err());
    }
}
