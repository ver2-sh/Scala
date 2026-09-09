//! Reviewed chat wire contract. Observations are process-local, never settings.
use super::*;

const REVIEWED_TREE: &str = "ef599001012ff8bee837a832decde4c564702cc4";
const RESPONSE_LIMIT: usize = 16 * 1024 * 1024;

fn invalid(detail: impl std::fmt::Display) -> EngineError {
    EngineError::Operation(format!("invalid llama.cpp chat response: {detail}"))
}

pub(super) fn request_fields(body: &mut Value, request: &InferenceRequest) {
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(request.tools.iter().filter(|tool| {
            !matches!(&request.tool_choice, Some(InferenceToolChoice::Function { name }) if name != &tool.name)
        }).map(|tool| {
            let mut function = json!({"name": tool.name, "parameters": tool.parameters});
            if let Some(description) = &tool.description { function["description"] = json!(description); }
            json!({"type": "function", "function": function})
        }).collect());
    }
    if let Some(choice) = &request.tool_choice {
        // This revision accepts strings only. Narrowing the advertised functions
        // plus required is its native equivalent of a named function choice.
        body["tool_choice"] = json!(match choice {
            InferenceToolChoice::Auto => "auto",
            InferenceToolChoice::None => "none",
            InferenceToolChoice::Required | InferenceToolChoice::Function { .. } => "required",
        });
    }
    if let Some(parallel) = request.parallel_tool_calls {
        body["parallel_tool_calls"] = json!(parallel);
    }
}

pub(super) fn tool_call_json(call: &InferenceToolCall) -> Value {
    json!({"id": call.id, "type": "function", "function": {"name": call.name, "arguments": call.arguments}})
}

pub(super) fn validate_request(request: &InferenceRequest) -> Result<(), EngineError> {
    if let Some(InferenceToolChoice::Function { name }) = &request.tool_choice
        && request.tools.iter().filter(|t| &t.name == name).count() != 1
    {
        return Err(EngineError::InvalidGenerationSettings(
            "named tool choice must identify exactly one supplied function".into(),
        ));
    }
    if matches!(request.tool_choice, Some(InferenceToolChoice::Required))
        && request.tools.is_empty()
    {
        return Err(EngineError::InvalidGenerationSettings(
            "required tool choice needs tools".into(),
        ));
    }
    if !request.tools.is_empty()
        && !matches!(request.tool_choice, Some(InferenceToolChoice::None))
        && matches!(
            request.output_format,
            Some(OutputFormat::JsonObject | OutputFormat::JsonSchema { .. })
        )
    {
        return Err(EngineError::Unsupported("simultaneous active tools and response_format is not a reviewed llama.cpp contract; use a separate finalization turn".into()));
    }
    Ok(())
}

#[derive(Deserialize)]
pub(super) struct WireToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: WireFunction,
}
#[derive(Deserialize)]
struct WireFunction {
    name: String,
    arguments: String,
}

pub(super) fn parse_calls(calls: Vec<WireToolCall>) -> Result<Vec<InferenceToolCall>, EngineError> {
    let mut ids = BTreeSet::new();
    calls
        .into_iter()
        .map(|call| {
            if call.kind != "function"
                || call.id.is_empty()
                || call.function.name.is_empty()
                || !ids.insert(call.id.clone())
            {
                return Err(invalid(
                    "tool calls require unique nonempty IDs, function type and name",
                ));
            }
            let args: Value = serde_json::from_str(&call.function.arguments).map_err(invalid)?;
            if !args.is_object() {
                return Err(invalid("function arguments must encode a JSON object"));
            }
            Ok(InferenceToolCall {
                id: call.id,
                name: call.function.name,
                arguments: call.function.arguments,
            })
        })
        .collect()
}

