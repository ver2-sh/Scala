use std::collections::VecDeque;

use bytes::Bytes;
use futures_util::stream::BoxStream;
use futures_util::{StreamExt, stream};
use norted_engine::{
    EngineError, InferenceEvent, InferenceFinishReason, InferenceMessage, InferenceOutput,
    InferenceRequest, InferenceRole, InferenceStream, InferenceUsage,
};
use serde::Deserialize;
use serde_json::{Value, json};

const SSE_FRAME_LIMIT: usize = 1024 * 1024;
const PRIVATE_BODY_LIMIT: usize = 32 * 1024 * 1024;

pub(crate) fn backend_request(request: &InferenceRequest, streaming: bool) -> Value {
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
    if let Some(maximum) = request.max_output_tokens {
        body["max_tokens"] = json!(maximum);
    }
    if streaming {
        body["stream_options"] = json!({ "include_usage": true });
    }
    body
}

fn message_json(message: &InferenceMessage) -> Value {
    json!({
        "role": match message.role {
            InferenceRole::System => "system",
            InferenceRole::Developer => "developer",
            InferenceRole::User => "user",
            InferenceRole::Assistant => "assistant",
        },
        "content": message.text,
    })
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
    Ok(InferenceOutput {
        text: choice.message.content.unwrap_or_default(),
        usage: response.usage.map(Into::into),
        finish_reason: map_finish_reason(choice.finish_reason.as_deref())?,
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
                InferenceMessage {
                    role: InferenceRole::Developer,
                    text: "developer".to_owned(),
                },
                InferenceMessage {
                    role: InferenceRole::System,
                    text: "system".to_owned(),
                },
                InferenceMessage {
                    role: InferenceRole::User,
                    text: "user".to_owned(),
                },
                InferenceMessage {
                    role: InferenceRole::Assistant,
                    text: "assistant".to_owned(),
                },
            ],
            generation_settings: Default::default(),
            output_format: None,
            max_output_tokens: None,
            stream: false,
        }
    }

    #[test]
    fn message_order_and_sampler_omission_are_preserved() {
        let body = backend_request(&request(), false);
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
        let body = backend_request(&explicit, false);
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
