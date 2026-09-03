use std::collections::VecDeque;

use bytes::Bytes;
use futures_util::stream::BoxStream;
use futures_util::{StreamExt, stream};
use norted_engine::{
    EngineError, InferenceContentPart, InferenceEvent, InferenceFinishReason, InferenceMessage,
    InferenceOutput, InferenceRequest, InferenceRole, InferenceStream, InferenceToolCall,
    InferenceToolChoice, InferenceUsage,
};
use serde::Deserialize;
use serde_json::{Value, json};

const SSE_FRAME_LIMIT: usize = 1024 * 1024;
const PRIVATE_BODY_LIMIT: usize = 32 * 1024 * 1024;

pub(crate) fn backend_request(
    request: &InferenceRequest,
    streaming: bool,
    request_protocol_semantics: bool,
    request_sampler_semantics: bool,
    tool_calling: bool,
    vision: bool,
    greedy: bool,
) -> Result<Value, EngineError> {
    if !request_sampler_semantics
        && (request.generation_settings.temperature.is_some()
            || request.generation_settings.top_p.is_some()
            || request.generation_settings.top_k.is_some()
            || request.generation_settings.min_p.is_some()
            || request.generation_settings.seed.is_some()
            || request.generation_settings.presence_penalty.is_some()
            || request.generation_settings.frequency_penalty.is_some()
            || request.max_output_tokens.is_some())
    {
        return Err(EngineError::InvalidGenerationSettings(
            "this NInfer executable has no reviewed request-sampler semantic contract".to_owned(),
        ));
    }
    if !request_protocol_semantics
        && (request.generation_settings.stop.is_some()
            || request.generation_settings.reasoning_enabled.is_some()
            || request.generation_settings.reasoning_effort.is_some())
    {
        return Err(EngineError::InvalidGenerationSettings(
            "this NInfer executable has no reviewed source contract for request semantics"
                .to_owned(),
        ));
    }
    let has_tool_history = request
        .messages
        .iter()
        .any(|message| !message.tool_calls.is_empty() || message.tool_call_id.is_some());
    if (!request.tools.is_empty() || has_tool_history) && !tool_calling {
        return Err(EngineError::InvalidGenerationSettings(
            "this exact NInfer source contract does not prove tool calling".to_owned(),
        ));
    }
    if request.messages.iter().any(InferenceMessage::has_media) && !vision {
        return Err(EngineError::InvalidGenerationSettings(
            "NInfer media input requires exact reviewed runtime support and enabled vision residency"
                .to_owned(),
        ));
    }
    for message in &request.messages {
        for part in &message.content {
            let supported = match part {
                InferenceContentPart::Text { .. } => true,
                InferenceContentPart::ImageUrl { .. } => {
                    matches!(message.role, InferenceRole::User | InferenceRole::Tool)
                }
                InferenceContentPart::VideoUrl { .. } => message.role == InferenceRole::User,
            };
            if !supported {
                return Err(EngineError::InvalidGenerationSettings(
                    "NInfer Chat media supports user images/videos and tool-result images only"
                        .to_owned(),
                ));
            }
        }
    }
    if request.generation_settings.reasoning_budget.is_some() {
        return Err(EngineError::InvalidGenerationSettings(
            "NInfer Chat Completions does not provide a per-request thinking-budget field"
                .to_owned(),
        ));
    }
    if greedy
        && (request
            .generation_settings
            .temperature
            .is_some_and(|temperature| temperature > 0.0)
            || request.generation_settings.top_p.is_some()
            || request.generation_settings.top_k.is_some()
            || request.generation_settings.min_p.is_some()
            || request.generation_settings.seed.is_some())
    {
        return Err(EngineError::InvalidGenerationSettings(
            "NInfer was launched in process-wide greedy mode, so sampled request overrides are unavailable"
                .to_owned(),
        ));
    }
    if !request.tools.is_empty()
        && matches!(
            request.tool_choice,
            Some(InferenceToolChoice::Required | InferenceToolChoice::Function { .. })
        )
    {
        return Err(EngineError::InvalidGenerationSettings(
            "NInfer can enforce only auto or none tool_choice".to_owned(),
        ));
    }
    if !request.tools.is_empty() && request.parallel_tool_calls == Some(false) {
        return Err(EngineError::InvalidGenerationSettings(
            "NInfer cannot guarantee a single tool call; parallel_tool_calls=false is unsupported"
                .to_owned(),
        ));
    }
    let mut body = json!({
        "model": request.model_profile_id.as_str(),
        "messages": request.messages.iter().map(message_json).collect::<Vec<_>>(),
        "stream": streaming,
    });
    if let Some(temperature) = request.generation_settings.temperature {
        body["temperature"] = json!(temperature);
    }
    if let Some(top_p) = request.generation_settings.top_p {
        body["top_p"] = json!(top_p);
    }
    if let Some(top_k) = request.generation_settings.top_k {
        body["top_k"] = json!(top_k);
    }
    if let Some(min_p) = request.generation_settings.min_p {
        body["min_p"] = json!(min_p);
    }
    if let Some(seed) = request.generation_settings.seed {
        body["seed"] = json!(seed);
    }
    if let Some(penalty) = request.generation_settings.presence_penalty {
        body["presence_penalty"] = json!(penalty);
    }
    if let Some(penalty) = request.generation_settings.frequency_penalty {
        body["frequency_penalty"] = json!(penalty);
    }
    if let Some(stop) = request.generation_settings.stop.as_ref() {
        body["stop"] = json!(stop);
    }
    if let Some(enabled) = request.generation_settings.reasoning_enabled {
        body["enable_thinking"] = json!(enabled);
    }
    if let Some(effort) = request.generation_settings.reasoning_effort {
        body["reasoning_effort"] = json!(effort.as_str());
    }
    if let Some(maximum) = request.max_output_tokens {
        body["max_tokens"] = json!(maximum);
    }
    if streaming {
        body["stream_options"] = json!({ "include_usage": true });
    }
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(
            request
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                        }
                    })
                })
                .collect(),
        );
        if let Some(choice) = request.tool_choice.as_ref() {
            body["tool_choice"] = match choice {
                InferenceToolChoice::Auto => json!("auto"),
                InferenceToolChoice::None => json!("none"),
                InferenceToolChoice::Required | InferenceToolChoice::Function { .. } => {
                    unreachable!("unsupported choices rejected above")
                }
            };
        }
        if let Some(parallel) = request.parallel_tool_calls {
            body["parallel_tool_calls"] = json!(parallel);
        }
    }
    Ok(body)
}