pub(super) fn validate_call_finish(
    calls: &[InferenceToolCall],
    reason: &InferenceFinishReason,
) -> Result<(), EngineError> {
    if (calls.is_empty() && *reason == InferenceFinishReason::ToolCalls)
        || (!calls.is_empty() && *reason != InferenceFinishReason::ToolCalls)
    {
        return Err(invalid("tool calls and terminal finish reason disagree"));
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct StreamCalls {
    calls: BTreeMap<u32, InferenceToolCall>,
    bytes: usize,
}
impl StreamCalls {
    pub(super) fn deltas(&mut self, value: &Value) -> Result<Vec<InferenceEvent>, EngineError> {
        let mut events = Vec::new();
        let Some(choices) = value.get("choices") else {
            return Ok(events);
        };
        let choices = choices
            .as_array()
            .ok_or_else(|| invalid("choices is not an array"))?;
        // Norted requests one assistant turn; multiple choices cannot share indexes.
        if choices.len() > 1 {
            return Err(invalid("multiple completion choices are unsupported"));
        }
        for choice in choices {
            let Some(calls) = choice.pointer("/delta/tool_calls") else {
                continue;
            };
            let calls = calls
                .as_array()
                .ok_or_else(|| invalid("delta.tool_calls is not an array"))?;
            for call in calls {
                let index = call
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|v| u32::try_from(v).ok())
                    .ok_or_else(|| invalid("tool delta omitted its unsigned index"))?;
                if let Some(kind) = call.get("type")
                    && kind.as_str() != Some("function")
                {
                    return Err(invalid("non-function tool delta"));
                }
                let optional_string =
                    |value: Option<&Value>| -> Result<Option<String>, EngineError> {
                        value
                            .map(|v| {
                                v.as_str()
                                    .map(str::to_owned)
                                    .ok_or_else(|| invalid("non-string tool delta field"))
                            })
                            .transpose()
                    };
                let id = optional_string(call.get("id"))?;
                let function = call
                    .get("function")
                    .ok_or_else(|| invalid("tool delta omitted function"))?;
                if !function.is_object() {
                    return Err(invalid("tool delta function is not an object"));
                }
                let name = optional_string(function.get("name"))?;
                let arguments_delta =
                    optional_string(function.get("arguments"))?.unwrap_or_default();
                self.bytes += id.as_ref().map_or(0, String::len)
                    + name.as_ref().map_or(0, String::len)
                    + arguments_delta.len();
                if self.bytes > RESPONSE_LIMIT
                    || self.calls.len() >= 4096 && !self.calls.contains_key(&index)
                {
                    return Err(invalid("tool stream exceeded local size limit"));
                }
                let accumulated = self
                    .calls
                    .entry(index)
                    .or_insert_with(|| InferenceToolCall {
                        id: String::new(),
                        name: String::new(),
                        arguments: String::new(),
                    });
                // ID and name are optional headers, not concatenated fragments in
                // Norted's ToolCallDelta contract. Arguments alone are appended.
                for (field, incoming) in
                    [(&mut accumulated.id, &id), (&mut accumulated.name, &name)]
                {
                    if let Some(incoming) = incoming {
                        if incoming.is_empty() || !field.is_empty() && field != incoming {
                            return Err(invalid("empty or conflicting tool delta header"));
                        }
                        field.clone_from(incoming);
                    }
                }
                accumulated.arguments.push_str(&arguments_delta);
                events.push(InferenceEvent::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments_delta,
                });
            }
        }
        Ok(events)
    }
    pub(super) fn finish(&self, reason: &InferenceFinishReason) -> Result<(), EngineError> {
        let calls = self
            .calls
            .values()
            .map(|c| WireToolCall {
                id: c.id.clone(),
                kind: "function".into(),
                function: WireFunction {
                    name: c.name.clone(),
                    arguments: c.arguments.clone(),
                },
            })
            .collect();
        let calls = parse_calls(calls)?;
        validate_call_finish(&calls, reason)
    }
}

