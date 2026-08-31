use std::collections::VecDeque;
use std::convert::Infallible;

use axum::Json;
use axum::body::Body;
use axum::extract::{Extension, State, rejection::JsonRejection};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::{StreamExt, stream};
use norted_engine::{
    EffectiveGenerationSettings, InferenceEvent, InferenceFinishReason, InferenceMessage,
    InferenceRole, InferenceStream, InferenceUsage,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::auth::RequestCorrelation;
use crate::error::{OpenAiError, runtime_error};
use crate::input::{
    NormalizedRequest, generation_settings, object, optional_bool, optional_positive_u32,
    optional_reasoning_effort, optional_string, reject_unknown_fields, require_null_or,
    required_string, role, text_content,
};
use crate::{PublicApiState, unix_timestamp};

const ALLOWED_TOP_LEVEL_FIELDS: &[&str] = &[
    "background",
    "conversation",
    "include",
    "input",
    "instructions",
    "max_output_tokens",
    "max_tool_calls",
    "metadata",
    "model",
    "parallel_tool_calls",
    "previous_response_id",
    "prompt_cache_key",
    "prompt_cache_retention",
    "reasoning",
    "safety_identifier",
    "service_tier",
    "store",
    "stream",
    "stream_options",
    "temperature",
    "text",
    "tool_choice",
    "tools",
    "top_logprobs",
    "top_p",
    "truncation",
    "user",
];

pub(super) async fn create(
    State(state): State<PublicApiState>,
    Extension(correlation): Extension<RequestCorrelation>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Response, OpenAiError> {
    let Json(value) = payload.map_err(|error| OpenAiError::malformed_json(&error))?;
    let parsed = parse_request(value)?;
    correlation.record_inference(&parsed.normalized.model, parsed.normalized.stream);
    let response_id = format!("resp_{}", Uuid::new_v4().simple());
    let message_id = format!("msg_{}", Uuid::new_v4().simple());
    let created_at = unix_timestamp();
    let inference = parsed.normalized.inference_request();
    let public_model = parsed.normalized.model.clone();
    let max_output_tokens = parsed.normalized.max_output_tokens;
    if parsed.normalized.stream {
        let routed = state
            .runtime
            .infer_stream(inference)
            .await
            .map_err(runtime_error)?;
        Ok(streaming_response(
            ResponseContext {
                response_id,
                message_id,
                created_at,
                model: public_model,
                instructions: parsed.instructions,
                max_output_tokens,
                effective_generation_settings: routed.effective_generation_settings,
            },
            routed.stream,
        ))
    } else {
        let routed = state
            .runtime
            .infer(inference)
            .await
            .map_err(runtime_error)?;
        let status = if routed.output.finish_reason == InferenceFinishReason::MaxOutputTokens {
            ResponseStatus::Incomplete
        } else {
            ResponseStatus::Completed
        };
        let context = ResponseContext {
            response_id,
            message_id,
            created_at,
            model: public_model,
            instructions: parsed.instructions,
            max_output_tokens,
            effective_generation_settings: routed.effective_generation_settings,
        };
        Ok(Json(response_document(
            &context,
            status,
            Some(&routed.output.text),
            routed.output.usage.as_ref(),
            None,
        ))
        .into_response())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedRequest {
    pub(crate) normalized: NormalizedRequest,
    instructions: Option<String>,
}

pub(crate) fn parse_request(value: Value) -> Result<ParsedRequest, OpenAiError> {
    let object = object(&value)?;
    reject_unknown_fields(object, ALLOWED_TOP_LEVEL_FIELDS, "Responses")?;
    let stream = optional_bool(object, "stream", false)?;
    validate_identity_fields(object, stream)?;

    let model = required_string(object, "model")?;
    let instructions = optional_string(object, "instructions")?;
    let max_output_tokens = optional_positive_u32(object, "max_output_tokens")?;
    let mut generation_settings = generation_settings(object)?;
    generation_settings.reasoning_effort = responses_reasoning_effort(object.get("reasoning"))?;
    let input = object.get("input").ok_or_else(|| {
        OpenAiError::invalid(
            "Missing required field: input",
            Some("input"),
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
                    Some("input"),
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
                Some("input"),
                "invalid_type",
            ));
        }
    }
    Ok(ParsedRequest {
        normalized: NormalizedRequest {
            model,
            messages,
            max_output_tokens,
            generation_settings,
            stream,
        },
        instructions,
    })
}

fn parse_message(value: &Value, index: usize) -> Result<InferenceMessage, OpenAiError> {
    let parameter = format!("input[{index}]");
    let message = value.as_object().ok_or_else(|| {
        OpenAiError::invalid(
            "Each input item must be a text message object.",
            Some(parameter.clone()),
            "invalid_type",
        )
    })?;
    reject_unknown_fields(message, &["type", "role", "content"], "input message")?;
    if let Some(kind) = message.get("type")
        && !kind.is_null()
        && kind.as_str() != Some("message")
    {
        return Err(OpenAiError::unsupported(
            "Only `message` input items are supported.",
            format!("{parameter}.type"),
        ));
    }
    let role = role(message.get("role"), &format!("{parameter}.role"))?;
    let content = message.get("content").ok_or_else(|| {
        OpenAiError::invalid(
            "Input messages require `content`.",
            Some(format!("{parameter}.content")),
            "missing_required_parameter",
        )
    })?;
    let text = text_content(content, &format!("{parameter}.content"), "input_text")?;
    Ok(InferenceMessage { role, text })
}

fn validate_identity_fields(
    object: &serde_json::Map<String, Value>,
    stream: bool,
) -> Result<(), OpenAiError> {
    require_null_or(object, "store", |value| value == false, "`false`")?;
    require_null_or(object, "background", |value| value == false, "`false`")?;
    require_null_or(
        object,
        "tools",
        |value| value.as_array().is_some_and(Vec::is_empty),
        "an empty array",
    )?;
    require_null_or(
        object,
        "tool_choice",
        |value| value.as_str() == Some("none"),
        "`\"none\"`",
    )?;
    require_null_or(
        object,
        "truncation",
        |value| value.as_str() == Some("disabled"),
        "`\"disabled\"`",
    )?;
    require_null_or(
        object,
        "metadata",
        |value| value.as_object().is_some_and(serde_json::Map::is_empty),
        "an empty object",
    )?;
    require_null_or(
        object,
        "include",
        |value| value.as_array().is_some_and(Vec::is_empty),
        "an empty array",
    )?;
    require_null_or(
        object,
        "parallel_tool_calls",
        |value| value == false,
        "`false`",
    )?;
    require_null_or(
        object,
        "top_logprobs",
        |value| value.as_u64() == Some(0),
        "zero",
    )?;
    require_null_or(
        object,
        "service_tier",
        |value| matches!(value.as_str(), Some("auto" | "default")),
        "`\"auto\"` or `\"default\"`",
    )?;
    for field in [
        "conversation",
        "max_tool_calls",
        "previous_response_id",
        "prompt_cache_key",
        "prompt_cache_retention",
    ] {
        require_null_or(object, field, |_| false, "`null`")?;
    }
    for field in ["safety_identifier", "user"] {
        require_null_or(object, field, Value::is_string, "a string")?;
    }
    validate_text_format(object.get("text"))?;
    validate_stream_options(object.get("stream_options"), stream)
}

fn responses_reasoning_effort(
    value: Option<&Value>,
) -> Result<Option<norted_engine::ReasoningEffort>, OpenAiError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let reasoning = value.as_object().ok_or_else(|| {
        OpenAiError::invalid(
            "`reasoning` must be an object.",
            Some("reasoning"),
            "invalid_type",
        )
    })?;
    reject_unknown_fields(reasoning, &["effort"], "reasoning")?;
    optional_reasoning_effort(reasoning.get("effort"), "reasoning.effort")
}

fn validate_text_format(value: Option<&Value>) -> Result<(), OpenAiError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(());
    };
    let text = value.as_object().ok_or_else(|| {
        OpenAiError::invalid("`text` must be an object.", Some("text"), "invalid_type")
    })?;
    reject_unknown_fields(text, &["format"], "Responses text")?;
    let Some(format) = text.get("format").filter(|format| !format.is_null()) else {
        return Ok(());
    };
    let format = format.as_object().ok_or_else(|| {
        OpenAiError::invalid(
            "`text.format` must be an object.",
            Some("text.format"),
            "invalid_type",
        )
    })?;
    reject_unknown_fields(format, &["type"], "Responses text format")?;
    if format.get("type").and_then(Value::as_str) != Some("text") {
        return Err(OpenAiError::unsupported(
            "Only plain text output format is supported.",
            "text.format.type",
        ));
    }
    Ok(())
}

