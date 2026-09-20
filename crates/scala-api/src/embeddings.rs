use crate::{
    PublicApiState,
    auth::RequestCorrelation,
    error::{OpenAiError, runtime_error},
    input::{object, optional_string, reject_unknown_fields, required_string},
};
use axum::{
    Json,
    extract::{Extension, State, rejection::JsonRejection},
    http::HeaderMap,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use scala_engine::EmbeddingRequest;
use serde_json::{Value, json};

pub(super) async fn create(
    State(state): State<PublicApiState>,
    Extension(correlation): Extension<RequestCorrelation>,
    headers: HeaderMap,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, OpenAiError> {
    let Json(value) = payload.map_err(|e| OpenAiError::malformed_json(&e))?;
    let object = object(&value)?;
    reject_unknown_fields(
        object,
        &["model", "input", "encoding_format", "dimensions", "user"],
        "Embeddings",
    )?;
    let model = required_string(object, "model")?;
    let model_profile_id = crate::execution_profile_id(model.clone()).map_err(|_| {
        OpenAiError::invalid("Invalid Model Profile ID.", Some("model"), "invalid_value")
    })?;
    if object.contains_key("dimensions") {
        return Err(OpenAiError::unsupported(
            "`dimensions` is not supported by the embedding runtime.",
            "dimensions",
        ));
    }
    optional_string(object, "user")?;
    let encoding =
        optional_string(object, "encoding_format")?.unwrap_or_else(|| "float".to_owned());
    if !matches!(encoding.as_str(), "float" | "base64") {
        return Err(OpenAiError::unsupported(
            "`encoding_format` must be float or base64.",
            "encoding_format",
        ));
    }
    let input = match object.get("input") {
        Some(Value::String(text)) => vec![text.clone()],
        Some(Value::Array(items)) if !items.is_empty() => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                OpenAiError::unsupported(
                    "Only string and string-array embedding inputs are supported.",
                    "input",
                )
            })?,
        _ => {
            return Err(OpenAiError::invalid(
                "`input` must be a string or non-empty string array.",
                Some("input"),
                "invalid_value",
            ));
        }
    };
    if input.iter().any(String::is_empty) {
        return Err(OpenAiError::invalid(
            "Embedding inputs must not be empty strings.",
            Some("input"),
            "invalid_value",
        ));
    }
    correlation.record_inference(&model, false);
    let output = state
        .runtime
        .embed_routed(
            EmbeddingRequest {
                model_profile_id,
                input,
            },
            crate::inference_routing_context(&headers)?,
        )
        .await
        .map_err(runtime_error)?;
    let data = output
        .vectors
        .into_iter()
        .enumerate()
        .map(|(index, vector)| {
            let embedding = if encoding == "base64" {
                json!(
                    STANDARD.encode(
                        vector
                            .iter()
                            .flat_map(|v| v.to_le_bytes())
                            .collect::<Vec<_>>()
                    )
                )
            } else {
                json!(vector)
            };
            json!({"object": "embedding", "index": index, "embedding": embedding})
        })
        .collect::<Vec<_>>();
    let mut response = json!({"object": "list", "data": data, "model": model});
    if let (Some(prompt), Some(total)) = (output.prompt_tokens, output.total_tokens) {
        response["usage"] = json!({"prompt_tokens": prompt, "total_tokens": total});
    }
    Ok(Json(response))
}
