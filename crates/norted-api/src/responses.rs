use std::collections::VecDeque;
use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{Json, State, rejection::JsonRejection};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::{StreamExt, stream};
use norted_core::ModelId;
use norted_engine::{
    EffectiveGenerationSettings, InferenceEvent, InferenceFinishReason, InferenceMessage,
    InferenceRequest, InferenceRole, InferenceStream, InferenceUsage, RuntimeError,
};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::PublicApiState;

const ALLOWED_TOP_LEVEL_FIELDS: &[&str] = &[
    "model",
    "input",
    "stream",
    "instructions",
    "max_output_tokens",
];

pub(super) async fn create(
    State(state): State<PublicApiState>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Response, OpenAiError> {
    let Json(value) = payload.map_err(|error| {
        OpenAiError::invalid(
            format!("Malformed JSON request: {}", error.body_text()),
            None,
            "invalid_json",
        )
    })?;
    let parsed = parse_request(value)?;
    let response_id = format!("resp_{}", Uuid::new_v4().simple());
    let message_id = format!("msg_{}", Uuid::new_v4().simple());
    let created_at = unix_timestamp();
    let public_model = parsed.model.clone();
    let inference = InferenceRequest {
        model_id: ModelId(parsed.model),
        messages: parsed.messages,
        max_output_tokens: parsed.max_output_tokens,
        stream: parsed.stream,
    };
    if parsed.stream {
        let routed = state
            .runtime
            .infer_stream(inference)
            .await
            .map_err(runtime_error)?;
        let context = ResponseContext {
            response_id,
            message_id,
            created_at,
            model: public_model,
            instructions: parsed.instructions,
            max_output_tokens: parsed.max_output_tokens,
            effective_generation_settings: routed.effective_generation_settings,
        };
        Ok(streaming_response(context, routed.stream))
    } else {
        let routed = state
            .runtime
            .infer(inference)
            .await
            .map_err(runtime_error)?;
        let output = routed.output;
        let context = ResponseContext {
            response_id,
            message_id,
            created_at,
            model: public_model,
            instructions: parsed.instructions,
            max_output_tokens: parsed.max_output_tokens,
            effective_generation_settings: routed.effective_generation_settings,
        };
        let status = if output.finish_reason == InferenceFinishReason::MaxOutputTokens {
            "incomplete"
        } else {
            "completed"
        };
        Ok(Json(response_document(
            &context,
            status,
            Some(&output.text),
            output.usage.as_ref(),
            None,
        ))
        .into_response())
    }
}

struct ParsedRequest {
    model: String,
    messages: Vec<InferenceMessage>,
    stream: bool,
    instructions: Option<String>,
    max_output_tokens: Option<u32>,
}

fn parse_request(value: Value) -> Result<ParsedRequest, OpenAiError> {
    let object = value.as_object().ok_or_else(|| {
        OpenAiError::invalid(
            "The request body must be a JSON object.",
            None,
            "invalid_request",
        )
    })?;
    if let Some(field) = object
        .keys()
        .find(|field| !ALLOWED_TOP_LEVEL_FIELDS.contains(&field.as_str()))
    {
        return Err(OpenAiError::unsupported(
            format!("Unsupported Responses field: {field}"),
            field.clone(),
        ));
    }
    let model = required_string(object, "model")?;
    if model.trim().is_empty() {
        return Err(OpenAiError::invalid(
            "`model` must not be empty.",
            Some("model".to_owned()),
            "invalid_value",
        ));
    }
    let instructions = optional_string(object, "instructions")?;
    let stream = match object.get("stream") {
        None => false,
        Some(Value::Bool(stream)) => *stream,
        Some(_) => {
            return Err(OpenAiError::invalid(
                "`stream` must be a boolean.",
                Some("stream".to_owned()),
                "invalid_type",
            ));
        }
    };
    let max_output_tokens = match object.get("max_output_tokens") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let tokens = value.as_u64().ok_or_else(|| {
                OpenAiError::invalid(
                    "`max_output_tokens` must be a positive integer.",
                    Some("max_output_tokens".to_owned()),
                    "invalid_type",
                )
            })?;
            if tokens == 0 || tokens > u64::from(u32::MAX) {
                return Err(OpenAiError::invalid(
                    "`max_output_tokens` is outside the supported positive integer range.",
                    Some("max_output_tokens".to_owned()),
                    "invalid_value",
                ));
            }
            Some(tokens as u32)
        }
    };
    let input = object.get("input").ok_or_else(|| {
        OpenAiError::invalid(
            "Missing required field: input",
            Some("input".to_owned()),
            "missing_required_parameter",
        )
    })?;
    let mut messages = Vec::new();
    if let Some(instructions) = &instructions {
        messages.push(InferenceMessage {
            role: InferenceRole::Developer,
            text: instructions.clone(),
        });
    }
    match input {
        Value::String(text) => messages.push(InferenceMessage {
            role: InferenceRole::User,
            text: text.clone(),
        }),
        Value::Array(items) => {
            if items.is_empty() {
                return Err(OpenAiError::invalid(
                    "`input` must contain at least one message.",
                    Some("input".to_owned()),
                    "invalid_value",
                ));
            }
            for (index, item) in items.iter().enumerate() {
                messages.push(parse_message(item, index)?);
            }
        }
        _ => {
            return Err(OpenAiError::invalid(
                "`input` must be a string or an array of text messages.",
                Some("input".to_owned()),
                "invalid_type",
            ));
        }
    }
    Ok(ParsedRequest {
        model,
        messages,
        stream,
        instructions,
        max_output_tokens,
    })
}

