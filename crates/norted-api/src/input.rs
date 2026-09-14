use norted_engine::{
    GenerationSettingsPatch, InferenceMessage, InferenceRequest, InferenceRole, InferenceTool,
    InferenceToolChoice, OutputFormat, ReasoningEffort,
};
use serde_json::{Map, Value};

use crate::error::OpenAiError;

#[derive(Debug, Clone)]
pub(crate) struct NormalizedRequest {
    pub(crate) model: String,
    pub(crate) messages: Vec<InferenceMessage>,
    pub(crate) max_output_tokens: Option<u32>,
    pub(crate) generation_settings: GenerationSettingsPatch,
    pub(crate) tools: Vec<InferenceTool>,
    pub(crate) tool_choice: Option<InferenceToolChoice>,
    pub(crate) parallel_tool_calls: Option<bool>,
    pub(crate) output_format: Option<OutputFormat>,
    pub(crate) stream: bool,
}

impl NormalizedRequest {
    pub(crate) fn inference_request(&self) -> Result<InferenceRequest, OpenAiError> {
        let model_profile_id = crate::execution_profile_id(self.model.clone()).map_err(|_| {
            OpenAiError::invalid(
                "`model` must be a valid Model Profile ID.",
                Some("model"),
                "invalid_value",
            )
        })?;
        Ok(InferenceRequest {
            model_profile_id,
            messages: self.messages.clone(),
            max_output_tokens: self.max_output_tokens,
            generation_settings: self.generation_settings.clone(),
            tools: self.tools.clone(),
            tool_choice: self.tool_choice.clone(),
            parallel_tool_calls: self.parallel_tool_calls,
            output_format: self.output_format.clone(),
            stream: self.stream,
        })
    }
}

pub(crate) fn object(value: &Value) -> Result<&Map<String, Value>, OpenAiError> {
    value.as_object().ok_or_else(|| {
        OpenAiError::invalid(
            "The request body must be a JSON object.",
            None::<String>,
            "invalid_request",
        )
    })
}

pub(crate) fn reject_unknown_fields(
    object: &Map<String, Value>,
    allowed: &[&str],
    surface: &str,
) -> Result<(), OpenAiError> {
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(OpenAiError::unsupported(
            format!("Unsupported {surface} field: {field}"),
            field,
        ));
    }
    Ok(())
}

pub(crate) fn required_string(
    object: &Map<String, Value>,
    field: &str,
) -> Result<String, OpenAiError> {
    let value = object.get(field).ok_or_else(|| {
        OpenAiError::invalid(
            format!("Missing required field: {field}"),
            Some(field),
            "missing_required_parameter",
        )
    })?;
    let value = value.as_str().ok_or_else(|| {
        OpenAiError::invalid(
            format!("`{field}` must be a string."),
            Some(field),
            "invalid_type",
        )
    })?;
    if value.trim().is_empty() {
        return Err(OpenAiError::invalid(
            format!("`{field}` must not be empty."),
            Some(field),
            "invalid_value",
        ));
    }
    Ok(value.to_owned())
}

pub(crate) fn optional_string(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<String>, OpenAiError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(OpenAiError::invalid(
            format!("`{field}` must be a string."),
            Some(field),
            "invalid_type",
        )),
    }
}

pub(crate) fn optional_bool(
    object: &Map<String, Value>,
    field: &str,
    default: bool,
) -> Result<bool, OpenAiError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(OpenAiError::invalid(
            format!("`{field}` must be a boolean."),
            Some(field),
            "invalid_type",
        )),
    }
}

pub(crate) fn optional_bool_value(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<bool>, OpenAiError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(OpenAiError::invalid(
            format!("`{field}` must be a boolean."),
            Some(field),
            "invalid_type",
        )),
    }
}

pub(crate) fn optional_nonnegative_u32(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<u32>, OpenAiError> {
    optional_u32(object, field)
}

pub(crate) fn optional_positive_u32(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<u32>, OpenAiError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let number = value.as_u64().ok_or_else(|| {
                OpenAiError::invalid(
                    format!("`{field}` must be a positive integer."),
                    Some(field),
                    "invalid_type",
                )
            })?;
            if number == 0 || number > u64::from(u32::MAX) {
                return Err(OpenAiError::invalid(
                    format!("`{field}` is outside the supported positive integer range."),
                    Some(field),
                    "invalid_value",
                ));
            }
            Ok(Some(number as u32))
        }
    }
}

pub(crate) fn generation_settings(
    object: &Map<String, Value>,
) -> Result<GenerationSettingsPatch, OpenAiError> {
    Ok(GenerationSettingsPatch {
        temperature: optional_f64(object, "temperature", 0.0, 2.0)?,
        top_p: optional_f64(object, "top_p", 0.0, 1.0)?,
        top_k: optional_u32(object, "top_k")?.map(u64::from),
        min_p: optional_f64(object, "min_p", 0.0, 1.0)?,
        seed: optional_u32(object, "seed")?.map(u64::from),
        repeat_penalty: optional_nonnegative_f64(object, "repeat_penalty")?,
        presence_penalty: optional_f64(object, "presence_penalty", -2.0, 2.0)?,
        frequency_penalty: optional_f64(object, "frequency_penalty", -2.0, 2.0)?,
        stop: optional_stop(object.get("stop"), "stop")?,
        reasoning_enabled: None,
        reasoning_budget: None,
        reasoning_effort: None,
    })
}

