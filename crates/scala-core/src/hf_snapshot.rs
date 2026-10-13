//! Local Hugging Face checkpoints. Inspection is bounded metadata work;
//! execution fingerprints all original files, including tokenizer/templates.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct HuggingFaceSnapshot {
    pub root: PathBuf,
    pub architecture: String,
    pub context_length: Option<u64>,
    pub files: BTreeMap<String, PathBuf>,
}
impl HuggingFaceSnapshot {
    pub fn read(config: &Path) -> Result<Self, String> {
        if config.file_name().is_none_or(|n| n != "config.json") {
            return Err("Hugging Face artifact must identify its original config.json".into());
        }
        let root = config
            .parent()
            .ok_or("Missing snapshot directory")?
            .to_path_buf();
        let read = |p: &Path| -> Result<Value, String> {
            let mut b = Vec::new();
            std::fs::File::open(p)
                .map_err(|e| e.to_string())?
                .take(1024 * 1024 + 1)
                .read_to_end(&mut b)
                .map_err(|e| e.to_string())?;
            if b.len() > 1024 * 1024 {
                return Err("Snapshot JSON exceeds inspection bound".into());
            }
            serde_json::from_slice(&b).map_err(|e| e.to_string())
        };
        let cfg = read(config)?;
        let arch = cfg["architectures"]
            .as_array()
            .filter(|a| a.len() == 1)
            .and_then(|a| a[0].as_str())
            .ok_or("Expected one explicit model architecture")?;
        let mut files = BTreeMap::new();
        for entry in walkdir::WalkDir::new(&root)
            .follow_links(false)
            .max_depth(4)
            .into_iter()
            .filter_entry(|e| {
                !matches!(
                    e.file_name().to_str(),
                    Some(".git" | ".cache" | "__pycache__")
                )
            })
        {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_type().is_dir() {
                if entry.depth() == 4
                    && std::fs::read_dir(entry.path())
                        .map_err(|e| e.to_string())?
                        .next()
                        .is_some()
                {
                    return Err("Snapshot directory depth exceeds inspection bound".into());
                }
                continue;
            }
            if files.len() >= 4096 {
                return Err("Snapshot exceeds 4096-file inspection bound".into());
            }
            let path = entry.path();
            // HF cache snapshot symlinks are legitimate. Bind and hash their actual
            // file bytes; a retarget changes the execution fingerprint.
            let resolved = path.canonicalize().map_err(|e| e.to_string())?;
            if !resolved.is_file() {
                return Err("Snapshot contains a non-file member".into());
            }
            let name = path
                .strip_prefix(&root)
                .map_err(|e| e.to_string())?
                .to_str()
                .ok_or("Snapshot names must be UTF-8")?
                .replace('\\', "/");
            files.insert(name, resolved);
        }
        if !files.keys().any(|n| n.ends_with(".safetensors")) {
            return Err("Missing safetensors checkpoint".into());
        }
        if files.contains_key("model.safetensors.index.json") {
            let index = read(&root.join("model.safetensors.index.json"))?;
            let weights = index["weight_map"]
                .as_object()
                .ok_or("Invalid safetensors shard index")?;
            if weights.is_empty()
                || weights.values().any(|v| {
                    v.as_str()
                        .is_none_or(|n| !n.ends_with(".safetensors") || !files.contains_key(n))
                })
            {
                return Err("Missing or unsafe safetensors shard".into());
            }
        } else if !files.contains_key("model.safetensors") {
            return Err("Sharded checkpoint requires its original index".into());
        }
        Ok(Self {
            root,
            architecture: arch.into(),
            context_length: cfg["max_position_embeddings"].as_u64(),
            files,
        })
    }
    pub fn fingerprint(&self) -> Result<String, String> {
        let mut total = Sha256::new();
        for (name, path) in &self.files {
            let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
            let mut hash = Sha256::new();
            let bytes = std::io::copy(&mut file, &mut hash).map_err(|e| e.to_string())?;
            total.update(format!("{name}\0{bytes}\0{:x}\n", hash.finalize()).as_bytes());
        }
        Ok(format!("{:x}", total.finalize()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovery_keeps_hf_models_engine_neutral_and_verifies_inventory_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("config.json"),
            r#"{"architectures":["FutureVisionModel"],"auto_map":{"model":"original.Native"}}"#,
        )
        .unwrap();
        std::fs::write(root.join("model.safetensors"), b"synthetic weights").unwrap();
        std::fs::write(root.join("original.py"), b"# original model-owned source").unwrap();
        let registry = crate::ModelRegistry::discover(&[root.into()]);
        assert_eq!(registry.artifacts().len(), 1);
        assert_eq!(
            registry.artifacts()[0].format,
            crate::ArtifactFormat::HuggingFace
        );
        assert!(
            registry.artifacts()[0].size_bytes
                > std::fs::metadata(root.join("config.json")).unwrap().len()
        );
        let before = HuggingFaceSnapshot::read(&root.join("config.json"))
            .unwrap()
            .fingerprint()
            .unwrap();
        std::fs::write(root.join("model.safetensors"), b"different weights").unwrap();
        assert_ne!(
            before,
            HuggingFaceSnapshot::read(&root.join("config.json"))
                .unwrap()
                .fingerprint()
                .unwrap()
        );
    }
    #[cfg(unix)]
    #[test]
    fn hf_cache_symlinks_preserve_snapshot_identity_and_original_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("snapshot");
        let blobs = dir.path().join("blobs");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&blobs).unwrap();
        for (name, value) in [
            ("config.json", r#"{"architectures":["LlamaForCausalLM"]}"#),
            (
                "tokenizer_config.json",
                r#"{"chat_template":"{{ messages }}"}"#,
            ),
            ("tokenizer.json", "{}"),
            ("model.safetensors", "synthetic weights"),
        ] {
            let payload = blobs.join(name);
            std::fs::write(&payload, value).unwrap();
            std::os::unix::fs::symlink(payload, root.join(name)).unwrap();
        }
        let registry = crate::ModelRegistry::discover(std::slice::from_ref(&root));
        assert_eq!(registry.artifacts().len(), 1);
        assert_eq!(registry.artifacts()[0].path, root.join("config.json"));
        let snapshot = HuggingFaceSnapshot::read(&root.join("config.json")).unwrap();
        assert_eq!(snapshot.root, root);
        let helper = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("scala-engine-vllm/runtime/server.py");
        let script = "import importlib.util,sys; s=importlib.util.spec_from_file_location('runtime',sys.argv[1]); m=importlib.util.module_from_spec(s); s.loader.exec_module(m); print(m.snapshot_fingerprint(sys.argv[2])[1])";
        let output = std::process::Command::new("python3")
            .args(["-I", "-B", "-c", script])
            .arg(helper)
            .arg(root.join("config.json"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            snapshot.fingerprint().unwrap()
        );
    }
}