fn parse_message(value: &Value, index: usize) -> Result<InferenceMessage, OpenAiError> {
    let parameter = format!("input[{index}]");
    let object = value.as_object().ok_or_else(|| {
        OpenAiError::invalid(
            "Each input item must be a text message object.",
            Some(parameter.clone()),
            "invalid_type",
        )
    })?;
    if let Some(field) = object
        .keys()
        .find(|field| !["type", "role", "content"].contains(&field.as_str()))
    {
        return Err(OpenAiError::unsupported(
            format!("Unsupported input message field: {field}"),
            format!("{parameter}.{field}"),
        ));
    }
    if let Some(kind) = object.get("type")
        && kind.as_str() != Some("message")
    {
        return Err(OpenAiError::unsupported(
            "Only `message` input items are supported.",
            format!("{parameter}.type"),
        ));
    }
    let role = match object.get("role").and_then(Value::as_str) {
        Some("system") => InferenceRole::System,
        Some("developer") => InferenceRole::Developer,
        Some("user") => InferenceRole::User,
        Some("assistant") => InferenceRole::Assistant,
        Some(_) => {
            return Err(OpenAiError::unsupported(
                "Unsupported input message role.",
                format!("{parameter}.role"),
            ));
        }
        None => {
            return Err(OpenAiError::invalid(
                "Input messages require a string `role`.",
                Some(format!("{parameter}.role")),
                "missing_required_parameter",
            ));
        }
    };
    let content = object.get("content").ok_or_else(|| {
        OpenAiError::invalid(
            "Input messages require `content`.",
            Some(format!("{parameter}.content")),
            "missing_required_parameter",
        )
    })?;
    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => {
            let mut text = String::new();
            for (content_index, part) in parts.iter().enumerate() {
                let part_parameter = format!("{parameter}.content[{content_index}]");
                let part = part.as_object().ok_or_else(|| {
                    OpenAiError::invalid(
                        "Text content parts must be objects.",
                        Some(part_parameter.clone()),
                        "invalid_type",
                    )
                })?;
                if let Some(field) = part
                    .keys()
                    .find(|field| !["type", "text"].contains(&field.as_str()))
                {
                    return Err(OpenAiError::unsupported(
                        format!("Unsupported text content field: {field}"),
                        format!("{part_parameter}.{field}"),
                    ));
                }
                if part.get("type").and_then(Value::as_str) != Some("input_text") {
                    return Err(OpenAiError::unsupported(
                        "Only `input_text` content parts are supported.",
                        format!("{part_parameter}.type"),
                    ));
                }
                let part_text = part.get("text").and_then(Value::as_str).ok_or_else(|| {
                    OpenAiError::invalid(
                        "`input_text` content requires a string `text` field.",
                        Some(format!("{part_parameter}.text")),
                        "missing_required_parameter",
                    )
                })?;
                text.push_str(part_text);
            }
            text
        }
        _ => {
            return Err(OpenAiError::invalid(
                "Message `content` must be a string or an array of `input_text` parts.",
                Some(format!("{parameter}.content")),
                "invalid_type",
            ));
        }
    };
    Ok(InferenceMessage { role, text })
}

fn required_string(object: &Map<String, Value>, field: &str) -> Result<String, OpenAiError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            OpenAiError::invalid(
                format!("Missing or invalid required string field: {field}"),
                Some(field.to_owned()),
                "missing_required_parameter",
            )
        })
}

fn optional_string(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<String>, OpenAiError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(OpenAiError::invalid(
            format!("`{field}` must be a string."),
            Some(field.to_owned()),
            "invalid_type",
        )),
    }
}

#[derive(Clone)]
struct ResponseContext {
    response_id: String,
    message_id: String,
    created_at: i64,
    model: String,
    instructions: Option<String>,
    max_output_tokens: Option<u32>,
    effective_generation_settings: EffectiveGenerationSettings,
}

