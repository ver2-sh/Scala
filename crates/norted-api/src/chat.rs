use std::collections::VecDeque;
use std::convert::Infallible;

use axum::Json;
use axum::body::Body;
use axum::extract::{Extension, State, rejection::JsonRejection};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::{StreamExt, stream};
use norted_engine::{
    InferenceContentPart, InferenceEvent, InferenceFinishReason, InferenceMessage, InferenceOutput,
    InferenceRole, InferenceStream, InferenceTool, InferenceToolCall, InferenceToolChoice,
    InferenceUsage, OutputFormat,
};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::auth::RequestCorrelation;
use crate::error::{OpenAiError, runtime_error};
use crate::input::{
    NormalizedRequest, generation_settings, object, optional_bool, optional_bool_value,
    optional_nonnegative_u32, optional_positive_u32, optional_reasoning_effort, optional_string,
    reject_unknown_fields, require_null_or, required_string, role,
};
use crate::{PublicApiState, unix_timestamp};

const ALLOWED_TOP_LEVEL_FIELDS: &[&str] = &[
    "audio",
    "frequency_penalty",
    "enable_thinking",
    "function_call",
    "functions",
    "logit_bias",
    "logprobs",
    "max_completion_tokens",
    "max_tokens",
    "messages",
    "metadata",
    "modalities",
    "model",
    "n",
    "parallel_tool_calls",
    "prediction",
    "presence_penalty",
    "repeat_penalty",
    "reasoning_effort",
    "response_format",
    "seed",
    "service_tier",
    "stop",
    "store",
    "stream",
    "stream_options",
    "temperature",
    "thinking_token_budget",
    "tool_choice",
    "tools",
    "top_logprobs",
    "top_p",
    "top_k",
    "min_p",
    "user",
    "verbosity",
    "web_search_options",
];

