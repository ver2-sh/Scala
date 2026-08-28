use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde_json::{Map, Value};

const HEADER_LEN: u64 = 16;
const FORMAT_VERSION: u32 = 1;
const MAX_METADATA_BYTES: u32 = 1024 * 1024;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum Q27Tier {
    Qwen36Default,
    Qwen36Q4s,
    Qwen36Q5f,
    Qwen36Q6,
    Qwen36Q6f,
    Qwen36Q6k,
    Qwen36Q8,
    Qwen38Default,
    Qwen38Q4s,
    Qwen38Q6,
    Qwen38Q6k,
}

impl Q27Tier {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Qwen36Default => "Qwen3.6 default",
            Self::Qwen36Q4s => "Qwen3.6 q4s",
            Self::Qwen36Q5f => "Qwen3.6 q5f",
            Self::Qwen36Q6 => "Qwen3.6 q6",
            Self::Qwen36Q6f => "Qwen3.6 q6f",
            Self::Qwen36Q6k => "Qwen3.6 q6k",
            Self::Qwen36Q8 => "Qwen3.6 q8",
            Self::Qwen38Default => "Qwen3.8 default",
            Self::Qwen38Q4s => "Qwen3.8 q4s",
            Self::Qwen38Q6 => "Qwen3.8 q6",
            Self::Qwen38Q6k => "Qwen3.8 q6k",
        }
    }

    pub(crate) fn minimum_vram_class_gib(self) -> u16 {
        match self {
            Self::Qwen36Default
            | Self::Qwen36Q4s
            | Self::Qwen36Q5f
            | Self::Qwen38Default
            | Self::Qwen38Q4s
            | Self::Qwen38Q6 => 24,
            Self::Qwen36Q6 | Self::Qwen36Q6f | Self::Qwen36Q6k | Self::Qwen38Q6k => 32,
            Self::Qwen36Q8 => 48,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Q27ModelFacts {
    pub(crate) tier: Option<Q27Tier>,
}

pub(crate) fn inspect_q27_model(path: &Path) -> Result<Q27ModelFacts, String> {
    let mut file = File::open(path)
        .map_err(|error| format!("could not open Q27 model {}: {error}", path.display()))?;
    let file_len = file
        .metadata()
        .map_err(|error| format!("could not inspect Q27 model {}: {error}", path.display()))?
        .len();
    let mut header = [0_u8; HEADER_LEN as usize];
    file.read_exact(&mut header)
        .map_err(|error| format!("Q27 model header is truncated: {error}"))?;
    if &header[..4] != b"Q27F" {
        return Err("Q27 model does not contain Q27F magic".to_owned());
    }
    let version = u32::from_le_bytes(header[4..8].try_into().expect("four-byte version"));
    if version != FORMAT_VERSION {
        return Err(format!(
            "Q27 model format version {version} is unsupported; expected {FORMAT_VERSION}"
        ));
    }
    let metadata_len = u32::from_le_bytes(
        header[12..16]
            .try_into()
            .expect("four-byte metadata length"),
    );
    if metadata_len > MAX_METADATA_BYTES {
        return Err(format!(
            "Q27 metadata length {metadata_len} exceeds the {MAX_METADATA_BYTES}-byte inspection limit"
        ));
    }
    let required_len = HEADER_LEN + u64::from(metadata_len);
    if file_len < required_len {
        return Err(format!(
            "Q27 metadata is truncated: header declares {metadata_len} bytes, but the file ends early"
        ));
    }
    let mut metadata_bytes = vec![0_u8; metadata_len as usize];
    file.read_exact(&mut metadata_bytes)
        .map_err(|error| format!("Q27 metadata is truncated: {error}"))?;
    let metadata = serde_json::from_slice::<Value>(&metadata_bytes)
        .map_err(|error| format!("Q27 metadata JSON is invalid: {error}"))?;
    let metadata = metadata
        .as_object()
        .ok_or_else(|| "Q27 metadata JSON must be an object".to_owned())?;
    validate_current_architecture(metadata)?;
    Ok(Q27ModelFacts {
        tier: published_tier(metadata),
    })
}

fn validate_current_architecture(metadata: &Map<String, Value>) -> Result<(), String> {
    require_string(metadata, "general.architecture", "qwen35")?;
    for (key, expected) in [
        ("qwen35.block_count", 65),
        ("qwen35.nextn_predict_layers", 1),
        ("qwen35.embedding_length", 5120),
        ("qwen35.feed_forward_length", 17408),
        ("qwen35.attention.head_count", 24),
        ("qwen35.attention.head_count_kv", 4),
        ("qwen35.attention.key_length", 256),
        ("qwen35.attention.value_length", 256),
        ("qwen35.rope.dimension_count", 64),
        ("qwen35.ssm.state_size", 128),
        ("qwen35.ssm.group_count", 16),
        ("qwen35.ssm.inner_size", 6144),
        ("qwen35.ssm.time_step_rank", 48),
        ("qwen35.ssm.conv_kernel", 4),
        ("group_q4", 64),
        ("group_q8", 128),
    ] {
        require_u64(metadata, key, expected)?;
    }
    require_f64(
        metadata,
        "qwen35.attention.layer_norm_rms_epsilon",
        0.000001,
        1e-12,
    )?;
    require_f64(metadata, "qwen35.rope.freq_base", 10_000_000.0, 0.0)?;
    require_string(metadata, "nibble_order", "even=low")?;
    Ok(())
}

fn require_u64(metadata: &Map<String, Value>, key: &str, expected: u64) -> Result<(), String> {
    let actual = metadata.get(key).and_then(Value::as_u64);
    if actual == Some(expected) {
        Ok(())
    } else {
        Err(format!(
            "Q27 architecture metadata `{key}` is {actual:?}; current q27 runtimes require {expected}"
        ))
    }
}

fn require_f64(
    metadata: &Map<String, Value>,
    key: &str,
    expected: f64,
    tolerance: f64,
) -> Result<(), String> {
    let actual = metadata.get(key).and_then(Value::as_f64);
    if actual.is_some_and(|actual| (actual - expected).abs() <= tolerance) {
        Ok(())
    } else {
        Err(format!(
            "Q27 architecture metadata `{key}` is {actual:?}; current q27 runtimes require {expected}"
        ))
    }
}

fn require_string(metadata: &Map<String, Value>, key: &str, expected: &str) -> Result<(), String> {
    let actual = metadata.get(key).and_then(Value::as_str);
    if actual == Some(expected) {
        Ok(())
    } else {
        Err(format!(
            "Q27 architecture metadata `{key}` is {actual:?}; current q27 runtimes require `{expected}`"
        ))
    }
}

fn published_tier(metadata: &Map<String, Value>) -> Option<Q27Tier> {
    let policy = metadata.get("quant_policy")?.as_str()?;
    let q4_head = metadata.get("q4_head").and_then(Value::as_bool);
    let q8_extra = metadata.get("q8_extra").and_then(Value::as_str);
    match (policy, q4_head, q8_extra) {
        ("v1.4", None, Some("(ssm_out|attn_output)\\.")) => Some(Q27Tier::Qwen36Default),
        ("q4s-v1", Some(true), None) => Some(Q27Tier::Qwen36Q4s),
        ("q5f-v1", Some(true), Some("(ffn_down)\\.")) => Some(Q27Tier::Qwen36Q5f),
        ("q6-v1", None, Some("(ssm_out|attn_output|ffn_down)\\.")) => Some(Q27Tier::Qwen36Q6),
        ("q6f-v1", Some(true), Some("(ffn_down|ffn_gate)\\.")) => Some(Q27Tier::Qwen36Q6f),
        ("q6k-v1", None, Some("(ssm_out|attn_output|ffn_down|ffn_gate)\\.")) => {
            Some(Q27Tier::Qwen36Q6k)
        }
        ("q8-v1", None, Some(".*")) => Some(Q27Tier::Qwen36Q8),
        ("v2.0", None, Some("(attn_output)\\.")) => Some(Q27Tier::Qwen38Default),
        ("q4s-v2", Some(true), Some("(attn_output)\\.")) => Some(Q27Tier::Qwen38Q4s),
        ("q6-v2", None, Some("(attn_output|ffn_down)\\.")) => Some(Q27Tier::Qwen38Q6),
        ("q6k-v2", None, Some("(attn_output|ffn_down|ffn_up)\\.")) => Some(Q27Tier::Qwen38Q6k),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Seek, Write};

    use serde_json::{Value, json};

    use super::{MAX_METADATA_BYTES, Q27Tier, inspect_q27_model};

    fn architecture_metadata() -> Value {
        json!({
            "general.architecture": "qwen35",
            "qwen35.block_count": 65,
            "qwen35.nextn_predict_layers": 1,
            "qwen35.embedding_length": 5120,
            "qwen35.feed_forward_length": 17408,
            "qwen35.attention.head_count": 24,
            "qwen35.attention.head_count_kv": 4,
            "qwen35.attention.key_length": 256,
            "qwen35.attention.value_length": 256,
            "qwen35.rope.dimension_count": 64,
            "qwen35.ssm.state_size": 128,
            "qwen35.ssm.group_count": 16,
            "qwen35.ssm.inner_size": 6144,
            "qwen35.ssm.time_step_rank": 48,
            "qwen35.ssm.conv_kernel": 4,
            "qwen35.attention.layer_norm_rms_epsilon": 0.000001,
            "qwen35.rope.freq_base": 10000000.0,
            "group_q4": 64,
            "group_q8": 128,
            "nibble_order": "even=low"
        })
    }

    fn write_model(metadata: &Value) -> tempfile::NamedTempFile {
        let encoded = serde_json::to_vec(metadata).expect("metadata JSON");
        let mut file = tempfile::NamedTempFile::new().expect("fixture");
        file.write_all(b"Q27F").expect("magic");
        file.write_all(&1_u32.to_le_bytes()).expect("version");
        file.write_all(&0_u32.to_le_bytes()).expect("tensor count");
        file.write_all(&(encoded.len() as u32).to_le_bytes())
            .expect("metadata length");
        file.write_all(&encoded).expect("metadata");
        file
    }

    #[test]
    fn bounded_reader_rejects_bad_truncated_and_oversized_metadata() {
        let mut bad_magic = write_model(&architecture_metadata());
        bad_magic.as_file_mut().rewind().expect("rewind");
        bad_magic.as_file_mut().write_all(b"GGUF").expect("magic");
        assert!(
            inspect_q27_model(bad_magic.path())
                .unwrap_err()
                .contains("Q27F")
        );

        let mut truncated = tempfile::NamedTempFile::new().expect("fixture");
        truncated.write_all(b"Q27F\x01\0\0\0").expect("header");
        assert!(
            inspect_q27_model(truncated.path())
                .unwrap_err()
                .contains("truncated")
        );

        let mut oversized = tempfile::NamedTempFile::new().expect("fixture");
        oversized.write_all(b"Q27F").expect("magic");
        oversized.write_all(&1_u32.to_le_bytes()).expect("version");
        oversized.write_all(&0_u32.to_le_bytes()).expect("tensors");
        oversized
            .write_all(&(MAX_METADATA_BYTES + 1).to_le_bytes())
            .expect("length");
        assert!(
            inspect_q27_model(oversized.path())
                .unwrap_err()
                .contains("inspection limit")
        );
    }

    #[test]
    fn architecture_and_published_tier_come_only_from_metadata() {
        let mut metadata = architecture_metadata();
        metadata.as_object_mut().expect("object").extend([
            ("quant_policy".to_owned(), json!("q8-v1")),
            ("q8_extra".to_owned(), json!(".*")),
        ]);
        let file = write_model(&metadata);
        assert_eq!(
            inspect_q27_model(file.path()).expect("model").tier,
            Some(Q27Tier::Qwen36Q8)
        );

        metadata
            .as_object_mut()
            .expect("object")
            .insert("q8_extra".to_owned(), json!("different"));
        let unknown = write_model(&metadata);
        assert_eq!(inspect_q27_model(unknown.path()).expect("model").tier, None);

        metadata
            .as_object_mut()
            .expect("object")
            .insert("qwen35.embedding_length".to_owned(), json!(4096));
        let incompatible = write_model(&metadata);
        assert!(
            inspect_q27_model(incompatible.path())
                .unwrap_err()
                .contains("embedding_length")
        );
    }
}
