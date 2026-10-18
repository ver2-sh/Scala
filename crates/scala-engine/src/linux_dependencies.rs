//! Engine-owned, immutable Linux dependency inputs. Construction performs no I/O.
use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_LINUX_DEPENDENCY_LOCK_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_LINUX_DEPENDENCY_PACKAGES: usize = 2048;
const MAX_PACKAGE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
#[error("invalid pinned Linux dependency lock: {0}")]
pub struct LinuxDependencyRecipeError(pub String);

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxDependencyPackage {
    pub name: String,
    pub version: String,
    pub build: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

impl LinuxDependencyPackage {
    /// Only called on validated recipe metadata by the acquisition backend.
    pub fn filename(&self) -> String {
        // Conda epoch versions use `!`; URLs encode it, cache filenames do not.
        let suffix = if self.url.ends_with(".conda") {
            ".conda"
        } else {
            ".tar.bz2"
        };
        format!("{}{suffix}", self.archive_stem())
    }

    pub fn archive_stem(&self) -> String {
        format!("{}-{}-{}", self.name, self.version, self.build)
    }
}

fn required_nullable_string<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxDependencyLock {
    pub schema_version: u32,
    pub platform: String,
    pub minimum_glibc: String,
    // The custom deserializer makes omission different from explicit null.
    #[serde(deserialize_with = "required_nullable_string")]
    pub cuda_release: Option<String>,
    /// Upstream LINK order; never sorted or solved again by Scala.
    pub packages: Vec<LinuxDependencyPackage>,
}

impl LinuxDependencyLock {
    pub fn package(&self, name: &str) -> Option<&LinuxDependencyPackage> {
        self.packages.iter().find(|package| package.name == name)
    }
}

/// A reviewed adapter supplies the exact embedded lock bytes. This type does not
/// choose an engine, a CUDA release, an executable, or a filesystem location.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LinuxDependencyRecipe {
    lock_bytes: &'static [u8],
    metadata: LinuxDependencyLock,
    sha256: String,
}

impl LinuxDependencyRecipe {
    pub fn new(lock_bytes: &'static [u8]) -> Result<Self, LinuxDependencyRecipeError> {
        let metadata = validate_lock(lock_bytes)?;
        Ok(Self {
            lock_bytes,
            metadata,
            sha256: format!("{:x}", Sha256::digest(lock_bytes)),
        })
    }

    pub fn lock_bytes(&self) -> &'static [u8] {
        self.lock_bytes
    }

    pub fn metadata(&self) -> &LinuxDependencyLock {
        &self.metadata
    }

    /// Digest of original bytes, not of reserialized or reordered JSON.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    pub fn explicit_lock(&self) -> String {
        let mut text = String::from("@EXPLICIT\n");
        for package in &self.metadata.packages {
            text.push_str(&package.url);
            text.push('#');
            text.push_str(&package.sha256);
            text.push('\n');
        }
        text
    }
}

/// Persist in an owning runtime receipt. It is evidence, not permission to use an
/// arbitrary deserialized prefix: consumers must validate it against the owned
/// durable root, the adapter's recipe, and the environment's ready receipt.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxDependencyReference {
    pub schema_version: u32,
    pub recipe_sha256: String,
    pub environment_sha256: String,
    pub bootstrap_sha256: String,
    pub implementation_version: String,
    pub inventory_sha256: String,
    pub prefix: PathBuf,
}

fn safe_atom(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && !matches!(value, "." | "..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.+-!".contains(&byte))
}

pub(crate) fn validate_lock(
    bytes: &[u8],
) -> Result<LinuxDependencyLock, LinuxDependencyRecipeError> {
    let invalid = |detail: &str| LinuxDependencyRecipeError(detail.into());
    if bytes.is_empty() || bytes.len() > MAX_LINUX_DEPENDENCY_LOCK_BYTES {
        return Err(invalid("lock byte limit exceeded or lock is empty"));
    }
    let lock: LinuxDependencyLock = serde_json::from_slice(bytes)
        .map_err(|error| LinuxDependencyRecipeError(error.to_string()))?;
    if lock.schema_version != 1 || lock.platform != "linux-64" || lock.minimum_glibc != "2.28" {
        return Err(invalid("expected schema 1, linux-64, glibc 2.28"));
    }
    if !lock.cuda_release.as_ref().is_none_or(|v| {
        v.split('.').count() == 2
            && v.split('.')
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    }) {
        return Err(invalid("CUDA release is outside the reviewed policy"));
    }
    if lock.packages.is_empty() || lock.packages.len() > MAX_LINUX_DEPENDENCY_PACKAGES {
        return Err(invalid("invalid package count"));
    }
    let mut names = BTreeSet::new();
    let mut filenames = BTreeSet::new();
    let mut total = 0_u64;
    for package in &lock.packages {
        if !safe_atom(&package.name) || !safe_atom(&package.version) || !safe_atom(&package.build) {
            return Err(invalid("unsafe package name, version, or build"));
        }
        if package.name == "cuda-drivers"
            || package.name == "cuda-driver"
            || package.name.starts_with("nvidia-driver")
        {
            return Err(invalid("GPU driver installation is not permitted"));
        }
        if package.sha256.len() != 64
            || !package.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            || package.size == 0
            || package.size > MAX_PACKAGE_BYTES
        {
            return Err(invalid("invalid package SHA-256 or size"));
        }
        total = total
            .checked_add(package.size)
            .ok_or_else(|| invalid("package size overflow"))?;
        if total > MAX_TOTAL_BYTES {
            return Err(invalid("aggregate package size limit exceeded"));
        }
        let url = reqwest::Url::parse(&package.url).map_err(|_| invalid("invalid package URL"))?;
        if url.scheme() != "https"
            || url.host_str() != Some("conda.anaconda.org")
            || url.port().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid(
                "package URL must be an uncredentialed conda-forge HTTPS URL",
            ));
        }
        let stem = package.archive_stem();
        let valid = ["linux-64", "noarch"].into_iter().any(|subdir| {
            [".conda", ".tar.bz2"].into_iter().any(|extension| {
                package.url
                    == format!("https://conda.anaconda.org/conda-forge/{subdir}/{stem}{extension}")
                        .replace('!', "%21")
            })
        });
        if !valid {
            return Err(invalid(
                "package URL does not exactly bind the declared package identity",
            ));
        }
        if !names.insert(package.name.clone()) || !filenames.insert(package.filename().to_owned()) {
            return Err(invalid(
                "duplicate package name or conflicting archive filename",
            ));
        }
    }
    let cuda_version = lock.package("cuda-version");
    match (&lock.cuda_release, cuda_version) {
        (Some(release), Some(package))
            if package.version == *release
                || package.version.starts_with(&format!("{release}.")) => {}
        (None, None) => {
            if lock
                .packages
                .iter()
                .any(|package| package.name.starts_with("cuda-"))
            {
                return Err(invalid("CPU lock contains CUDA packages"));
            }
        }
        _ => return Err(invalid("cuda-version does not match cuda_release")),
    }
    Ok(lock)
}