pub(super) async fn create(
    State(state): State<PublicApiState>,
    Extension(correlation): Extension<RequestCorrelation>,
    headers: HeaderMap,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Response, OpenAiError> {
    let Json(value) = payload.map_err(|error| OpenAiError::malformed_json(&error))?;
    let parsed = parse_request(value)?;
    correlation.record_inference(&parsed.normalized.model, parsed.normalized.stream);
    let completion_id = format!("chatcmpl_{}", Uuid::new_v4().simple());
    let created = unix_timestamp();
    let model = parsed.normalized.model.clone();
    let inference = parsed.normalized.inference_request()?;
    let routing = crate::inference_routing_context(&headers)?;
    if parsed.normalized.stream {
        let routed = state
            .runtime
            .infer_stream_routed(inference, routing)
            .await
            .map_err(runtime_error)?;
        Ok(streaming_response(
            ChatContext {
                completion_id,
                created,
                model,
                include_usage: parsed.include_usage,
            },
            routed.stream,
        ))
    } else {
        let routed = state
            .runtime
            .infer_routed(inference, routing)
            .await
            .map_err(runtime_error)?;
        Ok(Json(completion_document(
            &ChatContext {
                completion_id,
                created,
                model,
                include_usage: false,
            },
            &routed.output,
        ))
        .into_response())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedRequest {
    pub(crate) normalized: NormalizedRequest,
    include_usage: bool,
}

pub(crate) fn parse_request(value: Value) -> Result<ParsedRequest, OpenAiError> {
    let object = object(&value)?;
    reject_unknown_fields(object, ALLOWED_TOP_LEVEL_FIELDS, "Chat Completions")?;
    let model = required_string(object, "model")?;
    let stream = optional_bool(object, "stream", false)?;
    let include_usage = validate_identity_fields(object, stream)?;
    validate_n(object.get("n"))?;
    let max_output_tokens = token_limit(object)?;
    let mut generation_settings = generation_settings(object)?;
    generation_settings.reasoning_effort =
        optional_reasoning_effort(object.get("reasoning_effort"), "reasoning_effort")?;
    generation_settings.reasoning_enabled = optional_bool_value(object, "enable_thinking")?;
    generation_settings.reasoning_budget =
        optional_nonnegative_u32(object, "thinking_token_budget")?.map(i64::from);
    let tools = parse_tools(object.get("tools"))?;
    let tool_choice = parse_tool_choice(object.get("tool_choice"), &tools)?;
    let parallel_tool_calls = optional_bool_value(object, "parallel_tool_calls")?;
    let messages = object
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            OpenAiError::invalid(
                "`messages` must be a non-empty array of text messages.",
                Some("messages"),
                "missing_required_parameter",
            )
        })?;
    if messages.is_empty() {
        return Err(OpenAiError::invalid(
            "`messages` must contain at least one message.",
            Some("messages"),
            "invalid_value",
        ));
    }
    let messages = messages
        .iter()
        .enumerate()
        .map(|(index, message)| parse_message(message, index))
        .collect::<Result<Vec<_>, _>>()?;
    let output_format = parse_response_format(object.get("response_format"))?;
    Ok(ParsedRequest {
        normalized: NormalizedRequest {
            model,
            messages,
            max_output_tokens,
            generation_settings,
            tools,
            tool_choice,
            parallel_tool_calls,
            output_format,
            stream,
        },
        include_usage,
    })
}

fn parse_message(value: &Value, index: usize) -> Result<InferenceMessage, OpenAiError> {
    let parameter = format!("messages[{index}]");
    let message = value.as_object().ok_or_else(|| {
        OpenAiError::invalid(
            "Each Chat message must be an object.",
            Some(parameter.clone()),
            "invalid_type",
        )
    })?;
    reject_unknown_fields(
        message,
        &[
            "role",
            "content",
            "name",
            "audio",
            "function_call",
            "refusal",
            "tool_call_id",
            "tool_calls",
        ],
        "Chat message",
    )?;
    for field in ["name", "audio", "function_call", "refusal"] {
        require_null_or(message, field, |_| false, "`null`")?;
    }
    let role = role(message.get("role"), &format!("{parameter}.role"))?;
    let tool_calls = parse_message_tool_calls(message.get("tool_calls"), &parameter, role)?;
    let content = match message.get("content") {
        None | Some(Value::Null) if role == InferenceRole::Assistant && !tool_calls.is_empty() => {
            Vec::new()
        }
        None | Some(Value::Null) => {
            return Err(OpenAiError::invalid(
                "Chat messages require `content` unless an assistant message contains tool calls.",
                Some(format!("{parameter}.content")),
                "missing_required_parameter",
            ));
        }
        Some(content) => parse_chat_content(content, &format!("{parameter}.content"), role)?,
    };
    let tool_call_id = match role {
        InferenceRole::Tool => Some(
            message
                .get("tool_call_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    OpenAiError::invalid(
                        "Tool messages require a non-empty `tool_call_id`.",
                        Some(format!("{parameter}.tool_call_id")),
                        "missing_required_parameter",
                    )
                })?
                .to_owned(),
        ),
        _ => {
            require_null_or(message, "tool_call_id", |_| false, "`null`")?;
            None
        }
    };
    Ok(InferenceMessage {
        role,
        content,
        tool_calls,
        tool_call_id,
    })
}