fn validate_stream_options(value: Option<&Value>, stream: bool) -> Result<(), OpenAiError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(());
    };
    if !stream {
        return Err(OpenAiError::invalid(
            "`stream_options` requires `stream = true`.",
            Some("stream_options"),
            "invalid_value",
        ));
    }
    let options = value.as_object().ok_or_else(|| {
        OpenAiError::invalid(
            "`stream_options` must be an object.",
            Some("stream_options"),
            "invalid_type",
        )
    })?;
    reject_unknown_fields(
        options,
        &["include_obfuscation"],
        "Responses stream_options",
    )?;
    require_null_or(
        options,
        "include_obfuscation",
        |value| value == false,
        "`false`",
    )
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

#[derive(Clone, Copy, Eq, PartialEq)]
enum ResponseStatus {
    InProgress,
    Completed,
    Incomplete,
    Failed,
}

#[derive(Clone, Copy)]
enum OutputMessageStatus {
    Completed,
    Incomplete,
}

impl OutputMessageStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Incomplete => "incomplete",
        }
    }
}

impl ResponseStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Incomplete => "incomplete",
            Self::Failed => "failed",
        }
    }
}

fn response_document(
    context: &ResponseContext,
    status: ResponseStatus,
    text: Option<&str>,
    usage: Option<&InferenceUsage>,
    error: Option<Value>,
) -> Value {
    let message_status = match status {
        ResponseStatus::Completed => Some(OutputMessageStatus::Completed),
        ResponseStatus::Incomplete => Some(OutputMessageStatus::Incomplete),
        ResponseStatus::InProgress | ResponseStatus::Failed => None,
    };
    let output = text
        .zip(message_status)
        .map_or_else(Vec::new, |(text, status)| {
            vec![message_item(context, status, text)]
        });
    let completed_at = if status == ResponseStatus::Completed {
        json!(unix_timestamp())
    } else {
        Value::Null
    };
    let mut document = json!({
        "id": context.response_id,
        "object": "response",
        "created_at": context.created_at,
        "completed_at": completed_at,
        "status": status.as_str(),
        "error": error,
        "incomplete_details": if status == ResponseStatus::Incomplete {
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
    if let Some(usage) = usage.and_then(usage_document) {
        document
            .as_object_mut()
            .expect("Response document is an object")
            .insert("usage".to_owned(), usage);
    }
    document
}

fn message_item(context: &ResponseContext, status: OutputMessageStatus, text: &str) -> Value {
    json!({
        "id": context.message_id,
        "type": "message",
        "status": status.as_str(),
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
    let initial_response =
        response_document(&state.context, ResponseStatus::InProgress, None, None, None);
    state.push_event("response.created", json!({ "response": initial_response }));
    let in_progress =
        response_document(&state.context, ResponseStatus::InProgress, None, None, None);
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

    let public_stream = stream::unfold(state, |mut state| async move {
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
                })) => state.complete(usage.as_ref(), finish_reason),
                Some(Err(error)) => {
                    tracing::warn!(%error, "private engine Responses stream failed");
                    state.fail();
                }
                None => {
                    tracing::warn!("private engine Responses stream ended without completion");
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
        .body(Body::from_stream(public_stream))
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
        let (status, message_status, terminal_event) =
            if finish_reason == InferenceFinishReason::MaxOutputTokens {
                (
                    ResponseStatus::Incomplete,
                    OutputMessageStatus::Incomplete,
                    "response.incomplete",
                )
            } else {
                (
                    ResponseStatus::Completed,
                    OutputMessageStatus::Completed,
                    "response.completed",
                )
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
                "item": message_item(&self.context, message_status, &text),
            }),
        );
        let response = response_document(&self.context, status, Some(&text), usage, None);
        self.push_event(terminal_event, json!({ "response": response }));
        self.terminal = true;
    }

    fn fail(&mut self) {
        let error = json!({
            "code": "server_error",
            "message": "The model failed to generate a response.",
        });
        let response = response_document(
            &self.context,
            ResponseStatus::Failed,
            None,
            None,
            Some(error),
        );
        self.push_event("response.failed", json!({ "response": response }));
        self.terminal = true;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use axum::body::to_bytes;
    use futures_util::stream;
    use norted_engine::{
        EffectiveGenerationSettings, EngineError, InferenceEvent, InferenceFinishReason,
        InferenceRole, InferenceStream, InferenceUsage, ReasoningEffort,
    };
    use serde_json::{Value, json};

    use super::{
        ResponseContext, ResponseStatus, parse_request, response_document, streaming_response,
    };

    #[test]
    fn string_and_message_inputs_normalize_to_the_canonical_request() {
        let string = parse_request(json!({
            "model": "local/model",
            "instructions": "Be concise",
            "input": "Hello",
            "temperature": 0.25,
            "top_p": 0.9,
            "reasoning": {"effort": "low"},
        }))
        .expect("valid string input");
        assert_eq!(string.normalized.messages.len(), 2);
        assert_eq!(string.normalized.messages[0].role, InferenceRole::Developer);
        assert_eq!(string.normalized.messages[1].role, InferenceRole::User);
        assert_eq!(
            string.normalized.generation_settings.temperature,
            Some(0.25)
        );
        assert_eq!(string.normalized.generation_settings.top_p, Some(0.9));
        assert_eq!(
            string.normalized.generation_settings.reasoning_effort,
            Some(ReasoningEffort::Low)
        );

        let messages = parse_request(json!({
            "model": "local/model",
            "input": [
                {"role": "system", "content": [{"type": "input_text", "text": "S"}]},
                {"role": "developer", "content": "D"},
                {"role": "user", "content": "U"},
                {"role": "assistant", "content": "A"}
            ],
            "store": false,
            "background": false,
            "tools": [],
            "tool_choice": "none",
            "metadata": {},
            "truncation": "disabled",
            "text": {"format": {"type": "text"}}
        }))
        .expect("valid text messages");
        assert_eq!(messages.normalized.messages.len(), 4);
        assert_eq!(messages.normalized.messages[0].role, InferenceRole::System);
        assert_eq!(
            messages.normalized.messages[3].role,
            InferenceRole::Assistant
        );
    }

    #[test]
    fn behavior_requesting_identity_fields_are_rejected() {
        for (field, value) in [
            ("store", json!(true)),
            ("background", json!(true)),
            ("tools", json!([{"type": "function"}])),
            ("truncation", json!("auto")),
        ] {
            let mut request = json!({"model": "m", "input": "hello"});
            request[field] = value;
            assert!(parse_request(request).is_err(), "field {field} must fail");
        }
    }

    #[test]
    fn stream_options_are_stream_only_and_obfuscation_must_be_disabled() {
        parse_request(json!({
            "model": "m",
            "input": "hello",
            "stream": true,
            "stream_options": {"include_obfuscation": false}
        }))
        .expect("disabled obfuscation on a stream is compatible");
        parse_request(json!({
            "model": "m",
            "input": "hello",
            "stream_options": null
        }))
        .expect("null stream options are harmless");

        for invalid in [
            json!({
                "model": "m",
                "input": "hello",
                "stream_options": {"include_obfuscation": false}
            }),
            json!({
                "model": "m",
                "input": "hello",
                "stream": true,
                "stream_options": {"include_obfuscation": true}
            }),
        ] {
            assert!(parse_request(invalid).is_err());
        }
    }

    #[test]
    fn non_stream_documents_report_completion_and_max_output_truthfully() {
        let completed = response_document(
            &context(),
            ResponseStatus::Completed,
            Some("done"),
            None,
            None,
        );
        assert_eq!(completed["object"], "response");
        assert_eq!(completed["status"], "completed");
        assert_eq!(completed["output"][0]["content"][0]["text"], "done");
        assert_eq!(completed["output"][0]["status"], "completed");
        assert!(completed.get("usage").is_none());

        let incomplete = response_document(
            &context(),
            ResponseStatus::Incomplete,
            Some("partial"),
            Some(&InferenceUsage {
                input_tokens: 2,
                output_tokens: 1,
                total_tokens: 3,
                ..Default::default()
            }),
            None,
        );
        assert_eq!(incomplete["status"], "incomplete");
        assert_eq!(
            incomplete["incomplete_details"]["reason"],
            "max_output_tokens"
        );
        assert_eq!(incomplete["output"][0]["status"], "incomplete");
        assert!(incomplete.get("usage").is_none());
    }

    #[tokio::test]
    async fn stream_events_have_the_text_sequence_and_stable_ids() {
        let backend: InferenceStream = Box::pin(stream::iter([
            Ok(InferenceEvent::TextDelta {
                delta: "hi".to_owned(),
            }),
            Ok(InferenceEvent::Completed {
                usage: None,
                finish_reason: InferenceFinishReason::Stop,
            }),
        ]));
        let response = streaming_response(context(), backend);
        let bytes = to_bytes(response.into_body(), 128 * 1024)
            .await
            .expect("stream body");
        let body = String::from_utf8(bytes.to_vec()).expect("UTF-8 SSE");
        let expected = [
            "response.created",
            "response.in_progress",
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
            "response.completed",
        ];
        let events = body
            .lines()
            .filter_map(|line| line.strip_prefix("event: "))
            .collect::<Vec<_>>();
        assert_eq!(events, expected);
        assert!(body.contains("resp_stable"));
        assert!(body.contains("msg_stable"));
    }

    #[tokio::test]
    async fn backend_stream_failure_emits_one_schema_valid_sanitized_failed_response() {
        let backend: InferenceStream = Box::pin(stream::iter([
            Ok(InferenceEvent::TextDelta {
                delta: "partial".to_owned(),
            }),
            Err(EngineError::Operation(
                "failed at C:\\private\\model against http://127.0.0.1:54321".to_owned(),
            )),
        ]));
        let response = streaming_response(context(), backend);
        let bytes = to_bytes(response.into_body(), 128 * 1024)
            .await
            .expect("failed stream body");
        let body = String::from_utf8(bytes.to_vec()).expect("UTF-8 SSE");
        let events = body
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .map(|line| serde_json::from_str::<Value>(line).expect("JSON event"))
            .collect::<Vec<_>>();
        let event_types = events
            .iter()
            .map(|event| event["type"].as_str().expect("event type"))
            .collect::<Vec<_>>();
        assert_eq!(event_types.last(), Some(&"response.failed"));
        assert!(!event_types.contains(&"response.completed"));

        let failed = events.last().expect("terminal failed event");
        assert_eq!(failed["response"]["status"], "failed");
        assert_eq!(failed["response"]["error"]["code"], "server_error");
        assert_eq!(
            failed["response"]["error"]["message"],
            "The model failed to generate a response."
        );
        assert_eq!(failed["response"]["output"], json!([]));
        assert!(!body.contains("C:\\private"));
        assert!(!body.contains("127.0.0.1:54321"));

        let sequences = events
            .iter()
            .map(|event| event["sequence_number"].as_u64().expect("sequence"))
            .collect::<Vec<_>>();
        assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
    }

    struct DropSignal(Arc<AtomicBool>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[tokio::test]
    async fn dropping_public_body_drops_owned_backend_stream() {
        let dropped = Arc::new(AtomicBool::new(false));
        let guard = DropSignal(Arc::clone(&dropped));
        let backend: InferenceStream = Box::pin(stream::unfold(guard, |guard| async move {
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            Some((
                Ok(InferenceEvent::TextDelta {
                    delta: String::new(),
                }),
                guard,
            ))
        }));
        let response = streaming_response(context(), backend);
        drop(response);
        assert!(dropped.load(Ordering::Acquire));
    }

    fn context() -> ResponseContext {
        ResponseContext {
            response_id: "resp_stable".to_owned(),
            message_id: "msg_stable".to_owned(),
            created_at: 1,
            model: "model".to_owned(),
            instructions: None,
            max_output_tokens: None,
            effective_generation_settings: EffectiveGenerationSettings {
                temperature: 0.7,
                top_p: 0.95,
            },
        }
    }
}
