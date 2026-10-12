//! Frozen llama-server extension. Proofs belong to one loaded process/tuple.
use super::*;
use scala_engine::{EmbeddingEntry, EmbeddingRequest, media::MediaKind};

#[cfg(test)]
mod tests;

const RESPONSE_LIMIT: usize = 16 * 1024 * 1024;

fn reviewed(runtime: &InstalledRuntime) -> bool {
    runtime.manifest.identity.engine_id == ENGINE_ID
        && matches!(
            runtime.manifest.identity.upstream_revision.as_deref(),
            Some(
                "d81235049384534c167caea52b85a694f6103d14"
                    | "f9a95aeb2096d0c0218e0cd6e6d901cd37be798a"
            )
        )
}

#[derive(Clone)]
pub(super) struct LaunchProof {
    key: String,
    runtime_id: RuntimeId,
    executable_sha256: String,
    model_id: scala_core::ModelId,
    model_path: PathBuf,
    profile_id: Option<scala_core::ModelProfileId>,
    revision: String,
    candidate: bool,
    ready: bool,
    projector: Option<scala_core::GgufArtifactIdentity>,
    media_tools: Vec<(PathBuf, String)>,
    vision: bool,
    audio: bool,
    video: bool,
    files: Vec<(PathBuf, Option<String>)>,
}

impl LaunchProof {
    pub(super) fn matches_pair(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        settings: &scala_core::ResolvedSettings,
    ) -> bool {
        self.ready && self.key == decision::tuple_key(runtime, model, settings)
    }
}

