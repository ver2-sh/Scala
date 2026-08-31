use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ArtifactFormat, ArtifactNativeIdentity, AuxiliaryArtifact, AuxiliaryArtifactRole,
    inspect_ninfer_container,
};

pub(crate) const MAX_PACKAGE_JSON_BYTES: u64 = 16 * 1024 * 1024;
const MAX_HASHED_SIDECAR_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NortedPackageKind {
    Gguf,
    Q27,
    Ninfer,
}

impl std::fmt::Display for NortedPackageKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gguf => formatter.write_str("Norted GGUF"),
            Self::Q27 => formatter.write_str("Norted q27"),
            Self::Ninfer => formatter.write_str("Norted NInfer"),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct NortedPackageFile {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NortedPackageBinding {
    pub kind: NortedPackageKind,
    pub manifest_schema: String,
    pub manifest_version: u32,
    pub package_root: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest_sha256: String,
    pub output_key: String,
    pub expected_primary_size: u64,
    pub expected_primary_sha256: String,
    pub build_key: Option<String>,
    pub master_id: Option<String>,
    pub quant_recipe_key: Option<String>,
    pub canonical_source_lineage_key: Option<String>,
    pub sharp: Option<NortedPackageFile>,
    pub sharp_revision: Option<String>,
    pub sharp_version: Option<String>,
    pub tokenizer: Option<NortedPackageFile>,
    pub projector: Option<NortedPackageFile>,
}

#[derive(Debug, Clone)]
pub(crate) struct PackageMember {
    pub binding: NortedPackageBinding,
    pub auxiliary: Vec<AuxiliaryArtifact>,
    pub native_identity: Option<ArtifactNativeIdentity>,
}

#[derive(Debug, Clone)]
pub(crate) enum PackageDirectory {
    Valid {
        members: HashMap<PathBuf, PackageMember>,
        suppressed: HashSet<PathBuf>,
    },
    Invalid {
        reason: String,
        /// `None` means the manifest could not be read well enough to recover
        /// its claims, so every same-format artifact remains quarantined.
        claimed: Option<HashSet<PathBuf>>,
    },
}

#[derive(Debug, Deserialize)]
struct FileRecord {
    filename: String,
    size: u64,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct Q27Manifest {
    source_lineage: SourceLineage,
    sharp: Q27Sharp,
    tokenizer: FileRecord,
    outputs: BTreeMap<String, Q27Output>,
}

#[derive(Debug, Deserialize)]
struct SourceLineage {
    key: String,
}

#[derive(Debug, Deserialize)]
struct Q27Sharp {
    filename: String,
    template_sha256: String,
    resolved_commit: String,
    version: String,
}

#[derive(Debug, Deserialize)]
struct Q27Output {
    filename: String,
    size: u64,
    sha256: String,
    source_lineage: SourceLineage,
    tokenizer_sha256: String,
}

#[derive(Debug, Deserialize)]
struct NinferManifest {
    schema: String,
    canonical_source_lineage_key: String,
    outputs: BTreeMap<String, NinferOutput>,
    sharp: NinferSharp,
}

#[derive(Debug, Deserialize)]
struct NinferOutput {
    artifact: NinferArtifact,
    source_lineage: SourceLineage,
}

#[derive(Debug, Deserialize)]
struct NinferArtifact {
    filename: String,
    model_id: String,
    weights_id: String,
    container_version: u32,
    size: u64,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct NinferSharp {
    filename: String,
    revision: String,
    version: String,
    size: u64,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct BuildManifest {
    build_key: String,
    #[serde(default)]
    master: Option<BuildMaster>,
    #[serde(default)]
    route: Option<BuildRoute>,
    #[serde(default)]
    lineage: Option<BuildLineage>,
    outputs: BTreeMap<String, BuildOutput>,
}

#[derive(Debug, Deserialize)]
struct BuildMaster {
    master_id: String,
}

#[derive(Debug, Deserialize)]
struct BuildRoute {
    #[serde(default)]
    quants: BTreeMap<String, BuildQuantRoute>,
}

#[derive(Debug, Deserialize)]
struct BuildQuantRoute {
    #[serde(default)]
    cache_source_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BuildLineage {
    #[serde(default)]
    quants: BTreeMap<String, BuildQuantLineage>,
}

#[derive(Debug, Deserialize)]
struct BuildQuantLineage {
    raw_quant_key: String,
    #[serde(default)]
    unsloth_quant_recipe_key: Option<String>,
    #[serde(default)]
    provider_recipe_key: Option<String>,
    #[serde(default)]
    effective_quant_source: Option<SourceLineage>,
}

#[derive(Debug, Deserialize)]
struct BuildOutput {
    filename: String,
    size: u64,
    sha256: String,
    #[serde(default)]
    quant: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    projector_key: Option<String>,
}

pub(crate) fn discover_package_directory(
    root: &Path,
    format: ArtifactFormat,
) -> Option<PackageDirectory> {
    let manifest_name = match format {
        ArtifactFormat::Gguf => "BUILD-MANIFEST.json",
        ArtifactFormat::Q27 => "Q27-MANIFEST.json",
        ArtifactFormat::Ninfer => "NINFER-MANIFEST.json",
    };
    let manifest = root.join(manifest_name);
    if !manifest.exists() {
        return None;
    }
    let result = match format {
        ArtifactFormat::Gguf => discover_gguf(root, &manifest),
        ArtifactFormat::Q27 => discover_q27(root, &manifest),
        ArtifactFormat::Ninfer => discover_ninfer(root, &manifest),
    };
    Some(result.unwrap_or_else(|reason| PackageDirectory::Invalid {
        reason,
        claimed: recover_manifest_claims(root, &manifest, format),
    }))
}

fn discover_q27(root: &Path, manifest_path: &Path) -> Result<PackageDirectory, String> {
    let root = canonical_root(root)?;
    let manifest_path = canonical_manifest(&root, manifest_path)?;
    let (value, manifest_sha): (serde_json::Value, _) = read_json(&manifest_path)?;
    let version = value
        .get("schema")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "Q27-MANIFEST schema must be an integer".to_owned())?;
    if version != 6 {
        return Err(format!(
            "unsupported Norted q27 package schema {version}; rebuild this artifact with the current Norted Builder"
        ));
    }
    validate_lineage_value(&value["source_lineage"], "q27 package source lineage")?;
    let manifest: Q27Manifest = serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid Q27-MANIFEST v6: {error}"))?;
    validate_sha(&manifest.source_lineage.key, "q27 source lineage key")?;
    let manifest_file = package_file_from_observed(&manifest_path, &manifest_sha)?;
    let tokenizer = resolve_file(
        &root,
        &manifest.tokenizer.filename,
        Some(manifest.tokenizer.size),
        &manifest.tokenizer.sha256,
        true,
    )?;
    super::model::validate_q27_tokenizer_header(&tokenizer.path).map_err(|reason| {
        format!(
            "manifest-bound q27 tokenizer {} is invalid: {reason}",
            tokenizer.path.display()
        )
    })?;
    let sharp = resolve_file(
        &root,
        &manifest.sharp.filename,
        None,
        &manifest.sharp.template_sha256,
        true,
    )?;
    require_distinct_files(&[("tokenizer", &tokenizer.path), ("Sharp", &sharp.path)])?;
    let mut members = HashMap::new();
    let mut bound = HashSet::new();
    for (target, output) in &manifest.outputs {
        if !matches!(target.as_str(), "q6" | "q6k") {
            return Err(format!(
                "q27 manifest contains unsupported output target `{target}`"
            ));
        }
        if output.source_lineage.key != manifest.source_lineage.key
            || output.tokenizer_sha256 != tokenizer.sha256
        {
            return Err(format!(
                "q27 output `{target}` is not bound to package lineage/tokenizer"
            ));
        }
        validate_lineage_value(
            &value["outputs"][target]["source_lineage"],
            &format!("q27 output `{target}` source lineage"),
        )?;
        let primary = resolve_file(
            &root,
            &output.filename,
            Some(output.size),
            &output.sha256,
            false,
        )?;
        if [&tokenizer.path, &sharp.path].contains(&&primary.path)
            || !bound.insert(primary.path.clone())
        {
            return Err(format!("q27 output `{target}` is ambiguously bound"));
        }
        let binding = NortedPackageBinding {
            kind: NortedPackageKind::Q27,
            manifest_schema: "norted.q27-manifest".to_owned(),
            manifest_version: 6,
            package_root: root.clone(),
            manifest_path: manifest_path.clone(),
            manifest_sha256: manifest_sha.clone(),
            output_key: target.clone(),
            expected_primary_size: output.size,
            expected_primary_sha256: output.sha256.clone(),
            build_key: None,
            master_id: None,
            quant_recipe_key: None,
            canonical_source_lineage_key: Some(manifest.source_lineage.key.clone()),
            sharp: Some(sharp.clone()),
            sharp_revision: Some(manifest.sharp.resolved_commit.clone()),
            sharp_version: Some(manifest.sharp.version.clone()),
            tokenizer: Some(tokenizer.clone()),
            projector: None,
        };
        let auxiliary = vec![
            as_auxiliary(&tokenizer, AuxiliaryArtifactRole::Tokenizer),
            as_auxiliary(&sharp, AuxiliaryArtifactRole::Sharp),
            as_auxiliary(&manifest_file, AuxiliaryArtifactRole::Manifest),
        ];
        members.insert(
            primary.path,
            PackageMember {
                binding,
                auxiliary,
                native_identity: None,
            },
        );
    }
    if members.is_empty() {
        return Err("q27 manifest declares no outputs".to_owned());
    }
    Ok(PackageDirectory::Valid {
        members,
        suppressed: HashSet::new(),
    })
}

fn discover_ninfer(root: &Path, manifest_path: &Path) -> Result<PackageDirectory, String> {
    let root = canonical_root(root)?;
    let manifest_path = canonical_manifest(&root, manifest_path)?;
    let (value, manifest_sha): (serde_json::Value, _) = read_json(&manifest_path)?;
    let version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "NINFER-MANIFEST schema_version must be an integer".to_owned())?;
    if value.get("schema").and_then(serde_json::Value::as_str) != Some("norted.ninfer-manifest") {
        return Err("unsupported Norted NInfer manifest identity; rebuild this artifact with the current Norted Builder".to_owned());
    }
    if version != 6 {
        return Err(format!(
            "unsupported Norted NInfer package schema v{version}; rebuild this artifact with the current Norted Builder"
        ));
    }
    let manifest: NinferManifest = serde_json::from_value(value)
        .map_err(|error| format!("invalid NINFER-MANIFEST v6: {error}"))?;
    validate_sha(
        &manifest.canonical_source_lineage_key,
        "NInfer canonical source lineage key",
    )?;
    let manifest_file = package_file_from_observed(&manifest_path, &manifest_sha)?;
    let sharp = resolve_file(
        &root,
        &manifest.sharp.filename,
        Some(manifest.sharp.size),
        &manifest.sharp.sha256,
        true,
    )?;
    require_distinct_files(&[("Sharp", &sharp.path)])?;
    let mut members = HashMap::new();
    let mut bound = HashSet::new();
    for (key, output) in &manifest.outputs {
        if output.source_lineage.key != manifest.canonical_source_lineage_key
            || output.artifact.weights_id != *key
        {
            return Err(format!(
                "NInfer output `{key}` is not bound to package lineage/identity"
            ));
        }
        if output.artifact.container_version != 2 {
            return Err(format!("NInfer output `{key}` is not container v2"));
        }
        let primary = resolve_file(
            &root,
            &output.artifact.filename,
            Some(output.artifact.size),
            &output.artifact.sha256,
            false,
        )?;
        if primary.path == sharp.path || !bound.insert(primary.path.clone()) {
            return Err(format!("NInfer output `{key}` is ambiguously bound"));
        }
        let native = inspect_ninfer_container(&primary.path).map_err(|reason| {
            format!(
                "manifest-bound NInfer artifact {} is invalid: {reason}",
                primary.path.display()
            )
        })?;
        if native.identity.model_id != output.artifact.model_id
            || native.identity.weights_id != output.artifact.weights_id
        {
            return Err(format!(
                "NInfer output `{key}` native identity disagrees with manifest"
            ));
        }
        let binding = NortedPackageBinding {
            kind: NortedPackageKind::Ninfer,
            manifest_schema: manifest.schema.clone(),
            manifest_version: 6,
            package_root: root.clone(),
            manifest_path: manifest_path.clone(),
            manifest_sha256: manifest_sha.clone(),
            output_key: key.clone(),
            expected_primary_size: output.artifact.size,
            expected_primary_sha256: output.artifact.sha256.clone(),
            build_key: None,
            master_id: None,
            quant_recipe_key: None,
            canonical_source_lineage_key: Some(manifest.canonical_source_lineage_key.clone()),
            sharp: Some(sharp.clone()),
            sharp_revision: Some(manifest.sharp.revision.clone()),
            sharp_version: Some(manifest.sharp.version.clone()),
            tokenizer: None,
            projector: None,
        };
        let auxiliary = vec![
            as_auxiliary(&sharp, AuxiliaryArtifactRole::Sharp),
            as_auxiliary(&manifest_file, AuxiliaryArtifactRole::Manifest),
        ];
        members.insert(
            primary.path,
            PackageMember {
                binding,
                auxiliary,
                native_identity: Some(ArtifactNativeIdentity::Ninfer(native.identity)),
            },
        );
    }
    if members.is_empty() {
        return Err("NInfer manifest declares no outputs".to_owned());
    }
    Ok(PackageDirectory::Valid {
        members,
        suppressed: HashSet::new(),
    })
}

fn discover_gguf(root: &Path, manifest_path: &Path) -> Result<PackageDirectory, String> {
    let root = canonical_root(root)?;
    let manifest_path = canonical_manifest(&root, manifest_path)?;
    let (manifest_value, manifest_sha): (serde_json::Value, _) = read_json(&manifest_path)?;
    let manifest_file = package_file_from_observed(&manifest_path, &manifest_sha)?;
    let observed_schema = manifest_value
        .get("schema")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "BUILD-MANIFEST schema must be an integer".to_owned())?;
    if !matches!(observed_schema, 2 | 3) {
        return Err(format!(
            "unsupported BUILD-MANIFEST schema {observed_schema}; expected 2 or 3"
        ));
    }
    let manifest: BuildManifest = serde_json::from_value(manifest_value.clone())
        .map_err(|error| format!("invalid BUILD-MANIFEST schema {observed_schema}: {error}"))?;
    validate_sha(&manifest.build_key, "GGUF build key")?;
    if let Some(master) = &manifest.master {
        validate_sha(&master.master_id, "GGUF master ID")?;
    }
    let mut records = Vec::new();
    let mut projector = None;
    let mut bound = HashSet::new();
    for (key, output) in &manifest.outputs {
        let file = resolve_file(
            &root,
            &output.filename,
            Some(output.size),
            &output.sha256,
            false,
        )?;
        if !bound.insert(file.path.clone()) {
            return Err(format!(
                "BUILD-MANIFEST ambiguously binds {}",
                output.filename
            ));
        }
        if output.format.as_deref() == Some("high-precision-projector")
            || output.projector_key.is_some()
        {
            if projector.replace(file).is_some() {
                return Err("BUILD-MANIFEST declares multiple projectors".to_owned());
            }
        } else if output.filename.to_ascii_lowercase().ends_with(".gguf") {
            records.push((key.clone(), output, file));
        }
    }
    if records.is_empty() {
        return Err("BUILD-MANIFEST declares no primary GGUF outputs".to_owned());
    }
    let mut members = HashMap::new();
    for (key, output, primary) in records {
        let quant_lineage = output
            .quant
            .as_ref()
            .and_then(|quant| manifest.lineage.as_ref()?.quants.get(quant));
        if let Some(lineage) = quant_lineage {
            validate_sha(&lineage.raw_quant_key, "GGUF raw quant key")?;
            if let Some(recipe) = lineage
                .unsloth_quant_recipe_key
                .as_ref()
                .or(lineage.provider_recipe_key.as_ref())
            {
                validate_sha(recipe, "GGUF quant recipe key")?;
            }
        }
        let lineage_key = if observed_schema == 3 {
            let quant = output
                .quant
                .as_ref()
                .ok_or_else(|| format!("schema-3 GGUF output `{key}` has no quant identity"))?;
            let lineage = quant_lineage.ok_or_else(|| {
                format!("schema-3 GGUF output `{key}` has no durable quant lineage")
            })?;
            let effective = lineage.effective_quant_source.as_ref().ok_or_else(|| {
                format!("schema-3 GGUF output `{key}` has no effective quant source")
            })?;
            validate_effective_quant_source(
                &manifest_value["lineage"]["quants"][quant]["effective_quant_source"],
                &format!("schema-3 GGUF output `{key}` effective quant source"),
            )?;
            Some(effective.key.clone())
        } else {
            None
        };
        if let Some(route_key) = output
            .quant
            .as_ref()
            .and_then(|quant| manifest.route.as_ref()?.quants.get(quant))
            .and_then(|route| route.cache_source_key.as_ref())
        {
            validate_sha(route_key, "GGUF route cache source key")?;
            if lineage_key.as_ref().is_some_and(|key| key != route_key) {
                return Err(format!(
                    "schema-3 GGUF output `{key}` route cache identity disagrees with durable lineage"
                ));
            }
        }
        let binding = NortedPackageBinding {
            kind: NortedPackageKind::Gguf,
            manifest_schema: "norted.build-manifest".to_owned(),
            manifest_version: observed_schema as u32,
            package_root: root.clone(),
            manifest_path: manifest_path.clone(),
            manifest_sha256: manifest_sha.clone(),
            output_key: output.quant.clone().unwrap_or(key),
            expected_primary_size: output.size,
            expected_primary_sha256: output.sha256.clone(),
            build_key: Some(manifest.build_key.clone()),
            master_id: manifest
                .master
                .as_ref()
                .map(|master| master.master_id.clone()),
            quant_recipe_key: quant_lineage.and_then(|lineage| {
                lineage
                    .unsloth_quant_recipe_key
                    .clone()
                    .or_else(|| lineage.provider_recipe_key.clone())
            }),
            canonical_source_lineage_key: lineage_key,
            sharp: None,
            sharp_revision: None,
            sharp_version: None,
            tokenizer: None,
            projector: projector.clone(),
        };
        let auxiliary = projector
            .as_ref()
            .map(|file| vec![as_auxiliary(file, AuxiliaryArtifactRole::Projector)])
            .unwrap_or_default();
        let mut auxiliary = auxiliary;
        auxiliary.push(as_auxiliary(
            &manifest_file,
            AuxiliaryArtifactRole::Manifest,
        ));
        members.insert(
            primary.path,
            PackageMember {
                binding,
                auxiliary,
                native_identity: None,
            },
        );
    }
    let suppressed = projector.into_iter().map(|file| file.path).collect();
    Ok(PackageDirectory::Valid {
        members,
        suppressed,
    })
}

fn canonical_root(root: &Path) -> Result<PathBuf, String> {
    let root = root.canonicalize().map_err(io_string)?;
    if !root.is_dir() {
        return Err(format!(
            "package root {} is not a directory",
            root.display()
        ));
    }
    Ok(root)
}

fn recover_manifest_claims(
    root: &Path,
    manifest_path: &Path,
    format: ArtifactFormat,
) -> Option<HashSet<PathBuf>> {
    let root = root.canonicalize().ok()?;
    let manifest = manifest_path.canonicalize().ok()?;
    if !manifest.starts_with(&root) {
        return None;
    }
    let (value, _): (serde_json::Value, String) = read_json(&manifest).ok()?;
    let outputs = value.get("outputs")?.as_object()?;
    let mut claims = HashSet::new();
    for output in outputs.values() {
        let filename = match format {
            ArtifactFormat::Ninfer => output.get("artifact")?.get("filename")?.as_str(),
            ArtifactFormat::Gguf | ArtifactFormat::Q27 => output.get("filename")?.as_str(),
        }?;
        let relative = Path::new(filename);
        if filename.is_empty()
            || relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return None;
        }
        let candidate = root.join(relative);
        match candidate.canonicalize() {
            Ok(path) if path.starts_with(&root) => {
                claims.insert(path);
            }
            Ok(_) => return None,
            Err(_) => {
                // A missing claimed file still makes the package invalid, but
                // cannot accidentally identify an unrelated existing model.
            }
        }
    }
    (!claims.is_empty()).then_some(claims)
}

fn canonical_manifest(root: &Path, manifest: &Path) -> Result<PathBuf, String> {
    let manifest = manifest.canonicalize().map_err(io_string)?;
    if !manifest.starts_with(root) {
        return Err("Norted package manifest escapes its package root".to_owned());
    }
    if !manifest.metadata().map_err(io_string)?.is_file() {
        return Err("Norted package manifest is not a regular file".to_owned());
    }
    Ok(manifest)
}

fn require_distinct_files(files: &[(&str, &PathBuf)]) -> Result<(), String> {
    let mut seen = HashMap::new();
    for (role, path) in files {
        if let Some(previous) = seen.insert((*path).clone(), *role) {
            return Err(format!(
                "package ambiguously binds {} as both {previous} and {role}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn resolve_file(
    root: &Path,
    relative: &str,
    expected_size: Option<u64>,
    sha256: &str,
    verify_hash: bool,
) -> Result<NortedPackageFile, String> {
    validate_sha(sha256, "package file SHA256")?;
    let relative_path = Path::new(relative);
    if relative.is_empty()
        || relative_path.is_absolute()
        || relative_path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(format!("unsafe package-relative path `{relative}`"));
    }
    let path = root
        .join(relative_path)
        .canonicalize()
        .map_err(|error| format!("package file `{relative}` could not be resolved: {error}"))?;
    if !path.starts_with(root) {
        return Err(format!("package file `{relative}` escapes package root"));
    }
    let metadata = path.metadata().map_err(io_string)?;
    if !metadata.is_file() {
        return Err(format!("package file `{relative}` is not a regular file"));
    }
    if expected_size.is_some_and(|size| size != metadata.len()) {
        return Err(format!(
            "package file `{relative}` size mismatch: expected {}, observed {}",
            expected_size.unwrap(),
            metadata.len()
        ));
    }
    if verify_hash {
        if metadata.len() > MAX_HASHED_SIDECAR_BYTES {
            return Err(format!(
                "package sidecar `{relative}` exceeds the {MAX_HASHED_SIDECAR_BYTES}-byte validation limit"
            ));
        }
        let observed = sha256_file(&path)?;
        if observed != sha256 {
            return Err(format!("package file `{relative}` SHA256 mismatch"));
        }
    }
    Ok(NortedPackageFile {
        path,
        size_bytes: metadata.len(),
        sha256: sha256.to_owned(),
    })
}

fn as_auxiliary(file: &NortedPackageFile, role: AuxiliaryArtifactRole) -> AuxiliaryArtifact {
    AuxiliaryArtifact {
        role,
        path: file.path.clone(),
        size_bytes: file.size_bytes,
        hash: Some(file.sha256.clone()),
    }
}

fn package_file_from_observed(path: &Path, sha256: &str) -> Result<NortedPackageFile, String> {
    Ok(NortedPackageFile {
        path: path.to_path_buf(),
        size_bytes: path.metadata().map_err(io_string)?.len(),
        sha256: sha256.to_owned(),
    })
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<(T, String), String> {
    let metadata = path.metadata().map_err(io_string)?;
    if !metadata.is_file() {
        return Err(format!(
            "{} is not a regular manifest/sidecar",
            path.display()
        ));
    }
    if metadata.len() == 0 || metadata.len() > MAX_PACKAGE_JSON_BYTES {
        return Err(format!(
            "{} must be between 1 and {} bytes",
            path.display(),
            MAX_PACKAGE_JSON_BYTES
        ));
    }
    let mut file = File::open(path).map_err(io_string)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    std::io::Read::by_ref(&mut file)
        .take(MAX_PACKAGE_JSON_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io_string)?;
    if bytes.len() as u64 != metadata.len() {
        return Err(format!("{} changed while being read", path.display()));
    }
    let sha = hex_digest(Sha256::digest(&bytes));
    serde_json::from_slice(&bytes)
        .map(|value| (value, sha))
        .map_err(|error| format!("invalid bounded JSON in {}: {error}", path.display()))
}

pub(crate) fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(io_string)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(io_string)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex_digest(digest.finalize()))
}

fn validate_sha(value: &str, label: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!("{label} is not a lowercase SHA256"));
    }
    Ok(())
}

fn validate_lineage_value(value: &serde_json::Value, label: &str) -> Result<(), String> {
    let expected = value
        .get("key")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("{label} lacks a key"))?;
    validate_sha(expected, label)?;
    if canonical_hash_without_key(value, "key")? != expected {
        return Err(format!("{label} key does not match its canonical content"));
    }
    Ok(())
}