fn parse_chat_content(
    value: &Value,
    parameter: &str,
    role: InferenceRole,
) -> Result<Vec<InferenceContentPart>, OpenAiError> {
    if let Some(text) = value.as_str() {
        return Ok(vec![InferenceContentPart::Text {
            text: text.to_owned(),
        }]);
    }
    let parts = value.as_array().ok_or_else(|| {
        OpenAiError::invalid(
            "Message `content` must be a string or an array of content parts.",
            Some(parameter),
            "invalid_type",
        )
    })?;
    let mut content = Vec::with_capacity(parts.len());
    for (index, part) in parts.iter().enumerate() {
        let part_parameter = format!("{parameter}[{index}]");
        let part = part.as_object().ok_or_else(|| {
            OpenAiError::invalid(
                "Content parts must be JSON objects.",
                Some(part_parameter.clone()),
                "invalid_type",
            )
        })?;
        match part.get("type").and_then(Value::as_str) {
            Some("text") => {
                reject_unknown_fields(part, &["type", "text"], "Chat text content")?;
                let text = part.get("text").and_then(Value::as_str).ok_or_else(|| {
                    OpenAiError::invalid(
                        "Text content requires a string `text` field.",
                        Some(format!("{part_parameter}.text")),
                        "missing_required_parameter",
                    )
                })?;
                content.push(InferenceContentPart::Text {
                    text: text.to_owned(),
                });
            }
            Some("image_url") if matches!(role, InferenceRole::User | InferenceRole::Tool) => {
                reject_unknown_fields(part, &["type", "image_url"], "Chat image content")?;
                let image = part
                    .get("image_url")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        OpenAiError::invalid(
                            "`image_url` content requires an object.",
                            Some(format!("{part_parameter}.image_url")),
                            "invalid_type",
                        )
                    })?;
                reject_unknown_fields(image, &["url", "detail"], "Chat image URL")?;
                require_null_or(
                    image,
                    "detail",
                    |value| matches!(value.as_str(), Some("auto")),
                    "`\"auto\"`",
                )?;
                let url = media_url(image.get("url"), &format!("{part_parameter}.image_url.url"))?;
                content.push(InferenceContentPart::ImageUrl { url });
            }
            Some("video_url") if role == InferenceRole::User => {
                reject_unknown_fields(part, &["type", "video_url"], "Chat video content")?;
                let video = part
                    .get("video_url")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        OpenAiError::invalid(
                            "`video_url` content requires an object.",
                            Some(format!("{part_parameter}.video_url")),
                            "invalid_type",
                        )
                    })?;
                reject_unknown_fields(video, &["url"], "Chat video URL")?;
                let url = media_url(video.get("url"), &format!("{part_parameter}.video_url.url"))?;
                content.push(InferenceContentPart::VideoUrl { url });
            }
            Some(kind) => {
                return Err(OpenAiError::unsupported(
                    format!("Unsupported `{kind}` content for this message role."),
                    format!("{part_parameter}.type"),
                ));
            }
            None => {
                return Err(OpenAiError::invalid(
                    "Content parts require a string `type`.",
                    Some(format!("{part_parameter}.type")),
                    "missing_required_parameter",
                ));
            }
        }
    }
    Ok(content)
}

fn media_url(value: Option<&Value>, parameter: &str) -> Result<String, OpenAiError> {
    let url = value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            OpenAiError::invalid(
                "Media URLs must be non-empty strings.",
                Some(parameter),
                "invalid_value",
            )
        })?;
    if !(url.starts_with("https://") || url.starts_with("http://") || url.starts_with("data:")) {
        return Err(OpenAiError::unsupported(
            "Media input supports only HTTP(S) and data URLs; local file paths are forbidden.",
            parameter,
        ));
    }
    Ok(url.to_owned())
}

fn parse_message_tool_calls(
    value: Option<&Value>,
    parameter: &str,
    role: InferenceRole,
) -> Result<Vec<InferenceToolCall>, OpenAiError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(Vec::new());
    };
    if role != InferenceRole::Assistant {
        return Err(OpenAiError::unsupported(
            "Only assistant messages may contain `tool_calls`.",
            format!("{parameter}.tool_calls"),
        ));
    }
    let calls = value.as_array().ok_or_else(|| {
        OpenAiError::invalid(
            "`tool_calls` must be an array.",
            Some(format!("{parameter}.tool_calls")),
            "invalid_type",
        )
    })?;
    calls
        .iter()
        .enumerate()
        .map(|(index, call)| {
            let call_parameter = format!("{parameter}.tool_calls[{index}]");
            let call = call.as_object().ok_or_else(|| {
                OpenAiError::invalid(
                    "Tool calls must be objects.",
                    Some(call_parameter.clone()),
                    "invalid_type",
                )
            })?;
            reject_unknown_fields(call, &["id", "type", "function"], "assistant tool call")?;
            if call.get("type").and_then(Value::as_str) != Some("function") {
                return Err(OpenAiError::unsupported(
                    "Only function tool calls are supported.",
                    format!("{call_parameter}.type"),
                ));
            }
            let id = required_nonempty(call.get("id"), &format!("{call_parameter}.id"))?;
            let function = call
                .get("function")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    OpenAiError::invalid(
                        "Tool calls require a `function` object.",
                        Some(format!("{call_parameter}.function")),
                        "missing_required_parameter",
                    )
                })?;
            reject_unknown_fields(function, &["name", "arguments"], "tool call function")?;
            Ok(InferenceToolCall {
                id,
                name: required_nonempty(
                    function.get("name"),
                    &format!("{call_parameter}.function.name"),
                )?,
                arguments: required_nonempty(
                    function.get("arguments"),
                    &format!("{call_parameter}.function.arguments"),
                )?,
            })
        })
        .collect()
}

