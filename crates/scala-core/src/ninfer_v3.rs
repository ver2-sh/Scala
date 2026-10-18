//! Exact native v3 framing/schema from Neroued/ninfer 81c8ce09.
//! Bounded cold inspection only; native binder remains execution authority.
use super::*;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn error(message: impl Into<String>) -> NinferContainerError {
    invalid_ninfer_directory(message)
}
fn object<'a>(
    v: &'a Value,
    label: &str,
    required: &[&str],
    optional: &[&str],
) -> Result<&'a Map<String, Value>, NinferContainerError> {
    let v = v
        .as_object()
        .ok_or_else(|| error(format!("{label} must be an object")))?;
    if required.iter().any(|k| !v.contains_key(*k))
        || v.keys()
            .any(|k| !required.contains(&k.as_str()) && !optional.contains(&k.as_str()))
    {
        return Err(error(format!("{label} has missing or unknown members")));
    }
    Ok(v)
}
fn id(v: &Value, label: &str) -> Result<String, NinferContainerError> {
    let s = nonempty_json_string(v, label)?;
    if s.contains('\0') {
        return Err(error(format!("{label} contains NUL")));
    }
    Ok(s)
}
fn uint(v: &Value, label: &str, positive: bool) -> Result<u64, NinferContainerError> {
    v.as_u64().filter(|n| !positive || *n > 0).ok_or_else(|| {
        error(format!(
            "{label} must be {}integer",
            if positive {
                "a positive "
            } else {
                "a nonnegative "
            }
        ))
    })
}
fn add(a: u64, b: u64) -> Result<u64, NinferContainerError> {
    a.checked_add(b)
        .ok_or_else(|| error("v3 range overflows u64"))
}
fn mul(a: u64, b: u64) -> Result<u64, NinferContainerError> {
    a.checked_mul(b)
        .ok_or_else(|| error("v3 shape/size overflows u64"))
}
fn align(n: u64, a: u64) -> Result<u64, NinferContainerError> {
    Ok(add(n, a - 1)? / a * a)
}
fn elements(shape: &Value) -> Result<u64, NinferContainerError> {
    let shape = shape
        .as_array()
        .filter(|a| a.len() <= 16)
        .ok_or_else(|| error("shape rank must be 0..16"))?;
    shape
        .iter()
        .try_fold(1, |n, d| mul(n, uint(d, "shape dimension", true)?))
}
fn geometry(tensor: &Map<String, Value>) -> Result<(), NinferContainerError> {
    let count = elements(&tensor["shape"])?;
    let format = id(&tensor["format"], "tensor format")?;
    let layout = id(&tensor["layout"], "tensor layout")?;
    let bytes = if layout == "contiguous_le_v1" {
        mul(
            count,
            match format.as_str() {
                "bf16" => 2,
                "fp32" | "int32" => 4,
                _ => return Err(error("contiguous layout requires bf16/fp32/int32")),
            },
        )?
    } else {
        let shape = tensor["shape"].as_array().unwrap();
        if shape.len() != 2 {
            return Err(error("quantized v3 tensors must be matrices"));
        }
        let n = shape[0].as_u64().unwrap();
        let k = shape[1].as_u64().unwrap();
        match (format.as_str(), layout.as_str()) {
            (
                "q4_g64_fp16" | "q5_g64_fp16" | "q6_g64_fp16" | "q8_g32_fp16",
                "row_split_k128_v1",
            ) => {
                let (group, high) = match format.as_str() {
                    "q5_g64_fp16" => (64, 8),
                    "q6_g64_fp16" => (64, 16),
                    "q8_g32_fp16" => (32, 0),
                    _ => (64, 0),
                };
                let groups = align(k, 128)? / group;
                add(
                    add(
                        align(mul(mul(n, groups)?, 32)?, 256)?,
                        align(mul(mul(n, groups)?, high)?, 256)?,
                    )?,
                    mul(mul(n, groups)?, 2)?,
                )?
            }
            ("fp8_e4m3fn_row_bf16", "row_scale_v1") => add(align(count, 256)?, mul(n, 2)?)?,
            ("nvfp4", "block_scale_k16_m128x4_v1") if n % 128 == 0 && k % 64 == 0 => {
                add(add(align(count / 2, 256)?, count / 16)?, 4)?
            }
            _ => return Err(error("unknown or incompatible v3 format/layout pair")),
        }
    };
    if uint(&tensor["bytes"], "tensor bytes", true)? != bytes
        || uint(&tensor["offset"], "tensor offset", false)? % 256 != 0
    {
        return Err(error(
            "v3 tensor encoded bytes or alignment differs from native geometry",
        ));
    }
    Ok(())
}
// nlohmann's reader rejects duplicate members at every depth. serde Value's
// ordinary parser loses those facts, so use a recursive unique-member visitor.
struct Unique(Value);
impl<'de> serde::Deserialize<'de> for Unique {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Unique;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON with unique members")
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Unique, E> {
                Ok(Unique(Value::Bool(v)))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Unique, E> {
                Ok(Unique(
                    serde_json::Number::from_f64(v)
                        .ok_or_else(|| E::custom("nonfinite number"))?
                        .into(),
                ))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                let mut out = Vec::new();
                while let Some(Unique(v)) = a.next_element()? {
                    out.push(v);
                }
                Ok(Unique(Value::Array(out)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                let mut out = Map::new();
                while let Some((k, Unique(v))) = a.next_entry::<String, Unique>()? {
                    if out.insert(k, v).is_some() {
                        return Err(serde::de::Error::custom("duplicate JSON member"));
                    }
                }
                Ok(Unique(Value::Object(out)))
            }
        }
        d.deserialize_any(Visitor)
    }
}

pub(super) fn inspect(
    path: &Path,
    file: &mut std::fs::File,
    file_bytes: u64,
) -> Result<NinferContainerMetadata, NinferContainerError> {
    let mut header = [0; 32];
    file.read_exact(&mut header)?;
    let json_bytes = u64::from_le_bytes(header[8..16].try_into().unwrap());
    if json_bytes == 0 || json_bytes > MAX_NINFER_DIRECTORY_BYTES {
        return Err(NinferContainerError::InvalidDirectoryLength {
            observed: json_bytes,
            maximum: MAX_NINFER_DIRECTORY_BYTES,
        });
    }
    let payload_start = align(add(32, json_bytes)?, 4096)?;
    if payload_start > file_bytes {
        return Err(NinferContainerError::TruncatedDirectory);
    }
    let mut bytes = vec![0; json_bytes as usize];
    file.read_exact(&mut bytes)?;
    let Unique(directory) = serde_json::from_slice(&bytes).map_err(|e| error(e.to_string()))?;
    let root = object(
        &directory,
        "directory",
        &["components", "objects", "bindings", "uses", "files"],
        &["metadata", "provenance"],
    )?;
    for k in ["metadata", "provenance"] {
        if root.get(k).is_some_and(|v| !v.is_object()) {
            return Err(error(format!("{k} must be an object")));
        }
    }
    if let Some(name) = root.get("metadata").and_then(|v| v.get("name")) {
        id(name, "metadata.name")?;
    }
    let files = root["files"]
        .as_array()
        .filter(|f| !f.is_empty() && f.len() <= 1024)
        .ok_or_else(|| error("files must contain 1..1024 entries"))?;
    let mut names = BTreeSet::new();
    names.insert(
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_owned(),
    );
    let mut payload_bytes = 0;
    let mut v3_files = Vec::new();
    for (index, f) in files.iter().enumerate() {
        let f = object(f, "file", &["path", "payload_bytes"], &[])?;
        let count = uint(&f["payload_bytes"], "file payload_bytes", true)?;
        let name = if index == 0 {
            if !f["path"].is_null() {
                return Err(error("files[0].path must be null"));
            }
            None
        } else {
            let name = id(&f["path"], "part path")?;
            if name == "."
                || name == ".."
                || name.contains(['/', '\\', ':'])
                || !names.insert(name.clone())
            {
                return Err(error("invalid or duplicate sibling part path"));
            }
            Some(name)
        };
        if let Some(name) = &name {
            let part_path = path.parent().unwrap_or(Path::new(".")).join(name);
            if std::fs::symlink_metadata(&part_path)?
                .file_type()
                .is_symlink()
            {
                return Err(error("v3 continuation cannot be a symlink"));
            }
            let mut part = std::fs::File::open(part_path)?;
            let mut part_header = [0; 32];
            part.read_exact(&mut part_header)?;
            if &part_header[..8] != b"NINPRT\0\x03"
                || u64::from_le_bytes(part_header[8..16].try_into().unwrap()) != index as u64
                || part_header[16..] != header[16..]
                || part.metadata()?.len() != add(4096, count)?
            {
                return Err(error("v3 continuation header, identity or length mismatch"));
            }
        } else if file_bytes != add(payload_start, count)? {
            return Err(error("v3 entry length differs from directory"));
        }
        v3_files.push((
            name,
            payload_bytes,
            if index == 0 { payload_start } else { 4096 },
        ));
        payload_bytes = add(payload_bytes, count)?;
    }
    let objects = root["objects"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 100_000)
        .ok_or_else(|| error("objects must contain 1..100000 descriptors"))?;
    let mut inventory = BTreeMap::new();
    let mut end = 0;
    for o in objects {
        let tensor = o["kind"] == "tensor";
        let o = if tensor {
            object(
                o,
                "tensor",
                &["id", "kind", "shape", "format", "layout", "offset", "bytes"],
                &[],
            )?
        } else {
            object(
                o,
                "resource",
                &["id", "kind", "encoding", "offset", "bytes"],
                &[],
            )?
        };
        if !tensor && (o["kind"] != "resource" || o["encoding"] != "raw_bytes_v1") {
            return Err(error("invalid resource kind/encoding"));
        }
        let name = id(&o["id"], "object id")?;
        let offset = uint(&o["offset"], "object offset", false)?;
        let count = uint(&o["bytes"], "object bytes", true)?;
        let next = add(offset, count)?;
        if offset < end || next > payload_bytes || inventory.insert(name, (tensor, o)).is_some() {
            return Err(error(
                "objects overlap, are unordered, duplicate or exceed payload",
            ));
        }
        end = next;
        if tensor {
            geometry(o)?;
        }
    }
    let components = root["components"]
        .as_object()
        .filter(|c| c.contains_key("text"))
        .ok_or_else(|| error("components must contain text"))?;
    let mut resources = Vec::new();
    for (name, c) in components {
        id(&json!(name), "component id")?;
        let c = object(
            c,
            "component",
            &["config"],
            &["target", "resources", "proposal"],
        )?;
        if !c["config"].is_object() {
            return Err(error("component config must be an object"));
        }
        if let Some(target) = c.get("target") {
            if !components.contains_key(&id(target, "component target")?) {
                return Err(error("component target is missing"));
            }
        }
        if name != "text" && c.get("target").and_then(Value::as_str) != Some("text") {
            return Err(error("optional model components must target text"));
        }
        if let Some(r) = c.get("resources") {
            let r = r
                .as_object()
                .ok_or_else(|| error("component resources must be an object"))?;
            for (role, reference) in r {
                id(&json!(role), "resource role")?;
                let reference = id(reference, "resource reference")?;
                let (tensor, o) = inventory
                    .get(&reference)
                    .ok_or_else(|| error("missing resource object"))?;
                if *tensor {
                    return Err(error("component resource references a tensor"));
                }
                // Only the native frontend owner's resources qualify frontend
                // semantics; optional/unknown components cannot shadow them.
                if name == "text"
                    && matches!(
                        role.as_str(),
                        "tokenizer.json"
                            | "tokenizer_config.json"
                            | "chat_template.jinja"
                            | "generation_config.json"
                    )
                    || name == "vision"
                        && matches!(
                            role.as_str(),
                            "preprocessor_config.json" | "video_preprocessor_config.json"
                        )
                {
                    resources.push((
                        format!("frontend/{role}"),
                        uint(&o["offset"], "resource offset", false)?,
                        uint(&o["bytes"], "resource bytes", true)?,
                    ));
                }
            }
        }
        if let Some(p) = c.get("proposal") {
            let p = object(p, "proposal", &["domain"], &["rows"])?;
            if name != "text"
                || !(p["domain"] == "full" && !p.contains_key("rows")
                    || p["domain"] == "indexed"
                        && p.get("rows")
                            .is_some_and(|v| uint(v, "proposal rows", true).is_ok()))
            {
                return Err(error("invalid native proposal domain/rows"));
            }
        }
    }
    for role in [
        "tokenizer.json",
        "tokenizer_config.json",
        "chat_template.jinja",
        "generation_config.json",
    ] {
        if components["text"]["resources"].get(role).is_none() {
            return Err(error(format!("missing text frontend resource {role}")));
        }
    }
    if components.contains_key("vision") {
        for role in ["preprocessor_config.json", "video_preprocessor_config.json"] {
            if components["vision"]["resources"].get(role).is_none() {
                return Err(error(format!("missing vision resource {role}")));
            }
        }
    }
    let config = &components["text"]["config"];
    let architectures = config["architectures"]
        .as_array()
        .filter(|a| a.len() == 1)
        .ok_or_else(|| error("text architectures must name exactly one implementation"))?;
    let architecture = id(&architectures[0], "text architecture")?;
    if !matches!(
        (architecture.as_str(), config["model_type"].as_str()),
        ("Qwen3_5ForCausalLM", Some("qwen3_5_text"))
            | ("Qwen3_5MoeForCausalLM", Some("qwen3_5_moe_text"))
    ) {
        return Err(error("unsupported native architecture/config pair"));
    }
    let bindings = root["bindings"]
        .as_object()
        .ok_or_else(|| error("bindings must be an object"))?;
    let mut binding_counts = BTreeMap::new();
    for (name, b) in bindings {
        id(&json!(name), "parameter name")?;
        binding_counts.insert(name.clone(), binding_elements(b, &inventory)?);
    }
    let h = uint(&config["hidden_size"], "hidden_size", true)?;
    let vocab = uint(&config["vocab_size"], "vocab_size", true)?;
    for (name, count) in [
        ("text/token_embedding", mul(vocab, h)?),
        ("text/output_head", mul(vocab, h)?),
        ("text/final_norm", h),
    ] {
        if binding_counts.get(name) != Some(&count) {
            return Err(error(format!(
                "missing or conflicting text parameter {name}"
            )));
        }
    }
    let uses = root["uses"]
        .as_array()
        .ok_or_else(|| error("uses must be an array"))?;
    let mut seen = BTreeSet::new();
    for u in uses {
        let u = object(
            u,
            "Use",
            &["parameter", "input"],
            &["activation_policy", "auxiliaries"],
        )?;
        let parameter = id(&u["parameter"], "Use parameter")?;
        let input = id(&u["input"], "Use input")?;
        if !bindings.contains_key(&parameter) || !seen.insert((parameter, input)) {
            return Err(error(
                "Use references a missing parameter or duplicates another Use",
            ));
        }
        if u.get("activation_policy")
            .is_some_and(|p| !matches!(p.as_str(), Some("A16Only" | "AllowA8" | "AllowA4")))
        {
            return Err(error("invalid activation policy"));
        }
        if let Some(aux) = u.get("auxiliaries") {
            let aux = aux
                .as_object()
                .ok_or_else(|| error("Use auxiliaries must be an object"))?;
            for (role, b) in aux {
                id(&json!(role), "auxiliary role")?;
                binding_elements(b, &inventory)?;
            }
        }
    }
    validate_text(config, &binding_counts, components)?;
    validate_binding_contracts(components, bindings, &inventory, uses)?;
    let dflash2 = components.contains_key("dflash2")
        && validate_draft(
            "dflash2",
            &components["dflash2"],
            &binding_counts,
            h,
            vocab,
            config,
        )?;
    let directory_digest = format!("{:x}", Sha256::digest([&header[16..], &bytes].concat()));
    let metadata = NinferContainerMetadata {
        identity: NinferArtifactIdentity {
            container_version: 3,
            model_id: architecture,
            weights_id: format!("v3-{directory_digest}"),
        },
        dflash2,
        resources,
        metadata_bytes_read: 32 + json_bytes,
        v3_directory: Some(directory),
        v3_files,
    };
    validate_bound_values(path, &metadata)?;
    Ok(metadata)
}
fn validate_bound_values(
    path: &Path,
    metadata: &NinferContainerMetadata,
) -> Result<(), NinferContainerError> {
    let d = metadata.v3_directory.as_ref().unwrap();
    let objects: BTreeMap<_, _> = d["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| (o["id"].as_str().unwrap(), o))
        .collect();
    let read = |binding: &Value, limit: u64| -> Result<Vec<u8>, NinferContainerError> {
        let ranges = if let Some(name) = binding["object"].as_str() {
            let o = objects[name];
            vec![(o["offset"].as_u64().unwrap(), o["bytes"].as_u64().unwrap())]
        } else {
            binding["parts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| {
                    let o = objects[p["object"].as_str().unwrap()];
                    let begin = p["range"][0].as_u64().unwrap();
                    let end = p["range"][1].as_u64().unwrap();
                    Ok((
                        add(o["offset"].as_u64().unwrap(), mul(begin, 4)?)?,
                        mul(end - begin, 4)?,
                    ))
                })
                .collect::<Result<Vec<_>, NinferContainerError>>()?
        };
        let mut result = Vec::new();
        for (offset, bytes) in ranges {
            if add(result.len() as u64, bytes)? > limit {
                return Err(error(
                    "native semantic binding exceeds bounded inspection limit",
                ));
            }
            result.extend(read_resource(path, metadata, offset, bytes)?);
        }
        Ok(result)
    };
    for u in d["uses"].as_array().unwrap() {
        if let Some(binding) = u.pointer("/auxiliaries/activation_input_divisor") {
            let bytes = read(binding, 4)?;
            let value = f32::from_le_bytes(
                bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| error("activation divisor must be a scalar FP32"))?,
            );
            if !value.is_finite() || value <= 0.0 {
                return Err(error(
                    "activation input divisor must be finite and positive",
                ));
            }
        }
    }
    if d.pointer("/components/text/proposal/domain")
        .and_then(Value::as_str)
        == Some("indexed")
    {
        let bytes = read(&d["bindings"]["proposal/token_ids"], 8 * 1024 * 1024)?;
        let vocab = d["components"]["text"]["config"]["vocab_size"]
            .as_u64()
            .unwrap();
        let mut seen = BTreeSet::new();
        for token in bytes.chunks_exact(4) {
            let token = i32::from_le_bytes(token.try_into().unwrap());
            if token < 0 || token as u64 >= vocab || !seen.insert(token) {
                return Err(error(
                    "proposal token IDs must be unique and within the target vocabulary",
                ));
            }
        }
    }
    Ok(())
}
fn binding_elements(
    binding: &Value,
    objects: &BTreeMap<String, (bool, &Map<String, Value>)>,
) -> Result<u64, NinferContainerError> {
    let binding = binding
        .as_object()
        .ok_or_else(|| error("binding must be an object"))?;
    let reference = |v: &Value| -> Result<u64, NinferContainerError> {
        let name = id(v, "binding object")?;
        let (tensor, o) = objects
            .get(&name)
            .ok_or_else(|| error("binding references missing object"))?;
        if !tensor {
            return Err(error("binding references resource"));
        }
        elements(&o["shape"])
    };
    if binding.len() == 1 && binding.contains_key("object") {
        return reference(&binding["object"]);
    }
    if binding.len() != 1 || !binding.contains_key("parts") {
        return Err(error("binding requires object or parts"));
    }
    let parts = binding["parts"]
        .as_array()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| error("binding parts must be nonempty"))?;
    parts.iter().try_fold(0, |sum, p| {
        let p = object(p, "Part", &["object", "range"], &[])?;
        let bounds = p["range"]
            .as_array()
            .filter(|b| b.len() == 2)
            .ok_or_else(|| error("Part range requires two integers"))?;
        let begin = uint(&bounds[0], "Part begin", false)?;
        let end = uint(&bounds[1], "Part end", false)?;
        if begin >= end || end > reference(&p["object"])? {
            return Err(error("Part exceeds source elements"));
        }
        add(sum, end - begin)
    })
}
fn require_count(
    counts: &BTreeMap<String, u64>,
    name: &str,
    count: u64,
) -> Result<(), NinferContainerError> {
    if counts.get(name) != Some(&count) {
        return Err(error(format!(
            "missing or conflicting native parameter {name}"
        )));
    }
    Ok(())
}
fn dim(c: &Value, k: &str) -> Result<u64, NinferContainerError> {
    uint(&c[k], k, true).and_then(|n| {
        if n <= u32::MAX as u64 {
            Ok(n)
        } else {
            Err(error(format!("{k} exceeds u32")))
        }
    })
}
fn positive(c: &Value, k: &str) -> Result<f64, NinferContainerError> {
    c[k].as_f64()
        .filter(|v| v.is_finite() && (*v as f32).is_finite() && (*v as f32) > 0.0)
        .map(|v| f64::from(v as f32))
        .ok_or_else(|| error(format!("{k} must be positive finite FP32")))
}
fn attention_dims(c: &Value) -> Result<(u64, u64, u64), NinferContainerError> {
    let n = dim(c, "num_attention_heads")?;
    let kv = dim(c, "num_key_value_heads")?;
    let d = dim(c, "head_dim")?;
    if n % kv != 0 {
        return Err(error("attention heads are not divisible by KV heads"));
    }
    Ok((mul(n, d)?, mul(kv, d)?, d))
}
fn block(
    c: &Value,
    counts: &BTreeMap<String, u64>,
    p: &str,
    h: u64,
    linear: bool,
) -> Result<(), NinferContainerError> {
    let required = |name: &str, n: u64| require_count(counts, &format!("{p}{name}"), n);
    required("input_norm", h)?;
    required("post_attention_norm", h)?;
    if linear {
        let k = mul(
            dim(c, "linear_num_key_heads")?,
            dim(c, "linear_key_head_dim")?,
        )?;
        let heads = dim(c, "linear_num_value_heads")?;
        let v = mul(heads, dim(c, "linear_value_head_dim")?)?;
        if heads % dim(c, "linear_num_key_heads")? != 0 {
            return Err(error("GDN heads conflict"));
        }
        for (name, n) in [
            ("query", mul(k, h)?),
            ("key", mul(k, h)?),
            ("value", mul(v, h)?),
            ("z", mul(v, h)?),
            ("a_projection", mul(heads, h)?),
            ("b_projection", mul(heads, h)?),
            ("a_log", heads),
            ("dt_bias", heads),
            (
                "convolution",
                mul(dim(c, "linear_conv_kernel_dim")?, add(mul(2, k)?, v)?)?,
            ),
            ("norm", dim(c, "linear_value_head_dim")?),
            ("output", mul(h, v)?),
        ] {
            required(&format!("gdn/{name}"), n)?;
        }
    } else {
        let (q, k, d) = attention_dims(c)?;
        for (name, n) in [
            ("query", mul(q, h)?),
            ("key", mul(k, h)?),
            ("gate", mul(q, h)?),
            ("value", mul(k, h)?),
            ("query_norm", d),
            ("key_norm", d),
            ("output", mul(h, q)?),
        ] {
            required(&format!("attention/{name}"), n)?;
        }
    }
    if c["model_type"] == "qwen3_5_moe_text" {
        if c.get("intermediate_size").is_some() {
            return Err(error("MoE config carries Dense width"));
        }
        let experts = dim(c, "num_experts")?;
        if experts > 100_000 || dim(c, "num_experts_per_tok")? > experts {
            return Err(error("invalid expert count"));
        }
        let ir = dim(c, "moe_intermediate_size")?;
        let shared = dim(c, "shared_expert_intermediate_size")?;
        required("moe/router", mul(experts, h)?)?;
        required("moe/shared_score", h)?;
        for i in 0..experts {
            for name in ["gate", "up", "down"] {
                required(&format!("moe/experts/{i}/{name}"), mul(ir, h)?)?;
            }
        }
        for name in ["gate", "up", "down"] {
            required(&format!("moe/shared/{name}"), mul(shared, h)?)?;
        }
    } else {
        for key in [
            "num_experts",
            "num_experts_per_tok",
            "moe_intermediate_size",
            "shared_expert_intermediate_size",
        ] {
            if c.get(key).is_some() {
                return Err(error("Dense config carries MoE fields"));
            }
        }
        for name in ["gate", "up", "down"] {
            required(
                &format!("mlp/{name}"),
                mul(dim(c, "intermediate_size")?, h)?,
            )?;
        }
    }
    Ok(())
}
fn validate_text(
    c: &Value,
    counts: &BTreeMap<String, u64>,
    components: &Map<String, Value>,
) -> Result<(), NinferContainerError> {
    object(
        c,
        "text config",
        &[
            "architectures",
            "model_type",
            "hidden_size",
            "vocab_size",
            "num_hidden_layers",
            "max_position_embeddings",
            "tie_word_embeddings",
            "rms_norm_eps",
            "layer_types",
        ],
        &[
            "num_attention_heads",
            "num_key_value_heads",
            "head_dim",
            "rope_parameters",
            "linear_num_key_heads",
            "linear_key_head_dim",
            "linear_num_value_heads",
            "linear_value_head_dim",
            "linear_conv_kernel_dim",
            "intermediate_size",
            "num_experts",
            "num_experts_per_tok",
            "moe_intermediate_size",
            "shared_expert_intermediate_size",
        ],
    )?;
    let h = dim(c, "hidden_size")?;
    dim(c, "vocab_size")?;
    dim(c, "max_position_embeddings")?;
    positive(c, "rms_norm_eps")?;
    if !c["tie_word_embeddings"].is_boolean() {
        return Err(error("tie_word_embeddings must be boolean"));
    }
    let layers = dim(c, "num_hidden_layers")?;
    if layers > 1024 {
        return Err(error("layer count exceeds bounded inspection limit"));
    }
    let types = c["layer_types"]
        .as_array()
        .filter(|v| v.len() == layers as usize)
        .ok_or_else(|| error("layer_types must cover every text block"))?;
    if types.iter().any(|v| v == "full_attention")
        || components.contains_key("mtp")
        || [
            "rope_parameters",
            "head_dim",
            "num_attention_heads",
            "num_key_value_heads",
        ]
        .iter()
        .any(|k| c.get(*k).is_some())
    {
        let (_, _, d) = attention_dims(c)?;
        let r = &c["rope_parameters"];
        object(
            r,
            "text RoPE",
            &["rope_theta", "partial_rotary_factor", "mrope_section"],
            &[],
        )?;
        positive(r, "rope_theta")?;
        let factor = positive(r, "partial_rotary_factor")?;
        let width = (d as f64 * factor) as u64;
        if factor > 1.0 || width == 0 || width % 2 != 0 {
            return Err(error("invalid rotary dimension"));
        }
        let sections = r["mrope_section"]
            .as_array()
            .filter(|v| v.len() == 3)
            .ok_or_else(|| error("MRoPE requires three sections"))?;
        let values = sections
            .iter()
            .map(|v| {
                uint(v, "MRoPE section", false).and_then(|n| {
                    if n <= u32::MAX as u64 {
                        Ok(n)
                    } else {
                        Err(error("MRoPE section exceeds native u32"))
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if values.iter().sum::<u64>() != width / 2 {
            return Err(error("MRoPE sections conflict with rotary width"));
        }
        // Count the native interleaved axes without allocating/iterating a
        // potentially enormous untrusted rotary width.
        let pairs = width / 2;
        let axis1 = values[1].min((pairs + 1) / 3);
        let axis2 = values[2].min(pairs / 3);
        let actual = [pairs - axis1 - axis2, axis1, axis2];
        if actual.as_slice() != values.as_slice() {
            return Err(error("MRoPE sections exceed interleaved axis ranges"));
        }
    }
    for (i, kind) in types.iter().enumerate() {
        if kind != "full_attention" && kind != "linear_attention" {
            return Err(error("unknown layer type"));
        }
        block(
            c,
            counts,
            &format!("text/layers/{i}/"),
            h,
            kind == "linear_attention",
        )?;
    }
    if let Some(mtp) = components.get("mtp") {
        let arch = if c["model_type"] == "qwen3_5_moe_text" {
            "Qwen3_5MoeMTP"
        } else {
            "Qwen3_5MTP"
        };
        object(&mtp["config"], "MTP config", &["architectures"], &[])?;
        if mtp["config"]["architectures"] != json!([arch]) {
            return Err(error("MTP architecture conflicts with text mathematics"));
        }
        require_count(counts, "mtp/input_projection", mul(2, mul(h, h)?)?)?;
        for name in ["embedding_norm", "hidden_norm", "final_norm"] {
            require_count(counts, &format!("mtp/{name}"), h)?;
        }
        block(c, counts, "mtp/layers/0/", h, false)?;
    }
    if let Some(d) = components.get("dflash") {
        validate_draft("dflash", d, counts, h, dim(c, "vocab_size")?, c)?;
    }
    if let Some(v) = components.get("vision") {
        validate_vision(&v["config"], counts, h)?;
    }
    if let Some(p) = components["text"].get("proposal") {
        let rows = if p["domain"] == "indexed" {
            dim(p, "rows")?
        } else {
            dim(c, "vocab_size")?
        };
        if rows > dim(c, "vocab_size")? {
            return Err(error("proposal rows exceed vocabulary"));
        }
        require_count(counts, "proposal/head", mul(rows, h)?)?;
        if p["domain"] == "indexed" {
            require_count(counts, "proposal/token_ids", rows)?;
        }
    }
    Ok(())
}
fn validate_draft(
    component: &str,
    value: &Value,
    counts: &BTreeMap<String, u64>,
    h: u64,
    vocab: u64,
    target: &Value,
) -> Result<bool, NinferContainerError> {
    let c = &value["config"];
    object(
        c,
        "draft config",
        &[
            "architectures",
            "model_type",
            "intermediate_size",
            "num_attention_heads",
            "num_key_value_heads",
            "head_dim",
            "num_hidden_layers",
            "max_position_embeddings",
            "rms_norm_eps",
            "rope_parameters",
            "layer_types",
            "dflash_config",
        ],
        &["sliding_window"],
    )?;
    if c["architectures"]
        != json!([if component == "dflash2" {
            "DFlash2DraftModel"
        } else {
            "DFlashDraftModel"
        }])
        || c["model_type"] != "qwen3"
    {
        return Err(error("draft architecture/config mismatch"));
    }
    let layers = dim(c, "num_hidden_layers")?;
    if layers > 1024 {
        return Err(error("draft layer count exceeds inspection limit"));
    }
    let (q, k, d) = attention_dims(c)?;
    if d % 2 != 0 {
        return Err(error("draft head dimension must be even"));
    }
    dim(c, "max_position_embeddings")?;
    positive(c, "rms_norm_eps")?;
    positive(&c["rope_parameters"], "rope_theta")?;
    let types = c["layer_types"]
        .as_array()
        .filter(|v| v.len() == layers as usize)
        .ok_or_else(|| error("draft layer types conflict"))?;
    for kind in types {
        if kind != "full_attention" && kind != "sliding_attention" {
            return Err(error("invalid draft attention type"));
        }
        if kind == "sliding_attention" {
            dim(c, "sliding_window")?;
        }
    }
    let flash = &c["dflash_config"];
    object(
        flash,
        "draft feature config",
        &["target_layer_ids", "mask_token_id"],
        if component == "dflash2" {
            &[
                "conv_kernel_size",
                "conv_group_size",
                "selector_rank",
                "selector_top_k",
            ]
        } else {
            &[]
        },
    )?;
    let taps = flash["target_layer_ids"]
        .as_array()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| error("draft target layers must be nonempty"))?;
    let mut seen = BTreeSet::new();
    for tap in taps {
        let tap = uint(tap, "target layer", false)?;
        if tap >= dim(target, "num_hidden_layers")? || !seen.insert(tap) {
            return Err(error("invalid or duplicate draft target layer"));
        }
    }
    if uint(&flash["mask_token_id"], "mask token", false)? >= vocab {
        return Err(error("draft mask token exceeds vocabulary"));
    }
    require_count(
        counts,
        &format!("{component}/feature_projection"),
        mul(mul(h, h)?, taps.len() as u64)?,
    )?;
    for name in ["context_norm", "final_norm"] {
        require_count(counts, &format!("{component}/{name}"), h)?;
    }
    for i in 0..layers {
        let p = format!("{component}/layers/{i}/");
        for (name, n) in [
            ("input_norm", h),
            ("post_attention_norm", h),
            ("attention/query", mul(q, h)?),
            ("attention/key", mul(k, h)?),
            ("attention/value", mul(k, h)?),
            ("attention/context_key", mul(k, h)?),
            ("attention/context_value", mul(k, h)?),
            ("attention/query_norm", d),
            ("attention/key_norm", d),
            ("attention/output", mul(h, q)?),
        ] {
            require_count(counts, &format!("{p}{name}"), n)?;
        }
        for name in ["gate", "up", "down"] {
            require_count(
                counts,
                &format!("{p}mlp/{name}"),
                mul(dim(c, "intermediate_size")?, h)?,
            )?;
        }
    }
    if component == "dflash2" {
        let kernel = dim(flash, "conv_kernel_size")?;
        let group = dim(flash, "conv_group_size")?;
        let rank = dim(flash, "selector_rank")?;
        dim(flash, "selector_top_k")?;
        if h % group != 0 {
            return Err(error(
                "DFlash2 convolution group conflicts with hidden width",
            ));
        }
        for i in 0..layers {
            for name in ["attention_conv", "mlp_conv"] {
                for (suffix, size) in [
                    ("base_kernel", mul(mul(2, kernel)?, h)?),
                    (
                        "kernel_projection",
                        mul(mul(mul(2, kernel)?, h / group)?, h)?,
                    ),
                ] {
                    require_count(counts, &format!("dflash2/layers/{i}/{name}/{suffix}"), size)?;
                }
            }
        }
        for (name, size) in [
            ("hidden_projection", mul(rank, h)?),
            ("predecessor_codebook", mul(vocab, rank)?),
            ("successor_codebook", mul(vocab, rank)?),
        ] {
            require_count(counts, &format!("dflash2/candidate_selector/{name}"), size)?;
        }
    }
    Ok(true)
}
fn validate_vision(
    c: &Value,
    counts: &BTreeMap<String, u64>,
    target: u64,
) -> Result<(), NinferContainerError> {
    object(
        c,
        "vision config",
        &[
            "model_type",
            "depth",
            "hidden_size",
            "intermediate_size",
            "num_heads",
            "patch_size",
            "temporal_patch_size",
            "spatial_merge_size",
            "num_position_embeddings",
        ],
        &[],
    )?;
    if !matches!(
        c["model_type"].as_str(),
        Some("qwen3_5_vision" | "qwen3_5_moe_vision")
    ) {
        return Err(error("unknown vision model type"));
    }
    let h = dim(c, "hidden_size")?;
    let heads = dim(c, "num_heads")?;
    let positions = dim(c, "num_position_embeddings")?;
    let side = (positions as f64).sqrt() as u64;
    if h % heads != 0 || (h / heads) % 4 != 0 || side * side != positions {
        return Err(error("invalid vision head/position geometry"));
    }
    let layers = dim(c, "depth")?;
    if layers > 1024 {
        return Err(error("vision depth exceeds inspection limit"));
    }
    let intermediate = dim(c, "intermediate_size")?;
    let patch = mul(
        mul(
            mul(3, dim(c, "temporal_patch_size")?)?,
            dim(c, "patch_size")?,
        )?,
        dim(c, "patch_size")?,
    )?;
    let merger = mul(
        mul(dim(c, "spatial_merge_size")?, dim(c, "spatial_merge_size")?)?,
        h,
    )?;
    for (name, n) in [
        ("patch_embedding", mul(h, patch)?),
        ("patch_embedding_bias", h),
        ("position_embedding", mul(positions, h)?),
        ("merger/norm_weight", h),
        ("merger/norm_bias", h),
        ("merger/fc1", mul(merger, merger)?),
        ("merger/fc1_bias", merger),
        ("merger/fc2", mul(target, merger)?),
        ("merger/fc2_bias", target),
    ] {
        require_count(counts, &format!("vision/{name}"), n)?;
    }
    for i in 0..layers {
        for name in [
            "norm1_weight",
            "norm1_bias",
            "norm2_weight",
            "norm2_bias",
            "attention/query_bias",
            "attention/key_bias",
            "attention/value_bias",
            "attention/output_bias",
            "mlp/fc2_bias",
        ] {
            require_count(counts, &format!("vision/layers/{i}/{name}"), h)?;
        }
        for name in ["query", "key", "value", "output"] {
            require_count(
                counts,
                &format!("vision/layers/{i}/attention/{name}"),
                mul(h, h)?,
            )?;
        }
        for name in ["fc1", "fc2"] {
            require_count(
                counts,
                &format!("vision/layers/{i}/mlp/{name}"),
                mul(h, intermediate)?,
            )?;
        }
        require_count(
            counts,
            &format!("vision/layers/{i}/mlp/fc1_bias"),
            intermediate,
        )?;
    }
    Ok(())
}
pub(super) fn read_resource(
    path: &Path,
    metadata: &NinferContainerMetadata,
    offset: u64,
    bytes: u64,
) -> Result<Vec<u8>, NinferContainerError> {
    let mut content = vec![0; bytes as usize];
    let mut copied = 0;
    for (i, (name, begin, start)) in metadata.v3_files.iter().enumerate() {
        let end = metadata
            .v3_files
            .get(i + 1)
            .map(|f| f.1)
            .unwrap_or(u64::MAX);
        let position = add(offset, copied)?;
        if position < *begin || position >= end {
            continue;
        }
        let count = (bytes - copied).min(end - position);
        let file_path = name
            .as_ref()
            .map(|n| path.parent().unwrap_or(Path::new(".")).join(n))
            .unwrap_or_else(|| path.to_owned());
        let mut file = std::fs::File::open(file_path)?;
        file.seek(SeekFrom::Start(add(*start, position - *begin)?))?;
        file.read_exact(&mut content[copied as usize..(copied + count) as usize])?;
        copied += count;
        if copied == bytes {
            break;
        }
    }
    if copied != bytes {
        return Err(error("resource range exceeds v3 file set"));
    }
    Ok(content)
}

// Native logical shapes and required Uses. Parts cover flat ranges; whole
// objects must retain mathematical rank/orientation and direct value formats.
type NativeBindingContract = (Vec<u64>, Option<&'static str>, Vec<String>);
fn binding_contract(
    name: &str,
    components: &Map<String, Value>,
) -> Result<Option<NativeBindingContract>, NinferContainerError> {
    let Some((component, rest)) = name.split_once('/') else {
        return Ok(None);
    };
    let text = &components["text"]["config"];
    let th = dim(text, "hidden_size")?;
    let vocab = dim(text, "vocab_size")?;
    if component == "proposal" {
        let Some(p) = components["text"].get("proposal") else {
            return Ok(None);
        };
        let rows = if p["domain"] == "indexed" {
            uint(&p["rows"], "proposal rows", true)?
        } else {
            vocab
        };
        return Ok(match rest {
            "head" => Some((vec![rows, th], None, Vec::new())),
            "token_ids" => Some((vec![rows], Some("int32"), Vec::new())),
            _ => None,
        });
    }
    let Some(component_value) = components.get(component) else {
        return Ok(None);
    };
    let c = if component == "mtp" {
        text
    } else {
        &component_value["config"]
    };
    let h = if matches!(component, "dflash" | "dflash2") {
        th
    } else {
        dim(c, "hidden_size")?
    };
    let direct = |shape| Some((shape, Some("bf16"), Vec::new()));
    let matrix = |shape, input: String| Some((shape, None, vec![input]));
    if rest == "token_embedding" {
        return Ok(Some((vec![vocab, h], None, Vec::new())));
    }
    if rest == "output_head" {
        return Ok(matrix(vec![vocab, h], "text/final_hidden".into()));
    }
    if matches!(
        rest,
        "final_norm" | "embedding_norm" | "hidden_norm" | "context_norm"
    ) {
        return Ok(direct(vec![h]));
    }
    if component == "mtp" && rest == "input_projection" {
        return Ok(matrix(vec![h, mul(2, h)?], "mtp/stem_input".into()));
    }
    if matches!(component, "dflash" | "dflash2") && rest == "feature_projection" {
        let taps = c["dflash_config"]["target_layer_ids"]
            .as_array()
            .ok_or_else(|| error("missing draft taps"))?
            .len() as u64;
        return Ok(matrix(
            vec![h, mul(taps, h)?],
            format!("{component}/target_features"),
        ));
    }
    if component == "dflash2" && rest.starts_with("candidate_selector/") {
        let rank = dim(&c["dflash_config"], "selector_rank")?;
        return Ok(match rest {
            "candidate_selector/hidden_projection" => {
                matrix(vec![rank, h], "dflash2/final_hidden".into())
            }
            "candidate_selector/predecessor_codebook" | "candidate_selector/successor_codebook" => {
                direct(vec![vocab, rank])
            }
            _ => None,
        });
    }
    if component == "vision" {
        let intermediate = dim(c, "intermediate_size")?;
        let patch = mul(
            mul(
                mul(3, dim(c, "temporal_patch_size")?)?,
                dim(c, "patch_size")?,
            )?,
            dim(c, "patch_size")?,
        )?;
        let merger = mul(
            mul(dim(c, "spatial_merge_size")?, dim(c, "spatial_merge_size")?)?,
            h,
        )?;
        match rest {
            "patch_embedding" => return Ok(matrix(vec![h, patch], "vision/patch_input".into())),
            "patch_embedding_bias" => return Ok(direct(vec![h])),
            "position_embedding" => return Ok(direct(vec![dim(c, "num_position_embeddings")?, h])),
            "merger/norm_weight" | "merger/norm_bias" => return Ok(direct(vec![h])),
            "merger/fc1" => return Ok(matrix(vec![merger, merger], "vision/merger/input".into())),
            "merger/fc1_bias" => return Ok(direct(vec![merger])),
            "merger/fc2" => return Ok(matrix(vec![th, merger], "vision/merger/activation".into())),
            "merger/fc2_bias" => return Ok(direct(vec![th])),
            _ => {}
        }
        let Some((prefix, suffix)) = layer_parameter(name) else {
            return Ok(None);
        };
        return Ok(match suffix {
            "norm1_weight"
            | "norm1_bias"
            | "norm2_weight"
            | "norm2_bias"
            | "attention/query_bias"
            | "attention/key_bias"
            | "attention/value_bias"
            | "attention/output_bias"
            | "mlp/fc2_bias" => direct(vec![h]),
            "mlp/fc1_bias" => direct(vec![intermediate]),
            "attention/query" | "attention/key" | "attention/value" => {
                matrix(vec![h, h], format!("{prefix}attention_input"))
            }
            "attention/output" => matrix(vec![h, h], format!("{prefix}attention_output")),
            "mlp/fc1" => matrix(vec![intermediate, h], format!("{prefix}mlp_input")),
            "mlp/fc2" => matrix(vec![h, intermediate], format!("{prefix}mlp_activation")),
            _ => None,
        });
    }
    let Some((prefix, suffix)) = layer_parameter(name) else {
        return Ok(None);
    };
    if matches!(suffix, "input_norm" | "post_attention_norm") {
        return Ok(direct(vec![h]));
    }
    let draft = matches!(component, "dflash" | "dflash2");
    if suffix.starts_with("attention/") {
        let (q, k, d) = attention_dims(c)?;
        return Ok(match suffix {
            "attention/query" | "attention/gate" => matrix(
                vec![q, h],
                format!(
                    "{prefix}{}",
                    if draft {
                        "query_projection_input"
                    } else {
                        "mixer_input"
                    }
                ),
            ),
            "attention/key" | "attention/value" => matrix(
                vec![k, h],
                format!(
                    "{prefix}{}",
                    if draft {
                        "query_projection_input"
                    } else {
                        "mixer_input"
                    }
                ),
            ),
            "attention/context_key" | "attention/context_value" => {
                matrix(vec![k, h], format!("{component}/context_input"))
            }
            "attention/query_norm" | "attention/key_norm" => direct(vec![d]),
            "attention/output" => matrix(
                vec![h, q],
                format!(
                    "{prefix}{}",
                    if draft {
                        "attention_output"
                    } else {
                        "attention/gated_output"
                    }
                ),
            ),
            _ => None,
        });
    }
    if suffix.starts_with("gdn/") {
        let k = mul(
            dim(c, "linear_num_key_heads")?,
            dim(c, "linear_key_head_dim")?,
        )?;
        let heads = dim(c, "linear_num_value_heads")?;
        let v = mul(heads, dim(c, "linear_value_head_dim")?)?;
        return Ok(match suffix {
            "gdn/query" | "gdn/key" => matrix(vec![k, h], format!("{prefix}mixer_input")),
            "gdn/value" | "gdn/z" => matrix(vec![v, h], format!("{prefix}mixer_input")),
            "gdn/a_projection" | "gdn/b_projection" => {
                matrix(vec![heads, h], format!("{prefix}mixer_input"))
            }
            "gdn/a_log" | "gdn/dt_bias" => Some((vec![heads], Some("fp32"), vec![])),
            "gdn/norm" => direct(vec![dim(c, "linear_value_head_dim")?]),
            "gdn/convolution" => {
                direct(vec![dim(c, "linear_conv_kernel_dim")?, add(mul(2, k)?, v)?])
            }
            "gdn/output" => matrix(vec![h, v], format!("{prefix}gdn/gated_output")),
            _ => None,
        });
    }
    if suffix.starts_with("mlp/") {
        let i = dim(c, "intermediate_size")?;
        return Ok(match suffix {
            "mlp/gate" | "mlp/up" => matrix(
                vec![i, h],
                format!("{prefix}{}", if draft { "mlp_input" } else { "ffn_input" }),
            ),
            "mlp/down" => matrix(
                vec![h, i],
                format!(
                    "{prefix}{}",
                    if draft { "mlp_product" } else { "mlp/product" }
                ),
            ),
            _ => None,
        });
    }
    if suffix.starts_with("moe/") {
        let ir = dim(c, "moe_intermediate_size")?;
        let shared = dim(c, "shared_expert_intermediate_size")?;
        let input = format!("{prefix}ffn_input");
        return Ok(match suffix {
            "moe/router" => matrix(vec![dim(c, "num_experts")?, h], input),
            "moe/shared_score" => matrix(vec![1, h], input),
            s if s.starts_with("moe/experts/") || s.starts_with("moe/shared/") => {
                let width = if s.starts_with("moe/shared/") {
                    shared
                } else {
                    ir
                };
                let (parent, last) = s.rsplit_once('/').unwrap();
                if last == "down" {
                    matrix(vec![h, width], format!("{prefix}{parent}/product"))
                } else if matches!(last, "gate" | "up") {
                    matrix(vec![width, h], input)
                } else {
                    None
                }
            }
            _ => None,
        });
    }
    if component == "dflash2" {
        for name in ["attention_conv", "mlp_conv"] {
            if suffix == format!("{name}/base_kernel") {
                return Ok(direct(vec![
                    2,
                    dim(&c["dflash_config"], "conv_kernel_size")?,
                    h,
                ]));
            }
            if suffix == format!("{name}/kernel_projection") {
                return Ok(matrix(
                    vec![
                        mul(
                            mul(2, dim(&c["dflash_config"], "conv_kernel_size")?)?,
                            h / dim(&c["dflash_config"], "conv_group_size")?,
                        )?,
                        h,
                    ],
                    format!("{prefix}{name}_input"),
                ));
            }
        }
    }
    Ok(None)
}
fn layer_parameter(name: &str) -> Option<(&str, &str)> {
    let (_, layers) = name.split_once("/layers/")?;
    let (index, suffix) = layers.split_once('/')?;
    index.parse::<u32>().ok()?;
    let prefix_len = name.len() - suffix.len();
    Some((&name[..prefix_len], suffix))
}
fn validate_binding_contracts(
    components: &Map<String, Value>,
    bindings: &Map<String, Value>,
    objects: &BTreeMap<String, (bool, &Map<String, Value>)>,
    uses: &[Value],
) -> Result<(), NinferContainerError> {
    for (name, binding) in bindings {
        let Some((shape, format, inputs)) = binding_contract(name, components)? else {
            continue;
        };
        let references = if let Some(o) = binding.get("object") {
            vec![id(o, "binding object")?]
        } else {
            binding["parts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| id(&p["object"], "Part object"))
                .collect::<Result<Vec<_>, _>>()?
        };
        for reference in references {
            let (_, o) = objects[&reference];
            if binding.get("object").is_some() && o["shape"] != json!(shape) {
                return Err(error(format!(
                    "{name}: whole-object logical shape differs from native binder"
                )));
            }
            if format.is_some_and(|f| o["format"] != f) {
                return Err(error(format!(
                    "{name}: native direct value format mismatch"
                )));
            }
        }
        for input in inputs {
            let u = uses
                .iter()
                .find(|u| u["parameter"] == *name && u["input"] == input)
                .ok_or_else(|| error(format!("missing native Use {name}@{input}")))?;
            if u.get("activation_policy").is_none() {
                return Err(error(format!(
                    "missing activation policy for {name}@{input}"
                )));
            }
            if let Some(aux) = u.get("auxiliaries").and_then(Value::as_object) {
                for (role, binding) in aux {
                    if role != "activation_input_divisor"
                        || binding_elements(binding, objects)? != 1
                    {
                        return Err(error(
                            "native activation auxiliary must be a scalar input divisor",
                        ));
                    }
                    let refs = if let Some(o) = binding.get("object") {
                        vec![o]
                    } else {
                        binding["parts"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|p| &p["object"])
                            .collect()
                    };
                    for o in refs {
                        if objects[o.as_str().unwrap()].1["format"] != "fp32" {
                            return Err(error(
                                "native activation divisor must have FP32 representation",
                            ));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, Write};
    #[test]
    fn draft_bindings_inherit_target_width_and_nested_configuration() {
        let mut components = json!({
            "text":{"config":{"hidden_size":5120,"vocab_size":248320}},
            "dflash2":{"target":"text","config":{
                "num_attention_heads":32,"num_key_value_heads":8,"head_dim":128,
                "intermediate_size":17408,
                "dflash_config":{"target_layer_ids":[5,19,33,47,61],
                    "selector_rank":256,"conv_kernel_size":2,"conv_group_size":16}
            }}
        });
        for (name, shape) in [
            ("dflash2/final_norm", vec![5120]),
            ("dflash2/feature_projection", vec![5120, 25600]),
            (
                "dflash2/candidate_selector/hidden_projection",
                vec![256, 5120],
            ),
            (
                "dflash2/candidate_selector/predecessor_codebook",
                vec![248320, 256],
            ),
            ("dflash2/layers/0/attention/query", vec![4096, 5120]),
            (
                "dflash2/layers/0/attention_conv/base_kernel",
                vec![2, 2, 5120],
            ),
            (
                "dflash2/layers/0/mlp_conv/kernel_projection",
                vec![1280, 5120],
            ),
        ] {
            let contract = binding_contract(name, components.as_object().unwrap())
                .unwrap()
                .unwrap();
            assert_eq!(contract.0, shape, "{name}");
        }
        components["dflash2"]["config"]["selector_rank"] = json!(256);
        components["dflash2"]["config"]["dflash_config"]
            .as_object_mut()
            .unwrap()
            .remove("selector_rank");
        assert!(
            binding_contract(
                "dflash2/candidate_selector/hidden_projection",
                components.as_object().unwrap()
            )
            .is_err()
        );
        components["text"]["config"]["hidden_size"] = json!(0);
        assert!(binding_contract("dflash2/final_norm", components.as_object().unwrap()).is_err());
    }
    #[test]
    fn dflash_feature_binding_uses_target_hidden_width() {
        let components = json!({
            "text":{"config":{"hidden_size":2048,"vocab_size":248320}},
            "dflash":{"target":"text","config":{"dflash_config":{"target_layer_ids":[1,6,11,16,22,27,32,37]}}}
        });
        let (shape, _, uses) =
            binding_contract("dflash/feature_projection", components.as_object().unwrap())
                .unwrap()
                .unwrap();
        assert_eq!(shape, vec![2048, 16384]);
        assert_eq!(uses, vec!["dflash/target_features"]);
    }
    fn fixture() -> Value {
        serde_json::from_str(include_str!("../testdata/ninfer-v3.json")).unwrap()
    }
    fn write(path: &Path, directory: &Value) {
        let bytes = serde_json::to_vec(directory).unwrap();
        let offset = (32 + bytes.len() as u64).div_ceil(4096) * 4096;
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(b"NINFER\0\x03").unwrap();
        file.write_all(&(bytes.len() as u64).to_le_bytes()).unwrap();
        file.write_all(&[7; 16]).unwrap();
        file.write_all(&bytes).unwrap();
        file.set_len(offset + directory["files"][0]["payload_bytes"].as_u64().unwrap())
            .unwrap();
        for o in directory["objects"].as_array().unwrap() {
            if o["kind"] == "resource"
                && o["offset"].as_u64().unwrap() + 2
                    <= directory["files"][0]["payload_bytes"].as_u64().unwrap()
            {
                file.seek(SeekFrom::Start(offset + o["offset"].as_u64().unwrap()))
                    .unwrap();
                file.write_all(b"{}").unwrap();
            }
        }
    }
    #[test]
    fn complete_native_mtp_uses_target_mathematics_and_explicit_bindings() {
        let mut d = fixture();
        d["components"]["mtp"] = json!({"target":"text","config":{"architectures":["Qwen3_5MTP"]}});
        let source_bindings: Vec<_> = d["bindings"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(name, _)| name.starts_with("text/layers/0/"))
            .map(|(n, b)| (n.clone(), b.clone()))
            .collect();
        let source_uses: Vec<_> = d["uses"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|u| {
                u["parameter"]
                    .as_str()
                    .unwrap()
                    .starts_with("text/layers/0/")
            })
            .cloned()
            .collect();
        for (name, binding) in source_bindings {
            let native_name = name.replacen("text/", "mtp/", 1);
            let source_id = binding["object"].as_str().unwrap();
            let mut o = d["objects"]
                .as_array()
                .unwrap()
                .iter()
                .find(|o| o["id"] == source_id)
                .unwrap()
                .clone();
            let last = d["objects"].as_array().unwrap().last().unwrap();
            o["offset"] = json!(
                align(
                    last["offset"].as_u64().unwrap() + last["bytes"].as_u64().unwrap(),
                    256
                )
                .unwrap()
            );
            o["id"] = json!(native_name);
            d["objects"].as_array_mut().unwrap().push(o);
            d["bindings"][&native_name] = json!({"object":native_name});
        }
        for mut u in source_uses {
            u["parameter"] = json!(
                u["parameter"]
                    .as_str()
                    .unwrap()
                    .replacen("text/", "mtp/", 1)
            );
            u["input"] = json!(u["input"].as_str().unwrap().replacen("text/", "mtp/", 1));
            d["uses"].as_array_mut().unwrap().push(u);
        }
        for (name, shape) in [
            ("input_projection", vec![8u64, 16]),
            ("embedding_norm", vec![8]),
            ("hidden_norm", vec![8]),
            ("final_norm", vec![8]),
        ] {
            let name = format!("mtp/{name}");
            let last = d["objects"].as_array().unwrap().last().unwrap();
            let offset = align(
                last["offset"].as_u64().unwrap() + last["bytes"].as_u64().unwrap(),
                256,
            )
            .unwrap();
            d["objects"].as_array_mut().unwrap().push(json!({"kind":"tensor","id":name,"offset":offset,"bytes":shape.iter().product::<u64>()*2,"shape":shape,"format":"bf16","layout":"contiguous_le_v1"}));
            d["bindings"][&name] = json!({"object":name});
        }
        d["uses"].as_array_mut().unwrap().push(json!({"parameter":"mtp/input_projection","input":"mtp/stem_input","activation_policy":"A16Only"}));
        d["uses"].as_array_mut().unwrap().push(json!({"parameter":"text/output_head","input":"mtp/final_hidden","activation_policy":"A16Only"}));
        let last = d["objects"].as_array().unwrap().last().unwrap();
        d["files"][0]["payload_bytes"] =
            json!(last["offset"].as_u64().unwrap() + last["bytes"].as_u64().unwrap());
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("model.ninfer");
        write(&path, &d);
        assert!(inspect_ninfer_container(&path).is_ok());
        d["bindings"]
            .as_object_mut()
            .unwrap()
            .remove("mtp/hidden_norm");
        write(&path, &d);
        assert!(inspect_ninfer_container(&path).is_err());
    }

    #[test]
    fn native_v3_is_origin_neutral_and_checks_full_binding_inventory() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("arbitrary.ninfer");
        let directory = fixture();
        write(&path, &directory);
        let m = inspect_ninfer_container(&path).unwrap();
        assert_eq!(m.identity.container_version, 3);
        assert_eq!(m.identity.model_id, "Qwen3_5ForCausalLM");
        assert!(!m.dflash2);
        assert!(m.continuation_paths(&path).is_empty());
        assert_eq!(
            m.frontend_resource_sha256(&path, "frontend/tokenizer.json")
                .unwrap(),
            Some(format!("{:x}", Sha256::digest(b"{}")))
        );
        for case in [
            "shape",
            "format",
            "missing",
            "use",
            "resource",
            "architecture",
            "offset",
            "oversized",
            "component",
        ] {
            let mut d = directory.clone();
            match case {
                "shape" => d["objects"][0]["shape"] = json!([8, 16]),
                "format" => {
                    d["objects"][2]["format"] = json!("int32");
                    d["objects"][2]["bytes"] = json!(32);
                }
                "missing" => {
                    d["bindings"]
                        .as_object_mut()
                        .unwrap()
                        .remove("text/layers/0/mlp/down");
                }
                "use" => d["uses"] = json!([]),
                "resource" => {
                    d["components"]["text"]["resources"]
                        .as_object_mut()
                        .unwrap()
                        .remove("tokenizer.json");
                }
                "architecture" => {
                    d["components"]["text"]["config"]["architectures"] = json!(["Unsupported"])
                }
                "offset" => d["objects"][1]["offset"] = json!(1),
                "oversized" => d["components"]["text"]["config"]["hidden_size"] = json!(u64::MAX),
                _ => d["components"]["mtp"] = json!({"target":"text","config":{}}),
            };
            write(&path, &d);
            assert!(inspect_ninfer_container(&path).is_err(), "{case}");
        }
    }
    #[test]
    fn continuations_bind_header_identity_indices_and_exact_ranges() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("entry.ninfer");
        let part = temp.path().join("payload.part");
        let mut d = fixture();
        let total = d["files"][0]["payload_bytes"].as_u64().unwrap();
        d["files"] = json!([{"path":null,"payload_bytes":2048},{"path":"payload.part","payload_bytes":total-2048}]);
        write(&path, &d);
        let mut file = std::fs::File::create(&part).unwrap();
        file.write_all(b"NINPRT\0\x03").unwrap();
        file.write_all(&1u64.to_le_bytes()).unwrap();
        file.write_all(&[7; 16]).unwrap();
        file.set_len(4096 + total - 2048).unwrap();
        let m = inspect_ninfer_container(&path).unwrap();
        assert_eq!(m.continuation_paths(&path), [part.clone()]);
        file.seek(SeekFrom::Start(16)).unwrap();
        file.write_all(&[8; 16]).unwrap();
        assert!(inspect_ninfer_container(&path).is_err());
        std::fs::remove_file(part).unwrap();
        assert!(inspect_ninfer_container(&path).is_err());
        d["files"][1]["path"] = json!("../escape");
        write(&path, &d);
        assert!(inspect_ninfer_container(&path).is_err());
    }
    #[test]
    fn truncated_oversized_duplicate_and_conflicting_framing_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("bad.ninfer");
        write(&path, &fixture());
        let original = std::fs::read(&path).unwrap();
        for length in [8, 31, 33] {
            std::fs::write(&path, &original[..length]).unwrap();
            assert!(inspect_ninfer_container(&path).is_err());
        }
        let mut bytes = original;
        bytes[8..16].copy_from_slice(&(MAX_NINFER_DIRECTORY_BYTES + 1).to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        assert!(inspect_ninfer_container(&path).is_err());
        let raw =
            br#"{"components":{},"components":{},"files":[],"objects":[],"bindings":{},"uses":[]}"#;
        let mut bytes = b"NINFER\0\x03".to_vec();
        bytes.extend((raw.len() as u64).to_le_bytes());
        bytes.extend([7; 16]);
        bytes.extend(raw);
        bytes.resize(4096, 0);
        std::fs::write(&path, bytes).unwrap();
        assert!(inspect_ninfer_container(&path).is_err());
    }
}
