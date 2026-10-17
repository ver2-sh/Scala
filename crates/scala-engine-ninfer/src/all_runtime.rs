//! Independently reviewed NInfer-all 08fd7019 serving contract, schema 28.
//! No v2 conversion, model-name presets, runtime patches or GPU qualification claims.
use super::*;
use serde_json::Value;

mod owners;
#[cfg(test)]
mod tests;

pub(super) const ROTATED_KV: &[&str] = &["rk8v4", "rk4v4", "rk4v4-e8", "rk2v4-e8"];
const MAX_CONTEXT: u64 = 1_048_576;
const KV_PAGE_TOKENS: u64 = 64; // pinned src/core/paged_kv_cache.h

pub(super) fn is_all(c: NinferRuntimeCapabilities) -> bool {
    c.trustworthy_identity && c.startup_proof && c.request_log_schema == Some(28)
}
fn capabilities() -> NinferRuntimeCapabilities {
    // The independent owner closure covers the existing Chat, sampler, thinking,
    // tool, media, cache and speculation contracts. Schema-25 constrained-output
    // completion evidence is not provided by this fork and is not advertised.
    let mut c = ninfer_reviewed_capabilities(true, NinferReviewedDomains::all(true));
    c.request_log_schema = Some(28);
    c.model_sampler_defaults = false; // native presets are conditional, never invented
    c
}
pub(super) fn source_matches(root: &Path) -> bool {
    owners::FILES.iter().all(|(path, digest)| {
        std::fs::read(root.join(path))
            .is_ok_and(|bytes| format!("{:x}", Sha256::digest(bytes)) == *digest)
    })
}
pub(super) fn available_capabilities(r: &AvailableRuntime) -> Option<NinferRuntimeCapabilities> {
    all_catalog::available(r).then(capabilities)
}
pub(super) fn installed_capabilities(r: &InstalledRuntime) -> Option<NinferRuntimeCapabilities> {
    (all_catalog::installed(r) && source_matches(&r.installation_root.join("source")))
        .then(capabilities)
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct ExtendedContext {
    pub rope_yarn: Option<bool>,
    pub rope_yarn_factor: Option<f64>,
    pub gdn_state_fp16: Option<bool>,
}
impl ExtendedContext {
    pub(super) fn from_settings(settings: &ResolvedSettings) -> Result<Self, EngineError> {
        let toggle = |id: &str| match settings.value(id) {
            None => Ok(None),
            Some(SettingValue::Toggle(v)) => Ok(Some(*v)),
            _ => Err(EngineError::InvalidConfiguration(format!(
                "{id} must be a toggle"
            ))),
        };
        let factor = match settings.value("ninfer.rope_yarn_factor") {
            None => None,
            Some(SettingValue::Float(v)) if v.is_finite() && (1.0..=4.0).contains(v) => Some(*v),
            _ => {
                return Err(EngineError::InvalidConfiguration(
                    "ninfer.rope_yarn_factor must be a finite number in 1..=4".into(),
                ));
            }
        };
        Ok(Self {
            rope_yarn: toggle("ninfer.rope_yarn")?,
            rope_yarn_factor: factor,
            gdn_state_fp16: toggle("ninfer.gdn_state_fp16")?,
        })
    }
}

pub(super) fn validate_settings(
    settings: &ResolvedSettings,
    c: NinferRuntimeCapabilities,
) -> Result<(), String> {
    let extended = ExtendedContext::from_settings(settings).map_err(|e| e.to_string())?;
    if !is_all(c) {
        if extended != ExtendedContext::default()
            || matches!(settings.value("ninfer.kv_dtype"), Some(SettingValue::Choice(v)) if ROTATED_KV.contains(&v.as_str()))
        {
            return Err("YaRN, FP16 GDN state and rotated KV formats require the independently qualified NInfer-all runtime".into());
        }
        return Ok(());
    }
    if matches!(settings.value("ninfer.context_length"), Some(SettingValue::UnsignedInteger(v)) if *v == 0 || *v > MAX_CONTEXT)
    {
        return Err(format!(
            "NInfer-all context_length must be in 1..={MAX_CONTEXT}; artifact position capacity is checked separately"
        ));
    }
    // The reviewed parser offers only --no-thinking and --preserve-thinking.
    // Omission is a template-owned policy, not proof of the opposite value.
    if settings.value("ninfer.thinking") == Some(&SettingValue::Toggle(true)) {
        return Err("this NInfer-all revision has no process flag to force thinking on; use ninfer.reasoning=on, which is a supported request default".into());
    }
    if settings.value("ninfer.preserve_thinking") == Some(&SettingValue::Toggle(false)) {
        return Err("this NInfer-all revision cannot explicitly disable preserve_thinking at launch; use Inherit to restore the native template policy".into());
    }
    Ok(())
}

pub(super) fn validate_model(
    settings: &ResolvedSettings,
    model: &ModelArtifact,
) -> Result<(), String> {
    let metadata = inspect_ninfer_container(&model.path).map_err(|e| e.to_string())?;
    let directory = metadata
        .v3_directory
        .as_ref()
        .ok_or("NInfer-all requires a native v3 artifact; automatic conversion is not performed")?;
    validate_position_capacity(settings, directory)
}
fn validate_position_capacity(
    settings: &ResolvedSettings,
    directory: &Value,
) -> Result<(), String> {
    let Some(SettingValue::UnsignedInteger(requested)) = settings.value("ninfer.context_length")
    else {
        return Ok(());
    };
    let components = &directory["components"];
    let mut selected = vec!["text"];
    if settings.value("ninfer.speculation") != Some(&SettingValue::Toggle(false))
        && let Some(SettingValue::Choice(backend)) = settings.value("ninfer.speculative_backend")
        && matches!(backend.as_str(), "dflash" | "dflash2")
    {
        selected.push(backend);
    }
    for component in selected {
        let native = components[component]["config"]["max_position_embeddings"]
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or_else(|| format!("native {component} position capacity is missing"))?;
        let maximum = native
            .checked_mul(4)
            .ok_or("native position capacity overflow")?
            .min(MAX_CONTEXT);
        if *requested > maximum {
            return Err(format!(
                "requested context {requested} exceeds {component} native extended capacity {maximum}"
            ));
        }
    }
    Ok(())
}

pub(super) fn finish_launch_arguments(settings: &ResolvedSettings, arguments: &mut Vec<OsString>) {
    // Native Auto may choose the proposal head; an explicit false is not Auto.
    if settings.value("ninfer.lm_head_draft") == Some(&SettingValue::Toggle(false)) {
        arguments.push("--full-draft-head".into());
    }
}

fn failure(message: &str) -> EngineError {
    EngineError::Operation(format!("NInfer-all schema-28 startup: {message}"))
}
pub(super) fn observe_startup(
    p: &PendingStartup,
    s: &Value,
    observed: &mut BTreeMap<String, Value>,
) -> Result<(), EngineError> {
    if !is_all(p.capabilities) || s["schema_version"] != 28 {
        return Err(failure("unqualified runtime or mismatched schema"));
    }
    let server = &s["server"];
    let e = &s["engine"];
    match server["default_output_policy"].as_str() {
        Some("concurrent_lane_budget")
            if server.get("default_output_tokens") == Some(&Value::Null) => {}
        Some("fixed")
            if server["default_output_tokens"]
                .as_u64()
                .is_some_and(|n| n > 0) => {}
        _ => return Err(failure("output budget policy is missing or inconsistent")),
    }
    observed.insert(
        "native_output_policy".into(),
        server["default_output_policy"].clone(),
    );
    let factor = e["rope_yarn_factor"]
        .as_f64()
        .filter(|n| n.is_finite() && (1.0..=4.0).contains(n))
        .ok_or_else(|| failure("invalid native YaRN factor"))?;
    for field in ["rope_yarn", "gdn_state_fp16"] {
        if e[field].as_bool().is_none() {
            return Err(failure("missing native extended-context observation"));
        }
        observed.insert(format!("ninfer.{field}"), e[field].clone());
    }
    observed.insert("ninfer.rope_yarn_factor".into(), json!(factor));
    for (key, field) in [
        ("speculative_backend", "speculative_backend"),
        ("draft_tokens", "speculative_draft_window"),
    ] {
        observed.insert(format!("ninfer.{key}"), e[field].clone());
    }
    observed.insert(
        "ninfer.speculation".into(),
        json!(e["speculative_backend"] != "none"),
    );
    match e["proposal_head"].as_str() {
        Some("optimized") => {
            observed.insert("ninfer.lm_head_draft".into(), json!(true));
        }
        Some("full") => {
            observed.insert("ninfer.lm_head_draft".into(), json!(false));
        }
        Some("auto") => {}
        _ => return Err(failure("missing proposal-head observation")),
    }
    let context = e["max_context"]
        .as_u64()
        .filter(|n| (1..=MAX_CONTEXT).contains(n))
        .ok_or_else(|| failure("invalid native context capacity"))?;
    let capacity = e["kv_capacity"]
        .as_u64()
        .ok_or_else(|| failure("missing KV capacity"))?;
    if capacity < context || capacity % KV_PAGE_TOKENS != 0 {
        return Err(failure(
            "KV capacity cannot hold the context or is not native page-aligned",
        ));
    }
    if let Some(r) = &p.settings_requirements {
        if r.minimum_context_tokens.is_some_and(|n| n != context) {
            return Err(failure("requested context was not applied"));
        }
        if let Some(requested) = r.kv_capacity {
            let expected = requested
                .checked_add(KV_PAGE_TOKENS - 1)
                .map(|n| n / KV_PAGE_TOKENS * KV_PAGE_TOKENS)
                .ok_or_else(|| failure("KV capacity overflow"))?;
            if capacity != expected {
                return Err(failure(
                    "KV capacity differs from the requested native page-rounded allocation",
                ));
            }
        }
        let x = &r.extended_context;
        if x.rope_yarn
            .is_some_and(|v| e["rope_yarn"].as_bool() != Some(v))
            || x.gdn_state_fp16
                .is_some_and(|v| e["gdn_state_fp16"].as_bool() != Some(v))
            || x.rope_yarn_factor
                .is_some_and(|v| !matches_runtime_float(factor, v))
        {
            return Err(failure(
                "explicit YaRN or FP16 state setting was not applied",
            ));
        }
    }
    // This is the native requested policy, not a Scala-computed or persisted
    // effective positional scaling factor. NInfer owns the calculation.
    Ok(())
}
