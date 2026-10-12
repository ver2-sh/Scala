//! Bounded OpenAI Decisions conversion; execution remains native and manager-owned.
use std::collections::{BTreeMap, BTreeSet};

use axum::{
    Json,
    extract::{Extension, State, rejection::JsonRejection},
    http::HeaderMap,
};
use scala_engine::{
    DecisionAnswer, DecisionContent, DecisionOutput, DecisionQuestion, DecisionRequest,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    PublicApiState,
    auth::RequestCorrelation,
    error::{OpenAiError, runtime_error},
    input::{object, reject_unknown_fields, required_string},
};

mod images;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(untagged)]
enum ChoiceValue {
    String(String),
    Boolean(bool),
}
impl ChoiceValue {
    fn json(&self) -> Value {
        match self {
            Self::String(v) => json!(v),
            Self::Boolean(v) => json!(v),
        }
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Choice {
    value: ChoiceValue,
    #[serde(default)]
    description: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Level {
    label: String,
    #[serde(default)]
    description: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Question {
    Predicate {
        instructions: String,
        #[serde(default)]
        name: Option<String>,
    },
    Choice {
        instructions: String,
        choices: Vec<Choice>,
        #[serde(default)]
        name: Option<String>,
    },
    Score {
        instructions: String,
        levels: Vec<Level>,
        #[serde(default)]
        name: Option<String>,
    },
}
impl Question {
    fn name(&self) -> &Option<String> {
        match self {
            Self::Predicate { name, .. } | Self::Choice { name, .. } | Self::Score { name, .. } => {
                name
            }
        }
    }
}
struct Conversion {
    model: String,
    request: DecisionRequest,
    questions: Vec<Question>,
}
fn invalid(message: impl Into<String>, field: impl Into<String>) -> OpenAiError {
    OpenAiError::invalid(message, Some(field), "invalid_value")
}
fn missing(message: impl Into<String>) -> OpenAiError {
    OpenAiError::link(
        axum::http::StatusCode::BAD_GATEWAY,
        message,
        "native_decision_contract_mismatch",
    )
}
fn key(index: usize) -> String {
    format!("q{index:08}")
}
fn choice_key(index: usize) -> String {
    format!("c{index:08}")
}

fn parse(value: &Value) -> Result<Conversion, OpenAiError> {
    let body = object(value)?;
    reject_unknown_fields(
        body,
        &["model", "input", "questions", "safety_identifier"],
        "Decisions",
    )?;
    let model = required_string(body, "model")?;
    if let Some(identifier) = body.get("safety_identifier")
        && !identifier.is_null()
        && identifier.as_str().is_none_or(|s| s.chars().count() > 128)
    {
        return Err(invalid(
            "safety_identifier must be a string of at most 128 characters or null.",
            "safety_identifier",
        ));
    }
    let model_profile_id = crate::execution_profile_id(model.clone())
        .map_err(|_| invalid("Invalid Model Profile ID.", "model"))?;
    let (state, images) = parse_input(
        body.get("input")
            .ok_or_else(|| invalid("input is required.", "input"))?,
    )?;
    let questions: Vec<Question> = serde_json::from_value(body.get("questions").cloned().unwrap_or(Value::Null)).map_err(|_| invalid("questions must be an ordered array of predicate, choice or score objects with valid fields.","questions"))?;
    // Local transport bound; native option/context limits are checked separately.
    if questions.is_empty() || questions.len() > 1024 {
        return Err(invalid(
            "Supply 1 to 1024 questions; the selected native model may impose a lower limit.",
            "questions",
        ));
    }
    let mut native = BTreeMap::new();
    for (index, question) in questions.iter().enumerate() {
        let q = match question {
            Question::Predicate { instructions, .. } => DecisionQuestion::Noul {
                instructions: Some(DecisionContent::String(instructions.clone())),
                criteria: None,
            },
            Question::Choice {
                instructions,
                choices,
                ..
            } => {
                if !(2..=255).contains(&choices.len()) {
                    return Err(invalid(
                        "choice requires 2 to 255 options; native model limits also apply.",
                        format!("questions[{index}].choices"),
                    ));
                }
                let mut seen = BTreeSet::new();
                let mut criteria = BTreeMap::new();
                for (i, choice) in choices.iter().enumerate() {
                    if !seen.insert(&choice.value) {
                        return Err(invalid(
                            "Choice values must be unique, including their JSON type.",
                            format!("questions[{index}].choices"),
                        ));
                    }
                    // Keys are private routing identifiers. Both the typed label and
                    // its description remain native structured criterion content.
                    let mut content = serde_json::Map::new();
                    content.insert("value".into(), choice.value.json());
                    if let Some(description) = &choice.description {
                        content.insert("description".into(), json!(description));
                    }
                    criteria.insert(choice_key(i), Some(DecisionContent::Object(content)));
                }
                DecisionQuestion::Choice {
                    instructions: Some(DecisionContent::String(instructions.clone())),
                    criteria,
                }
            }
            Question::Score {
                instructions,
                levels,
                ..
            } => {
                if levels.len() < 2 || levels.len() > 255 {
                    return Err(invalid(
                        "score requires 2 to 255 ordered levels; native model limits also apply.",
                        format!("questions[{index}].levels"),
                    ));
                }
                let criteria = levels
                    .iter()
                    .map(|level| {
                        let mut content = serde_json::Map::new();
                        content.insert("label".into(), json!(level.label));
                        if let Some(description) = &level.description {
                            content.insert("description".into(), json!(description));
                        }
                        DecisionContent::Object(content)
                    })
                    .collect();
                DecisionQuestion::Score {
                    instructions: Some(DecisionContent::String(instructions.clone())),
                    criteria,
                }
            }
        };
        native.insert(key(index), q);
    }
    Ok(Conversion {
        model,
        request: DecisionRequest {
            model_profile_id,
            state,
            questions: native,
            images,
        },
        questions,
    })
}

fn parse_input(input: &Value) -> Result<(DecisionContent, Vec<String>), OpenAiError> {
    if let Some(text) = input.as_str() {
        return Ok((DecisionContent::String(text.into()), vec![]));
    }
    let messages = input.as_array().filter(|m| !m.is_empty()).ok_or_else(|| {
        invalid(
            "input must be text or a nonempty ordered array of user messages.",
            "input",
        )
    })?;
    let mut native = Vec::new();
    let mut urls = Vec::new();
    for (i, message) in messages.iter().enumerate() {
        let message = message
            .as_object()
            .ok_or_else(|| invalid("Expected a user message.", format!("input[{i}]")))?;
        reject_unknown_fields(message, &["type", "role", "content"], "Decision message")?;
        if message.get("role").and_then(Value::as_str) != Some("user")
            || message
                .get("type")
                .is_some_and(|v| v.as_str() != Some("message"))
        {
            return Err(invalid(
                "Only user messages are representable by native Decisions.",
                format!("input[{i}]"),
            ));
        }
        if let Some(text) = message.get("content").and_then(Value::as_str) {
            native.push(json!({"role":"user","content":text}));
            continue;
        }
        let parts = message
            .get("content")
            .and_then(Value::as_array)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| {
                invalid(
                    "content must be a nonempty array of input_text/input_image parts.",
                    format!("input[{i}].content"),
                )
            })?;
        let mut content = Vec::new();
        for (j, part) in parts.iter().enumerate() {
            let field = format!("input[{i}].content[{j}]");
            let part = part
                .as_object()
                .ok_or_else(|| invalid("Expected a content object.", &field))?;
            match part.get("type").and_then(Value::as_str) {
                Some("input_text") => {
                    reject_unknown_fields(part, &["type", "text"], "Decision text")?;
                    content.push(json!({"type":"text","text":part.get("text").and_then(Value::as_str).ok_or_else(||invalid("text must be a string.",format!("{field}.text")))?}));
                }
                Some("input_image") => {
                    reject_unknown_fields(
                        part,
                        &["type", "image_url", "detail"],
                        "Decision image",
                    )?;
                    if part
                        .get("detail")
                        .is_some_and(|v| !v.is_null() && v.as_str() != Some("auto"))
                    {
                        return Err(OpenAiError::unsupported(
                            "The native Decision image route supports detail=auto only.",
                            format!("{field}.detail"),
                        ));
                    }
                    urls.push(required_string(part, "image_url")?);
                    // Preserve the requested position for adapter-specific admission.
                    // A native media list alone does not prove position preservation.
                    content
                        .push(json!({"type":"image_url","image_url":{"url":urls.last().unwrap()}}));
                }
                _ => {
                    return Err(invalid(
                        "Only input_text and input_image are supported.",
                        &field,
                    ));
                }
            }
        }
        native.push(json!({"role":"user","content":content}));
    }
    if urls.len() > scala_engine::MAX_DECISION_IMAGES {
        return Err(invalid(
            "The local Decision transport accepts at most two images; native model limits also apply.",
            "input",
        ));
    }
    Ok((DecisionContent::Array(native), urls))
}

fn probability(value: f64) -> Result<f64, OpenAiError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err(missing(
            "Native probability/confidence was invalid. Select a qualified runtime/model pair.",
        ))
    }
}
fn distribution<'a>(
    value: &'a Option<BTreeMap<String, f64>>,
    keys: &[String],
) -> Result<&'a BTreeMap<String, f64>, OpenAiError> {
    let value=value.as_ref().ok_or_else(|| missing("Native backend omitted the required distribution. Use /v1/systemone or select an OpenAI Decisions-qualified pair."))?;
    if value.len() != keys.len() || keys.iter().any(|k| !value.contains_key(k)) {
        return Err(missing(
            "Native distribution identifiers do not match the supplied options.",
        ));
    }
    for v in value.values() {
        probability(*v)?;
    }
    if (value.values().sum::<f64>() - 1.0).abs() > 1e-5 {
        return Err(missing("Native distribution is not normalized."));
    }
    Ok(value)
}
fn response(conversion: &Conversion, output: DecisionOutput) -> Result<Value, OpenAiError> {
    if output.answers.len() != conversion.questions.len() {
        return Err(missing(
            "Native backend did not return exactly one answer per question.",
        ));
    }
    let model = output.model.filter(|m| !m.is_empty()).ok_or_else(|| {
        missing("Native backend omitted model identity. Select a runtime which reports it.")
    })?;
    let mut answers = Vec::new();
    for (index, q) in conversion.questions.iter().enumerate() {
        let answer = output
            .answers
            .get(&key(index))
            .ok_or_else(|| missing("Native backend omitted a question answer."))?;
        let name = q.name();
        let answer = match (q, answer) {
            (_, DecisionAnswer::Refusal {}) => json!({"type":"refusal","name":name}),
            (
                Question::Predicate { .. },
                DecisionAnswer::Noul {
                    noul, observations, ..
                },
            ) => {
                if observations.abstained == Some(true) {
                    return Err(missing(
                        "Native abstention is not a typed refusal and cannot supply predicate probability.",
                    ));
                }
                json!({"type":"predicate","name":name,"probability":probability(*noul)?})
            }
            (
                Question::Choice { choices, .. },
                DecisionAnswer::Choice {
                    choice,
                    probabilities,
                    confidence,
                    observations,
                },
            ) => {
                if observations.abstained == Some(true) {
                    return Err(missing(
                        "Native abstention cannot supply an OpenAI choice answer.",
                    ));
                }
                let keys: Vec<_> = (0..choices.len()).map(choice_key).collect();
                let p = distribution(probabilities, &keys)?;
                let selected = keys
                    .iter()
                    .position(|k| k == choice)
                    .ok_or_else(|| missing("Native selected choice is outside the request."))?;
                json!({"type":"choice","name":name,"choice":choices[selected].value.json(),"confidence":probability(confidence.ok_or_else(|| missing("Native backend omitted required confidence."))?)?,"probabilities":choices.iter().enumerate().map(|(i,c)| json!({"value":c.value.json(),"probability":p[&keys[i]]})).collect::<Vec<_>>()})
            }
            (
                Question::Score { levels, .. },
                DecisionAnswer::Score {
                    score,
                    legend,
                    probabilities,
                    confidence,
                    observations,
                },
            ) => {
                if observations.abstained == Some(true) {
                    return Err(missing(
                        "Native abstention cannot supply an OpenAI score answer.",
                    ));
                }
                if let Some(legend) = legend {
                    let Some(DecisionQuestion::Score { criteria, .. }) =
                        conversion.request.questions.get(&key(index))
                    else {
                        unreachable!("converted score")
                    };
                    if legend.len() != criteria.len()
                        || criteria
                            .iter()
                            .enumerate()
                            .any(|(i, level)| legend.get(&i.to_string()) != Some(level))
                    {
                        return Err(missing(
                            "Native score legend differs from the supplied ordered levels.",
                        ));
                    }
                }
                let keys: Vec<_> = (0..levels.len()).map(|i| i.to_string()).collect();
                let p = distribution(probabilities, &keys)?;
                let expected = p
                    .iter()
                    .map(|(k, v)| k.parse::<usize>().unwrap() as f64 * v)
                    .sum::<f64>();
                if !score.is_finite() || (*score - expected).abs() > 1e-5 {
                    return Err(missing(
                        "Native score is not the expected value of ordered level indices.",
                    ));
                }
                json!({"type":"score","name":name,"score":score,"confidence":probability(confidence.ok_or_else(|| missing("Native backend omitted required confidence."))?)?,"probabilities":levels.iter().enumerate().map(|(i,l)| json!({"value":i,"label":l.label,"probability":p[&keys[i]]})).collect::<Vec<_>>()})
            }
            _ => {
                return Err(missing(
                    "Native answer type does not match the requested question.",
                ));
            }
        };
        answers.push(answer);
    }
    let usage = required_usage(output.native_usage.as_ref())?;
    Ok(json!({"model":model,"answers":answers,"usage":usage}))
}