fn message_json(message: &InferenceMessage) -> Value {
    let content = if message
        .content
        .iter()
        .all(|part| matches!(part, InferenceContentPart::Text { .. }))
    {
        json!(message.text_only().unwrap_or_default())
    } else {
        Value::Array(
            message
                .content
                .iter()
                .map(|part| match part {
                    InferenceContentPart::Text { text } => json!({"type": "text", "text": text}),
                    InferenceContentPart::ImageUrl { url } => {
                        json!({"type": "image_url", "image_url": {"url": url, "detail": "auto"}})
                    }
                    InferenceContentPart::VideoUrl { url } => {
                        json!({"type": "video_url", "video_url": {"url": url}})
                    }
                })
                .collect(),
        )
    };
    let mut value = json!({
        "role": match message.role {
            InferenceRole::System => "system",
            InferenceRole::Developer => "developer",
            InferenceRole::User => "user",
            InferenceRole::Assistant => "assistant",
            InferenceRole::Tool => "tool",
        },
        "content": content,
    });
    if !message.tool_calls.is_empty() {
        value["tool_calls"] = Value::Array(
            message
                .tool_calls
                .iter()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function": {"name": call.name, "arguments": call.arguments},
                    })
                })
                .collect(),
        );
        if message.content.is_empty() {
            value["content"] = Value::Null;
        }
    }
    if let Some(id) = message.tool_call_id.as_deref() {
        value["tool_call_id"] = json!(id);
    }
    value
}