fn parse_tools(value: Option<&Value>) -> Result<Vec<InferenceTool>, OpenAiError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(Vec::new());
    };
    let tools = value.as_array().ok_or_else(|| {
        OpenAiError::invalid("`tools` must be an array.", Some("tools"), "invalid_type")
    })?;
    let mut parsed = Vec::with_capacity(tools.len());
    for (index, tool) in tools.iter().enumerate() {
        let parameter = format!("tools[{index}]");
        let tool = tool.as_object().ok_or_else(|| {
            OpenAiError::invalid(
                "Tools must be objects.",
                Some(parameter.clone()),
                "invalid_type",
            )
        })?;
        reject_unknown_fields(tool, &["type", "function"], "Chat tool")?;
        if tool.get("type").and_then(Value::as_str) != Some("function") {
            return Err(OpenAiError::unsupported(
                "Only function tools are supported.",
                format!("{parameter}.type"),
            ));
        }
        let function = tool
            .get("function")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                OpenAiError::invalid(
                    "Function tools require a `function` object.",
                    Some(format!("{parameter}.function")),
                    "missing_required_parameter",
                )
            })?;
        reject_unknown_fields(
            function,
            &["name", "description", "parameters", "strict"],
            "function tool",
        )?;
        require_null_or(function, "strict", |value| value == false, "`false`")?;
        let name = required_nonempty(function.get("name"), &format!("{parameter}.function.name"))?;
        if parsed
            .iter()
            .any(|existing: &InferenceTool| existing.name == name)
        {
            return Err(OpenAiError::invalid(
                "Function tool names must be unique.",
                Some(format!("{parameter}.function.name")),
                "duplicate_parameter",
            ));
        }
        let parameters = function
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
        if !parameters.is_object() {
            return Err(OpenAiError::invalid(
                "Function `parameters` must be a JSON object.",
                Some(format!("{parameter}.function.parameters")),
                "invalid_type",
            ));
        }
        parsed.push(InferenceTool {
            name,
            description: function
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned),
            parameters,
        });
    }
    Ok(parsed)
}

fn parse_tool_choice(
    value: Option<&Value>,
    tools: &[InferenceTool],
) -> Result<Option<InferenceToolChoice>, OpenAiError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let choice = match value {
        Value::String(value) => match value.as_str() {
            "auto" => InferenceToolChoice::Auto,
            "none" => InferenceToolChoice::None,
            "required" => InferenceToolChoice::Required,
            _ => {
                return Err(OpenAiError::unsupported(
                    "`tool_choice` must be `auto`, `none`, `required`, or a named function.",
                    "tool_choice",
                ));
            }
        },
        Value::Object(choice) => {
            reject_unknown_fields(choice, &["type", "function"], "tool_choice")?;
            if choice.get("type").and_then(Value::as_str) != Some("function") {
                return Err(OpenAiError::unsupported(
                    "Only named function tool choices are supported.",
                    "tool_choice.type",
                ));
            }
            let function = choice
                .get("function")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    OpenAiError::invalid(
                        "Named tool choice requires a `function` object.",
                        Some("tool_choice.function"),
                        "missing_required_parameter",
                    )
                })?;
            reject_unknown_fields(function, &["name"], "named tool choice")?;
            InferenceToolChoice::Function {
                name: required_nonempty(function.get("name"), "tool_choice.function.name")?,
            }
        }
        _ => {
            return Err(OpenAiError::invalid(
                "`tool_choice` must be a string or object.",
                Some("tool_choice"),
                "invalid_type",
            ));
        }
    };
    if matches!(
        choice,
        InferenceToolChoice::Required | InferenceToolChoice::Function { .. }
    ) && tools.is_empty()
    {
        return Err(OpenAiError::invalid(
            "This `tool_choice` requires at least one declared function tool.",
            Some("tool_choice"),
            "invalid_value",
        ));
    }
    if let InferenceToolChoice::Function { name } = &choice
        && !tools.iter().any(|tool| tool.name == *name)
    {
        return Err(OpenAiError::invalid(
            "Named `tool_choice` must reference a declared function.",
            Some("tool_choice.function.name"),
            "invalid_value",
        ));
    }
    Ok(Some(choice))
}