pub(super) struct OutputConstraint(jsonschema::Validator);
impl OutputConstraint {
    pub(super) fn from_body(body: &Value) -> Result<Option<Self>, EngineError> {
        let schema = match body
            .pointer("/response_format/type")
            .and_then(Value::as_str)
        {
            Some("json_object") => json!({"type":"object"}),
            Some("json_schema") => body
                .pointer("/response_format/json_schema/schema")
                .cloned()
                .ok_or_else(|| invalid("missing response schema"))?,
            _ => return Ok(None),
        };
        // Network/file resolvers are disabled in Cargo. Unresolvable external
        // references fail before generation rather than fetching caller URLs.
        jsonschema::validator_for(&schema)
            .map(|v| Some(Self(v)))
            .map_err(|e| {
                EngineError::InvalidGenerationSettings(format!("invalid output schema: {e}"))
            })
    }
    pub(super) fn validate(
        &self,
        text: &str,
        calls: &[InferenceToolCall],
        reason: &InferenceFinishReason,
    ) -> Result<(), EngineError> {
        if !calls.is_empty() || *reason != InferenceFinishReason::Stop {
            return Err(invalid(
                "structured output did not finish with a complete text object",
            ));
        }
        let value: Value = serde_json::from_str(text).map_err(invalid)?;
        self.0
            .validate(&value)
            .map_err(|e| invalid(format!("output violates response schema: {e}")))
    }
}

pub(super) fn validate_stream(
    source: InferenceStream,
    constraint: Option<OutputConstraint>,
) -> InferenceStream {
    let Some(constraint) = constraint else {
        return source;
    };
    Box::pin(stream::unfold(
        (source, constraint, String::new(), false),
        |(mut source, constraint, mut text, done)| async move {
            if done {
                return None;
            }
            let event = source.next().await?;
            let mut done = false;
            let event = match event {
                Ok(InferenceEvent::TextDelta { delta }) => {
                    text.push_str(&delta);
                    if text.len() > RESPONSE_LIMIT {
                        done = true;
                        Err(invalid("structured output exceeded local size limit"))
                    } else {
                        Ok(InferenceEvent::TextDelta { delta })
                    }
                }
                Ok(InferenceEvent::Completed {
                    usage,
                    finish_reason,
                }) => {
                    done = true;
                    constraint.validate(&text, &[], &finish_reason).map(|()| {
                        InferenceEvent::Completed {
                            usage,
                            finish_reason,
                        }
                    })
                }
                Ok(InferenceEvent::ToolCallDelta { .. }) => {
                    done = true;
                    Err(invalid("tool call in structured output"))
                }
                Err(error) => {
                    done = true;
                    Err(error)
                }
            };
            Some((event, (source, constraint, text, done)))
        },
    ))
}

fn reviewed(runtime: &InstalledRuntime) -> bool {
    let m = &runtime.manifest;
    let i = &m.identity;
    let expected = norted_engine::managed_source_overlay_sha256(ENGINE_ID, &i.variant);
    m.validate().is_ok()
        && is_managed_llama_linux_cuda(i)
        && m.acquisition_method == RuntimeAcquisitionMethod::SourceBuild
        && i.upstream_revision.as_deref() == Some(norted_engine::LLAMA_TOKEN_STOP_REVISION)
        && matches!(
            i.variant.as_str(),
            "managed-portable-v4"
                | "managed-portable-cuda13-v2"
                | "managed-portable-exact-stop-v2"
                | "managed-portable-cuda13-exact-stop-v2"
        )
        && m.source_build.as_ref().is_some_and(|b| {
            b.source.commit_sha == norted_engine::LLAMA_TOKEN_STOP_REVISION
                && b.source.tree_sha == REVIEWED_TREE
                && b.source.repository == MANAGED_LLAMA_REPOSITORY
                && b.source.repository_url == format!("{UPSTREAM_REPOSITORY}.git")
                && b.source.source_provider == LLAMA_CPP_SOURCE_RUNTIME_PROVIDER_ID
                && b.recipe_version == i.variant
                && b.source_overlay_sha256 == expected
                && b.entrypoint_sha256 == m.entrypoint_sha256
        })
}

