use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
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

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NortedPackageAcquisitionRole {
    Manifest,
    Primary,
    Tokenizer,
    Projector,
    Sharp,
    Other,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct NortedPackageAcquisitionFile {
    pub path: PathBuf,
    pub role: NortedPackageAcquisitionRole,
    pub output_key: Option<String>,
    pub size_bytes: Option<u64>,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct NortedPackageAcquisitionPlan {
    pub kind: NortedPackageKind,
    pub manifest_name: String,
    pub manifest_schema: String,
    pub manifest_version: u32,
    pub files: Vec<NortedPackageAcquisitionFile>,
}

impl NortedPackageAcquisitionPlan {
    pub fn primary_files(&self) -> impl Iterator<Item = &NortedPackageAcquisitionFile> {
        self.files
            .iter()
            .filter(|file| file.role == NortedPackageAcquisitionRole::Primary)
    }
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
}

#[derive(Debug, Deserialize)]
struct NinferOutput {
    artifact: NinferArtifact,
    source_lineage: SourceLineage,
    draft: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct NinferValidation {
    frontends: BTreeMap<String, NinferResource>,
}

#[derive(Debug, Deserialize)]
struct NinferResource {
    size: u64,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct NinferArtifact {
    validation: NinferValidation,
    filename: String,
    model_id: String,
    weights_id: String,
    container_version: u32,
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

pub fn norted_package_manifest_name(format: ArtifactFormat) -> &'static str {
    match format {
        ArtifactFormat::Gguf => "BUILD-MANIFEST.json",
        ArtifactFormat::Q27 => "Q27-MANIFEST.json",
        ArtifactFormat::Ninfer => "NINFER-MANIFEST.json",
    }
}

/// Produces the exact file closure required to reproduce a locally accepted
/// Norted package. Paths are package-root-relative; transport concerns remain
/// with the catalog provider.
pub fn plan_norted_package_acquisition(
    bytes: &[u8],
    format: ArtifactFormat,
) -> Result<NortedPackageAcquisitionPlan, String> {
    if bytes.is_empty() || bytes.len() as u64 > MAX_PACKAGE_JSON_BYTES {
        return Err(format!(
            "package manifest must be between 1 and {MAX_PACKAGE_JSON_BYTES} bytes"
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid bounded package manifest JSON: {error}"))?;
    match format {
        ArtifactFormat::Gguf => plan_gguf(&value),
        ArtifactFormat::Q27 => plan_q27(&value),
        ArtifactFormat::Ninfer => plan_ninfer(&value),
    }
}

/// Best-effort recovery used only to decide whether a malformed manifest
/// explicitly claims a selected artifact. It never makes an invalid package
/// installable.
pub fn recover_norted_package_primary_paths(
    bytes: &[u8],
    format: ArtifactFormat,
) -> Option<HashSet<PathBuf>> {
    if bytes.is_empty() || bytes.len() as u64 > MAX_PACKAGE_JSON_BYTES {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let outputs = value.get("outputs")?.as_object()?;
    let mut paths = HashSet::new();
    for output in outputs.values() {
        let filename = match format {
            ArtifactFormat::Ninfer => output
                .get("artifact")
                .and_then(|artifact| artifact.get("filename"))
                .and_then(serde_json::Value::as_str),
            ArtifactFormat::Gguf | ArtifactFormat::Q27 => {
                output.get("filename").and_then(serde_json::Value::as_str)
            }
        };
        let Some(filename) = filename else {
            continue;
        };
        let Ok(path) = package_relative_path(filename) else {
            continue;
        };
        let is_primary = match format {
            ArtifactFormat::Gguf => {
                output.get("format").and_then(serde_json::Value::as_str)
                    != Some("high-precision-projector")
                    && output
                        .get("projector_key")
                        .is_none_or(serde_json::Value::is_null)
                    && filename.to_ascii_lowercase().ends_with(".gguf")
            }
            ArtifactFormat::Q27 | ArtifactFormat::Ninfer => true,
        };
        if is_primary {
            paths.insert(path);
        }
    }
    (!paths.is_empty()).then_some(paths)
}

fn plan_q27(value: &serde_json::Value) -> Result<NortedPackageAcquisitionPlan, String> {
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
    let mut files = vec![acquisition_file(
        "Q27-MANIFEST.json",
        NortedPackageAcquisitionRole::Manifest,
        None,
        None,
        None,
    )?];
    files.push(acquisition_file(
        &manifest.tokenizer.filename,
        NortedPackageAcquisitionRole::Tokenizer,
        None,
        Some(manifest.tokenizer.size),
        Some(&manifest.tokenizer.sha256),
    )?);
    files.push(acquisition_file(
        &manifest.sharp.filename,
        NortedPackageAcquisitionRole::Sharp,
        None,
        None,
        Some(&manifest.sharp.template_sha256),
    )?);
    for (target, output) in &manifest.outputs {
        if !matches!(target.as_str(), "q6" | "q6k") {
            return Err(format!(
                "q27 manifest contains unsupported output target `{target}`"
            ));
        }
        if output.source_lineage.key != manifest.source_lineage.key
            || output.tokenizer_sha256 != manifest.tokenizer.sha256
        {
            return Err(format!(
                "q27 output `{target}` is not bound to package lineage/tokenizer"
            ));
        }
        validate_lineage_value(
            &value["outputs"][target]["source_lineage"],
            &format!("q27 output `{target}` source lineage"),
        )?;
        files.push(acquisition_file(
            &output.filename,
            NortedPackageAcquisitionRole::Primary,
            Some(target),
            Some(output.size),
            Some(&output.sha256),
        )?);
    }
    finish_plan(
        NortedPackageKind::Q27,
        "Q27-MANIFEST.json",
        "norted.q27-manifest",
        6,
        files,
    )
}

fn plan_ninfer(value: &serde_json::Value) -> Result<NortedPackageAcquisitionPlan, String> {
    let version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "NINFER-MANIFEST schema_version must be an integer".to_owned())?;
    if value.get("schema").and_then(serde_json::Value::as_str) != Some("norted.ninfer-manifest") {
        return Err("unsupported Norted NInfer manifest identity; rebuild this artifact with the current Norted Builder".to_owned());
    }
    if version != 7 {
        return Err(format!(
            "unsupported Norted NInfer package schema v{version}; rebuild this artifact with the current Norted Builder"
        ));
    }
    let manifest: NinferManifest = serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid NINFER-MANIFEST v7: {error}"))?;
    validate_sha(
        &manifest.canonical_source_lineage_key,
        "NInfer canonical source lineage key",
    )?;
    let declared = value["model_identity"]["weights_ids"]
        .as_array()
        .ok_or_else(|| "NInfer model identity requires selected weight IDs".to_owned())?;
    let selected: BTreeSet<_> = declared
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    if selected.len() != declared.len()
        || selected.is_empty()
        || selected != manifest.outputs.keys().map(String::as_str).collect()
        || selected
            .iter()
            .any(|id| !matches!(*id, "groupwise-int" | "nvfp4"))
    {
        return Err("NInfer selected output map differs from model identity".to_owned());
    }
    let mut files = vec![acquisition_file(
        "NINFER-MANIFEST.json",
        NortedPackageAcquisitionRole::Manifest,
        None,
        None,
        None,
    )?];
    for (key, output) in &manifest.outputs {
        if value["model_identity"]["model_id"].as_str() != Some(output.artifact.model_id.as_str()) {
            return Err("NInfer output native ID differs from package model identity".to_owned());
        }
        validate_lineage_value(
            &value["outputs"][key]["source_lineage"],
            "NInfer source lineage",
        )?;
        let resources = &output.artifact.validation.frontends;
        let expected = [
            "tokenizer.json",
            "tokenizer_config.json",
            "chat_template.jinja",
            "generation_config.json",
            "preprocessor_config.json",
            "video_preprocessor_config.json",
        ];
        if resources.len() != expected.len()
            || expected
                .iter()
                .any(|name| !resources.contains_key(&format!("frontend/{name}")))
        {
            return Err("NInfer manifest requires six embedded frontend identities".to_owned());
        }
        for resource in resources.values() {
            validate_sha(&resource.sha256, "NInfer embedded frontend")?;
            if resource.size == 0 || resource.size > 32 * 1024 * 1024 {
                return Err("invalid frontend resource size".to_owned());
            }
        }
        if let Some(draft) = &output.draft {
            if let Some(identity) = draft.get("identity") {
                validate_lineage_value(identity, "NInfer draft assembly identity")?;
                if identity.get("source_lineage") != Some(&value["outputs"][key]["source_lineage"])
                {
                    return Err("NInfer draft assembly lineage differs from target".to_owned());
                }
            } else if draft
                .get("reused_input")
                .and_then(serde_json::Value::as_bool)
                != Some(true)
                || !draft["source"].is_object()
            {
                return Err("invalid NInfer reused draft provenance".to_owned());
            }
        }
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
        files.push(acquisition_file(
            &output.artifact.filename,
            NortedPackageAcquisitionRole::Primary,
            Some(key),
            Some(output.artifact.size),
            Some(&output.artifact.sha256),
        )?);
    }
    finish_plan(
        NortedPackageKind::Ninfer,
        "NINFER-MANIFEST.json",
        &manifest.schema,
        7,
        files,
    )
}

fn plan_gguf(value: &serde_json::Value) -> Result<NortedPackageAcquisitionPlan, String> {
    if value.get("schema").and_then(serde_json::Value::as_str)
        == Some("norted.grep-student-gguf.v1")
    {
        return plan_deployment(value);
    }
    let version = value
        .get("schema")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "BUILD-MANIFEST schema must be an integer".to_owned())?;
    if !matches!(version, 2 | 3) {
        return Err(format!(
            "unsupported BUILD-MANIFEST schema {version}; expected 2 or 3"
        ));
    }
    let manifest: BuildManifest = serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid BUILD-MANIFEST schema {version}: {error}"))?;
    validate_sha(&manifest.build_key, "GGUF build key")?;
    if let Some(master) = &manifest.master {
        validate_sha(&master.master_id, "GGUF master ID")?;
    }
    let mut files = vec![acquisition_file(
        "BUILD-MANIFEST.json",
        NortedPackageAcquisitionRole::Manifest,
        None,
        None,
        None,
    )?];
    let mut projector_count = 0;
    for (key, output) in &manifest.outputs {
        validate_sha(&output.sha256, "package file SHA256")?;
        let role = if output.format.as_deref() == Some("high-precision-projector")
            || output.projector_key.is_some()
        {
            projector_count += 1;
            NortedPackageAcquisitionRole::Projector
        } else if output.filename.to_ascii_lowercase().ends_with(".gguf") {
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
            if version == 3 {
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
                    &value["lineage"]["quants"][quant]["effective_quant_source"],
                    &format!("schema-3 GGUF output `{key}` effective quant source"),
                )?;
                if let Some(route_key) = manifest
                    .route
                    .as_ref()
                    .and_then(|route| route.quants.get(quant))
                    .and_then(|route| route.cache_source_key.as_ref())
                {
                    validate_sha(route_key, "GGUF route cache source key")?;
                    if route_key != &effective.key {
                        return Err(format!(
                            "schema-3 GGUF output `{key}` route cache identity disagrees with durable lineage"
                        ));
                    }
                }
            }
            NortedPackageAcquisitionRole::Primary
        } else {
            NortedPackageAcquisitionRole::Other
        };
        files.push(acquisition_file(
            &output.filename,
            role,
            (role == NortedPackageAcquisitionRole::Primary).then_some(key.as_str()),
            Some(output.size),
            Some(&output.sha256),
        )?);
    }
    if projector_count > 1 {
        return Err("BUILD-MANIFEST declares multiple projectors".to_owned());
    }
    finish_plan(
        NortedPackageKind::Gguf,
        "BUILD-MANIFEST.json",
        "norted.build-manifest",
        version as u32,
        files,
    )
}

fn acquisition_file(
    path: &str,
    role: NortedPackageAcquisitionRole,
    output_key: Option<&str>,
    size_bytes: Option<u64>,
    sha256: Option<&str>,
) -> Result<NortedPackageAcquisitionFile, String> {
    if let Some(sha256) = sha256 {
        validate_sha(sha256, "package file SHA256")?;
    }
    Ok(NortedPackageAcquisitionFile {
        path: package_relative_path(path)?,
        role,
        output_key: output_key.map(str::to_owned),
        size_bytes,
        sha256: sha256.map(str::to_owned),
    })
}

fn finish_plan(
    kind: NortedPackageKind,
    manifest_name: &str,
    manifest_schema: &str,
    manifest_version: u32,
    files: Vec<NortedPackageAcquisitionFile>,
) -> Result<NortedPackageAcquisitionPlan, String> {
    let mut seen = HashSet::new();
    for file in &files {
        if seen.iter().any(|existing: &PathBuf| {
            existing == &file.path
                || existing.starts_with(&file.path)
                || file.path.starts_with(existing)
        }) {
            return Err(format!(
                "package contains a colliding path at `{}`",
                file.path.display()
            ));
        }
        seen.insert(file.path.clone());
    }
    if !files
        .iter()
        .any(|file| file.role == NortedPackageAcquisitionRole::Primary)
    {
        return Err(format!("{kind} manifest declares no primary outputs"));
    }
    Ok(NortedPackageAcquisitionPlan {
        kind,
        manifest_name: manifest_name.to_owned(),
        manifest_schema: manifest_schema.to_owned(),
        manifest_version,
        files,
    })
}

fn package_relative_path(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains('\\')
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("unsafe package-relative path `{value}`"));
    }
    Ok(path.to_path_buf())
}

pub(crate) fn discover_package_directory(
    root: &Path,
    format: ArtifactFormat,
) -> Option<PackageDirectory> {
    let manifest = local_norted_package_manifest(root, format);
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
    let _ = plan_q27(&value)?;
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
    let _ = plan_ninfer(&value)?;
    let version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "NINFER-MANIFEST schema_version must be an integer".to_owned())?;
    if value.get("schema").and_then(serde_json::Value::as_str) != Some("norted.ninfer-manifest") {
        return Err("unsupported Norted NInfer manifest identity; rebuild this artifact with the current Norted Builder".to_owned());
    }
    if version != 7 {
        return Err(format!(
            "unsupported Norted NInfer package schema v{version}; rebuild this artifact with the current Norted Builder"
        ));
    }
    let manifest: NinferManifest = serde_json::from_value(value)
        .map_err(|error| format!("invalid NINFER-MANIFEST v7: {error}"))?;
    validate_sha(
        &manifest.canonical_source_lineage_key,
        "NInfer canonical source lineage key",
    )?;
    let manifest_file = package_file_from_observed(&manifest_path, &manifest_sha)?;
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
        if !bound.insert(primary.path.clone()) {
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
        let resources = crate::model::ninfer_frontend_hashes(&primary.path, &native)
            .map_err(|e| e.to_string())?;
        if resources.len() != output.artifact.validation.frontends.len()
            || output
                .artifact
                .validation
                .frontends
                .iter()
                .any(|(name, r)| resources.get(name) != Some(&(r.size, r.sha256.clone())))
        {
            return Err("NInfer embedded frontend differs from manifest evidence".to_owned());
        }
        if native.dflash2 != output.draft.is_some() {
            return Err(
                "NInfer optional draft inventory differs from manifest provenance".to_owned(),
            );
        }
        let binding = NortedPackageBinding {
            kind: NortedPackageKind::Ninfer,
            manifest_schema: manifest.schema.clone(),
            manifest_version: 7,
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
            sharp: None,
            sharp_revision: None,
            sharp_version: None,
            tokenizer: None,
            projector: None,
        };
        let auxiliary = vec![as_auxiliary(
            &manifest_file,
            AuxiliaryArtifactRole::Manifest,
        )];
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
    let _ = plan_gguf(&manifest_value)?;
    if manifest_value
        .get("schema")
        .and_then(serde_json::Value::as_str)
        == Some("norted.grep-student-gguf.v1")
    {
        return discover_deployment(&root, &manifest_path, &manifest_value, &manifest_sha);
    }
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

/// Recognize only declared Norted deployment metadata as an alternate local
/// manifest. Other sibling manifest.json files do not acquire trust.
pub fn local_norted_package_manifest(root: &Path, format: ArtifactFormat) -> PathBuf {
    let conventional = root.join(norted_package_manifest_name(format));
    if conventional.exists() || format != ArtifactFormat::Gguf {
        return conventional;
    }
    let alternate = root.join("manifest.json");
    if alternate
        .metadata()
        .is_ok_and(|m| m.is_file() && m.len() <= MAX_PACKAGE_JSON_BYTES)
        && std::fs::read(&alternate)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .is_some_and(|v| {
                v.get("schema").and_then(serde_json::Value::as_str)
                    == Some("norted.grep-student-gguf.v1")
            })
    {
        alternate
    } else {
        conventional
    }
}

fn plan_deployment(value: &serde_json::Value) -> Result<NortedPackageAcquisitionPlan, String> {
    validate_deployment_seal(value, "artifact_id")?;
    let conversion = value
        .get("conversion")
        .ok_or("missing conversion lineage")?;
    validate_deployment_seal(conversion, "conversion_id")?;
    let parent = value
        .pointer("/parent/artifact_id")
        .and_then(serde_json::Value::as_str)
        .ok_or("missing parent lineage")?;
    validate_sha(parent, "deployment parent")?;
    if conversion.pointer("/recipe/parent") != value.get("parent") {
        return Err("deployment conversion parent mismatch".to_owned());
    }
    let targets = value
        .get("targets")
        .and_then(serde_json::Value::as_object)
        .ok_or("missing deployment targets")?;
    let mut files = vec![NortedPackageAcquisitionFile {
        path: "manifest.json".into(),
        role: NortedPackageAcquisitionRole::Manifest,
        output_key: None,
        size_bytes: None,
        sha256: None,
    }];
    for (key, target) in targets {
        validate_deployment_seal(target, "target_id")?;
        if target.pointer("/recipe/conversion_id") != conversion.get("conversion_id")
            || target.pointer("/recipe/high_precision") != conversion.get("output")
        {
            return Err("deployment target conversion lineage mismatch".to_owned());
        }
        let filename = target
            .get("filename")
            .and_then(serde_json::Value::as_str)
            .ok_or("missing target filename")?;
        let path = package_relative_path(filename)?;
        if ArtifactFormat::from_path(&path) != Some(ArtifactFormat::Gguf) {
            return Err("deployment target is not GGUF".to_owned());
        }
        let sha = target
            .pointer("/output/sha256")
            .and_then(serde_json::Value::as_str)
            .ok_or("missing target SHA256")?;
        validate_sha(sha, "deployment payload")?;
        let size = target
            .pointer("/output/size")
            .and_then(serde_json::Value::as_u64)
            .ok_or("missing target size")?;
        files.push(NortedPackageAcquisitionFile {
            path,
            role: NortedPackageAcquisitionRole::Primary,
            output_key: Some(key.clone()),
            size_bytes: Some(size),
            sha256: Some(sha.to_owned()),
        });
    }
    Ok(NortedPackageAcquisitionPlan {
        kind: NortedPackageKind::Gguf,
        manifest_name: "manifest.json".to_owned(),
        manifest_schema: "norted.grep-student-gguf.v1".to_owned(),
        manifest_version: 1,
        files,
    })
}

fn discover_deployment(
    root: &Path,
    manifest: &Path,
    value: &serde_json::Value,
    manifest_sha: &str,
) -> Result<PackageDirectory, String> {
    let plan = plan_deployment(value)?;
    let mut members = HashMap::new();
    for file in plan.primary_files() {
        let primary = resolve_file(
            root,
            file.path.to_str().ok_or("invalid filename")?,
            file.size_bytes,
            file.sha256.as_deref().ok_or("missing payload hash")?,
            false,
        )?;
        // Package metadata supplies integrity and lineage only.
        let binding = NortedPackageBinding {
            kind: NortedPackageKind::Gguf,
            manifest_schema: plan.manifest_schema.clone(),
            manifest_version: 1,
            package_root: root.to_path_buf(),
            manifest_path: manifest.to_path_buf(),
            manifest_sha256: manifest_sha.to_owned(),
            output_key: file.output_key.clone().ok_or("missing target key")?,
            expected_primary_size: primary.size_bytes,
            expected_primary_sha256: primary.sha256,
            build_key: value
                .get("artifact_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            master_id: None,
            quant_recipe_key: None,
            canonical_source_lineage_key: value
                .pointer("/parent/artifact_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            sharp: None,
            sharp_revision: None,
            sharp_version: None,
            tokenizer: None,
            projector: None,
        };
        members.insert(
            primary.path,
            PackageMember {
                binding,
                auxiliary: vec![as_auxiliary(
                    &package_file_from_observed(manifest, manifest_sha)?,
                    AuxiliaryArtifactRole::Manifest,
                )],
                native_identity: None,
            },
        );
    }
    Ok(PackageDirectory::Valid {
        members,
        suppressed: HashSet::new(),
    })
}

pub(crate) fn validate_deployment_seal(value: &serde_json::Value, key: &str) -> Result<(), String> {
    let mut unsigned = value.clone();
    let object = unsigned
        .as_object_mut()
        .ok_or("deployment record must be an object")?;
    let claimed = object.remove(key).ok_or_else(|| format!("missing {key}"))?;
    let digest = format!(
        "{:x}",
        Sha256::digest(deployment_canonical_json(&unsigned)?)
    );
    if claimed.as_str() != Some(digest.as_str()) {
        return Err(format!("deployment {key} seal mismatch"));
    }
    Ok(())
}

// Norted deployment seals use Python JSON canonicalization: UTF-8 strings,
// sorted keys, compact separators and shortest round-trip floats with signed,
// two-digit exponents. Ordinary serde_json exponent spelling differs.
fn deployment_canonical_json(value: &serde_json::Value) -> Result<Vec<u8>, String> {
    fn render(value: &serde_json::Value) -> Result<String, String> {
        use serde_json::Value;
        Ok(match value {
            Value::Array(values) => format!(
                "[{}]",
                values
                    .iter()
                    .map(render)
                    .collect::<Result<Vec<_>, _>>()?
                    .join(",")
            ),
            Value::Object(values) => {
                let mut entries = values.iter().collect::<Vec<_>>();
                entries.sort_by_key(|(key, _)| *key);
                format!(
                    "{{{}}}",
                    entries
                        .into_iter()
                        .map(|(key, value)| Ok(format!(
                            "{}:{}",
                            serde_json::to_string(key).map_err(|e| e.to_string())?,
                            render(value)?
                        )))
                        .collect::<Result<Vec<_>, String>>()?
                        .join(",")
                )
            }
            Value::Number(n) if n.is_f64() => {
                let text = format!("{:?}", n.as_f64().ok_or("invalid canonical float")?);
                if let Some((mantissa, exponent)) = text.split_once('e') {
                    let exponent: i32 = exponent.parse().map_err(|_| "invalid float exponent")?;
                    format!(
                        "{mantissa}e{}{abs:02}",
                        if exponent < 0 { "-" } else { "+" },
                        abs = exponent.unsigned_abs()
                    )
                } else {
                    text
                }
            }
            _ => serde_json::to_string(value).map_err(|e| e.to_string())?,
        })
    }
    render(value).map(String::into_bytes)
}
