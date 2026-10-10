use std::collections::BTreeMap;

use axum::{
    Json,
    extract::{Extension, State, rejection::JsonRejection},
    http::HeaderMap,
};
use scala_engine::{DecisionContent, DecisionQuestion, DecisionRequest};
use serde_json::{Value, json};

use crate::{
    PublicApiState,
    auth::RequestCorrelation,
    error::{OpenAiError, runtime_error},
    input::{object, reject_unknown_fields, required_string},
};

fn parse(value: &Value) -> Result<(String, DecisionRequest), OpenAiError> {
    let object = object(value)?;
    reject_unknown_fields(
        object,
        &["model", "state", "questions", "images"],
        "Decision",
    )?;
    let model = required_string(object, "model")?;
    let model_profile_id = crate::execution_profile_id(model.clone()).map_err(|_| {
        OpenAiError::invalid("Invalid Model Profile ID.", Some("model"), "invalid_value")
    })?;
    let state: DecisionContent = serde_json::from_value(
        object.get("state").cloned().unwrap_or(Value::Null),
    )
    .map_err(|_| {
        OpenAiError::invalid(
            "`state` must be a string, JSON object, or array.",
            Some("state"),
            "invalid_value",
        )
    })?;
    let questions: BTreeMap<String, DecisionQuestion> = serde_json::from_value(
        object.get("questions").cloned().unwrap_or(Value::Null),
    ).map_err(|_| OpenAiError::invalid(
        "`questions` must be a named map of choice, score, or noul questions with valid criteria.",
        Some("questions"), "invalid_value",
    ))?;
    let request = DecisionRequest {
        model_profile_id,
        state,
        questions,
        images: match object.get("images") {
            None => Vec::new(),
            Some(value) => serde_json::from_value(value.clone()).map_err(|_| {
                OpenAiError::invalid(
                    "`images` must be an array of data URL strings.",
                    Some("images"),
                    "invalid_value",
                )
            })?,
        },
    };
    request
        .validate_images()
        .map_err(|message| OpenAiError::invalid(message, Some("images"), "invalid_value"))?;
    request
        .validate()
        .map_err(|message| OpenAiError::invalid(message, Some("questions"), "invalid_value"))?;
    Ok((model, request))
}