fn response_document(
    context: &ResponseContext,
    status: &str,
    text: Option<&str>,
    usage: Option<&InferenceUsage>,
    error: Option<Value>,
) -> Value {
    let output = text.map_or_else(Vec::new, |text| {
        vec![message_item(
            context,
            if status == "completed" {
                "completed"
            } else {
                "incomplete"
            },
            text,
        )]
    });
    let mut document = json!({
        "id": context.response_id,
        "object": "response",
        "created_at": context.created_at,
        "status": status,
        "error": error,
        "incomplete_details": if status == "incomplete" {
            json!({ "reason": "max_output_tokens" })
        } else {
            Value::Null
        },
        "instructions": context.instructions,
        "metadata": {},
        "model": context.model,
        "output": output,
        "parallel_tool_calls": false,
        "temperature": context.effective_generation_settings.temperature,
        "tool_choice": "none",
        "tools": [],
        "top_p": context.effective_generation_settings.top_p,
        "background": false,
        "max_output_tokens": context.max_output_tokens,
        "previous_response_id": Value::Null,
        "reasoning": Value::Null,
        "store": false,
        "text": { "format": { "type": "text" } },
        "truncation": "disabled",
    });
    let response = document
        .as_object_mut()
        .expect("Response document is constructed as an object");
    if status == "completed" {
        response.insert("completed_at".to_owned(), json!(unix_timestamp()));
    }
    if let Some(usage) = usage.and_then(usage_document) {
        response.insert("usage".to_owned(), usage);
    }
    document
}

fn message_item(context: &ResponseContext, status: &str, text: &str) -> Value {
    json!({
        "id": context.message_id,
        "type": "message",
        "status": status,
        "role": "assistant",
        "content": [{
            "type": "output_text",
            "text": text,
            "annotations": [],
        }],
    })
}

fn usage_document(usage: &InferenceUsage) -> Option<Value> {
    let cached_tokens = usage.cached_input_tokens?;
    let cache_write_tokens = usage.cache_write_input_tokens?;
    let reasoning_tokens = usage.reasoning_output_tokens?;
    Some(json!({
        "input_tokens": usage.input_tokens,
        "input_tokens_details": {
            "cached_tokens": cached_tokens,
            "cache_write_tokens": cache_write_tokens,
        },
        "output_tokens": usage.output_tokens,
        "output_tokens_details": {
            "reasoning_tokens": reasoning_tokens,
        },
        "total_tokens": usage.total_tokens,
    }))
}

struct PublicStreamState {
    context: ResponseContext,
    backend: InferenceStream,
    queue: VecDeque<Result<Bytes, Infallible>>,
    text: String,
    sequence: u64,
    terminal: bool,
}

