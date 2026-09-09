use crate::{
    PublicApiState,
    auth::RequestCorrelation,
    error::{OpenAiError, runtime_error},
    input::{
        generation_settings, object, optional_bool, optional_positive_u32, optional_string,
        reject_unknown_fields, require_null_or, required_string,
    },
};
use axum::{
    Json,
    body::Body,
    extract::{Extension, State, rejection::JsonRejection},
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};
use futures_util::{StreamExt, stream};
use norted_core::ModelProfileId;
use norted_engine::{CompletionRequest, InferenceEvent, InferenceFinishReason, InferenceUsage};
use serde_json::{Value, json};
use std::convert::Infallible;
use uuid::Uuid;

pub(super) async fn create(
    State(state): State<PublicApiState>,
    Extension(correlation): Extension<RequestCorrelation>,
    headers: HeaderMap,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Response, OpenAiError> {
    let Json(value) = payload.map_err(|e| OpenAiError::malformed_json(&e))?;
    let object = object(&value)?;
    reject_unknown_fields(
        object,
        &[
            "model",
            "prompt",
            "stream",
            "stream_options",
            "max_tokens",
            "temperature",
            "top_p",
            "top_k",
            "min_p",
            "stop",
            "stop_token_ids",
            "seed",
            "frequency_penalty",
            "presence_penalty",
            "repeat_penalty",
            "logprobs",
            "echo",
            "best_of",
            "suffix",
            "n",
            "logit_bias",
            "user",
        ],
        "Completions",
    )?;
    let model = required_string(object, "model")?;
    let model_profile_id = ModelProfileId::new(model.clone()).map_err(|_| {
        OpenAiError::invalid("Invalid Model Profile ID.", Some("model"), "invalid_value")
    })?;
    let prompt = object
        .get("prompt")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            OpenAiError::unsupported(
                "`prompt` must be one raw string; token IDs and multiple prompts are unsupported.",
                "prompt",
            )
        })?
        .to_owned();
    let streaming = optional_bool(object, "stream", false)?;
    let include_usage =
        crate::chat::validate_stream_options(object.get("stream_options"), streaming)?;
    for name in ["n", "best_of"] {
        require_null_or(object, name, |v| v.as_u64() == Some(1), "1")?;
    }
    require_null_or(object, "echo", |v| v.as_bool() == Some(false), "false")?;
    for name in ["logprobs", "suffix"] {
        require_null_or(object, name, |_| false, "null")?;
    }
    require_null_or(
        object,
        "logit_bias",
        |v| v.as_object().is_some_and(|o| o.is_empty()),
        "an empty object",
    )?;
    optional_string(object, "user")?;
    let request = CompletionRequest {
        model_profile_id,
        prompt,
        generation_settings: generation_settings(object)?,
        max_output_tokens: optional_positive_u32(object, "max_tokens")?,
    };
    correlation.record_inference(&model, streaming);
    let routing = crate::inference_routing_context(&headers)?;
    let id = format!("cmpl_{}", Uuid::new_v4().simple());
    let created = crate::unix_timestamp();
    if !streaming {
        let output = state
            .runtime
            .complete_routed(request, routing)
            .await
            .map_err(runtime_error)?;
        if !output.tool_calls.is_empty() || output.finish_reason == InferenceFinishReason::ToolCalls
        {
            return Err(runtime_error(norted_engine::RuntimeError::Inference(
                "unexpected tool output from raw completion".to_owned(),
            )));
        }
        let mut document = json!({"id": id, "object": "text_completion", "created": created, "model": model,
            "choices": [{"text": output.text, "index": 0, "logprobs": null, "finish_reason": finish_reason(output.finish_reason)}]});
        if let Some(usage) = output.usage {
            document["usage"] = usage_document(&usage);
        }
        return Ok(Json(document).into_response());
    }
    let inference = state
        .runtime
        .complete_stream_routed(request, routing)
        .await
        .map_err(runtime_error)?;
    let events = stream::unfold(
        (inference, false, false),
        move |(mut inference, done, ended)| {
            let id = id.clone();
            let model = model.clone();
            async move {
                if ended {
                    return None;
                }
                if done {
                    return Some((
                        Ok::<_, Infallible>("data: [DONE]\n\n".to_owned()),
                        (inference, true, true),
                    ));
                }
                let mut document = json!({"id": id, "object": "text_completion", "created": created, "model": model});
                if include_usage {
                    document["usage"] = Value::Null;
                }
                let (data, done) = match inference.next().await {
                    Some(Ok(InferenceEvent::TextDelta { delta })) => {
                        document["choices"] = json!([{"text": delta, "index": 0, "logprobs": null, "finish_reason": null}]);
                        (format!("data: {document}\n\n"), false)
                    }
                    Some(Ok(InferenceEvent::Completed {
                        usage,
                        finish_reason: reason,
                    })) if reason != InferenceFinishReason::ToolCalls => {
                        document["choices"] = json!([{"text": "", "index": 0, "logprobs": null, "finish_reason": finish_reason(reason)}]);
                        let mut data = format!("data: {document}\n\n");
                        if include_usage && let Some(usage) = usage {
                            document["choices"] = json!([]);
                            document["usage"] = usage_document(&usage);
                            data.push_str(&format!("data: {document}\n\n"));
                        }
                        (data, true)
                    }
                    _ => (
                        format!(
                            "data: {}\n\n",
                            OpenAiError::streaming_backend_failure().envelope()
                        ),
                        true,
                    ),
                };
                Some((Ok(data), (inference, done, false)))
            }
        },
    );
    Ok((
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        Body::from_stream(events),
    )
        .into_response())
}

fn finish_reason(reason: InferenceFinishReason) -> &'static str {
    match reason {
        InferenceFinishReason::MaxOutputTokens => "length",
        _ => "stop",
    }
}
fn usage_document(usage: &InferenceUsage) -> Value {
    json!({"prompt_tokens": usage.input_tokens, "completion_tokens": usage.output_tokens, "total_tokens": usage.total_tokens})
}