fn required_nonempty(value: Option<&Value>, parameter: &str) -> Result<String, OpenAiError> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            OpenAiError::invalid(
                format!("`{parameter}` must be a non-empty string."),
                Some(parameter),
                "invalid_value",
            )
        })
}

fn token_limit(object: &Map<String, Value>) -> Result<Option<u32>, OpenAiError> {
    let current = optional_positive_u32(object, "max_completion_tokens")?;
    let legacy = optional_positive_u32(object, "max_tokens")?;
    match (current, legacy) {
        (Some(current), Some(legacy)) if current != legacy => Err(OpenAiError::invalid(
            "`max_completion_tokens` and legacy `max_tokens` must match when both are supplied.",
            Some("max_completion_tokens"),
            "conflicting_parameters",
        )),
        (Some(value), Some(_)) => Ok(Some(value)),
        (Some(value), None) | (None, Some(value)) => Ok(Some(value)),
        (None, None) => Ok(None),
    }
}

fn validate_n(value: Option<&Value>) -> Result<(), OpenAiError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(());
    };
    if value.as_u64() == Some(1) {
        Ok(())
    } else {
        Err(OpenAiError::unsupported(
            "Norted supports exactly one Chat completion (`n = 1`).",
            "n",
        ))
    }
}

fn validate_identity_fields(
    object: &Map<String, Value>,
    stream: bool,
) -> Result<bool, OpenAiError> {
    require_null_or(object, "store", |value| value == false, "`false`")?;
    require_null_or(
        object,
        "functions",
        |value| value.as_array().is_some_and(Vec::is_empty),
        "an empty array",
    )?;
    require_null_or(
        object,
        "function_call",
        |value| value.as_str() == Some("none"),
        "`\"none\"`",
    )?;
    require_null_or(
        object,
        "metadata",
        |value| value.as_object().is_some_and(serde_json::Map::is_empty),
        "an empty object",
    )?;
    require_null_or(
        object,
        "modalities",
        |value| {
            value
                .as_array()
                .is_some_and(|items| items.len() == 1 && items[0].as_str() == Some("text"))
        },
        "`[\"text\"]`",
    )?;
    require_null_or(
        object,
        "logit_bias",
        |value| value.as_object().is_some_and(serde_json::Map::is_empty),
        "an empty object",
    )?;
    require_null_or(object, "logprobs", |value| value == false, "`false`")?;
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
    require_null_or(object, "user", Value::is_string, "a string")?;
    for field in ["audio", "prediction", "verbosity", "web_search_options"] {
        require_null_or(object, field, |_| false, "`null`")?;
    }
    validate_stream_options(object.get("stream_options"), stream)
}

fn parse_response_format(value: Option<&Value>) -> Result<Option<OutputFormat>, OpenAiError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let format = value.as_object().ok_or_else(|| {
        OpenAiError::invalid(
            "`response_format` must be an object.",
            Some("response_format"),
            "invalid_type",
        )
    })?;
    match format.get("type").and_then(Value::as_str) {
        Some("text") => {
            reject_unknown_fields(format, &["type"], "Chat response_format")?;
            Ok(Some(OutputFormat::Text))
        }
        Some("json_object") => {
            reject_unknown_fields(format, &["type"], "Chat response_format")?;
            Ok(Some(OutputFormat::JsonObject))
        }
        Some("json_schema") => {
            reject_unknown_fields(format, &["type", "json_schema"], "Chat response_format")?;
            let wrapper = format
                .get("json_schema")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    OpenAiError::invalid(
                        "`response_format.json_schema` must be an object.",
                        Some("response_format.json_schema"),
                        "invalid_type",
                    )
                })?;
            reject_unknown_fields(
                wrapper,
                &["name", "description", "schema", "strict"],
                "Chat JSON Schema",
            )?;
            let name = optional_string(wrapper, "name")?;
            if name.as_ref().is_some_and(|name| name.trim().is_empty()) {
                return Err(OpenAiError::invalid(
                    "`response_format.json_schema.name` must not be empty.",
                    Some("response_format.json_schema.name"),
                    "invalid_value",
                ));
            }
            let description = optional_string(wrapper, "description")?;
            let schema = wrapper
                .get("schema")
                .filter(|value| value.is_object())
                .cloned()
                .ok_or_else(|| {
                    OpenAiError::invalid(
                        "`response_format.json_schema.schema` must be a JSON object.",
                        Some("response_format.json_schema.schema"),
                        "invalid_type",
                    )
                })?;
            let strict = match wrapper.get("strict") {
                None | Some(Value::Null) => None,
                Some(Value::Bool(value)) => Some(*value),
                Some(_) => {
                    return Err(OpenAiError::invalid(
                        "`response_format.json_schema.strict` must be a boolean.",
                        Some("response_format.json_schema.strict"),
                        "invalid_type",
                    ));
                }
            };
            Ok(Some(OutputFormat::JsonSchema {
                name,
                description,
                schema,
                strict,
            }))
        }
        _ => Err(OpenAiError::unsupported(
            "`response_format.type` must be `text`, `json_object`, or `json_schema`.",
            "response_format.type",
        )),
    }
}

