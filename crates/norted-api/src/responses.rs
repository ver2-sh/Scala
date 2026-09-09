use std::collections::{BTreeMap, VecDeque};
use std::convert::Infallible;

use axum::Json;
use axum::body::Body;
use axum::extract::{Extension, State, rejection::JsonRejection};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::{StreamExt, stream};
use norted_engine::{
    EffectiveGenerationSettings, InferenceContentPart, InferenceEvent, InferenceFinishReason,
    InferenceMessage, InferenceOutput, InferenceRole, InferenceStream, InferenceTool,
    InferenceToolCall, InferenceToolChoice, InferenceUsage, OutputFormat,
};
use serde_json::{Value, json};
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
    "seed",
    "presence_penalty",
    "frequency_penalty",
    "stop",
    "stop_token_ids",
    "top_k",
    "min_p",
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
    headers: HeaderMap,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Response, OpenAiError> {
    let Json(value) = payload.map_err(|error| OpenAiError::malformed_json(&error))?;
    let parsed = parse_request(value)?;
    correlation.record_inference(&parsed.normalized.model, parsed.normalized.stream);
    let response_id = format!("resp_{}", Uuid::new_v4().simple());
    let message_id = format!("msg_{}", Uuid::new_v4().simple());
    let created_at = unix_timestamp();
    let inference = parsed.normalized.inference_request()?;
    let routing = crate::inference_routing_context(&headers)?;
    let public_model = parsed.normalized.model.clone();
    let max_output_tokens = parsed.normalized.max_output_tokens;
    let reasoning_effort = parsed.normalized.generation_settings.reasoning_effort;
    let tools = parsed.normalized.tools.clone();
    let tool_choice = parsed.normalized.tool_choice.clone();
    let parallel_tool_calls = parsed.normalized.parallel_tool_calls.unwrap_or(true);
    if parsed.normalized.stream {
        let routed = state
            .runtime
            .infer_stream_routed(inference, routing)
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
                output_format: routed.effective_output_format,
                reasoning_effort,
                tools,
                tool_choice,
                parallel_tool_calls,
                effective_generation_settings: routed.effective_generation_settings,
            },
            routed.stream,
        ))
    } else {
        let routed = state
            .runtime
            .infer_routed(inference, routing)
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
            output_format: routed.effective_output_format,
            reasoning_effort,
            tools,
            tool_choice,
            parallel_tool_calls,
            effective_generation_settings: routed.effective_generation_settings,
        };
        Ok(Json(response_document(
            &context,
            status,
            Some(&routed.output),
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
    let reasoning = responses_reasoning(object.get("reasoning"))?;
    generation_settings.reasoning_effort = reasoning.effort;
    generation_settings.reasoning_enabled = reasoning.enabled;
    generation_settings.reasoning_budget = reasoning.budget;
    let tools = parse_tools(object.get("tools"))?;
    let tool_choice = parse_tool_choice(object.get("tool_choice"), &tools)?;
    let parallel_tool_calls = optional_bool_value(object, "parallel_tool_calls")?;
    let output_format = parse_text_format(object.get("text"))?;
    let input = object.get("input").ok_or_else(|| {
        OpenAiError::invalid(
            "Missing required field: input",
            Some("input"),
            "missing_required_parameter",
        )
    })?;
    let mut messages = Vec::new();
    if let Some(instructions) = &instructions {
        messages.push(InferenceMessage::text(
            InferenceRole::Developer,
            instructions.clone(),
        ));
    }
    match input {
        Value::String(text) => {
            messages.push(InferenceMessage::text(InferenceRole::User, text.clone()))
        }
        Value::Array(items) => {
            if items.is_empty() {
                return Err(OpenAiError::invalid(
                    "`input` must contain at least one message.",
                    Some("input"),
                    "invalid_value",
                ));
            }
            for (index, item) in items.iter().enumerate() {
                for message in parse_input_item(item, index)? {
                    if message.role == InferenceRole::Assistant
                        && let Some(previous) = messages.last_mut()
                        && previous.role == InferenceRole::Assistant
                    {
                        previous.content.extend(message.content);
                        previous.tool_calls.extend(message.tool_calls);
                    } else {
                        messages.push(message);
                    }
                }
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
            tools,
            tool_choice,
            parallel_tool_calls,
            output_format,
            stream,
        },
        instructions,
    })
}

fn parse_input_item(value: &Value, index: usize) -> Result<Vec<InferenceMessage>, OpenAiError> {
    let parameter = format!("input[{index}]");
    let message = value.as_object().ok_or_else(|| {
        OpenAiError::invalid(
            "Each input item must be a text message object.",
            Some(parameter.clone()),
            "invalid_type",
        )
    })?;
    let item_type = message
        .get("type")
        .filter(|value| !value.is_null())
        .and_then(Value::as_str)
        .unwrap_or("message");
    if item_type == "function_call" {
        reject_unknown_fields(
            message,
            &["type", "id", "call_id", "name", "arguments", "status"],
            "function_call input Item",
        )?;
        require_null_or(
            message,
            "status",
            |value| value.as_str() == Some("completed"),
            "`\"completed\"`",
        )?;
        let call_id = required_nonempty(message.get("call_id"), &format!("{parameter}.call_id"))?;
        let arguments =
            required_nonempty(message.get("arguments"), &format!("{parameter}.arguments"))?;
        if !serde_json::from_str::<Value>(&arguments).is_ok_and(|value| value.is_object()) {
            return Err(OpenAiError::invalid(
                "Function-call `arguments` must encode a JSON object.",
                Some(format!("{parameter}.arguments")),
                "invalid_value",
            ));
        }
        return Ok(vec![InferenceMessage {
            role: InferenceRole::Assistant,
            content: Vec::new(),
            tool_calls: vec![InferenceToolCall {
                id: call_id,
                name: required_nonempty(message.get("name"), &format!("{parameter}.name"))?,
                arguments,
            }],
            tool_call_id: None,
        }]);
    }
    if item_type == "function_call_output" {
        reject_unknown_fields(
            message,
            &["type", "id", "call_id", "name", "output", "status"],
            "function_call_output input Item",
        )?;
        require_null_or(
            message,
            "status",
            |value| value.as_str() == Some("completed"),
            "`\"completed\"`",
        )?;
        require_null_or(message, "name", Value::is_string, "a string")?;
        let output = message.get("output").ok_or_else(|| {
            OpenAiError::invalid(
                "Function-call output requires `output`.",
                Some(format!("{parameter}.output")),
                "missing_required_parameter",
            )
        })?;
        return Ok(vec![InferenceMessage {
            role: InferenceRole::Tool,
            content: parse_responses_content(
                output,
                &format!("{parameter}.output"),
                InferenceRole::Tool,
            )?,
            tool_calls: Vec::new(),
            tool_call_id: Some(required_nonempty(
                message.get("call_id"),
                &format!("{parameter}.call_id"),
            )?),
        }]);
    }
    if item_type != "message" {
        return Err(OpenAiError::unsupported(
            "Only `message`, `function_call`, and `function_call_output` input Items are supported.",
            format!("{parameter}.type"),
        ));
    }
    reject_unknown_fields(
        message,
        &["type", "role", "content", "id", "status"],
        "input message",
    )?;
    require_null_or(message, "id", Value::is_string, "a string")?;
    require_null_or(message, "status", Value::is_string, "a string")?;
    let role = role(message.get("role"), &format!("{parameter}.role"))?;
    let content = message.get("content").ok_or_else(|| {
        OpenAiError::invalid(
            "Input messages require `content`.",
            Some(format!("{parameter}.content")),
            "missing_required_parameter",
        )
    })?;
    Ok(vec![InferenceMessage {
        role,
        content: parse_responses_content(content, &format!("{parameter}.content"), role)?,
        tool_calls: Vec::new(),
        tool_call_id: None,
    }])
}

fn parse_responses_content(
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
            "Content must be a string or an array of typed parts.",
            Some(parameter),
            "invalid_type",
        )
    })?;
    if parts.is_empty() {
        return Err(OpenAiError::invalid(
            "Content part arrays must not be empty.",
            Some(parameter),
            "invalid_value",
        ));
    }
    let mut content = Vec::with_capacity(parts.len());
    for (index, part) in parts.iter().enumerate() {
        let part_parameter = format!("{parameter}[{index}]");
        let part = part.as_object().ok_or_else(|| {
            OpenAiError::invalid(
                "Content parts must be objects.",
                Some(part_parameter.clone()),
                "invalid_type",
            )
        })?;
        match part.get("type").and_then(Value::as_str) {
            Some("input_text" | "output_text" | "refusal") => {
                reject_unknown_fields(part, &["type", "text"], "Responses text content")?;
                content.push(InferenceContentPart::Text {
                    text: required_nonempty(part.get("text"), &format!("{part_parameter}.text"))?,
                });
            }
            Some("input_image")
                if matches!(
                    role,
                    InferenceRole::User | InferenceRole::Assistant | InferenceRole::Tool
                ) =>
            {
                reject_unknown_fields(
                    part,
                    &["type", "image_url", "detail"],
                    "Responses image content",
                )?;
                require_null_or(
                    part,
                    "detail",
                    |value| value.as_str() == Some("auto"),
                    "`\"auto\"`",
                )?;
                content.push(InferenceContentPart::ImageUrl {
                    url: media_url(
                        part.get("image_url"),
                        &format!("{part_parameter}.image_url"),
                    )?,
                });
            }
            Some("input_video") if role == InferenceRole::User => {
                reject_unknown_fields(part, &["type", "video_url"], "Responses video content")?;
                content.push(InferenceContentPart::VideoUrl {
                    url: media_url(
                        part.get("video_url"),
                        &format!("{part_parameter}.video_url"),
                    )?,
                });
            }
            Some(kind) => {
                return Err(OpenAiError::unsupported(
                    format!("Unsupported Responses content type `{kind}` for this role."),
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
    let url = required_nonempty(value, parameter)?;
    if !(url.starts_with("https://") || url.starts_with("http://") || url.starts_with("data:")) {
        return Err(OpenAiError::unsupported(
            "Media input supports only HTTP(S) and data URLs; local file paths are forbidden.",
            parameter,
        ));
    }
    Ok(url)
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
        reject_unknown_fields(
            tool,
            &["type", "name", "description", "parameters", "strict"],
            "Responses tool",
        )?;
        if tool.get("type").and_then(Value::as_str) != Some("function") {
            return Err(OpenAiError::unsupported(
                "Only direct function tools are supported.",
                format!("{parameter}.type"),
            ));
        }
        require_null_or(tool, "strict", |value| value == false, "`false`")?;
        let name = required_nonempty(tool.get("name"), &format!("{parameter}.name"))?;
        if parsed
            .iter()
            .any(|existing: &InferenceTool| existing.name == name)
        {
            return Err(OpenAiError::invalid(
                "Function tool names must be unique.",
                Some(format!("{parameter}.name")),
                "duplicate_parameter",
            ));
        }
        let parameters = tool
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
        if !parameters.is_object() {
            return Err(OpenAiError::invalid(
                "Function `parameters` must be a JSON object.",
                Some(format!("{parameter}.parameters")),
                "invalid_type",
            ));
        }
        parsed.push(InferenceTool {
            name,
            description: tool
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
            reject_unknown_fields(choice, &["type", "name"], "tool_choice")?;
            if choice.get("type").and_then(Value::as_str) != Some("function") {
                return Err(OpenAiError::unsupported(
                    "Only named function tool choice objects are supported.",
                    "tool_choice.type",
                ));
            }
            InferenceToolChoice::Function {
                name: required_nonempty(choice.get("name"), "tool_choice.name")?,
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
            Some("tool_choice.name"),
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

fn validate_identity_fields(
    object: &serde_json::Map<String, Value>,
    stream: bool,
) -> Result<(), OpenAiError> {
    require_null_or(object, "store", |value| value == false, "`false`")?;
    require_null_or(object, "background", |value| value == false, "`false`")?;
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
    validate_stream_options(object.get("stream_options"), stream)
}

#[derive(Default)]
struct ResponsesReasoning {
    effort: Option<norted_engine::ReasoningEffort>,
    enabled: Option<bool>,
    budget: Option<i64>,
}

fn responses_reasoning(value: Option<&Value>) -> Result<ResponsesReasoning, OpenAiError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(ResponsesReasoning::default());
    };
    let reasoning = value.as_object().ok_or_else(|| {
        OpenAiError::invalid(
            "`reasoning` must be an object.",
            Some("reasoning"),
            "invalid_type",
        )
    })?;
    reject_unknown_fields(
        reasoning,
        &["effort", "enabled", "budget_tokens"],
        "reasoning",
    )?;
    let effort = optional_reasoning_effort(reasoning.get("effort"), "reasoning.effort")?;
    let enabled = optional_bool_value(reasoning, "enabled")?;
    let budget = optional_nonnegative_u32(reasoning, "budget_tokens")?.map(i64::from);
    if effort == Some(norted_engine::ReasoningEffort::None) && enabled == Some(true) {
        return Err(OpenAiError::invalid(
            "`reasoning.effort=none` conflicts with `reasoning.enabled=true`.",
            Some("reasoning"),
            "conflicting_parameters",
        ));
    }
    Ok(ResponsesReasoning {
        effort,
        enabled,
        budget,
    })
}

fn parse_text_format(value: Option<&Value>) -> Result<Option<OutputFormat>, OpenAiError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let text = value.as_object().ok_or_else(|| {
        OpenAiError::invalid("`text` must be an object.", Some("text"), "invalid_type")
    })?;
    reject_unknown_fields(text, &["format"], "Responses text")?;
    let Some(format) = text.get("format").filter(|format| !format.is_null()) else {
        return Ok(None);
    };
    let format = format.as_object().ok_or_else(|| {
        OpenAiError::invalid(
            "`text.format` must be an object.",
            Some("text.format"),
            "invalid_type",
        )
    })?;
    match format.get("type").and_then(Value::as_str) {
        Some("text") => {
            reject_unknown_fields(format, &["type"], "Responses text format")?;
            Ok(Some(OutputFormat::Text))
        }
        Some("json_object") => {
            reject_unknown_fields(format, &["type"], "Responses text format")?;
            Ok(Some(OutputFormat::JsonObject))
        }
        Some("json_schema") => {
            reject_unknown_fields(
                format,
                &["type", "name", "description", "schema", "strict"],
                "Responses JSON Schema format",
            )?;
            let name = optional_string(format, "name")?;
            if name.as_ref().is_some_and(|name| name.trim().is_empty()) {
                return Err(OpenAiError::invalid(
                    "`text.format.name` must not be empty.",
                    Some("text.format.name"),
                    "invalid_value",
                ));
            }
            let description = optional_string(format, "description")?;
            let schema = format
                .get("schema")
                .filter(|value| value.is_object())
                .cloned()
                .ok_or_else(|| {
                    OpenAiError::invalid(
                        "`text.format.schema` must be a JSON object.",
                        Some("text.format.schema"),
                        "invalid_type",
                    )
                })?;
            let strict = match format.get("strict") {
                None | Some(Value::Null) => None,
                Some(Value::Bool(value)) => Some(*value),
                Some(_) => {
                    return Err(OpenAiError::invalid(
                        "`text.format.strict` must be a boolean.",
                        Some("text.format.strict"),
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
            "`text.format.type` must be `text`, `json_object`, or `json_schema`.",
            "text.format.type",
        )),
    }
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
    output_format: Option<OutputFormat>,
    reasoning_effort: Option<norted_engine::ReasoningEffort>,
    tools: Vec<InferenceTool>,
    tool_choice: Option<InferenceToolChoice>,
    parallel_tool_calls: bool,
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
    generated: Option<&InferenceOutput>,
    error: Option<Value>,
) -> Value {
    let message_status = match status {
        ResponseStatus::Completed => Some(OutputMessageStatus::Completed),
        ResponseStatus::Incomplete => Some(OutputMessageStatus::Incomplete),
        ResponseStatus::InProgress | ResponseStatus::Failed => None,
    };
    let mut output = Vec::new();
    if let Some(generated) = generated {
        if (!generated.text.is_empty() || generated.tool_calls.is_empty())
            && let Some(status) = message_status
        {
            output.push(message_item(context, status, &generated.text));
        }
        for (index, call) in generated.tool_calls.iter().enumerate() {
            output.push(function_call_item(context, index as u32, call));
        }
    }
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
        "parallel_tool_calls": context.parallel_tool_calls,
        "temperature": context.effective_generation_settings.temperature,
        "tool_choice": responses_tool_choice(context.tool_choice.as_ref()),
        "tools": context.tools.iter().map(responses_tool).collect::<Vec<_>>(),
        "top_p": context.effective_generation_settings.top_p,
        "background": false,
        "max_output_tokens": context.max_output_tokens,
        "previous_response_id": Value::Null,
        "reasoning": context.reasoning_effort.map(|effort| json!({ "effort": effort.as_str() })),
        "store": false,
        "text": { "format": responses_output_format(context.output_format.as_ref()) },
        "truncation": "disabled",
    });
    if let Some(usage) = generated
        .and_then(|generated| generated.usage.as_ref())
        .and_then(usage_document)
    {
        document
            .as_object_mut()
            .expect("Response document is an object")
            .insert("usage".to_owned(), usage);
    }
    document
}

fn responses_tool(tool: &InferenceTool) -> Value {
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.parameters,
        "strict": false,
    })
}

fn responses_tool_choice(choice: Option<&InferenceToolChoice>) -> Value {
    match choice {
        None | Some(InferenceToolChoice::Auto) => json!("auto"),
        Some(InferenceToolChoice::None) => json!("none"),
        Some(InferenceToolChoice::Required) => json!("required"),
        Some(InferenceToolChoice::Function { name }) => {
            json!({"type": "function", "name": name})
        }
    }
}

fn function_call_item(context: &ResponseContext, index: u32, call: &InferenceToolCall) -> Value {
    json!({
        "id": format!("fc_{}_{}", context.response_id.trim_start_matches("resp_"), index),
        "type": "function_call",
        "status": "completed",
        "call_id": call.id,
        "name": call.name,
        "arguments": call.arguments,
    })
}

fn responses_output_format(format: Option<&OutputFormat>) -> Value {
    match format {
        None | Some(OutputFormat::Text) => json!({ "type": "text" }),
        Some(OutputFormat::JsonObject) => json!({ "type": "json_object" }),
        Some(OutputFormat::JsonSchema {
            name,
            description,
            schema,
            strict,
        }) => {
            let mut value = json!({ "type": "json_schema", "schema": schema });
            let object = value.as_object_mut().expect("static object");
            if let Some(name) = name {
                object.insert("name".to_owned(), json!(name));
            }
            if let Some(description) = description {
                object.insert("description".to_owned(), json!(description));
            }
            if let Some(strict) = strict {
                object.insert("strict".to_owned(), json!(strict));
            }
            value
        }
    }
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
    text_output_index: Option<u32>,
    tool_calls: BTreeMap<u32, StreamToolCall>,
    next_output_index: u32,
    sequence: u64,
    terminal: bool,
}

#[derive(Default)]
struct StreamToolCall {
    output_index: Option<u32>,
    item_id: String,
    call_id: String,
    name: String,
    arguments: String,
    added: bool,
}

fn streaming_response(context: ResponseContext, backend: InferenceStream) -> Response {
    let mut state = PublicStreamState {
        context,
        backend,
        queue: VecDeque::new(),
        text: String::new(),
        text_output_index: None,
        tool_calls: BTreeMap::new(),
        next_output_index: 0,
        sequence: 0,
        terminal: false,
    };
    let initial_response =
        response_document(&state.context, ResponseStatus::InProgress, None, None);
    state.push_event("response.created", json!({ "response": initial_response }));
    let in_progress = response_document(&state.context, ResponseStatus::InProgress, None, None);
    state.push_event("response.in_progress", json!({ "response": in_progress }));
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
                    let output_index = state.ensure_text_started();
                    state.text.push_str(&delta);
                    state.push_event(
                        "response.output_text.delta",
                        json!({
                            "item_id": state.context.message_id,
                            "output_index": output_index,
                            "content_index": 0,
                            "delta": delta,
                            "logprobs": [],
                        }),
                    );
                }
                Some(Ok(InferenceEvent::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments_delta,
                })) => state.push_tool_delta(index, id, name, arguments_delta),
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
    fn ensure_text_started(&mut self) -> u32 {
        if let Some(index) = self.text_output_index {
            return index;
        }
        let output_index = self.next_output_index;
        self.next_output_index = self.next_output_index.saturating_add(1);
        self.text_output_index = Some(output_index);
        self.push_event(
            "response.output_item.added",
            json!({
                "output_index": output_index,
                "item": {
                    "id": self.context.message_id,
                    "type": "message",
                    "status": "in_progress",
                    "role": "assistant",
                    "content": [],
                },
            }),
        );
        self.push_event(
            "response.content_part.added",
            json!({
                "item_id": self.context.message_id,
                "output_index": output_index,
                "content_index": 0,
                "part": {
                    "type": "output_text",
                    "text": "",
                    "annotations": [],
                },
            }),
        );
        output_index
    }

    fn push_tool_delta(
        &mut self,
        index: u32,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: String,
    ) {
        let call = self.tool_calls.entry(index).or_default();
        if let Some(id) = id {
            call.call_id = id;
        }
        if let Some(name) = name {
            call.name = name;
        }
        call.arguments.push_str(&arguments_delta);
        if call.output_index.is_none() {
            call.output_index = Some(self.next_output_index);
            self.next_output_index = self.next_output_index.saturating_add(1);
            call.item_id = format!(
                "fc_{}_{}",
                self.context.response_id.trim_start_matches("resp_"),
                index
            );
        }
        let output_index = call.output_index.expect("assigned");
        let item_id = call.item_id.clone();
        let call_id = call.call_id.clone();
        let call_name = call.name.clone();
        let should_add = !call.added && !call_id.is_empty() && !call_name.is_empty();
        if should_add {
            call.added = true;
        }
        if should_add {
            self.push_event(
                "response.output_item.added",
                json!({
                    "output_index": output_index,
                    "item": {
                        "id": item_id,
                        "type": "function_call",
                        "status": "in_progress",
                        "call_id": call_id,
                        "name": call_name,
                        "arguments": "",
                    },
                }),
            );
        }
        if !arguments_delta.is_empty() {
            self.push_event(
                "response.function_call_arguments.delta",
                json!({
                    "item_id": item_id,
                    "output_index": output_index,
                    "delta": arguments_delta,
                }),
            );
        }
    }

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
        if let Some(output_index) = self.text_output_index {
            self.push_event(
                "response.output_text.done",
                json!({
                    "item_id": self.context.message_id,
                    "output_index": output_index,
                    "content_index": 0,
                    "text": text,
                    "logprobs": [],
                }),
            );
            self.push_event(
                "response.content_part.done",
                json!({
                    "item_id": self.context.message_id,
                    "output_index": output_index,
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
                    "output_index": output_index,
                    "item": message_item(&self.context, message_status, &text),
                }),
            );
        }
        let completed_calls = self
            .tool_calls
            .iter()
            .map(|(index, call)| {
                (
                    *index,
                    call.output_index.unwrap_or(0),
                    call.item_id.clone(),
                    InferenceToolCall {
                        id: call.call_id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    },
                )
            })
            .collect::<Vec<_>>();
        for (index, output_index, item_id, call) in &completed_calls {
            self.push_event(
                "response.function_call_arguments.done",
                json!({
                    "item_id": item_id,
                    "output_index": output_index,
                    "arguments": call.arguments,
                }),
            );
            self.push_event(
                "response.output_item.done",
                json!({
                    "output_index": output_index,
                    "item": function_call_item(&self.context, *index, call),
                }),
            );
        }
        let generated = InferenceOutput {
            text,
            tool_calls: completed_calls
                .into_iter()
                .map(|(_, _, _, call)| call)
                .collect(),
            usage: usage.cloned(),
            finish_reason,
        };
        let response = response_document(&self.context, status, Some(&generated), None);
        self.push_event(terminal_event, json!({ "response": response }));
        self.terminal = true;
    }

    fn fail(&mut self) {
        let error = json!({
            "code": "server_error",
            "message": "The model failed to generate a response.",
        });
        let response = response_document(&self.context, ResponseStatus::Failed, None, Some(error));
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
        InferenceOutput, InferenceRole, InferenceStream, InferenceUsage, ReasoningEffort,
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
            Some(&InferenceOutput {
                text: "done".to_owned(),
                tool_calls: Vec::new(),
                usage: None,
                finish_reason: InferenceFinishReason::Stop,
            }),
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
            Some(&InferenceOutput {
                text: "partial".to_owned(),
                tool_calls: Vec::new(),
                usage: Some(InferenceUsage {
                    input_tokens: 2,
                    output_tokens: 1,
                    total_tokens: 3,
                    ..Default::default()
                }),
                finish_reason: InferenceFinishReason::MaxOutputTokens,
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
            output_format: None,
            reasoning_effort: None,
            tools: Vec::new(),
            tool_choice: None,
            parallel_tool_calls: false,
            effective_generation_settings: EffectiveGenerationSettings {
                stop_token_ids: None,
                required_stop_token_ids: Vec::new(),
                temperature: 0.7,
                top_p: 0.95,
            },
        }
    }
}