fn tuple_key(
    runtime: &InstalledRuntime,
    model: &ModelArtifact,
    settings: Option<&norted_core::ResolvedSettings>,
) -> String {
    // Package preparation fills hash from the verified manifest. Discovery and
    // running-model views must address the same declared artifact identity.
    let mut model = model.clone();
    if let Some(package) = &model.norted_package {
        model.hash = Some(package.expected_primary_sha256.clone());
    }
    let configured = settings.map(|s| &s.configured);
    format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(LlamaCppAdapter::capability_key(runtime), model, configured))
                .expect("serializable tuple")
        )
    )
}

pub(super) struct LaunchProof {
    key: String,
    runtime_id: RuntimeId,
    executable_sha256: String,
    model_id: norted_core::ModelId,
    template_sha256: Option<String>,
    reviewed: bool,
    jinja: bool,
    ready: bool,
    tools: bool,
    parallel: bool,
}

impl LlamaCppAdapter {
    pub(super) async fn prepare_chat(&self, spec: &LaunchSpec) {
        let Some(endpoint) = &spec.endpoint else {
            return;
        };
        let key = tuple_key(&spec.runtime, &spec.model.primary, Some(&spec.settings));
        self.chat_proofs
            .write()
            .expect("chat proof lock")
            .retain(|address, proof| address != endpoint && proof.key != key);
        let reviewed = reviewed(&spec.runtime)
            && hash_file(&spec.executable).await.ok().as_deref()
                == Some(spec.runtime.manifest.entrypoint_sha256.as_str());
        // Explicit typed Jinja is required for tool serving; no inferred override.
        let jinja =
            spec.settings.configured.iter().any(|(k, v)| {
                k.as_str() == "llama.cpp.jinja" && v.value == SettingValue::Toggle(true)
            });
        self.chat_proofs.write().expect("chat proof lock").insert(
            endpoint.clone(),
            LaunchProof {
                key,
                runtime_id: spec.runtime.manifest.runtime_id.clone(),
                executable_sha256: spec.runtime.manifest.entrypoint_sha256.clone(),
                model_id: spec.model.primary.id.clone(),
                template_sha256: None,
                reviewed,
                jinja,
                ready: false,
                tools: false,
                parallel: false,
            },
        );
    }