fn validate_stream_options(value: Option<&Value>, stream: bool) -> Result<bool, OpenAiError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(false);
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
        &["include_usage", "include_obfuscation"],
        "Chat stream_options",
    )?;
    require_null_or(
        options,
        "include_obfuscation",
        |value| value == false,
        "`false`",
    )?;
    optional_bool(options, "include_usage", false)
}

#[derive(Clone)]
struct ChatContext {
    completion_id: String,
    created: i64,
    model: String,
    include_usage: bool,
}

fn completion_document(context: &ChatContext, output: &InferenceOutput) -> Value {
    let content = if output.text.is_empty() && !output.tool_calls.is_empty() {
        Value::Null
    } else {
        json!(output.text)
    };
    let mut document = json!({
        "id": context.completion_id,
        "object": "chat.completion",
        "created": context.created,
        "model": context.model,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": content,
                "refusal": Value::Null,
            },
            "logprobs": Value::Null,
            "finish_reason": finish_reason_name(output.finish_reason),
        }],
    });
    if !output.tool_calls.is_empty() {
        document["choices"][0]["message"]["tool_calls"] = Value::Array(
            output
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
    }
    if let Some(usage) = output.usage.as_ref() {
        document
            .as_object_mut()
            .expect("Chat completion document is an object")
            .insert("usage".to_owned(), chat_usage(usage));
    }
    document
}

fn chat_usage(usage: &InferenceUsage) -> Value {
    let mut value = json!({
        "prompt_tokens": usage.input_tokens,
        "completion_tokens": usage.output_tokens,
        "total_tokens": usage.total_tokens,
    });
    let object = value.as_object_mut().expect("Chat usage is an object");
    if let Some(cached_tokens) = usage.cached_input_tokens {
        object.insert(
            "prompt_tokens_details".to_owned(),
            json!({ "cached_tokens": cached_tokens }),
        );
    }
    if let Some(reasoning_tokens) = usage.reasoning_output_tokens {
        object.insert(
            "completion_tokens_details".to_owned(),
            json!({ "reasoning_tokens": reasoning_tokens }),
        );
    }
    value
}

fn finish_reason_name(reason: InferenceFinishReason) -> &'static str {
    match reason {
        InferenceFinishReason::Stop => "stop",
        InferenceFinishReason::MaxOutputTokens => "length",
        InferenceFinishReason::ToolCalls => "tool_calls",
    }
}

struct ChatStreamState {
    context: ChatContext,
    backend: InferenceStream,
    queue: VecDeque<Result<Bytes, Infallible>>,
    terminal: bool,
}

