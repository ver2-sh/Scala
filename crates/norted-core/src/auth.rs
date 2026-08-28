use std::collections::BTreeSet;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::AppPaths;

pub const API_KEYS_SCHEMA_VERSION: u32 = 1;
pub const API_KEY_PREFIX: &str = "norted_sk_";
pub const MAX_API_KEYS: usize = 256;
const SECRET_BYTES: usize = 32;
const SECRET_HEX_LEN: usize = SECRET_BYTES * 2;
const API_KEY_LENGTH: usize = API_KEY_PREFIX.len() + SECRET_HEX_LEN;
const DISPLAY_RANDOM_CHARS: usize = 8;

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PublicAuthMode {
    #[default]
    Auto,
    Required,
    Disabled,
}

impl fmt::Display for PublicAuthMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Auto => "auto",
            Self::Required => "required",
            Self::Disabled => "disabled",
        })
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectivePublicAuthMode {
    Required,
    Disabled,
}

impl fmt::Display for EffectivePublicAuthMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Required => "required",
            Self::Disabled => "disabled",
        })
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct PublicAuthStatus {
    pub bind: String,
    pub loopback: bool,
    pub configured_mode: PublicAuthMode,
    pub effective_mode: EffectivePublicAuthMode,
    pub active_key_count: usize,
    pub bind_allowed: bool,
    pub insecure_remote: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiKeyRecord {
    pub key_id: String,
    pub name: String,
    pub display_prefix: String,
    pub sha256_digest: String,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<i64>,
}

impl ApiKeyRecord {
    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }

    fn summary(&self) -> ApiKeySummary {
        ApiKeySummary {
            key_id: self.key_id.clone(),
            name: self.name.clone(),
            display_prefix: self.display_prefix.clone(),
            created_at: self.created_at,
            revoked_at: self.revoked_at,
            active: self.is_active(),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApiKeyState {
    pub version: u32,
    pub keys: Vec<ApiKeyRecord>,
}

impl Default for ApiKeyState {
    fn default() -> Self {
        Self {
            version: API_KEYS_SCHEMA_VERSION,
            keys: Vec::new(),
        }
    }
}

impl ApiKeyState {
    fn validate(&self) -> Result<(), ApiKeyStoreError> {
        if self.version != API_KEYS_SCHEMA_VERSION {
            return Err(ApiKeyStoreError::UnsupportedVersion {
                found: self.version,
                supported: API_KEYS_SCHEMA_VERSION,
            });
        }
        if self.keys.len() > MAX_API_KEYS {
            return Err(ApiKeyStoreError::TooManyKeys { max: MAX_API_KEYS });
        }

        let mut ids = BTreeSet::new();
        for key in &self.keys {
            if !ids.insert(&key.key_id) {
                return Err(ApiKeyStoreError::InvalidState(format!(
                    "duplicate key ID `{}`",
                    key.key_id
                )));
            }
            if !valid_key_id(&key.key_id) {
                return Err(ApiKeyStoreError::InvalidState(format!(
                    "invalid key ID `{}`",
                    key.key_id
                )));
            }
            validate_name(&key.name)?;
            if !valid_display_prefix(&key.display_prefix) {
                return Err(ApiKeyStoreError::InvalidState(format!(
                    "invalid display prefix for key `{}`",
                    key.key_id
                )));
            }
            if decode_sha256(&key.sha256_digest).is_none() {
                return Err(ApiKeyStoreError::InvalidState(format!(
                    "invalid SHA-256 digest for key `{}`",
                    key.key_id
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct ApiKeySummary {
    pub key_id: String,
    pub name: String,
    pub display_prefix: String,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<i64>,
    pub active: bool,
}

#[derive(Clone)]
pub struct CreatedApiKey {
    summary: ApiKeySummary,
    secret: String,
}

impl CreatedApiKey {
    pub fn summary(&self) -> &ApiKeySummary {
        &self.summary
    }

    pub fn secret(&self) -> &str {
        &self.secret
    }
}

impl fmt::Debug for CreatedApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreatedApiKey")
            .field("summary", &self.summary)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApiKeyStoreError {
    #[error("API-key state I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("API-key state at {path} is corrupt or invalid JSON: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("could not serialize API-key state: {0}")]
    Serialize(serde_json::Error),
    #[error("API-key state task failed: {0}")]
    Task(String),
    #[error(
        "API-key state schema version {found} is not supported; this build supports version {supported}"
    )]
    UnsupportedVersion { found: u32, supported: u32 },
    #[error("API-key state is invalid: {0}")]
    InvalidState(String),
    #[error("API-key name must be 1 to 128 printable characters")]
    InvalidName,
    #[error("API-key store is limited to {max} records")]
    TooManyKeys { max: usize },
    #[error("API key `{0}` was not found")]
    KeyNotFound(String),
    #[error("API key `{0}` is already revoked")]
    AlreadyRevoked(String),
    #[error("the operating system could not generate secure random bytes: {0}")]
    Random(String),
}

#[derive(Debug, Clone)]
pub struct ApiKeyStore {
    path: PathBuf,
    lock_path: PathBuf,
}

impl ApiKeyStore {
    pub fn new(paths: &AppPaths) -> Self {
        Self::from_data_dir(&paths.data_dir)
    }

    pub fn from_data_dir(data_dir: impl AsRef<Path>) -> Self {
        Self {
            path: data_dir.as_ref().join("api-keys.json"),
            lock_path: data_dir.as_ref().join(".api-keys.lock"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn read(&self) -> Result<ApiKeyState, ApiKeyStoreError> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || read_state(&path))
            .await
            .map_err(|error| ApiKeyStoreError::Task(error.to_string()))?
    }

    pub async fn list(&self) -> Result<Vec<ApiKeySummary>, ApiKeyStoreError> {
        Ok(self
            .read()
            .await?
            .keys
            .iter()
            .map(ApiKeyRecord::summary)
            .collect())
    }

    pub async fn active_count(&self) -> Result<usize, ApiKeyStoreError> {
        Ok(self
            .read()
            .await?
            .keys
            .iter()
            .filter(|key| key.is_active())
            .count())
    }

    pub async fn create(&self, name: Option<String>) -> Result<CreatedApiKey, ApiKeyStoreError> {
        let name = name.unwrap_or_else(|| "unnamed".to_owned());
        validate_name(&name)?;
        self.update(move |state| {
            if state.keys.len() >= MAX_API_KEYS {
                return Err(ApiKeyStoreError::TooManyKeys { max: MAX_API_KEYS });
            }

            let mut random = [0_u8; SECRET_BYTES];
            getrandom::fill(&mut random)
                .map_err(|error| ApiKeyStoreError::Random(error.to_string()))?;
            let secret = format!("{API_KEY_PREFIX}{}", encode_hex(&random));
            let digest = Sha256::digest(secret.as_bytes());
            let key_id = format!("key_{}", uuid::Uuid::new_v4().simple());
            let display_prefix = secret[..API_KEY_PREFIX.len() + DISPLAY_RANDOM_CHARS].to_owned();
            let created_at = unix_timestamp();
            let record = ApiKeyRecord {
                key_id,
                name,
                display_prefix,
                sha256_digest: encode_hex(&digest),
                created_at,
                revoked_at: None,
            };
            let summary = record.summary();
            state.keys.push(record);
            Ok(CreatedApiKey { summary, secret })
        })
        .await
    }

    pub async fn revoke(&self, key_id: String) -> Result<ApiKeySummary, ApiKeyStoreError> {
        self.update(move |state| {
            let key = state
                .keys
                .iter_mut()
                .find(|key| key.key_id == key_id)
                .ok_or_else(|| ApiKeyStoreError::KeyNotFound(key_id.clone()))?;
            if !key.is_active() {
                return Err(ApiKeyStoreError::AlreadyRevoked(key_id));
            }
            key.revoked_at = Some(unix_timestamp());
            Ok(key.summary())
        })
        .await
    }

    pub async fn verify(&self, credential: &str) -> Result<bool, ApiKeyStoreError> {
        if !valid_secret_shape(credential) {
            return Ok(false);
        }
        let supplied: [u8; 32] = Sha256::digest(credential.as_bytes()).into();
        let state = self.read().await?;
        Ok(state.keys.iter().filter(|key| key.is_active()).any(|key| {
            decode_sha256(&key.sha256_digest)
                .is_some_and(|expected| constant_time_equal(&supplied, &expected))
        }))
    }

    async fn update<F, T>(&self, update: F) -> Result<T, ApiKeyStoreError>
    where
        F: FnOnce(&mut ApiKeyState) -> Result<T, ApiKeyStoreError> + Send + 'static,
        T: Send + 'static,
    {
        let path = self.path.clone();
        let lock_path = self.lock_path.clone();
        tokio::task::spawn_blocking(move || {
            let parent = lock_path.parent().unwrap_or_else(|| Path::new("."));
            std::fs::create_dir_all(parent).map_err(|source| ApiKeyStoreError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
            let lock = open_sensitive_lock(&lock_path)?;
            lock.lock_exclusive()
                .map_err(|source| ApiKeyStoreError::Io {
                    path: lock_path.clone(),
                    source,
                })?;
            let mut state = read_state(&path)?;
            let result = update(&mut state)?;
            state.validate()?;
            write_state(&path, &state)?;
            Ok(result)
        })
        .await
        .map_err(|error| ApiKeyStoreError::Task(error.to_string()))?
    }
}

fn read_state(path: &Path) -> Result<ApiKeyState, ApiKeyStoreError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ApiKeyState::default());
        }
        Err(source) => {
            return Err(ApiKeyStoreError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let state: ApiKeyState =
        serde_json::from_slice(&bytes).map_err(|source| ApiKeyStoreError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    state.validate()?;
    Ok(state)
}

fn write_state(path: &Path, state: &ApiKeyState) -> Result<(), ApiKeyStoreError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|source| ApiKeyStoreError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let bytes = serde_json::to_vec_pretty(state).map_err(ApiKeyStoreError::Serialize)?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|source| ApiKeyStoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    set_owner_only(temporary.path())?;
    temporary
        .write_all(&bytes)
        .and_then(|_| temporary.write_all(b"\n"))
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|source| ApiKeyStoreError::Io {
            path: temporary.path().to_path_buf(),
            source,
        })?;
    temporary
        .persist(path)
        .map_err(|error| ApiKeyStoreError::Io {
            path: path.to_path_buf(),
            source: error.error,
        })?;
    set_owner_only(path)?;
    if let Ok(directory) = std::fs::File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn open_sensitive_lock(path: &Path) -> Result<std::fs::File, ApiKeyStoreError> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(|source| ApiKeyStoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    set_owner_only(path)?;
    Ok(file)
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> Result<(), ApiKeyStoreError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|source| {
        ApiKeyStoreError::Io {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> Result<(), ApiKeyStoreError> {
    Ok(())
}

fn validate_name(name: &str) -> Result<(), ApiKeyStoreError> {
    if name.is_empty()
        || name.chars().count() > 128
        || name.chars().any(|character| character.is_control())
    {
        return Err(ApiKeyStoreError::InvalidName);
    }
    Ok(())
}

fn valid_key_id(value: &str) -> bool {
    value.len() == 36
        && value.starts_with("key_")
        && value[4..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_display_prefix(value: &str) -> bool {
    value.len() == API_KEY_PREFIX.len() + DISPLAY_RANDOM_CHARS
        && value.starts_with(API_KEY_PREFIX)
        && value[API_KEY_PREFIX.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
}

fn valid_secret_shape(value: &str) -> bool {
    value.len() == API_KEY_LENGTH
        && value.starts_with(API_KEY_PREFIX)
        && value[API_KEY_PREFIX.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
}

fn decode_sha256(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut decoded = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        decoded[index] = (hex_value(pair[0])? << 4) | hex_value(pair[1])?;
    }
    Some(decoded)
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn constant_time_equal(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::{API_KEY_PREFIX, ApiKeyStore, ApiKeyStoreError};

    #[tokio::test]
    async fn generated_key_verifies_and_plaintext_is_not_persisted() {
        let temporary = tempfile::tempdir().expect("temporary key store");
        let store = ApiKeyStore::from_data_dir(temporary.path());
        let created = store
            .create(Some("vscode".to_owned()))
            .await
            .expect("create key");
        let secret = created.secret().to_owned();

        assert!(secret.starts_with(API_KEY_PREFIX));
        assert!(store.verify(&secret).await.expect("verify key"));
        assert!(!store.verify("norted_sk_wrong").await.expect("reject key"));

        let persisted = std::fs::read_to_string(store.path()).expect("read state");
        assert!(!persisted.contains(&secret));
    }

    #[tokio::test]
    async fn revoked_key_stops_verifying_without_reloading_the_store() {
        let temporary = tempfile::tempdir().expect("temporary key store");
        let store = ApiKeyStore::from_data_dir(temporary.path());
        let created = store.create(None).await.expect("create key");
        let secret = created.secret().to_owned();
        let key_id = created.summary().key_id.clone();

        assert!(store.verify(&secret).await.expect("verify active key"));
        store.revoke(key_id).await.expect("revoke key");
        assert!(!store.verify(&secret).await.expect("reject revoked key"));
    }

    #[tokio::test]
    async fn list_output_omits_digest_and_secret() {
        let temporary = tempfile::tempdir().expect("temporary key store");
        let store = ApiKeyStore::from_data_dir(temporary.path());
        let created = store.create(None).await.expect("create key");
        let serialized =
            serde_json::to_string(&store.list().await.expect("list keys")).expect("serialize list");

        assert!(!serialized.contains("digest"));
        assert!(!serialized.contains("secret"));
        assert!(!serialized.contains(created.secret()));
        assert!(serialized.contains("display_prefix"));
    }

    #[tokio::test]
    async fn corrupt_state_fails_closed_instead_of_becoming_empty() {
        let temporary = tempfile::tempdir().expect("temporary key store");
        let store = ApiKeyStore::from_data_dir(temporary.path());
        std::fs::write(store.path(), b"not json").expect("write corrupt state");

        assert!(matches!(
            store.active_count().await,
            Err(ApiKeyStoreError::Parse { .. })
        ));
        assert!(matches!(
            store
                .verify(&format!("{API_KEY_PREFIX}{}", "a".repeat(64)))
                .await,
            Err(ApiKeyStoreError::Parse { .. })
        ));
    }
}