pub(crate) async fn parse_completion_response(
    response: reqwest::Response,
) -> Result<InferenceOutput, EngineError> {
    let status = response.status();
    let body = read_bounded_body(response, PRIVATE_BODY_LIMIT).await?;
    if !status.is_success() {
        return Err(backend_http_error(status, &body));
    }
    decode_completion_body(&body)
}

fn decode_completion_body(body: &[u8]) -> Result<InferenceOutput, EngineError> {
    let response: ChatCompletionResponse = serde_json::from_slice(body).map_err(|error| {
        EngineError::Operation(format!("invalid NInfer completion response: {error}"))
    })?;
    let choice = response.choices.into_iter().next().ok_or_else(|| {
        EngineError::Operation("NInfer response contained no completion choice".to_owned())
    })?;
    let finish_reason = map_finish_reason(choice.finish_reason.as_deref())?;
    let tool_calls = choice
        .message
        .tool_calls
        .into_iter()
        .map(|call| InferenceToolCall {
            id: call.id,
            name: call.function.name,
            arguments: call.function.arguments,
        })
        .collect::<Vec<_>>();
    if finish_reason == InferenceFinishReason::ToolCalls && tool_calls.is_empty() {
        return Err(EngineError::Operation(
            "NInfer reported a tool-call finish without tool calls".to_owned(),
        ));
    }
    Ok(InferenceOutput {
        text: choice.message.content.unwrap_or_default(),
        tool_calls,
        usage: response.usage.map(Into::into),
        finish_reason,
    })
}

pub(crate) async fn parse_stream_response(
    response: reqwest::Response,
) -> Result<InferenceStream, EngineError> {
    let status = response.status();
    if !status.is_success() {
        let body = read_bounded_body(response, SSE_FRAME_LIMIT).await?;
        return Err(backend_http_error(status, &body));
    }
    Ok(sse_stream(response.bytes_stream().boxed()))
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
    usage: Option<ChatUsage>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: Option<String>,
    #[allow(dead_code)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ChatToolCall>,
}

#[derive(Deserialize)]
struct ChatToolCall {
    id: String,
    function: ChatToolFunction,
}

#[derive(Deserialize)]
struct ChatToolFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    prompt_tokens_details: Option<PromptTokenDetails>,
    completion_tokens_details: Option<CompletionTokenDetails>,
}

