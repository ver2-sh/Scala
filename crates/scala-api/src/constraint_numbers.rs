//! Preserve native schema-number admission before ordinary DTO parsing can round
//! numeric assertions. This validates representation, never compiles constraints.
use crate::error::OpenAiError;
use axum::{
    body::{Body, to_bytes},
    extract::Request,
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::value::RawValue;
use std::collections::BTreeMap;
type Object = BTreeMap<String, Box<RawValue>>;
fn object(v: &RawValue) -> Option<Object> {
    serde_json::from_str(v.get()).ok()
}
fn array(v: &RawValue) -> Option<Vec<Box<RawValue>>> {
    serde_json::from_str(v.get()).ok()
}
fn decimal(s: &str) -> Option<(bool, String, i64)> {
    let (negative, s) = s.strip_prefix('-').map_or((false, s), |s| (true, s));
    let (base, exponent) = s
        .split_once(['e', 'E'])
        .map_or((s, Some(0)), |(b, e)| (b, e.parse::<i64>().ok()));
    let fraction = base.split_once('.').map_or(0, |(_, f)| f.len());
    let mut digits = base.replace('.', "").trim_start_matches('0').to_owned();
    let mut exponent = exponent?.checked_sub(i64::try_from(fraction).ok()?)?;
    if digits.is_empty() {
        return Some((false, "0".into(), 0));
    }
    while digits.ends_with('0') {
        digits.pop();
        exponent = exponent.checked_add(1)?;
    }
    Some((negative, digits, exponent))
}
fn problem(v: &RawValue, path: &str, literal: bool, depth: usize) -> Option<String> {
    if depth > 64 {
        return Some(path.into());
    }
    let source = v.get().trim();
    if source.starts_with('-') || source.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        let parsed: serde_json::Number = match serde_json::from_str(source) {
            Ok(v) => v,
            Err(_) => return Some(path.into()),
        };
        if decimal(source) != decimal(&parsed.to_string()) {
            return Some(path.into());
        }
        return None;
    }
    if literal {
        if let Some(o) = object(v) {
            for (k, v) in o {
                if let Some(p) = problem(&v, &format!("{path}/{k}"), true, depth + 1) {
                    return Some(p);
                }
            }
        }
        if let Some(a) = array(v) {
            for (i, v) in a.iter().enumerate() {
                if let Some(p) = problem(v, &format!("{path}/{i}"), true, depth + 1) {
                    return Some(p);
                }
            }
        }
        return None;
    }
    let o = object(v)?;
    for k in [
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "const",
        "enum",
    ] {
        if let Some(v) = o.get(k)
            && let Some(p) = problem(v, &format!("{path}/{k}"), true, depth + 1)
        {
            return Some(p);
        }
    }
    for k in [
        "properties",
        "$defs",
        "definitions",
        "anyOf",
        "oneOf",
        "allOf",
        "prefixItems",
    ] {
        if let Some(v) = o.get(k) {
            if let Some(children) = object(v) {
                for (name, v) in children {
                    if let Some(p) = problem(&v, &format!("{path}/{k}/{name}"), false, depth + 1) {
                        return Some(p);
                    }
                }
            }
            if let Some(children) = array(v) {
                for (i, v) in children.iter().enumerate() {
                    if let Some(p) = problem(v, &format!("{path}/{k}/{i}"), false, depth + 1) {
                        return Some(p);
                    }
                }
            }
        }
    }
    for k in ["items", "additionalItems", "additionalProperties"] {
        if let Some(v) = o.get(k) {
            if let Some(children) = array(v) {
                for (i, v) in children.iter().enumerate() {
                    if let Some(p) = problem(v, &format!("{path}/{k}/{i}"), false, depth + 1) {
                        return Some(p);
                    }
                }
            } else if let Some(p) = problem(v, &format!("{path}/{k}"), false, depth + 1) {
                return Some(p);
            }
        }
    }
    None
}
fn validate(bytes: &[u8]) -> Result<(), OpenAiError> {
    let Ok(root) = serde_json::from_slice::<Object>(bytes) else {
        return Ok(());
    };
    let mut schemas = Vec::new();
    if let Some(format) = root.get("response_format").and_then(|v| object(v))
        && let Some(spec) = format.get("json_schema").and_then(|v| object(v))
        && let Some(schema) = spec.get("schema")
    {
        schemas.push((
            "response_format.json_schema.schema".to_owned(),
            schema.clone(),
        ));
    }
    if let Some(text) = root.get("text").and_then(|v| object(v))
        && let Some(format) = text.get("format").and_then(|v| object(v))
        && let Some(schema) = format.get("schema")
    {
        schemas.push(("text.format.schema".to_owned(), schema.clone()));
    }
    if let Some(tools) = root.get("tools").and_then(|v| array(v)) {
        for (i, t) in tools.iter().enumerate() {
            if let Some(t) = object(t) {
                let nested = t.get("function").and_then(|v| object(v));
                let definition = nested.as_ref().unwrap_or(&t);
                if definition.get("strict").is_some_and(|v| v.get() == "true")
                    && let Some(schema) = definition.get("parameters")
                {
                    schemas.push((
                        format!(
                            "tools[{i}]{}.parameters",
                            if nested.is_some() { ".function" } else { "" }
                        ),
                        schema.clone(),
                    ));
                }
            }
        }
    }
    for (path, schema) in schemas {
        if let Some(param) = problem(&schema, &path, false, 0) {
            return Err(OpenAiError::invalid(
                "Schema numeric assertion cannot be preserved by the native JSON number representation, or exceeds the bounded nesting limit.",
                Some(param),
                "unsupported_json_schema",
            ));
        }
    }
    Ok(())
}
pub(super) async fn admit(request: Request, next: Next) -> Response {
    if request.method() != axum::http::Method::POST
        || !matches!(
            request.uri().path(),
            "/v1/chat/completions" | "/v1/responses"
        )
    {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    let bytes = match to_bytes(body, crate::MAX_INFERENCE_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => {
            return OpenAiError::request_too_large().into_response();
        }
    };
    if let Err(error) = validate(&bytes) {
        return error.into_response();
    }
    next.run(Request::from_parts(parts, Body::from(bytes)))
        .await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_rounded_assertions_but_preserves_native_decimal_and_annotations() {
        for location in ["response_format", "text"] {
            let schema = r#"{"type":"number","minimum":1.0000000000000000001}"#;
            let body = if location == "text" {
                format!(r#"{{"text":{{"format":{{"schema":{schema}}}}}}}"#)
            } else {
                format!(r#"{{"response_format":{{"json_schema":{{"schema":{schema}}}}}}}"#)
            };
            assert!(validate(body.as_bytes()).is_err());
        }
        assert!(validate(br#"{"response_format":{"json_schema":{"schema":{"minimum":0.1,"default":1.000000000000000001}}}}"#).is_ok());
        assert!(validate(br#"{"tools":[{"type":"function","strict":true,"parameters":{"properties":{"x":{"const":9007199254740993.0}}}}]}"#).is_err());
    }
}