pub(super) async fn create(
    State(state): State<PublicApiState>,
    Extension(correlation): Extension<RequestCorrelation>,
    headers: HeaderMap,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, OpenAiError> {
    let Json(value) = payload.map_err(|error| OpenAiError::malformed_json(&error))?;
    let (model, request) = parse(&value)?;
    correlation.record_inference(&model, false);
    let routed = state
        .runtime
        .decide_routed(request, crate::inference_routing_context(&headers)?)
        .await
        .map_err(runtime_error)?;
    let mut response = json!({
        "model": routed.output.model.as_deref().unwrap_or(&model),
        "answers": routed.output.answers,
        "scala": routed.identity,
    });
    if let Some(usage) = routed.output.usage {
        response["usage"] = json!({
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
        });
    }
    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use axum::response::IntoResponse;
    use tower::ServiceExt;

    fn body(state: Value) -> Value {
        json!({
            "model":"fixture", "state":state,
            "questions":{
                "route":{"type":"choice","instructions":{"task":"route"},"criteria":{"yes":null,"no":"decline"}},
                "urgency":{"type":"score","criteria":["low",{"level":"high"}]},
                "safe":{"type":"noul","instructions":["evaluate","safety"],"criteria":{"true":"safe","false":null}}
            }
        })
    }

    #[test]
    fn systemone_parses_all_question_types_and_preserves_state() {
        for state in [
            json!("context"),
            json!({"message":"context"}),
            json!(["context",{"n":1}]),
        ] {
            let (model, request) = parse(&body(state.clone())).unwrap();
            assert_eq!(model, "fixture");
            assert_eq!(request.model_profile_id.as_str(), "fixture");
            assert_eq!(serde_json::to_value(request.state).unwrap(), state);
            assert_eq!(request.questions.len(), 3);
            assert!(request.images.is_empty());
            assert!(matches!(
                request.questions["route"],
                DecisionQuestion::Choice { .. }
            ));
            assert!(matches!(
                request.questions["urgency"],
                DecisionQuestion::Score { .. }
            ));
            assert!(matches!(
                request.questions["safe"],
                DecisionQuestion::Noul { .. }
            ));
        }
    }

    #[tokio::test]
    async fn systemone_preserves_native_images_and_rejects_invalid_transport() {
        let mut value = body(json!({"context":"compare"}));
        value["images"] = json!(["data:image/png;base64,YQ==", "data:image/webp;base64,Yg=="]);
        let (_, request) = parse(&value).unwrap();
        assert_eq!(json!(request.images), value["images"]);
        value["images"] = json!([]);
        assert!(parse(&value).unwrap().1.images.is_empty());
        for images in [
            Value::Null,
            json!("data:image/png;base64,YQ=="),
            json!([1]),
            json!(["data:image/jpeg;base64,!"]),
            json!(["data:image/gif;base64,YQ=="]),
            json!([
                "data:image/png;base64,YQ==",
                "data:image/png;base64,YQ==",
                "data:image/png;base64,YQ=="
            ]),
        ] {
            value["images"] = images;
            let error = parse(&value).unwrap_err();
            assert_eq!(error.status, StatusCode::BAD_REQUEST);
            let response = error.into_response();
            let body: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap())
                    .unwrap();
            assert_eq!(body["error"]["param"], "images");
        }
    }

    #[test]
    fn systemone_rejects_invalid_shapes_and_generation_controls() {
        for state in [Value::Null, json!(true), json!(1)] {
            assert_eq!(
                parse(&body(state)).unwrap_err().status,
                StatusCode::BAD_REQUEST
            );
        }
        for questions in [
            json!({}),
            json!([]),
            json!({"q":{"type":"chat"}}),
            json!({"q":{"type":"choice","criteria":[]}}),
            json!({"q":{"type":"choice","criteria":{}}}),
            json!({"q":{"type":"score","criteria":{}}}),
            json!({"q":{"type":"score","criteria":[]}}),
            json!({"q":{"type":"score","criteria":[null]}}),
            json!({"q":{"type":"noul","criteria":{"maybe":"unknown"}}}),
            json!({"q":{"type":"noul","prompt":"emulate"}}),
        ] {
            let mut value = body(json!("context"));
            value["questions"] = questions;
            assert_eq!(parse(&value).unwrap_err().status, StatusCode::BAD_REQUEST);
        }
        for field in ["stream", "temperature", "logprobs", "messages"] {
            let mut value = body(json!("context"));
            value[field] = json!(true);
            assert!(parse(&value).is_err());
        }
    }

    #[tokio::test]
    async fn systemone_route_returns_the_existing_unsupported_envelope_without_loading() {
        let (_temporary, runtime, _model, core) = crate::tests::control_fixture().await;
        let router = crate::public_routes(
            PublicApiState {
                core,
                runtime: runtime.clone(),
                instance_id: "synthetic".into(),
                link: None,
            },
            crate::PublicAuth::disabled(),
        );
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let image = format!(
            "data:image/png;base64,{}",
            STANDARD.encode(vec![0; scala_engine::MAX_DECISION_IMAGE_BYTES])
        );
        for images in [None, Some(json!([image, image]))] {
            let mut value = body(json!({"message":"context"}));
            if let Some(images) = images {
                value["images"] = images;
            }
            let response = router
                .clone()
                .oneshot(
                    Request::post("/v1/systemone")
                        .header("content-type", "application/json")
                        .body(Body::from(value.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert!(response.headers().contains_key("x-request-id"));
            let value: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 16384).await.unwrap())
                    .unwrap();
            assert_eq!(value["error"]["code"], "unsupported_capability");
            assert_eq!(value["error"]["param"], "model");
            assert_eq!(value["error"]["type"], "invalid_request_error");
            assert!(runtime.status().await.backends.is_empty());
        }
    }
}