#[derive(Debug, Clone, Deserialize)]
struct PromptTokenDetails {
    cached_tokens: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct CompletionTokenDetails {
    reasoning_tokens: Option<u64>,
}

impl From<ChatUsage> for InferenceUsage {
    fn from(usage: ChatUsage) -> Self {
        Self {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            cached_input_tokens: usage
                .prompt_tokens_details
                .and_then(|details| details.cached_tokens),
            cache_write_input_tokens: None,
            reasoning_output_tokens: usage
                .completion_tokens_details
                .and_then(|details| details.reasoning_tokens),
        }
    }
}

struct SseState {
    source: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    buffer: Vec<u8>,
    queued: VecDeque<Result<InferenceEvent, EngineError>>,
    usage: Option<InferenceUsage>,
    finish_reason: Option<InferenceFinishReason>,
    finished: bool,
}

pub(crate) fn sse_stream(
    source: BoxStream<'static, Result<Bytes, reqwest::Error>>,
) -> InferenceStream {
    let state = SseState {
        source,
        buffer: Vec::new(),
        queued: VecDeque::new(),
        usage: None,
        finish_reason: None,
        finished: false,
    };
    Box::pin(stream::unfold(state, |mut state| async move {
        loop {
            if let Some(event) = state.queued.pop_front() {
                return Some((event, state));
            }
            if state.finished {
                return None;
            }
            match state.source.next().await {
                Some(Ok(chunk)) => {
                    state.buffer.extend_from_slice(&chunk);
                    parse_sse_frames(&mut state);
                    if !state.finished && state.buffer.len() > SSE_FRAME_LIMIT {
                        state.buffer.clear();
                        state.queued.push_back(Err(EngineError::Operation(
                            "NInfer SSE frame exceeded the local size limit".to_owned(),
                        )));
                        state.finished = true;
                    }
                }
                Some(Err(error)) => {
                    state.finished = true;
                    return Some((Err(map_transport_error(error)), state));
                }
                None => {
                    state.finished = true;
                    return Some((
                        Err(EngineError::BackendUnavailable(
                            "NInfer stream ended before the [DONE] marker".to_owned(),
                        )),
                        state,
                    ));
                }
            }
        }
    }))
}

fn parse_sse_frames(state: &mut SseState) {
    while let Some((boundary, boundary_length)) = find_sse_boundary(&state.buffer) {
        if boundary > SSE_FRAME_LIMIT {
            state.buffer.clear();
            state.queued.push_back(Err(EngineError::Operation(
                "NInfer SSE frame exceeded the local size limit".to_owned(),
            )));
            state.finished = true;
            return;
        }
        let frame = state.buffer.drain(..boundary).collect::<Vec<_>>();
        state.buffer.drain(..boundary_length);
        let frame = String::from_utf8_lossy(&frame);
        let data = frame
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            match state.finish_reason.take() {
                Some(finish_reason) => {
                    state.queued.push_back(Ok(InferenceEvent::Completed {
                        usage: state.usage.take(),
                        finish_reason,
                    }));
                }
                None => state.queued.push_back(Err(EngineError::BackendUnavailable(
                    "NInfer stream reached [DONE] without a terminal finish reason".to_owned(),
                ))),
            }
            state.finished = true;
            return;
        }
        let value: Value = match serde_json::from_str(&data) {
            Ok(value) => value,
            Err(error) => {
                state.queued.push_back(Err(EngineError::Operation(format!(
                    "invalid NInfer SSE payload: {error}"
                ))));
                state.finished = true;
                return;
            }
        };
        if let Some(error) = value.get("error") {
            state.queued.push_back(Err(EngineError::Operation(format!(
                "NInfer streaming error: {}",
                backend_error_message(error)
            ))));
            state.finished = true;
            return;
        }
        if let Some(usage) = value.get("usage").filter(|usage| !usage.is_null()) {
            match serde_json::from_value::<ChatUsage>(usage.clone()) {
                Ok(usage) => state.usage = Some(usage.into()),
                Err(error) => {
                    state.queued.push_back(Err(EngineError::Operation(format!(
                        "invalid NInfer streaming usage: {error}"
                    ))));
                    state.finished = true;
                    return;
                }
            }
        }
        if let Some(finish_reason) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("finish_reason"))
            .filter(|reason| !reason.is_null())
        {
            let Some(finish_reason) = finish_reason.as_str() else {
                state.queued.push_back(Err(EngineError::Operation(
                    "NInfer stream returned a non-string finish reason".to_owned(),
                )));
                state.finished = true;
                return;
            };
            match map_finish_reason(Some(finish_reason)) {
                Ok(finish_reason) => state.finish_reason = Some(finish_reason),
                Err(error) => {
                    state.queued.push_back(Err(error));
                    state.finished = true;
                    return;
                }
            }
        }
        if let Some(delta) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("delta"))
            .and_then(|delta| delta.get("content"))
            .and_then(Value::as_str)
            .filter(|delta| !delta.is_empty())
        {
            state.queued.push_back(Ok(InferenceEvent::TextDelta {
                delta: delta.to_owned(),
            }));
        }
        if let Some(tool_calls) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("delta"))
            .and_then(|delta| delta.get("tool_calls"))
            .and_then(Value::as_array)
        {
            for call in tool_calls {
                let Some(index) = call.get("index").and_then(Value::as_u64) else {
                    state.queued.push_back(Err(EngineError::Operation(
                        "NInfer tool-call delta omitted its index".to_owned(),
                    )));
                    state.finished = true;
                    return;
                };
                let Ok(index) = u32::try_from(index) else {
                    state.queued.push_back(Err(EngineError::Operation(
                        "NInfer tool-call delta index exceeded u32".to_owned(),
                    )));
                    state.finished = true;
                    return;
                };
                let function = call.get("function");
                state.queued.push_back(Ok(InferenceEvent::ToolCallDelta {
                    index,
                    id: call.get("id").and_then(Value::as_str).map(str::to_owned),
                    name: function
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    arguments_delta: function
                        .and_then(|function| function.get("arguments"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                }));
            }
        }
    }
}