fn required_usage(usage: Option<&Value>) -> Result<&Value, OpenAiError> {
    let usage = usage.ok_or_else(|| {
        missing("Native backend omitted required usage. Use /v1/systemone or select a pair with complete native usage observations.")
    })?;
    // Every counter must come from this decision's native response. System One
    // accounting and observations from other requests cannot complete this shape.
    let fields = ["input_tokens", "output_tokens", "total_tokens"];
    if !fields.iter().all(|f| usage[*f].as_u64().is_some())
        || usage
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .is_none()
        || usage
            .pointer("/input_tokens_details/cache_write_tokens")
            .and_then(Value::as_u64)
            .is_none()
        || usage
            .pointer("/output_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64)
            .is_none()
    {
        return Err(missing(
            "Native backend omitted or invalidated required usage counters. Use /v1/systemone or select a pair with complete native usage observations.",
        ));
    }
    if usage["input_tokens"]
        .as_u64()
        .unwrap()
        .checked_add(usage["output_tokens"].as_u64().unwrap())
        != usage["total_tokens"].as_u64()
    {
        return Err(missing("Native usage counters are inconsistent."));
    }
    Ok(usage)
}

/// Link owners may run an older response converter. A forwarded success must
/// still meet this endpoint's required fields before the entry node returns it.
pub(super) fn validate_required_response(value: &Value) -> Result<(), OpenAiError> {
    if value.get("answers").and_then(Value::as_array).is_none()
        || value.get("model").and_then(Value::as_str).is_none()
    {
        return Err(missing(
            "Decision owner omitted required answers or model identity.",
        ));
    }
    required_usage(value.get("usage"))?;
    Ok(())
}

pub(super) async fn create(
    State(state): State<PublicApiState>,
    Extension(correlation): Extension<RequestCorrelation>,
    headers: HeaderMap,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, OpenAiError> {
    let Json(value) = payload.map_err(|e| OpenAiError::malformed_json(&e))?;
    let mut conversion = parse(&value)?;
    correlation.record_inference(&conversion.model, false);
    images::acquire(&mut conversion.request).await?;
    conversion
        .request
        .validate()
        .map_err(|m| invalid(m, "input"))?;
    let output = state
        .runtime
        .decide_correlated_routed(
            conversion.request.clone(),
            crate::inference_routing_context(&headers)?,
            true,
            correlation.request_id.clone(),
        )
        .await
        .map_err(|error| runtime_error(error).for_decisions())?;
    Ok(Json(response(&conversion, output.output)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn body() -> Value {
        json!({"model":"fixture","input":"evidence","questions":[
  {"type":"predicate","instructions":"true?"},
  {"type":"choice","name":"same","instructions":"choose","choices":[{"value":true,"description":"boolean"},{"value":"true","description":"string"}]},
  {"type":"score","name":"same","instructions":"rate","levels":[{"label":"low","description":"first"},{"label":"high"}]},
  {"type":"predicate","name":null,"instructions":"declined?"}]})
    }
    fn native() -> DecisionOutput {
        let mut output: DecisionOutput = serde_json::from_value(json!({"model":"native-reported","answers":{
  "q00000000":{"type":"noul","noul":0.81},
  "q00000001":{"type":"choice","choice":"c00000000","probabilities":{"c00000000":0.75,"c00000001":0.25},"confidence":0.5},
  "q00000002":{"type":"score","score":0.25,"probabilities":{"0":0.75,"1":0.25},"confidence":0.5},
  "q00000003":{"type":"refusal"}}})).unwrap();
        output.native_usage = Some(complete_usage());
        output
    }
    fn complete_usage() -> Value {
        json!({"input_tokens":42,"input_tokens_details":{"cached_tokens":7,"cache_write_tokens":3},
            "output_tokens":5,"output_tokens_details":{"reasoning_tokens":2},"total_tokens":47})
    }
    #[test]
    fn preserves_order_nullable_names_typed_values_levels_and_refusal() {
        let c = parse(&body()).unwrap();
        let wire = serde_json::to_value(&c.request).unwrap();
        assert_eq!(
            wire["questions"]["q00000001"]["criteria"]["c00000000"],
            json!({"value":true,"description":"boolean"})
        );
        assert_eq!(
            wire["questions"]["q00000002"]["criteria"][0],
            json!({"label":"low","description":"first"})
        );
        let result = response(&c, native()).unwrap();
        assert_eq!(result["model"], "native-reported");
        assert_eq!(result["usage"], complete_usage());
        assert_eq!(
            result["answers"][0],
            json!({"type":"predicate","name":null,"probability":0.81})
        );
        assert_eq!(result["answers"][1]["choice"], true);
        assert_eq!(result["answers"][1]["probabilities"][1]["value"], "true");
        assert_eq!(
            result["answers"][2]["probabilities"][1],
            json!({"value":1,"label":"high","probability":0.25})
        );
        assert_eq!(result["answers"][3], json!({"type":"refusal","name":null}));
    }
    #[test]
    fn missing_or_invalid_native_observations_fail_without_fabrication() {
        let c = parse(&body()).unwrap();
        for field in ["model", "confidence", "probabilities", "score", "answer"] {
            let mut n = native();
            match field {
                "model" => n.model = None,
                "confidence" => {
                    if let DecisionAnswer::Choice { confidence, .. } =
                        n.answers.get_mut("q00000001").unwrap()
                    {
                        *confidence = None;
                    }
                }
                "probabilities" => {
                    if let DecisionAnswer::Score { probabilities, .. } =
                        n.answers.get_mut("q00000002").unwrap()
                    {
                        *probabilities = None;
                    }
                }
                "score" => {
                    if let DecisionAnswer::Score { score, .. } =
                        n.answers.get_mut("q00000002").unwrap()
                    {
                        *score = 0.9;
                    }
                }
                _ => {
                    n.answers.remove("q00000000");
                }
            };
            assert!(response(&c, n).is_err(), "{field}");
        }
        let mut n = native();
        n.native_usage = Some(json!({"input_tokens":2,"output_tokens":0}));
        assert_eq!(
            response(&c, n).unwrap_err().status,
            axum::http::StatusCode::BAD_GATEWAY
        );
    }
    #[test]
    fn required_usage_never_comes_from_systemone_accounting_or_unobserved_zeros() {
        let c = parse(&body()).unwrap();
        for usage in [
            None,
            Some(Value::Null),
            Some(json!({})),
            Some(json!({"input_tokens":42,"output_tokens":0})),
        ] {
            let mut n = native();
            n.native_usage = usage;
            n.usage = Some(scala_engine::InferenceUsage {
                input_tokens: 42,
                total_tokens: 42,
                ..Default::default()
            });
            let error = response(&c, n).unwrap_err();
            assert_eq!(error.status, axum::http::StatusCode::BAD_GATEWAY);
            assert_eq!(
                error.envelope()["error"]["code"],
                "native_decision_contract_mismatch"
            );
        }
        for path in [
            "/input_tokens",
            "/output_tokens",
            "/total_tokens",
            "/input_tokens_details/cached_tokens",
            "/input_tokens_details/cache_write_tokens",
            "/output_tokens_details/reasoning_tokens",
        ] {
            for invalid in [Value::Null, json!(-1), json!(0.5), json!("0")] {
                let mut n = native();
                *n.native_usage.as_mut().unwrap().pointer_mut(path).unwrap() = invalid;
                assert!(response(&c, n).is_err(), "{path}");
            }
        }
        for (input, output, total) in [(42, 5, 46), (u64::MAX, 1, 0)] {
            let mut n = native();
            let usage = n.native_usage.as_mut().unwrap();
            usage["input_tokens"] = json!(input);
            usage["output_tokens"] = json!(output);
            usage["total_tokens"] = json!(total);
            assert!(response(&c, n).is_err());
        }
    }

    #[test]
    fn rejects_roles_ambiguous_identifiers_unknown_fields_and_constraints() {
        for input in [
            json!([]),
            json!([{"role":"assistant","content":"x"}]),
            json!([{"role":"user","content":[{"type":"input_audio","data":"x"}]}]),
            json!([{"role":"user","content":[{"type":"input_image","image_url":"x","detail":"high"}]}]),
        ] {
            let mut b = body();
            b["input"] = input;
            assert!(parse(&b).is_err());
        }
        let mut b = body();
        b["questions"][1]["choices"][1]["value"] = json!(true);
        assert!(parse(&b).is_err());
        let mut b = body();
        b["questions"][0]["constraint"] = json!("chat");
        assert!(parse(&b).is_err());
        let mut b = body();
        b["input"] = json!([{"role":"user","content":""}]);
        assert!(parse(&b).is_ok());
    }
    #[tokio::test]
    async fn acquisition_retains_requested_positions_and_bytes_for_adapter_admission() {
        let image = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNgYGAAAAAEAAH2FzhVAAAAAElFTkSuQmCC";
        let image2 = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";
        let mut b = body();
        b["input"] = json!([{"role":"user","content":[{"type":"input_text","text":"before"},{"type":"input_image","image_url":image},{"type":"input_text","text":"middle"},{"type":"input_image","image_url":image2}]}]);
        let mut c = parse(&b).unwrap();
        images::acquire(&mut c.request).await.unwrap();
        assert_eq!(c.request.images, vec![image, image2]);
        c.request.validate().unwrap();
        let state = serde_json::to_value(c.request.state).unwrap();
        assert_eq!(state[0]["content"][2]["text"], "middle");
        assert_eq!(state[0]["content"][3]["image_url"]["url"], image2);
    }
    #[tokio::test]
    async fn public_route_uses_existing_auth_correlation_and_unsupported_profile_error() {
        use axum::{
            body::{Body, to_bytes},
            http::Request,
        };
        use tower::ServiceExt;
        let (_dir, runtime, _model, core) = crate::tests::control_fixture().await;
        let router = crate::public_routes(
            PublicApiState {
                core,
                runtime,
                instance_id: "fixture".into(),
                link: None,
            },
            crate::PublicAuth::disabled(),
        );
        let response = router
            .oneshot(
                Request::post("/v1/decisions")
                    .header("content-type", "application/json")
                    .body(Body::from(body().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        assert!(response.headers().contains_key("x-request-id"));
        let error: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 16384).await.unwrap()).unwrap();
        assert_eq!(error["error"]["code"], "unsupported_capability");
    }
}