fn streaming_response(context: ResponseContext, backend: InferenceStream) -> Response {
    let mut state = PublicStreamState {
        context,
        backend,
        queue: VecDeque::new(),
        text: String::new(),
        sequence: 0,
        terminal: false,
    };
    let initial_response = response_document(&state.context, "in_progress", None, None, None);
    state.push_event("response.created", json!({ "response": initial_response }));
    let in_progress = response_document(&state.context, "in_progress", None, None, None);
    state.push_event("response.in_progress", json!({ "response": in_progress }));
    state.push_event(
        "response.output_item.added",
        json!({
            "output_index": 0,
            "item": {
                "id": state.context.message_id,
                "type": "message",
                "status": "in_progress",
                "role": "assistant",
                "content": [],
            },
        }),
    );
    state.push_event(
        "response.content_part.added",
        json!({
            "item_id": state.context.message_id,
            "output_index": 0,
            "content_index": 0,
            "part": {
                "type": "output_text",
                "text": "",
                "annotations": [],
            },
        }),
    );

    let stream = stream::unfold(state, |mut state| async move {
        loop {
            if let Some(event) = state.queue.pop_front() {
                return Some((event, state));
            }
            if state.terminal {
                return None;
            }
            match state.backend.next().await {
                Some(Ok(InferenceEvent::TextDelta { delta })) => {
                    state.text.push_str(&delta);
                    state.push_event(
                        "response.output_text.delta",
                        json!({
                            "item_id": state.context.message_id,
                            "output_index": 0,
                            "content_index": 0,
                            "delta": delta,
                            "logprobs": [],
                        }),
                    );
                }
                Some(Ok(InferenceEvent::Completed {
                    usage,
                    finish_reason,
                })) => {
                    state.complete(usage.as_ref(), finish_reason);
                }
                Some(Err(error)) => {
                    tracing::warn!(%error, "llama.cpp streaming inference failed");
                    state.fail();
                }
                None => {
                    tracing::warn!("llama.cpp inference stream ended without completion");
                    state.fail();
                }
            }
        }
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(Body::from_stream(stream))
        .expect("static streaming response headers are valid")
}

impl PublicStreamState {
    fn push_event(&mut self, event_type: &'static str, payload: Value) {
        let mut payload = payload.as_object().cloned().unwrap_or_default();
        payload.insert("type".to_owned(), Value::String(event_type.to_owned()));
        payload.insert("sequence_number".to_owned(), json!(self.sequence));
        self.sequence = self.sequence.saturating_add(1);
        let data = Value::Object(payload);
        self.queue.push_back(Ok(Bytes::from(format!(
            "event: {event_type}\ndata: {data}\n\n"
        ))));
    }

    fn complete(&mut self, usage: Option<&InferenceUsage>, finish_reason: InferenceFinishReason) {
        let (response_status, item_status, terminal_event) =
            if finish_reason == InferenceFinishReason::MaxOutputTokens {
                ("incomplete", "incomplete", "response.incomplete")
            } else {
                ("completed", "completed", "response.completed")
            };
        let text = self.text.clone();
        self.push_event(
            "response.output_text.done",
            json!({
                "item_id": self.context.message_id,
                "output_index": 0,
                "content_index": 0,
                "text": text,
                "logprobs": [],
            }),
        );
        self.push_event(
            "response.content_part.done",
            json!({
                "item_id": self.context.message_id,
                "output_index": 0,
                "content_index": 0,
                "part": {
                    "type": "output_text",
                    "text": text,
                    "annotations": [],
                },
            }),
        );
        self.push_event(
            "response.output_item.done",
            json!({
                "output_index": 0,
                "item": message_item(&self.context, item_status, &text),
            }),
        );
        let response = response_document(&self.context, response_status, Some(&text), usage, None);
        self.push_event(terminal_event, json!({ "response": response }));
        self.terminal = true;
    }

    fn fail(&mut self) {
        let error = json!({
            "code": "server_error",
            "message": "The local inference backend failed while generating this response.",
        });
        let text = self.text.clone();
        let response = response_document(
            &self.context,
            "failed",
            (!text.is_empty()).then_some(text.as_str()),
            None,
            Some(error),
        );
        self.push_event("response.failed", json!({ "response": response }));
        self.terminal = true;
    }
}

pub(super) struct OpenAiError {
    status: StatusCode,
    message: String,
    kind: &'static str,
    parameter: Option<String>,
    code: &'static str,
}

impl OpenAiError {
    fn invalid(message: impl Into<String>, parameter: Option<String>, code: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
            kind: "invalid_request_error",
            parameter,
            code,
        }
    }

    fn unsupported(message: impl Into<String>, parameter: String) -> Self {
        Self::invalid(message, Some(parameter), "unsupported_value")
    }
}

impl IntoResponse for OpenAiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({
                "error": {
                    "message": self.message,
                    "type": self.kind,
                    "param": self.parameter,
                    "code": self.code,
                }
            })),
        )
            .into_response()
    }
}

fn runtime_error(error: RuntimeError) -> OpenAiError {
    tracing::warn!(%error, "Responses request could not be routed");
    match error {
        RuntimeError::ModelNotFound(_) => OpenAiError {
            status: StatusCode::NOT_FOUND,
            message: "The requested model does not exist in the local model registry.".to_owned(),
            kind: "invalid_request_error",
            parameter: Some("model".to_owned()),
            code: "model_not_found",
        },
        RuntimeError::ModelNotLoaded(_) => OpenAiError {
            status: StatusCode::CONFLICT,
            message: "The requested model exists but is not loaded.".to_owned(),
            kind: "invalid_request_error",
            parameter: Some("model".to_owned()),
            code: "model_not_loaded",
        },
        RuntimeError::BackendCrashed(_) => OpenAiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: "The local inference backend is unavailable after an unexpected exit."
                .to_owned(),
            kind: "server_error",
            parameter: None,
            code: "backend_unavailable",
        },
        RuntimeError::Inference(_) => OpenAiError {
            status: StatusCode::BAD_GATEWAY,
            message: "The local inference backend failed to complete the request.".to_owned(),
            kind: "server_error",
            parameter: None,
            code: "backend_inference_error",
        },
        RuntimeError::InferenceTimedOut(_) => OpenAiError {
            status: StatusCode::GATEWAY_TIMEOUT,
            message: "The local inference backend timed out while completing the request."
                .to_owned(),
            kind: "server_error",
            parameter: None,
            code: "backend_timeout",
        },
        RuntimeError::InferenceUnavailable(_) => OpenAiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: "The local inference backend is unavailable.".to_owned(),
            kind: "server_error",
            parameter: None,
            code: "backend_unavailable",
        },
        _ => OpenAiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: "The local inference runtime is not available for this request.".to_owned(),
            kind: "server_error",
            parameter: None,
            code: "backend_unavailable",
        },
    }
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}