fn find_sse_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    for index in 0..buffer.len().saturating_sub(1) {
        if buffer[index..].starts_with(b"\r\n\r\n") {
            return Some((index, 4));
        }
        if buffer[index..].starts_with(b"\n\n") {
            return Some((index, 2));
        }
    }
    None
}

pub(crate) fn map_finish_reason(
    reason: Option<&str>,
) -> Result<InferenceFinishReason, EngineError> {
    match reason {
        Some("stop") => Ok(InferenceFinishReason::Stop),
        Some("length") => Ok(InferenceFinishReason::MaxOutputTokens),
        Some("tool_calls") => Ok(InferenceFinishReason::ToolCalls),
        Some(reason) => Err(EngineError::Operation(format!(
            "NInfer returned unsupported finish reason `{reason}`"
        ))),
        None => Err(EngineError::Operation(
            "NInfer response omitted its terminal finish reason".to_owned(),
        )),
    }
}

pub(crate) fn map_transport_error(error: reqwest::Error) -> EngineError {
    if error.is_timeout() {
        EngineError::TimedOut("NInfer inference request timed out".to_owned())
    } else {
        EngineError::BackendUnavailable(error.to_string())
    }
}

pub(crate) fn backend_http_error(status: reqwest::StatusCode, body: &[u8]) -> EngineError {
    let message = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value.get("error").cloned())
        .map_or_else(
            || {
                status
                    .canonical_reason()
                    .unwrap_or("backend request failed")
                    .to_owned()
            },
            |error| backend_error_message(&error),
        );
    EngineError::Operation(format!("NInfer returned HTTP {status}: {message}"))
}

fn backend_error_message(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .unwrap_or("private backend error")
        .to_owned()
}