fn validate_effective_quant_source(value: &serde_json::Value, label: &str) -> Result<(), String> {
    let expected = value
        .get("key")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("{label} lacks a key"))?;
    validate_sha(expected, label)?;
    let key_material = value
        .get("key_material")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| format!("{label} lacks Builder key_material"))?;
    let observed = serde_json::to_vec(key_material)
        .map(|bytes| hex_digest(Sha256::digest(bytes)))
        .map_err(|error| format!("could not canonicalize {label}: {error}"))?;
    if observed != expected {
        return Err(format!("{label} key does not match Builder key_material"));
    }
    let repository = value.get("repository").and_then(serde_json::Value::as_str);
    let route = value.get("route").and_then(serde_json::Value::as_str);
    let artifact_key = value
        .get("artifact_identity")
        .and_then(|identity| identity.get("key"))
        .and_then(serde_json::Value::as_str);
    if key_material
        .get("repository")
        .and_then(serde_json::Value::as_str)
        != repository
        || key_material
            .get("route")
            .and_then(serde_json::Value::as_str)
            != route
        || key_material
            .get("artifact")
            .and_then(serde_json::Value::as_str)
            != artifact_key
    {
        return Err(format!(
            "{label} Builder key_material disagrees with its declared source identity"
        ));
    }
    if let Some(restoration) = value.get("restoration").filter(|value| !value.is_null()) {
        let restoration_key = restoration
            .get("key")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("{label} restoration lacks a key"))?;
        validate_sha(restoration_key, &format!("{label} restoration key"))?;
        if key_material
            .get("restoration")
            .and_then(serde_json::Value::as_str)
            != Some(restoration_key)
        {
            return Err(format!(
                "{label} Builder key_material disagrees with restoration identity"
            ));
        }
    } else if key_material.contains_key("restoration") {
        return Err(format!(
            "{label} Builder key_material invents a restoration identity"
        ));
    }
    Ok(())
}

fn canonical_hash_without_key(value: &serde_json::Value, key: &str) -> Result<String, String> {
    let mut material = value.clone();
    material
        .as_object_mut()
        .ok_or_else(|| "canonical identity material must be an object".to_owned())?
        .remove(key);
    serde_json::to_vec(&material)
        .map(|bytes| hex_digest(Sha256::digest(bytes)))
        .map_err(|error| format!("could not canonicalize package identity: {error}"))
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn io_string(error: std::io::Error) -> String {
    error.to_string()
}