fn streaming_response(context: ChatContext, backend: InferenceStream) -> Response {
    let mut state = ChatStreamState {
        context,
        backend,
        queue: VecDeque::new(),
        terminal: false,
    };
    state.push_chunk(json!({
        "choices": [{
            "index": 0,
            "delta": {"role": "assistant", "content": ""},
            "logprobs": Value::Null,
            "finish_reason": Value::Null,
        }],
    }));
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
                    state.push_chunk(json!({
                        "choices": [{
                            "index": 0,
                            "delta": {"content": delta},
                            "logprobs": Value::Null,
                            "finish_reason": Value::Null,
                        }],
                    }));
                }
                Some(Ok(InferenceEvent::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments_delta,
                })) => {
                    let mut function = serde_json::Map::new();
                    if let Some(name) = name {
                        function.insert("name".to_owned(), json!(name));
                    }
                    function.insert("arguments".to_owned(), json!(arguments_delta));
                    let mut call = serde_json::Map::from_iter([
                        ("index".to_owned(), json!(index)),
                        ("type".to_owned(), json!("function")),
                        ("function".to_owned(), Value::Object(function)),
                    ]);
                    if let Some(id) = id {
                        call.insert("id".to_owned(), json!(id));
                    }
                    state.push_chunk(json!({
                        "choices": [{
                            "index": 0,
                            "delta": {"tool_calls": [Value::Object(call)]},
                            "logprobs": Value::Null,
                            "finish_reason": Value::Null,
                        }],
                    }));
                }
                Some(Ok(InferenceEvent::Completed {
                    usage,
                    finish_reason,
                })) => state.complete(usage.as_ref(), finish_reason),
                Some(Err(error)) => {
                    tracing::warn!(%error, "private engine Chat stream failed");
                    state.fail();
                }
                None => {
                    tracing::warn!("private engine Chat stream ended without completion");
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
        .expect("static Chat streaming response headers are valid")
}

impl ChatStreamState {
    fn push_chunk(&mut self, fields: Value) {
        let mut chunk = json!({
            "id": self.context.completion_id,
            "object": "chat.completion.chunk",
            "created": self.context.created,
            "model": self.context.model,
        });
        let chunk = chunk.as_object_mut().expect("Chat chunk is an object");
        chunk.extend(fields.as_object().cloned().unwrap_or_default());
        if self.context.include_usage {
            chunk.insert("usage".to_owned(), Value::Null);
        }
        self.queue.push_back(Ok(Bytes::from(format!(
            "data: {}\n\n",
            Value::Object(chunk.clone())
        ))));
    }

    fn complete(&mut self, usage: Option<&InferenceUsage>, finish_reason: InferenceFinishReason) {
        self.push_chunk(json!({
            "choices": [{
                "index": 0,
                "delta": {},
                "logprobs": Value::Null,
                "finish_reason": finish_reason_name(finish_reason),
            }],
        }));
        if self.context.include_usage
            && let Some(usage) = usage
        {
            let mut chunk = json!({
                "id": self.context.completion_id,
                "object": "chat.completion.chunk",
                "created": self.context.created,
                "model": self.context.model,
                "choices": [],
                "usage": chat_usage(usage),
            });
            self.queue
                .push_back(Ok(Bytes::from(format!("data: {}\n\n", chunk.take()))));
        }
        self.queue
            .push_back(Ok(Bytes::from_static(b"data: [DONE]\n\n")));
        self.terminal = true;
    }

    fn fail(&mut self) {
        let error = OpenAiError::streaming_backend_failure();
        self.queue
            .push_back(Ok(Bytes::from(format!("data: {}\n\n", error.envelope()))));
        self.queue
            .push_back(Ok(Bytes::from_static(b"data: [DONE]\n\n")));
        self.terminal = true;
    }
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use futures_util::stream;
    use norted_engine::{
        InferenceEvent, InferenceFinishReason, InferenceOutput, InferenceRole, InferenceStream,
        InferenceUsage, ReasoningEffort,
    };
    use serde_json::json;

    use super::{ChatContext, completion_document, parse_request, streaming_response};

    #[test]
    fn text_messages_and_generation_settings_normalize() {
        let parsed = parse_request(json!({
            "model": "model",
            "messages": [
                {"role": "developer", "content": "D"},
                {"role": "system", "content": [{"type": "text", "text": "S"}]},
                {"role": "user", "content": "U"},
                {"role": "assistant", "content": "A"}
            ],
            "temperature": 0.2,
            "top_p": 0.8,
            "reasoning_effort": "high",
            "n": 1,
            "tools": [],
            "tool_choice": "none",
            "max_completion_tokens": 42
        }))
        .expect("valid Chat request");
        assert_eq!(parsed.normalized.messages[0].role, InferenceRole::Developer);
        assert_eq!(parsed.normalized.messages[3].role, InferenceRole::Assistant);
        assert_eq!(parsed.normalized.max_output_tokens, Some(42));
        assert_eq!(parsed.normalized.generation_settings.temperature, Some(0.2));
        assert_eq!(parsed.normalized.generation_settings.top_p, Some(0.8));
        assert_eq!(
            parsed.normalized.generation_settings.reasoning_effort,
            Some(ReasoningEffort::High)
        );
    }