fn invalid(message: impl Into<String>) -> EngineError {
    EngineError::NativeRequest {
        message: message.into(),
        parameter: "input".into(),
        code: "unsupported_embedding_media",
    }
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub(super) async fn body(
    response: reqwest::Response,
) -> Result<(reqwest::StatusCode, Vec<u8>), EngineError> {
    let status = response.status();
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        // Avoid transport diagnostics containing URLs or native content echoes.
        let chunk = chunk.map_err(|_| {
            EngineError::BackendUnavailable("Embedding response transport failed.".into())
        })?;
        if bytes.len().saturating_add(chunk.len()) > RESPONSE_LIMIT {
            return Err(EngineError::Operation(
                "Embedding response exceeds 16 MiB.".into(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((status, bytes))
}

impl LlamaCppAdapter {
    pub(super) async fn prepare_embeddings(&self, spec: &LaunchSpec) {
        let Some(endpoint) = &spec.endpoint else {
            return;
        };
        self.embedding_proofs
            .write()
            .expect("embedding proof lock")
            .remove(endpoint);
        if !pooled_embedding_model(&spec.model.primary) || !reviewed(&spec.runtime) {
            return;
        }
        let mut projector = None;
        let selected: Vec<_> = spec
            .model
            .auxiliary
            .iter()
            .filter(|a| a.role == scala_core::AuxiliaryArtifactRole::Projector)
            .collect();
        let explicit = match spec.settings.value("llama.cpp.projector") {
            Some(SettingValue::Path(path)) => Some(path),
            _ => None,
        };
        let bound = match (selected.as_slice(), explicit) {
            ([artifact], None) => Some((&artifact.path, artifact.content_sha256.as_str())),
            ([], Some(path)) => match spec.settings.value("llama.cpp.projector_sha256") {
                Some(SettingValue::String(sha)) => Some((path, sha.as_str())),
                _ => None,
            },
            _ => None,
        };
        let mut paths = vec![spec.executable.clone(), spec.model.primary.path.clone()];
        let mut media_tools = Vec::new();
        // Native MTMD_VIDEO resolves ffmpeg/ffprobe on the launch PATH.
        // Observe identities without changing that default or executing tools.
        let search_path = spec.environment.get("PATH").cloned().or_else(|| {
            (spec.inherits_parent_environment
                && !spec.environment_remove.iter().any(|k| k == "PATH"))
            .then(|| std::env::var("PATH").ok())
            .flatten()
        });
        if bound.is_some()
            && let Some(search_path) = search_path
        {
            for name in ["ffmpeg", "ffprobe"] {
                let name = if cfg!(windows) {
                    format!("{name}.exe")
                } else {
                    name.into()
                };
                for directory in std::env::split_paths(&search_path) {
                    if !directory.is_absolute() {
                        break;
                    }
                    let path = directory.join(&name);
                    if !executable_file(&path) {
                        continue;
                    }
                    if let Ok(path) = tokio::fs::canonicalize(path).await
                        && let Ok(sha) = hash_file(&path).await
                    {
                        paths.push(path.clone());
                        media_tools.push((path, sha));
                    }
                    break;
                }
            }
        }
        paths.extend(spec.model.auxiliary.iter().map(|a| a.path.clone()));
        if let Some((path, sha)) = bound {
            paths.push(path.clone());
            if hash_file(path).await.ok().as_deref() == Some(sha) {
                projector = scala_core::inspect_gguf_metadata(path)
                    .ok()
                    .filter(|identity| identity.architecture == "clip");
            }
        }
        let candidate = spec.arguments.iter().any(|a| a == "--embedding")
            && hash_file(&spec.executable).await.ok().as_deref()
                == Some(spec.runtime.manifest.entrypoint_sha256.as_str())
            && spec.runtime.manifest.probe.compatible
            && matches!(&spec.model.primary.native_identity, Some(ArtifactNativeIdentity::Gguf(identity))
                if scala_core::inspect_gguf_metadata(&spec.model.primary.path).ok().as_ref() == Some(identity));
        self.embedding_proofs
            .write()
            .expect("embedding proof lock")
            .insert(
                endpoint.clone(),
                LaunchProof {
                    key: decision::tuple_key(&spec.runtime, &spec.model.primary, &spec.settings),
                    runtime_id: spec.runtime.manifest.runtime_id.clone(),
                    executable_sha256: spec.runtime.manifest.entrypoint_sha256.clone(),
                    model_id: spec.model.primary.id.clone(),
                    model_path: spec.model.primary.path.clone(),
                    profile_id: spec.settings.model_profile_id.clone(),
                    revision: spec
                        .runtime
                        .manifest
                        .identity
                        .upstream_revision
                        .clone()
                        .unwrap_or_default(),
                    candidate,
                    ready: false,
                    projector,
                    media_tools,
                    vision: false,
                    audio: false,
                    video: false,
                    files: paths
                        .into_iter()
                        .map(|p| {
                            let stamp = decision::file_fingerprint(&p);
                            (p, stamp)
                        })
                        .collect(),
                },
            );
    }

    pub(super) async fn observe_embeddings(&self, process: &ProcessDescriptor) {
        let Some(endpoint) = &process.endpoint else {
            return;
        };
        let proof = self
            .embedding_proofs
            .read()
            .expect("embedding proof lock")
            .get(endpoint)
            .cloned();
        let Some(proof) = proof.filter(|p| {
            p.candidate
                && p.runtime_id == process.runtime_id
                && p.executable_sha256 == process.runtime_executable_sha256
                && p.model_id == process.model_id
        }) else {
            return;
        };
        let observed = async {
            let profile = proof.profile_id.as_ref()?;
            let (status, bytes) = body(
                self.client
                    .get(format!("{endpoint}/props"))
                    .timeout(PROPS_TIMEOUT)
                    .send()
                    .await
                    .ok()?,
            )
            .await
            .ok()?;
            if !status.is_success() {
                return None;
            }
            let props: Value = serde_json::from_slice(&bytes).ok()?;
            let build = props.get("build_info")?.as_str()?;
            let (_, revision) = build.split_once('-')?;
            if revision.len() < 7
                || !revision.bytes().all(|b| b.is_ascii_hexdigit())
                || !proof.revision.starts_with(revision)
                || props.get("model_path")?.as_str()? != proof.model_path.to_str()?
                || props.get("model_alias")?.as_str()? != profile.as_str()
                || props.get("is_sleeping").and_then(Value::as_bool) != Some(false)
            {
                return None;
            }
            let (status, bytes) = body(
                self.client
                    .get(format!("{endpoint}/v1/models"))
                    .timeout(PROPS_TIMEOUT)
                    .send()
                    .await
                    .ok()?,
            )
            .await
            .ok()?;
            if !status.is_success() {
                return None;
            }
            let models: Value = serde_json::from_slice(&bytes).ok()?;
            let entries = models.get("data")?.as_array()?;
            if entries.len() != 1 || entries[0].get("id")?.as_str()? != profile.as_str() {
                return None;
            }
            let modalities = entries[0]
                .pointer("/architecture/input_modalities")?
                .as_array()?;
            let modality = |prop: &str, name: &str| {
                props
                    .pointer(&format!("/modalities/{prop}"))
                    .and_then(Value::as_bool)
                    == Some(true)
                    && modalities.iter().any(|m| m.as_str() == Some(name))
            };
            // Empty input exits before tokenization/task dispatch. The native
            // validation order proves --embedding and actual non-none pooling
            // without generating a vector or performing any model computation.
            let (status, bytes) = body(
                self.client
                    .post(format!("{endpoint}/v1/embeddings"))
                    .timeout(PROPS_TIMEOUT)
                    .json(&json!({"input":[]}))
                    .send()
                    .await
                    .ok()?,
            )
            .await
            .ok()?;
            let error: Value = serde_json::from_slice(&bytes).ok()?;
            if status != reqwest::StatusCode::BAD_REQUEST
                || error.pointer("/error/type")?.as_str()? != "invalid_request_error"
                || error.pointer("/error/message")?.as_str()? != "\"input\" must not be empty"
            {
                return None;
            }
            let projector_vision = proof
                .projector
                .as_ref()
                .is_some_and(|p| p.projector_vision == Some(true));
            let projector_audio = proof
                .projector
                .as_ref()
                .is_some_and(|p| p.projector_audio == Some(true));
            Some((
                projector_vision && modality("vision", "image"),
                projector_audio && modality("audio", "audio"),
                projector_vision && proof.media_tools.len() == 2 && modality("video", "video"),
            ))
        }
        .await;
        if let Some(current) = self
            .embedding_proofs
            .write()
            .expect("embedding proof lock")
            .get_mut(endpoint)
            && current.key == proof.key
        {
            current.ready = false;
            if let Some((vision, audio, video)) = observed {
                current.ready = current
                    .files
                    .iter()
                    .all(|(p, stamp)| stamp.is_some() && decision::file_fingerprint(p) == *stamp);
                current.vision = vision;
                current.audio = audio;
                current.video = video;
            }
        }
    }

    pub(super) fn validate_embedding_endpoint(
        &self,
        endpoint: &str,
        request: &EmbeddingRequest,
    ) -> Result<(), EngineError> {
        request
            .input
            .validate_inline_media()
            .map_err(|message| EngineError::NativeRequest {
                message,
                parameter: "input".into(),
                code: "invalid_value",
            })?;
        if !request.input.structured() {
            return Ok(());
        }
        let proofs = self.embedding_proofs.read().expect("embedding proof lock");
        let proof = proofs.get(endpoint).filter(|p| p.ready && p.profile_id.as_ref() == Some(&request.model_profile_id)
            && p.files.iter().all(|(path, stamp)| stamp.is_some() && decision::file_fingerprint(path) == *stamp))
            .ok_or_else(|| invalid("Native content embeddings require a reviewed immutable runtime, pooled model, and matching loaded endpoint evidence. Reload a qualified Model Profile."))?;
        for entry in request.input.entries() {
            if let EmbeddingEntry::Content(group) = entry {
                for part in &group.content {
                    if let Some((kind, source)) = part.media() {
                        let supported = match kind {
                            MediaKind::Image => proof.vision,
                            MediaKind::Audio => proof.audio,
                            MediaKind::Video => proof.video,
                        };
                        if !supported {
                            return Err(invalid(format!(
                                "{kind:?} embeddings require an integrity-bound projector with that encoder and matching loaded /props and /v1/models modality evidence. Video also requires identified ffmpeg and ffprobe on the native launch PATH. Vision alone does not establish audio or video support."
                            )));
                        }
                        if kind == MediaKind::Image
                            && scala_engine::media::validate_inline(kind, source)
                                .map(|(mime, _)| mime == "image/webp")
                                .unwrap_or(false)
                            && !proof.video
                        {
                            return Err(invalid(
                                "Native WebP decoding requires the MTMD_VIDEO build; select a qualified video runtime or use PNG/JPEG.",
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