    pub(super) async fn observe_chat(&self, process: &ProcessDescriptor) {
        let Some(endpoint) = &process.endpoint else {
            return;
        };
        let eligible = self
            .chat_proofs
            .read()
            .expect("chat proof lock")
            .get(endpoint)
            .is_some_and(|p| {
                p.reviewed
                    && p.runtime_id == process.runtime_id
                    && p.executable_sha256 == process.runtime_executable_sha256
                    && p.model_id == process.model_id
            });
        if !eligible {
            return;
        }
        let result = async {
            let props: Value = self.client.get(format!("{endpoint}/props")).timeout(PROPS_TIMEOUT).send().await.ok()?.error_for_status().ok()?.json().await.ok()?;
            let caps = props.get("chat_template_caps")?;
            let tools = caps.get("supports_tools").and_then(Value::as_bool) == Some(true) && caps.get("supports_tool_calls").and_then(Value::as_bool) == Some(true);
            let parallel = caps.get("supports_parallel_tool_calls").and_then(Value::as_bool) == Some(true);
            let mut rendered = false;
            if tools {
                let body = json!({"messages":[
                    {"role":"user","content":"norted_probe_user"},
                    {"role":"assistant","content":"","tool_calls":[{"id":"norted_probe_id","type":"function","function":{"name":"norted_probe_function","arguments":"{\"value\":\"norted_probe_argument\"}"}}]},
                    {"role":"tool","tool_call_id":"norted_probe_id","content":"norted_probe_result"},
                    {"role":"user","content":"norted_probe_continue"}],
                    "tools":[{"type":"function","function":{"name":"norted_probe_function","description":"norted_probe_definition","parameters":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}}}],
                    "tool_choice":"auto", "parallel_tool_calls":false});
                if let Ok(response) = self.client.post(format!("{endpoint}/apply-template")).timeout(PROPS_TIMEOUT).json(&body).send().await
                    && response.status().is_success()
                    && let Ok(value) = response.json::<Value>().await
                    && let Some(prompt) = value.get("prompt").and_then(Value::as_str) {
                    rendered = ["norted_probe_user", "norted_probe_function", "norted_probe_definition", "norted_probe_argument", "norted_probe_result", "norted_probe_continue"].iter().all(|s| prompt.contains(s));
                }
            }
            let template_sha256 = props.get("chat_template").and_then(Value::as_str).map(|s| format!("{:x}", Sha256::digest(s.as_bytes())));
            Some((tools && rendered, parallel, template_sha256))
        }.await;
        if let Some((tools, parallel, template_sha256)) = result
            && let Some(p) = self
                .chat_proofs
                .write()
                .expect("chat proof lock")
                .get_mut(endpoint)
            && p.runtime_id == process.runtime_id
            && p.executable_sha256 == process.runtime_executable_sha256
            && p.model_id == process.model_id
        {
            p.template_sha256 = template_sha256;
            p.ready = true;
            p.tools = tools && p.jinja;
            p.parallel = parallel && p.tools;
        }
    }

    pub(super) fn chat_observation(&self, endpoint: &str) -> Value {
        let proofs = self.chat_proofs.read().expect("chat proof lock");
        match proofs.get(endpoint) {
            Some(p) => json!({
                "reviewed_runtime": p.reviewed,
                "startup_verified": p.ready,
                "explicit_jinja": p.jinja,
                "template_sha256": p.template_sha256,
                "tool_calling": p.ready && p.tools,
                "parallel_tool_calls": p.ready && p.parallel,
                "structured_output": p.reviewed && p.ready,
            }),
            None => Value::Null,
        }
    }

    pub(super) fn chat_serving_features(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        settings: Option<&norted_core::ResolvedSettings>,
    ) -> Vec<EngineFeature> {
        let key = tuple_key(runtime, model, settings);
        let proofs = self.chat_proofs.read().expect("chat proof lock");
        let mut features = vec![EngineFeature::TextGeneration];
        if let Some(p) = proofs
            .values()
            .find(|p| p.key == key && p.reviewed && p.ready)
        {
            features.push(EngineFeature::StructuredOutput);
            if p.tools {
                features.push(EngineFeature::ToolCalling);
            }
        }
        features
    }

    pub(super) fn validate_chat_endpoint(
        &self,
        endpoint: &str,
        request: &InferenceRequest,
    ) -> Result<(), EngineError> {
        validate_request(request)?;
        let uses_tools = !request.tools.is_empty()
            || request
                .messages
                .iter()
                .any(|m| !m.tool_calls.is_empty() || m.role == InferenceRole::Tool);
        if !uses_tools && request.parallel_tool_calls != Some(true) {
            return Ok(());
        }
        let proofs = self.chat_proofs.read().expect("chat proof lock");
        let proof = proofs
            .get(endpoint)
            .filter(|p| p.reviewed && p.ready && p.tools)
            .ok_or_else(|| {
                EngineError::Unsupported(
                    "exact llama.cpp runtime/template/Jinja tuple has not proved tool calling"
                        .into(),
                )
            })?;
        if request.parallel_tool_calls == Some(true) && !proof.parallel {
            return Err(EngineError::Unsupported(
                "loaded llama.cpp template does not support parallel tool calls".into(),
            ));
        }
        Ok(())
    }
}