    #[test]
    fn legacy_token_limit_and_equal_aliases_are_supported_but_conflicts_are_rejected() {
        let legacy = parse_request(json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 12
        }))
        .expect("legacy max_tokens");
        assert_eq!(legacy.normalized.max_output_tokens, Some(12));

        let equal = parse_request(json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 12,
            "max_completion_tokens": 12
        }))
        .expect("equal token limit aliases");
        assert_eq!(equal.normalized.max_output_tokens, Some(12));

        assert!(
            parse_request(json!({
                "model": "m",
                "messages": [{"role": "user", "content": "hello"}],
                "max_tokens": 12,
                "max_completion_tokens": 13
            }))
            .is_err()
        );
        assert!(
            parse_request(json!({
                "model": "m",
                "messages": [{"role": "user", "content": "hello"}],
                "n": 2
            }))
            .is_err()
        );
    }

    #[test]
    fn stream_options_accept_disabled_obfuscation_and_reject_unimplemented_behavior() {
        let parsed = parse_request(json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true,
            "stream_options": {
                "include_usage": true,
                "include_obfuscation": false
            }
        }))
        .expect("compatible Chat stream options");
        assert!(parsed.include_usage);

        for invalid in [
            json!({
                "model": "m",
                "messages": [{"role": "user", "content": "hello"}],
                "stream_options": {"include_obfuscation": false}
            }),
            json!({
                "model": "m",
                "messages": [{"role": "user", "content": "hello"}],
                "stream": true,
                "stream_options": {"include_obfuscation": true}
            }),
        ] {
            assert!(parse_request(invalid).is_err());
        }
    }

    #[test]
    fn non_stream_documents_report_finish_reason_and_known_usage() {
        let context = ChatContext {
            completion_id: "chatcmpl_stable".to_owned(),
            created: 1,
            model: "model".to_owned(),
            include_usage: false,
        };
        let usage = InferenceUsage {
            input_tokens: 2,
            output_tokens: 1,
            total_tokens: 3,
            ..Default::default()
        };
        let stopped = completion_document(
            &context,
            &InferenceOutput {
                text: "done".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: InferenceFinishReason::Stop,
                usage: Some(usage),
            },
        );
        assert_eq!(stopped["object"], "chat.completion");
        assert_eq!(stopped["choices"][0]["message"]["content"], "done");
        assert_eq!(stopped["choices"][0]["finish_reason"], "stop");
        assert_eq!(stopped["usage"]["total_tokens"], 3);

        let limited = completion_document(
            &context,
            &InferenceOutput {
                text: "partial".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: InferenceFinishReason::MaxOutputTokens,
                usage: None,
            },
        );
        assert_eq!(limited["choices"][0]["finish_reason"], "length");
        assert!(limited.get("usage").is_none());
    }

    #[tokio::test]
    async fn stream_emits_role_delta_text_finish_usage_and_done() {
        let backend: InferenceStream = Box::pin(stream::iter([
            Ok(InferenceEvent::TextDelta {
                delta: "hello".to_owned(),
            }),
            Ok(InferenceEvent::Completed {
                usage: Some(norted_engine::InferenceUsage {
                    input_tokens: 2,
                    output_tokens: 1,
                    total_tokens: 3,
                    ..Default::default()
                }),
                finish_reason: InferenceFinishReason::MaxOutputTokens,
            }),
        ]));
        let response = streaming_response(
            ChatContext {
                completion_id: "chatcmpl_stable".to_owned(),
                created: 1,
                model: "model".to_owned(),
                include_usage: true,
            },
            backend,
        );
        let bytes = to_bytes(response.into_body(), 128 * 1024)
            .await
            .expect("stream body");
        let body = String::from_utf8(bytes.to_vec()).expect("UTF-8 SSE");
        assert!(body.contains("\"role\":\"assistant\""));
        assert!(body.contains("\"content\":\"hello\""));
        assert!(body.contains("\"finish_reason\":\"length\""));
        assert!(body.contains("\"prompt_tokens\":2"));
        assert!(body.ends_with("data: [DONE]\n\n"));
    }
}
