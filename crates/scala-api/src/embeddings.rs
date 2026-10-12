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
    let mut input: scala_engine::EmbeddingInput = serde_json::from_value(
        object.get("input").cloned().ok_or_else(|| invalid_input("Missing embedding input."))?,
    ).map_err(|_| invalid_input("`input` must be a string, content object, or nonempty ordered array of strings/content objects; only typed text, image_url, input_audio and input_video parts are accepted."))?;
    input.validate_structure().map_err(invalid_input)?;
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        for entry in input.entries_mut() {
            if let scala_engine::EmbeddingEntry::Content(group) = entry {
                for part in &mut group.content {
                    if let Some((kind, source)) = part.media_mut() {
                        *source = crate::media::acquire(kind, source).await?;
                    }
                }
            }
        }
        Ok::<_, OpenAiError>(())
    })
    .await
    .map_err(|_| invalid_input("Embedding media acquisition exceeded 30 seconds."))??;
    input.validate_inline_media().map_err(invalid_input)?;
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

fn invalid_input(message: impl Into<String>) -> OpenAiError {
    OpenAiError::invalid(message, Some("input"), "invalid_value")
}
