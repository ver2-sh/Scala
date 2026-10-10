//! Explicit, local source closures. Neither format nor provenance grants capability.
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionBundle {
    pub schema_version: u32,
    /// Native serving algorithm, independent of model name or origin.
    pub backend: String,
    pub sources: BTreeMap<String, DecisionSource>,
    /// Model-owned calibration/configuration, not Settings overrides.
    pub bindings: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionSource {
    pub path: PathBuf,
    pub repository: String,
    pub revision: String,
    /// Every file the native runtime may consume, relative to this source root.
    pub files: BTreeMap<PathBuf, DecisionFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionFile {
    pub size_bytes: u64,
    pub sha256: String,
}

impl DecisionBundle {
    /// Bounded metadata inspection only. Execution re-verifies the full closure.
    pub fn read(path: &Path) -> Result<Self, String> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 1024 * 1024 {
            return Err("Decision bundle exceeds 1 MiB".into());
        }
        let bundle: Self = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        bundle.validate()?;
        Ok(bundle)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 || self.backend.is_empty() || self.sources.is_empty() {
            return Err("Invalid Decision bundle schema/backend/sources".into());
        }
        if self.sources.len() > 16
            || self.sources.values().map(|s| s.files.len()).sum::<usize>() > 4096
        {
            return Err("Decision bundle inventory exceeds inspection bounds".into());
        }
        for source in self.sources.values() {
            if !source.path.is_absolute()
                || source.repository.is_empty()
                || !hex(&source.revision, 40)
                || source.files.is_empty()
            {
                return Err(
                    "Source requires an absolute path, repository, exact revision and inventory"
                        .into(),
                );
            }
            for (path, file) in &source.files {
                if path.as_os_str().is_empty()
                    || path
                        .components()
                        .any(|c| !matches!(c, std::path::Component::Normal(_)))
                    || !hex(&file.sha256, 64)
                {
                    return Err("Invalid Decision source file binding".into());
                }
                let full = source.path.join(path);
                let resolved = full.canonicalize().map_err(|e| e.to_string())?;
                if canonical_spelling(&resolved) != canonical_spelling(&full)
                    || !resolved.is_file()
                    || resolved.metadata().map_err(|e| e.to_string())?.len() != file.size_bytes
                {
                    return Err("Decision source file is missing, aliased or changed size".into());
                }
            }
        }
        let (roles, bindings, primary, required): (&[&str], &[&str], &str, &[&str]) =
            match self.backend.as_str() {
                "vllm-labels" => (
                    &["model"],
                    &["shim", "serve_config"],
                    "model",
                    &[
                        "config.json",
                        "model.safetensors",
                        "tokenizer.json",
                        "tokenizer_config.json",
                    ],
                ),
                "torch-readout" => (
                    &["adapter", "base"],
                    &["calibration"],
                    "adapter",
                    &[
                        "adapter_config.json",
                        "adapter_model.safetensors",
                        "decision_readout.json",
                        "decision_readout.safetensors",
                    ],
                ),
                _ => return Err("Unsupported native Decision backend".into()),
            };
        if self.sources.len() != roles.len()
            || roles.iter().any(|r| !self.sources.contains_key(*r))
            || self.bindings.len() != bindings.len()
            || bindings.iter().any(|r| !self.bindings.contains_key(*r))
        {
            return Err("Native Decision source/binding roles are incomplete".into());
        }
        let source = &self.sources[primary];
        if required
            .iter()
            .any(|f| !source.files.contains_key(Path::new(f)))
            || self
                .bindings
                .values()
                .any(|f| !source.files.contains_key(Path::new(f)))
        {
            return Err("Missing native weights, readout or bound serving configuration".into());
        }
        if self.backend == "torch-readout" {
            let base = &self.sources["base"];
            if !base.files.contains_key(Path::new("config.json"))
                || !base
                    .files
                    .keys()
                    .any(|p| p.extension().is_some_and(|e| e == "safetensors"))
            {
                return Err("Missing native base checkpoint".into());
            }
        }
        Ok(())
    }
}

fn canonical_spelling(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        let prefix: Vec<u16> = "\\\\?\\".encode_utf16().collect();
        let unc: Vec<u16> = "\\\\?\\UNC\\".encode_utf16().collect();
        let ordinary = if wide.starts_with(&unc) {
            let mut value: Vec<u16> = "\\\\".encode_utf16().collect();
            value.extend_from_slice(&wide[8..]);
            value
        } else if wide.starts_with(&prefix) {
            wide[4..].to_vec()
        } else {
            wide
        };
        PathBuf::from(std::ffi::OsString::from_wide(&ordinary))
    }
    #[cfg(not(windows))]
    path.to_owned()
}

fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
