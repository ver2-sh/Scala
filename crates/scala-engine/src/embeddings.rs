//! Typed native embedding inputs. Content groups never become chat prompts.
use crate::media::{MediaKind, validate_inline};
use scala_core::ModelProfileId;
use serde::{Deserialize, Serialize};

pub const MAX_EMBEDDING_INPUTS: usize = 1024;
pub const MAX_EMBEDDING_PARTS: usize = 64;
pub const MAX_EMBEDDING_MEDIA: usize = 2;

#[derive(Debug, Clone)]
pub struct EmbeddingRequest {
    pub model_profile_id: ModelProfileId,
    pub input: EmbeddingInput,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EmbeddingInput {
    Single(EmbeddingEntry),
    Batch(Vec<EmbeddingEntry>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EmbeddingEntry {
    Text(String),
    Content(EmbeddingContent),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingContent {
    pub content: Vec<EmbeddingPart>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EmbeddingPart {
    Text { text: String },
    ImageUrl { image_url: EmbeddingImage },
    InputAudio { input_audio: EmbeddingMedia },
    InputVideo { input_video: EmbeddingVideo },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingImage {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingMedia {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingVideo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl EmbeddingVideo {
    pub fn source(&self) -> Result<&str, String> {
        match (&self.data, &self.url) {
            (Some(value), None) | (None, Some(value)) if !value.is_empty() => Ok(value),
            _ => Err("Media requires exactly one nonempty `data` or `url` field.".into()),
        }
    }
    pub fn source_mut(&mut self) -> Option<&mut String> {
        match (&mut self.data, &mut self.url) {
            (Some(value), None) | (None, Some(value)) => Some(value),
            _ => None,
        }
    }
}

impl EmbeddingMedia {
    pub fn source(&self) -> Result<&str, String> {
        match (&self.data, &self.url) {
            (Some(value), None) | (None, Some(value)) if !value.is_empty() => Ok(value),
            _ => Err("Media requires exactly one nonempty `data` or `url` field.".into()),
        }
    }
    pub fn source_mut(&mut self) -> Option<&mut String> {
        match (&mut self.data, &mut self.url) {
            (Some(value), None) | (None, Some(value)) => Some(value),
            _ => None,
        }
    }
}

impl EmbeddingPart {
    pub fn media(&self) -> Option<(MediaKind, &str)> {
        match self {
            Self::Text { .. } => None,
            Self::ImageUrl { image_url } => Some((MediaKind::Image, &image_url.url)),
            Self::InputAudio { input_audio } => {
                input_audio.source().ok().map(|s| (MediaKind::Audio, s))
            }
            Self::InputVideo { input_video } => {
                input_video.source().ok().map(|s| (MediaKind::Video, s))
            }
        }
    }
    pub fn media_mut(&mut self) -> Option<(MediaKind, &mut String)> {
        match self {
            Self::Text { .. } => None,
            Self::ImageUrl { image_url } => Some((MediaKind::Image, &mut image_url.url)),
            Self::InputAudio { input_audio } => {
                input_audio.source_mut().map(|s| (MediaKind::Audio, s))
            }
            Self::InputVideo { input_video } => {
                input_video.source_mut().map(|s| (MediaKind::Video, s))
            }
        }
    }
}

impl EmbeddingInput {
    pub fn entries(&self) -> &[EmbeddingEntry] {
        match self {
            Self::Single(entry) => std::slice::from_ref(entry),
            Self::Batch(entries) => entries,
        }
    }
    pub fn entries_mut(&mut self) -> &mut [EmbeddingEntry] {
        match self {
            Self::Single(entry) => std::slice::from_mut(entry),
            Self::Batch(entries) => entries,
        }
    }
    pub fn len(&self) -> usize {
        self.entries().len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries().is_empty()
    }
    pub fn structured(&self) -> bool {
        self.entries()
            .iter()
            .any(|entry| matches!(entry, EmbeddingEntry::Content(_)))
    }
    /// Keep the existing private wire serialization of a plain string as [string].
    pub fn native_value(&self) -> serde_json::Value {
        match self {
            Self::Single(EmbeddingEntry::Text(text)) => serde_json::json!([text]),
            _ => serde_json::to_value(self).expect("typed embedding input"),
        }
    }
    /// Before acquisition, validate shape and counts without interpreting URLs.
    pub fn validate_structure(&self) -> Result<(), String> {
        if self.is_empty() || self.len() > MAX_EMBEDDING_INPUTS {
            return Err("Embedding input requires 1–1024 logical inputs.".into());
        }
        let mut media_count = 0;
        for entry in self.entries() {
            match entry {
                EmbeddingEntry::Text(text) if text.is_empty() => {
                    return Err("Embedding inputs must not be empty strings.".into());
                }
                EmbeddingEntry::Text(_) => {}
                EmbeddingEntry::Content(group) => {
                    if group.content.is_empty() || group.content.len() > MAX_EMBEDDING_PARTS {
                        return Err("Each content object requires 1–64 ordered parts.".into());
                    }
                    let mut nonempty = false;
                    for part in &group.content {
                        match part {
                            EmbeddingPart::Text { text } => nonempty |= !text.is_empty(),
                            EmbeddingPart::ImageUrl { image_url } => {
                                if image_url.url.is_empty()
                                    || image_url.detail.as_deref().is_some_and(|v| v != "auto")
                                {
                                    return Err("Images require a nonempty URL; only detail=auto is supported natively.".into());
                                }
                            }
                            EmbeddingPart::InputAudio { input_audio } => {
                                input_audio.source()?;
                                if input_audio.format.as_deref().is_some_and(|v| v != "wav") {
                                    return Err("Audio format must be omitted or wav; bounded PCM WAV is admitted.".into());
                                }
                            }
                            EmbeddingPart::InputVideo { input_video } => {
                                input_video.source()?;
                            }
                        }
                        if part.media().is_some() {
                            media_count += 1;
                            nonempty = true;
                        }
                    }
                    if !nonempty {
                        return Err("Embedding content must not be empty.".into());
                    }
                }
            }
        }
        if media_count > MAX_EMBEDDING_MEDIA {
            return Err("At most two media payloads are admitted per embedding request.".into());
        }
        Ok(())
    }
    /// The adapter admits inline bytes only, including for non-HTTP callers.
    pub fn validate_inline_media(&self) -> Result<(), String> {
        self.validate_structure()?;
        let mut size = 0usize;
        for entry in self.entries() {
            if let EmbeddingEntry::Content(group) = entry {
                for part in &group.content {
                    if let Some((kind, source)) = part.media() {
                        size = size.saturating_add(validate_inline(kind, source)?.1.len());
                    }
                }
            }
        }
        if size > 16 * 1024 * 1024 {
            return Err("Embedding media exceeds 16 MiB total.".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    #[test]
    fn embedding_shapes_grouping_and_exact_order_round_trip() {
        for input in [
            json!("  exact\ntext  "),
            json!(["one", "two"]),
            json!({"content":[{"type":"text","text":"  A\n"},{"type":"text","text":"B  "}]}),
            json!(["before",{"content":[{"type":"text","text":"middle"}]},"after"]),
        ] {
            let typed: EmbeddingInput = serde_json::from_value(input.clone()).unwrap();
            typed.validate_inline_media().unwrap();
            assert_eq!(serde_json::to_value(&typed).unwrap(), input);
            assert_eq!(
                typed.len(),
                if let Value::Array(v) = &input {
                    v.len()
                } else {
                    1
                }
            );
        }
    }
    #[test]
    fn embedding_rejects_token_arrays_unknown_fields_and_empty_groups() {
        for input in [
            json!([1, 2]),
            json!([[1, 2]]),
            json!(["ok", 9]),
            json!({"content":[{"type":"image_url","image_url":{"url":"x","unexpected":1}}]}),
            json!({"role":"user","content":[{"type":"text","text":"x"}]}),
            json!({"content":[{"type":"text","text":"x","url":"x"}]}),
            json!({"content":[{"type":"file","file":{"url":"x"}}]}),
            json!([]),
            json!(""),
            json!({"content":[]}),
            json!({"content":[{"type":"text","text":""}]}),
            json!({"content":[{"type":"input_audio","input_audio":{"data":"x","url":"y"}}]}),
            json!({"content":[{"type":"input_video","input_video":{"url":"x","format":"mp4"}}]}),
            json!({"content":[{"type":"input_video","input_video":{"url":"x","format":null}}]}),
        ] {
            assert!(
                serde_json::from_value::<EmbeddingInput>(input.clone())
                    .map_or(true, |typed| typed.validate_structure().is_err()),
                "{input}"
            );
        }
        let too_many = json!(vec!["x"; MAX_EMBEDDING_INPUTS + 1]);
        assert!(
            serde_json::from_value::<EmbeddingInput>(too_many)
                .unwrap()
                .validate_structure()
                .is_err()
        );
    }
}