pub(crate) fn optional_u32(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<u32>, OpenAiError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let value = value.as_u64().ok_or_else(|| {
                OpenAiError::invalid(
                    format!("`{field}` must be a non-negative integer."),
                    Some(field),
                    "invalid_type",
                )
            })?;
            u32::try_from(value).map(Some).map_err(|_| {
                OpenAiError::invalid(
                    format!("`{field}` is outside the supported 32-bit seed range."),
                    Some(field),
                    "invalid_value",
                )
            })
        }
    }
}

pub(crate) fn optional_stop(
    value: Option<&Value>,
    field: &'static str,
) -> Result<Option<Vec<String>>, OpenAiError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let values = match value {
        Value::String(value) => vec![value.clone()],
        Value::Array(values) => values
            .iter()
            .map(|value| {
                value.as_str().map(str::to_owned).ok_or_else(|| {
                    OpenAiError::invalid(
                        format!("`{field}` array entries must be strings."),
                        Some(field),
                        "invalid_type",
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => {
            return Err(OpenAiError::invalid(
                format!("`{field}` must be a string or an array of strings."),
                Some(field),
                "invalid_type",
            ));
        }
    };
    if values.is_empty()
        || values
            .iter()
            .any(|value| value.is_empty() || value.contains('\0'))
    {
        return Err(OpenAiError::invalid(
            format!("`{field}` must contain one or more non-empty strings."),
            Some(field),
            "invalid_value",
        ));
    }
    Ok(Some(values))
}

pub(crate) fn optional_reasoning_effort(
    value: Option<&Value>,
    field: &'static str,
) -> Result<Option<ReasoningEffort>, OpenAiError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => match value.as_str() {
            "none" => Ok(Some(ReasoningEffort::None)),
            "minimal" => Ok(Some(ReasoningEffort::Minimal)),
            "low" => Ok(Some(ReasoningEffort::Low)),
            "medium" => Ok(Some(ReasoningEffort::Medium)),
            "high" => Ok(Some(ReasoningEffort::High)),
            "xhigh" => Ok(Some(ReasoningEffort::Xhigh)),
            "max" => Ok(Some(ReasoningEffort::Max)),
            _ => Err(OpenAiError::invalid(
                format!(
                    "`{field}` must be `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, or `max`."
                ),
                Some(field),
                "invalid_value",
            )),
        },
        Some(_) => Err(OpenAiError::invalid(
            format!("`{field}` must be a string."),
            Some(field),
            "invalid_type",
        )),
    }
}

fn optional_nonnegative_f64(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<f64>, OpenAiError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let number = value.as_f64().ok_or_else(|| {
                OpenAiError::invalid(
                    format!("`{field}` must be a number."),
                    Some(field),
                    "invalid_type",
                )
            })?;
            if !number.is_finite() || number < 0.0 {
                return Err(OpenAiError::invalid(
                    format!("`{field}` must be finite and non-negative."),
                    Some(field),
                    "invalid_value",
                ));
            }
            Ok(Some(number))
        }
    }
}

fn optional_f64(
    object: &Map<String, Value>,
    field: &str,
    minimum: f64,
    maximum: f64,
) -> Result<Option<f64>, OpenAiError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let number = value.as_f64().ok_or_else(|| {
                OpenAiError::invalid(
                    format!("`{field}` must be a number."),
                    Some(field),
                    "invalid_type",
                )
            })?;
            if !number.is_finite() || number < minimum || number > maximum {
                return Err(OpenAiError::invalid(
                    format!("`{field}` must be between {minimum} and {maximum}, inclusive."),
                    Some(field),
                    "invalid_value",
                ));
            }
            Ok(Some(number))
        }
    }
}

pub(crate) fn role(value: Option<&Value>, parameter: &str) -> Result<InferenceRole, OpenAiError> {
    match value.and_then(Value::as_str) {
        Some("developer") => Ok(InferenceRole::Developer),
        Some("system") => Ok(InferenceRole::System),
        Some("user") => Ok(InferenceRole::User),
        Some("assistant") => Ok(InferenceRole::Assistant),
        Some("tool") => Ok(InferenceRole::Tool),
        Some(_) => Err(OpenAiError::unsupported(
            "Only developer, system, user, assistant, and tool messages are supported.",
            parameter,
        )),
        None => Err(OpenAiError::invalid(
            "Each message requires a string `role`.",
            Some(parameter),
            "missing_required_parameter",
        )),
    }
}

pub(crate) fn require_null_or(
    object: &Map<String, Value>,
    field: &str,
    predicate: impl FnOnce(&Value) -> bool,
    supported_description: &str,
) -> Result<(), OpenAiError> {
    if let Some(value) = object.get(field)
        && !value.is_null()
        && !predicate(value)
    {
        return Err(OpenAiError::unsupported(
            format!("`{field}` only supports {supported_description}."),
            field,
        ));
    }
    Ok(())
}