async fn read_bounded_body(
    response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, EngineError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(EngineError::Operation(
            "NInfer private response exceeded the local size limit".to_owned(),
        ));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(map_transport_error)?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(EngineError::Operation(
                "NInfer private response exceeded the local size limit".to_owned(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Poll};

    use futures_util::StreamExt;
    use norted_core::ModelProfileId;

    use super::*;

    fn request() -> InferenceRequest {
        InferenceRequest {
            model_profile_id: ModelProfileId::new("model").expect("profile ID"),
            messages: vec![
                InferenceMessage::text(InferenceRole::Developer, "developer"),
                InferenceMessage::text(InferenceRole::System, "system"),
                InferenceMessage::text(InferenceRole::User, "user"),
                InferenceMessage::text(InferenceRole::Assistant, "assistant"),
            ],
            generation_settings: Default::default(),
            tools: Vec::new(),
            tool_choice: None,
            parallel_tool_calls: None,
            output_format: None,
            max_output_tokens: None,
            stream: false,
        }
    }

    #[test]
    fn message_order_and_sampler_omission_are_preserved() {
        let body = backend_request(&request(), false, true, true, true, false, false)
            .expect("request body");
        let roles = body["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .map(|message| message["role"].as_str().expect("role"))
            .collect::<Vec<_>>();
        assert_eq!(roles, ["developer", "system", "user", "assistant"]);
        assert!(body.get("temperature").is_none());
        assert!(body.get("top_p").is_none());

        let mut explicit = request();
        explicit.generation_settings.temperature = Some(0.4);
        explicit.generation_settings.top_p = Some(0.8);
        let body = backend_request(&explicit, false, true, true, true, false, false)
            .expect("request body");
        assert_eq!(body["temperature"], 0.4);
        assert_eq!(body["top_p"], 0.8);
    }

    #[test]
    fn non_stream_answer_finish_and_known_usage_are_translated_conservatively() {
        let output = decode_completion_body(
            br#"{
                "choices": [{
                    "message": {"content": "answer", "reasoning_content": "private"},
                    "finish_reason": "length"
                }],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 7,
                    "total_tokens": 17,
                    "prompt_tokens_details": {"cached_tokens": 3},
                    "completion_tokens_details": {"reasoning_tokens": 2}
                }
            }"#,
        )
        .expect("completion translation");
        assert_eq!(output.text, "answer");
        assert_eq!(output.finish_reason, InferenceFinishReason::MaxOutputTokens);
        let usage = output.usage.expect("reported usage");
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 7);
        assert_eq!(usage.total_tokens, 17);
        assert_eq!(usage.cached_input_tokens, Some(3));
        assert_eq!(usage.reasoning_output_tokens, Some(2));
        assert_eq!(usage.cache_write_input_tokens, None);

        assert!(
            decode_completion_body(
                br#"{"choices":[{"message":{"content":"x"},"finish_reason":"tool_calls"}]}"#
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn stream_discards_reasoning_and_requires_terminal_state() {
        let frames = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"secret\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"answer\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let source = stream::iter(vec![Ok::<_, reqwest::Error>(Bytes::from(frames))]).boxed();
        let events = sse_stream(source).collect::<Vec<_>>().await;
        assert!(matches!(
            &events[0],
            Ok(InferenceEvent::TextDelta { delta }) if delta == "answer"
        ));
        assert!(matches!(
            &events[1],
            Ok(InferenceEvent::Completed {
                finish_reason: InferenceFinishReason::Stop,
                ..
            })
        ));
        assert_eq!(events.len(), 2);

        let no_reason = stream::iter(vec![Ok::<_, reqwest::Error>(Bytes::from(
            "data: [DONE]\n\n",
        ))])
        .boxed();
        let events = sse_stream(no_reason).collect::<Vec<_>>().await;
        assert!(matches!(events.as_slice(), [Err(_)]));

        let no_done = stream::iter(vec![Ok::<_, reqwest::Error>(Bytes::from(
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n",
        ))])
        .boxed();
        let events = sse_stream(no_done).collect::<Vec<_>>().await;
        assert!(matches!(
            events.as_slice(),
            [Ok(InferenceEvent::TextDelta { .. }), Err(_)]
        ));
    }

    #[tokio::test]
    async fn stream_maps_length_and_optional_usage_details() {
        let frames = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"token\"},\"finish_reason\":\"length\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":2,\"total_tokens\":6,\"prompt_tokens_details\":null,\"completion_tokens_details\":null}}\n\n",
            "data: [DONE]\n\n"
        );
        let source = stream::iter(vec![Ok::<_, reqwest::Error>(Bytes::from(frames))]).boxed();
        let events = sse_stream(source).collect::<Vec<_>>().await;
        assert!(matches!(
            &events[1],
            Ok(InferenceEvent::Completed {
                finish_reason: InferenceFinishReason::MaxOutputTokens,
                usage: Some(usage),
            }) if usage.input_tokens == 4
                && usage.output_tokens == 2
                && usage.cached_input_tokens.is_none()
                && usage.reasoning_output_tokens.is_none()
        ));
    }

    struct DropObservedStream(Arc<AtomicBool>);

    impl futures_util::Stream for DropObservedStream {
        type Item = Result<Bytes, reqwest::Error>;

        fn poll_next(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Poll::Pending
        }
    }

    impl Drop for DropObservedStream {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn dropping_translated_stream_drops_the_private_source() {
        let dropped = Arc::new(AtomicBool::new(false));
        let translated = sse_stream(DropObservedStream(Arc::clone(&dropped)).boxed());
        drop(translated);
        assert!(dropped.load(Ordering::SeqCst));
    }
}
