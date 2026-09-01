use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::IsTerminal;
use std::path::Path;
use std::time::Duration;

use norted_core::{
    ApiKeyStore, AppConfig, AppPaths, ConfigSource, LoadedConfig, ModelArtifact,
    ModelProfilesState, ModelProfilesStore, ModelRegistry, RuntimeCompatibility,
    RuntimeSelectionSource, SettingDefinition, SettingId, SettingsPatch, SettingsState,
    SettingsStore,
};
use norted_engine::{
    BackendLifecycle, ControlClient, ControlClientError, EngineRegistry, InstallationState,
    RuntimeLocalInspection, RuntimePackError, RuntimePackManager, RuntimeStoreIssueKind,
    detect_host_capabilities,
};
use serde::Serialize;

use crate::composition;

const AUTH_KEY_REMEDIATION: &str = "Run:\n  norted-server auth keys create --name <LABEL>";
const MODELS_REMEDIATION: &str = "Run:\n  norted-server models list\nThen configure an existing local model directory under [models].paths.";
const RUNTIMES_REMEDIATION: &str = "Run:\n  norted-server runtimes list";

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DoctorStatus {
    Pass,
    Warning,
    Fail,
}

impl DoctorStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Warning => "WARN",
            Self::Fail => "FAIL",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorFinding {
    pub code: String,
    pub category: String,
    pub status: DoctorStatus,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remediation: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct DoctorSummary {
    pub passes: usize,
    pub warnings: usize,
    pub failures: usize,
    pub problems: usize,
}

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub status: DoctorStatus,
    pub summary: DoctorSummary,
    pub checks: Vec<DoctorFinding>,
}

impl DoctorReport {
    fn new(checks: Vec<DoctorFinding>) -> Self {
        let passes = checks
            .iter()
            .filter(|finding| finding.status == DoctorStatus::Pass)
            .count();
        let warnings = checks
            .iter()
            .filter(|finding| finding.status == DoctorStatus::Warning)
            .count();
        let failures = checks
            .iter()
            .filter(|finding| finding.status == DoctorStatus::Fail)
            .count();
        let summary = DoctorSummary {
            passes,
            warnings,
            failures,
            problems: warnings + failures,
        };
        let status = if failures > 0 {
            DoctorStatus::Fail
        } else if warnings > 0 {
            DoctorStatus::Warning
        } else {
            DoctorStatus::Pass
        };
        Self {
            status,
            summary,
            checks,
        }
    }

    pub fn has_failures(&self) -> bool {
        self.summary.failures > 0
    }
}

pub async fn run() -> DoctorReport {
    match AppPaths::discover() {
        Ok(paths) => run_with_paths(paths).await,
        Err(error) => {
            let mut checks = vec![finding(
                "paths.discovery",
                "paths",
                DoctorStatus::Fail,
                "Application paths could not be discovered.",
                Some(error.to_string()),
                &[
                    "Set the platform configuration, data, state, and cache environment to usable absolute locations.",
                ],
            )];
            let host = detect_host_capabilities().await;
            add_basic_host_facts(&mut checks, &host);
            add_accelerator_facts(&mut checks, &host);
            inspect_source_toolchain(&host, &mut checks).await;
            add_terminal_facts(&mut checks);
            DoctorReport::new(checks)
        }
    }
}

async fn run_with_paths(paths: AppPaths) -> DoctorReport {
    let mut checks = vec![pass(
        "paths.discovery",
        "paths",
        "Application paths were discovered.",
        Some(format!(
            "Configuration file: {}",
            paths.config_file.display()
        )),
    )];

    for (code, label, path) in [
        ("paths.config", "Configuration directory", &paths.config_dir),
        ("paths.data", "Data directory", &paths.data_dir),
        ("paths.state", "State directory", &paths.state_dir),
        ("paths.cache", "Cache directory", &paths.cache_dir),
        ("paths.logs", "Log directory", &paths.log_dir),
        ("paths.runtimes", "Runtime store", &paths.runtimes_dir),
        (
            "paths.runtime_cache",
            "Runtime cache directory",
            &paths.runtime_cache_dir,
        ),
    ] {
        checks.push(directory_finding(code, label, path));
    }

    let config = inspect_config(&paths, &mut checks);
    let key_count = inspect_auth_store(&paths, &mut checks).await;
    if let Some(config) = config.as_ref() {
        inspect_server_auth(config, key_count, &mut checks);
    } else {
        checks.push(finding(
            "auth.skipped",
            "authentication",
            DoctorStatus::Warning,
            "Authentication policy could not be evaluated because configuration is invalid.",
            None,
            &["Repair the configuration reported above, then run Doctor again."],
        ));
    }

    let settings = inspect_settings(&paths, &mut checks).await;
    let profiles = inspect_profiles(&paths, &mut checks).await;

    // Defaults keep managed runtime integrity diagnosable when user config is
    // broken. They are used only in memory and never persisted.
    let adapter_config = config.clone().unwrap_or_default();
    let registry = match composition::engine_registry_from_config(
        &adapter_config,
        &paths.config_file,
    ) {
        Ok(registry) => {
            checks.push(pass(
                "engine.registry",
                "engines",
                format!("{} engine adapters registered.", registry.len()),
                None,
            ));
            Some(registry)
        }
        Err(error) => {
            checks.push(finding(
                "engine.registry",
                "engines",
                DoctorStatus::Fail,
                "Engine adapters could not be registered.",
                Some(error.to_string()),
                &["Review [engine] configuration and reinstall Norted Server if built-in adapters are missing."],
            ));
            None
        }
    };

    if let (Some(settings), Some(registry)) = (settings.as_ref(), registry.as_ref()) {
        inspect_setting_values(settings, registry, &mut checks);
    }
    if let Some(registry) = registry.as_ref() {
        inspect_engine_adapters(registry, &mut checks).await;
    }

    let (models, model_paths_available) = if let Some(config) = config.as_ref() {
        inspect_models(config, &mut checks)
    } else {
        checks.push(finding(
            "models.skipped",
            "models",
            DoctorStatus::Warning,
            "Model discovery could not use configured paths because configuration is invalid.",
            None,
            &[MODELS_REMEDIATION],
        ));
        (Vec::new(), false)
    };

    let (packs, runtime_inspection) = if let Some(registry) = registry.as_ref() {
        match composition::runtime_pack_manager_from_paths(&paths, registry.clone()) {
            Ok(packs) => {
                let inspection = packs.inspect_local().await;
                inspect_runtimes(&inspection, &models, &mut checks);
                inspect_model_runtime_selections(&packs, &inspection, &models, &mut checks);
                (Some(packs), Some(inspection))
            }
            Err(error) => {
                checks.push(finding(
                    "runtime.inspection_unavailable",
                    "runtimes",
                    DoctorStatus::Fail,
                    "Managed runtime inspection could not be initialized.",
                    Some(error.to_string()),
                    &[RUNTIMES_REMEDIATION],
                ));
                (None, None)
            }
        }
    } else {
        (None, None)
    };

    if let (Some(profiles), Some(settings), Some(registry), Some(packs), Some(inspection)) = (
        profiles.as_ref(),
        settings.as_ref(),
        registry.as_ref(),
        packs.as_ref(),
        runtime_inspection.as_ref(),
    ) && model_paths_available
    {
        inspect_model_profiles(
            profiles,
            settings,
            &models,
            registry,
            packs,
            inspection,
            &paths,
            &mut checks,
        )
        .await;
    }

    inspect_control_and_bind(&paths, config.as_ref(), &mut checks).await;

    let host = runtime_inspection
        .as_ref()
        .map(|inspection| inspection.host.clone());
    if let Some(host) = host {
        add_basic_host_facts(&mut checks, &host);
        add_accelerator_facts(&mut checks, &host);
        inspect_source_toolchain(&host, &mut checks).await;
    } else {
        let host = detect_host_capabilities().await;
        add_basic_host_facts(&mut checks, &host);
        add_accelerator_facts(&mut checks, &host);
        inspect_source_toolchain(&host, &mut checks).await;
    }
    add_terminal_facts(&mut checks);

    DoctorReport::new(checks)
}

fn inspect_config(paths: &AppPaths, checks: &mut Vec<DoctorFinding>) -> Option<AppConfig> {
    if !paths.config_file.exists() {
        checks.push(pass(
            "config.file",
            "configuration",
            "Configuration file is absent; valid built-in defaults apply.",
            Some(paths.config_file.display().to_string()),
        ));
        let loaded = LoadedConfig::load(paths).expect("absent configuration uses defaults");
        checks.push(pass(
            "config.schema",
            "configuration",
            "Built-in configuration schema is valid.",
            None,
        ));
        return Some(loaded.config);
    }

    let text = match fs::read_to_string(&paths.config_file) {
        Ok(text) => {
            checks.push(pass(
                "config.readable",
                "configuration",
                "Configuration file is readable.",
                Some(paths.config_file.display().to_string()),
            ));
            text
        }
        Err(error) => {
            checks.push(finding(
                "config.readable",
                "configuration",
                DoctorStatus::Fail,
                "Configuration file is not readable.",
                Some(format!("{}: {error}", paths.config_file.display())),
                &["Restore read access to the configuration file."],
            ));
            return None;
        }
    };

    if let Err(error) = toml::from_str::<toml::Value>(&text) {
        checks.push(finding(
            "config.toml",
            "configuration",
            DoctorStatus::Fail,
            "Configuration is not valid TOML.",
            Some(error.to_string()),
            &["Correct the TOML syntax and run Doctor again."],
        ));
        return None;
    }
    checks.push(pass(
        "config.toml",
        "configuration",
        "Configuration TOML syntax is valid.",
        None,
    ));

    match LoadedConfig::load(paths) {
        Ok(loaded) => {
            debug_assert_eq!(loaded.source, ConfigSource::File);
            checks.push(pass(
                "config.schema",
                "configuration",
                "Configuration structure, fields, and schema version are valid.",
                None,
            ));
            checks.push(pass(
                "config.server_address",
                "configuration",
                "Server bind address is valid.",
                loaded
                    .config
                    .server
                    .socket_addr()
                    .ok()
                    .map(|address| address.to_string()),
            ));
            Some(loaded.config)
        }
        Err(error) => {
            checks.push(finding(
                "config.schema",
                "configuration",
                DoctorStatus::Fail,
                "Configuration structure, fields, or schema version is invalid.",
                Some(error.to_string()),
                &["Remove unknown fields, correct field values, and use the schema version supported by this build."],
            ));
            None
        }
    }
}

async fn inspect_auth_store(paths: &AppPaths, checks: &mut Vec<DoctorFinding>) -> Option<usize> {
    let store = ApiKeyStore::new(paths);
    match store.read().await {
        Ok(state) => {
            let active = state.keys.iter().filter(|key| key.is_active()).count();
            checks.push(pass(
                "auth.key_store",
                "authentication",
                format!("API-key state is valid; {active} active key(s)."),
                Some(store.path().display().to_string()),
            ));
            Some(active)
        }
        Err(error) => {
            checks.push(finding(
                "auth.key_store",
                "authentication",
                DoctorStatus::Fail,
                "API-key state is unreadable or invalid.",
                Some(bounded_detail(error.to_string())),
                &["Restore a valid API-key state file. Doctor will not expose or rewrite key material."],
            ));
            None
        }
    }
}

fn inspect_server_auth(
    config: &AppConfig,
    key_count: Option<usize>,
    checks: &mut Vec<DoctorFinding>,
) {
    let Some(key_count) = key_count else {
        return;
    };
    match config.server.public_auth_status(key_count) {
        Ok(status) => {
            checks.push(pass(
                "auth.status",
                "authentication",
                format!(
                    "Public bind {} is {}.",
                    status.bind,
                    if status.loopback {
                        "loopback"
                    } else {
                        "remote"
                    }
                ),
                Some(format!(
                    "Configured auth: {}; effective auth: {}; active keys: {}.",
                    status.configured_mode, status.effective_mode, status.active_key_count
                )),
            ));
            if !status.bind_allowed {
                checks.push(finding(
                    "auth.required_without_keys",
                    "authentication",
                    DoctorStatus::Fail,
                    format!(
                        "Authentication is required at {}, but there are no active API keys.",
                        status.bind
                    ),
                    Some(format!(
                        "Configured mode: {}; effective mode: {}.",
                        status.configured_mode, status.effective_mode
                    )),
                    &[AUTH_KEY_REMEDIATION],
                ));
            } else if status.insecure_remote {
                checks.push(finding(
                    "auth.remote_insecure",
                    "authentication",
                    DoctorStatus::Warning,
                    format!(
                        "Remote serving at {} explicitly disables authentication.",
                        status.bind
                    ),
                    Some("Prompts and outputs travel over unauthenticated plain HTTP unless an external trusted transport protects the endpoint.".to_owned()),
                    &["Bind to loopback, or set server.auth to auto or required. Use a trusted encrypted reverse proxy when remote transport is necessary."],
                ));
            } else {
                checks.push(pass(
                    "auth.policy",
                    "authentication",
                    format!("Public authentication policy is safe for {}.", status.bind),
                    None,
                ));
            }
        }
        Err(error) => checks.push(finding(
            "auth.policy",
            "authentication",
            DoctorStatus::Fail,
            "Public authentication policy could not be evaluated.",
            Some(error.to_string()),
            &["Correct the configured server address and authentication mode."],
        )),
    }
}

async fn inspect_settings(
    paths: &AppPaths,
    checks: &mut Vec<DoctorFinding>,
) -> Option<SettingsState> {
    let store = SettingsStore::new(paths);
    match store.read().await {
        Ok(state) => {
            checks.push(pass(
                "settings.state",
                "settings",
                if store.path().exists() {
                    "Persisted settings state is readable and structurally valid."
                } else {
                    "Settings state is absent; valid defaults apply."
                },
                Some(store.path().display().to_string()),
            ));
            Some(state)
        }
        Err(error) => {
            checks.push(finding(
                "settings.state_invalid",
                "settings",
                DoctorStatus::Fail,
                "Persisted settings state is unreadable or invalid.",
                Some(bounded_detail(error.to_string())),
                &["Correct settings.json using the current schema. Doctor will not migrate or rewrite it."],
            ));
            None
        }
    }
}

async fn inspect_profiles(
    paths: &AppPaths,
    checks: &mut Vec<DoctorFinding>,
) -> Option<ModelProfilesState> {
    let store = ModelProfilesStore::new(paths);
    match store.read().await {
        Ok(state) => {
            if state.profiles.is_empty() {
                checks.push(finding(
                    "model_profiles.none",
                    "model_profiles",
                    DoctorStatus::Warning,
                    "No Model Profiles are configured.",
                    Some(store.path().display().to_string()),
                    &["Run:\n  norted-server model-profiles list\nCreate a Model Profile before loading a model."],
                ));
            } else {
                checks.push(pass(
                    "model_profiles.state",
                    "model_profiles",
                    format!(
                        "{} Model Profile(s) are structurally valid.",
                        state.profiles.len()
                    ),
                    Some(store.path().display().to_string()),
                ));
            }
            Some(state)
        }
        Err(error) => {
            checks.push(finding(
                "model_profiles.state_invalid",
                "model_profiles",
                DoctorStatus::Fail,
                "Model Profile state is unreadable or invalid.",
                Some(bounded_detail(error.to_string())),
                &["Correct model-profiles.json using the current schema. Doctor will not migrate or rewrite it."],
            ));
            None
        }
    }
}

fn inspect_setting_values(
    state: &SettingsState,
    registry: &EngineRegistry,
    checks: &mut Vec<DoctorFinding>,
) {
    let definitions = match registry.setting_definitions() {
        Ok(definitions) => definitions
            .into_iter()
            .map(|definition| (definition.id.clone(), definition))
            .collect::<BTreeMap<_, _>>(),
        Err(error) => {
            checks.push(finding(
                "settings.definitions",
                "settings",
                DoctorStatus::Fail,
                "Engine setting definitions conflict or are invalid.",
                Some(error.to_string()),
                &["Review engine configuration and installed Norted Server components."],
            ));
            return;
        }
    };

    let mut errors = Vec::new();
    validate_patch(&state.global_defaults, None, &definitions, &mut errors);
    for (engine_id, patch) in &state.engine_defaults {
        if registry.get(engine_id).is_none() {
            errors.push(format!(
                "engine defaults reference unregistered engine `{engine_id}`"
            ));
            continue;
        }
        validate_patch(patch, Some(engine_id), &definitions, &mut errors);
    }
    if errors.is_empty() {
        checks.push(pass(
            "settings.values",
            "settings",
            "Persisted setting IDs, types, and engine scopes are valid.",
            None,
        ));
    } else {
        for error in errors {
            checks.push(finding(
                "settings.value_invalid",
                "settings",
                DoctorStatus::Fail,
                "A persisted setting is invalid for the current engine definitions.",
                Some(error),
                &["Use `norted-server settings show` to review current Global and engine defaults."],
            ));
        }
    }
}

fn validate_patch(
    patch: &SettingsPatch,
    engine_id: Option<&str>,
    definitions: &BTreeMap<SettingId, SettingDefinition>,
    errors: &mut Vec<String>,
) {
    for (id, value) in patch.iter() {
        let Some(definition) = definitions.get(id) else {
            errors.push(format!("unknown setting `{id}`"));
            continue;
        };
        if let Some(engine_id) = engine_id
            && !id.applies_to_engine(engine_id)
        {
            errors.push(format!(
                "setting `{id}` does not apply to engine `{engine_id}`"
            ));
            continue;
        }
        if let Err(error) = definition.validate_value(value) {
            errors.push(error.to_string());
        }
    }
}

async fn inspect_engine_adapters(registry: &EngineRegistry, checks: &mut Vec<DoctorFinding>) {
    for adapter in registry.adapters() {
        let identity = adapter.identity();
        match adapter.probe().await {
            Ok(probe) => match probe.installation {
                InstallationState::NotInstalled => checks.push(pass(
                    format!("engine.adapter.{}", identity.id),
                    "engines",
                    format!(
                        "{} adapter is registered; no external binary is configured.",
                        identity.display_name
                    ),
                    Some("Managed runtime packs remain available as the normal path.".to_owned()),
                )),
                InstallationState::Installed { installation } if probe.healthy => {
                    checks.push(pass(
                        format!("engine.adapter.{}", identity.id),
                        "engines",
                        format!("{} external binary is healthy.", identity.display_name),
                        Some(format!(
                            "{} ({})",
                            installation.binary_path.display(),
                            bounded_detail(probe.detail)
                        )),
                    ));
                }
                InstallationState::Installed { installation } => checks.push(finding(
                    format!("engine.adapter.{}", identity.id),
                    "engines",
                    DoctorStatus::Warning,
                    format!(
                        "{} external binary is installed but unhealthy.",
                        identity.display_name
                    ),
                    Some(format!(
                        "{}: {}",
                        installation.binary_path.display(),
                        bounded_detail(probe.detail)
                    )),
                    &["Correct or remove the configured external binary. Managed runtime packs are unaffected."],
                )),
                InstallationState::Invalid { reason } => checks.push(finding(
                    format!("engine.adapter.{}", identity.id),
                    "engines",
                    DoctorStatus::Warning,
                    format!(
                        "{} external binary configuration is invalid.",
                        identity.display_name
                    ),
                    Some(bounded_detail(reason)),
                    &["Correct or remove the configured external binary. Managed runtime packs are unaffected."],
                )),
            },
            Err(error) => checks.push(finding(
                format!("engine.adapter.{}", identity.id),
                "engines",
                DoctorStatus::Fail,
                format!("{} adapter probe failed.", identity.display_name),
                Some(bounded_detail(error.to_string())),
                &["Review this engine's configuration. Managed runtime integrity is inspected separately."],
            )),
        }
    }
}

fn inspect_models(
    config: &AppConfig,
    checks: &mut Vec<DoctorFinding>,
) -> (Vec<ModelArtifact>, bool) {
    let mut usable_paths = Vec::new();
    for path in &config.models.paths {
        match fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => {
                checks.push(pass(
                    "models.path",
                    "models",
                    "Configured model directory is available.",
                    Some(path.display().to_string()),
                ));
                usable_paths.push(path.clone());
            }
            Ok(_) => checks.push(finding(
                "models.path_invalid",
                "models",
                DoctorStatus::Warning,
                "A configured model path is not a directory.",
                Some(path.display().to_string()),
                &[MODELS_REMEDIATION],
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => checks.push(finding(
                "models.path_missing",
                "models",
                DoctorStatus::Warning,
                "A configured model directory is missing.",
                Some(path.display().to_string()),
                &[MODELS_REMEDIATION],
            )),
            Err(error) => checks.push(finding(
                "models.path_inaccessible",
                "models",
                DoctorStatus::Warning,
                "A configured model directory is inaccessible.",
                Some(format!("{}: {error}", path.display())),
                &["Restore read and traversal access, then run `norted-server models list`."],
            )),
        }
    }

    let registry = ModelRegistry::discover(&usable_paths);
    for warning in registry.warnings() {
        checks.push(finding(
            "models.registry_warning",
            "models",
            DoctorStatus::Warning,
            "A local model artifact or Norted package was rejected.",
            Some(bounded_detail(warning)),
            &["Run:\n  norted-server models list\nRebuild obsolete schema-5 q27/NInfer packages with current Norted packaging tools when reported."],
        ));
    }
    if registry.artifacts().is_empty() {
        checks.push(finding(
            "models.none",
            "models",
            DoctorStatus::Warning,
            "No recognized local models were discovered.",
            None,
            &[MODELS_REMEDIATION],
        ));
    } else {
        checks.push(pass(
            "models.discovery",
            "models",
            format!(
                "{} local model artifact(s) were discovered.",
                registry.artifacts().len()
            ),
            None,
        ));
    }
    (registry.artifacts().to_vec(), true)
}

fn inspect_runtimes(
    inspection: &RuntimeLocalInspection,
    models: &[ModelArtifact],
    checks: &mut Vec<DoctorFinding>,
) {
    if let Some(error) = &inspection.store_error {
        checks.push(finding(
            "runtime.store_unavailable",
            "runtimes",
            DoctorStatus::Fail,
            "Managed runtime store is inaccessible or structurally unsafe.",
            Some(bounded_detail(error)),
            &[RUNTIMES_REMEDIATION],
        ));
    } else if inspection.store_issues.is_empty() {
        checks.push(pass(
            "runtime.store",
            "runtimes",
            format!(
                "Managed runtime store contains {} valid runtime(s).",
                inspection
                    .installed
                    .iter()
                    .filter(|runtime| {
                        runtime.runtime.manifest.acquisition_method
                            != norted_core::RuntimeAcquisitionMethod::ExternalBinary
                    })
                    .count()
            ),
            None,
        ));
    }
    for issue in &inspection.store_issues {
        let code = match issue.kind {
            RuntimeStoreIssueKind::Io => "runtime.store_io",
            RuntimeStoreIssueKind::InvalidManifest => "runtime.manifest_invalid",
            RuntimeStoreIssueKind::UnsafePath => "runtime.path_unsafe",
            RuntimeStoreIssueKind::ProvenanceConflict => "runtime.provenance_invalid",
        };
        checks.push(finding(
            code,
            "runtimes",
            DoctorStatus::Fail,
            "An installed managed runtime failed integrity validation.",
            Some(bounded_detail(&issue.message)),
            &[RUNTIMES_REMEDIATION],
        ));
    }
    for warning in &inspection.adapter_warnings {
        checks.push(finding(
            "runtime.external_invalid",
            "runtimes",
            DoctorStatus::Warning,
            "An external runtime could not be included.",
            Some(bounded_detail(warning)),
            &["Review `norted-server engines list`; managed runtime packs remain separate."],
        ));
    }
    for runtime in &inspection.installed {
        let manifest = &runtime.runtime.manifest;
        if manifest.acquisition_method == norted_core::RuntimeAcquisitionMethod::ExternalBinary {
            continue;
        }
        match &runtime.compatibility {
            RuntimeCompatibility::Recommended | RuntimeCompatibility::Compatible => {
                checks.push(pass(
                    format!("runtime.compatible.{}", manifest.runtime_id),
                    "runtimes",
                    format!(
                        "Runtime {} is compatible with this host and adapter.",
                        manifest.runtime_id
                    ),
                    Some(format!(
                        "{} {} / {}",
                        manifest.identity.engine_id,
                        manifest.identity.version,
                        manifest.identity.variant
                    )),
                ));
            }
            RuntimeCompatibility::NeedsAttention(reason) => checks.push(finding(
                "runtime.needs_attention",
                "runtimes",
                DoctorStatus::Warning,
                format!("Runtime {} needs attention.", manifest.runtime_id),
                Some(bounded_detail(reason)),
                &[RUNTIMES_REMEDIATION],
            )),
            RuntimeCompatibility::Incompatible(reason) => checks.push(finding(
                "runtime.incompatible",
                "runtimes",
                DoctorStatus::Warning,
                format!(
                    "Runtime {} is incompatible with this host or adapter.",
                    manifest.runtime_id
                ),
                Some(bounded_detail(reason)),
                &[RUNTIMES_REMEDIATION],
            )),
        }
    }

    let Some(selections) = inspection.selections.as_ref() else {
        checks.push(finding(
            "runtime.selections_invalid",
            "runtime_selections",
            DoctorStatus::Fail,
            "Runtime selections are unreadable or invalid.",
            inspection.selections_error.clone().map(bounded_detail),
            &["Correct runtime-selections.json using the current schema. Doctor will not clear selections."],
        ));
        return;
    };
    checks.push(pass(
        "runtime.selections_schema",
        "runtime_selections",
        "Runtime selection state is readable and valid.",
        None,
    ));
    let installed = inspection
        .installed
        .iter()
        .map(|status| (&status.runtime.manifest.runtime_id, status))
        .collect::<BTreeMap<_, _>>();
    for (format, runtime_id) in &selections.format_defaults {
        if let Some(status) = installed.get(runtime_id)
            && !status.runtime.manifest.supported_formats.contains(format)
        {
            checks.push(finding(
                "runtime.selection_incompatible",
                "runtime_selections",
                DoctorStatus::Warning,
                format!(
                    "The {} default selection references a runtime that does not support that format.",
                    format.as_str().to_ascii_uppercase()
                ),
                Some(runtime_id.to_string()),
                &[&format!(
                    "Run:\n  norted-server runtimes clear-selection --format {}",
                    format.as_str()
                )],
            ));
            continue;
        }
        inspect_selection(
            format!("{} default", format.as_str().to_ascii_uppercase()),
            runtime_id,
            installed.get(runtime_id).copied(),
            format!(
                "norted-server runtimes clear-selection --format {}",
                format.as_str()
            ),
            checks,
        );
    }
    let model_ids = models
        .iter()
        .map(|model| &model.id)
        .collect::<BTreeSet<_>>();
    for (model_id, runtime_id) in &selections.model_overrides {
        if !model_ids.contains(model_id) {
            checks.push(finding(
                "runtime.selection_model_missing",
                "runtime_selections",
                DoctorStatus::Warning,
                format!("Runtime selection references missing model `{model_id}`."),
                Some(format!("Selected runtime: {runtime_id}")),
                &[&format!(
                    "Run:\n  norted-server runtimes clear-selection --model {model_id}"
                )],
            ));
        }
        inspect_selection(
            format!("model {model_id}"),
            runtime_id,
            installed.get(runtime_id).copied(),
            format!("norted-server runtimes clear-selection --model {model_id}"),
            checks,
        );
    }
}

fn inspect_selection(
    target: String,
    runtime_id: &norted_core::RuntimeId,
    status: Option<&norted_engine::InstalledRuntimeStatus>,
    clear_command: String,
    checks: &mut Vec<DoctorFinding>,
) {
    match status {
        None => checks.push(finding(
            "runtime.selection_missing",
            "runtime_selections",
            DoctorStatus::Warning,
            format!("The {target} selection references missing runtime `{runtime_id}`."),
            Some("Automatic fallback would be required.".to_owned()),
            &[&format!(
                "Run:\n  norted-server runtimes list\n  {clear_command}"
            )],
        )),
        Some(status) if !status.compatibility.is_usable() => checks.push(finding(
            "runtime.selection_incompatible",
            "runtime_selections",
            DoctorStatus::Warning,
            format!("The {target} selection references an incompatible runtime."),
            Some(format!("{runtime_id}: {:?}", status.compatibility)),
            &[&format!(
                "Run:\n  norted-server runtimes list\n  {clear_command}"
            )],
        )),
        Some(_) => checks.push(pass(
            "runtime.selection",
            "runtime_selections",
            format!("The {target} runtime selection is installed and host-compatible."),
            Some(runtime_id.to_string()),
        )),
    }
}

fn inspect_model_runtime_selections(
    packs: &RuntimePackManager,
    inspection: &RuntimeLocalInspection,
    models: &[ModelArtifact],
    checks: &mut Vec<DoctorFinding>,
) {
    if inspection.selections.is_none() || inspection.store_error.is_some() {
        return;
    }
    for model in models {
        match packs.resolve_from_local_inspection(model, None, None, inspection) {
            Ok(selection) if !selection.notices.is_empty() => checks.push(finding(
                "runtime.selection_fallback",
                "runtime_selections",
                DoctorStatus::Warning,
                format!(
                    "Model `{}` requires automatic runtime fallback.",
                    model.id
                ),
                Some(bounded_detail(selection.notices.join("; "))),
                &[&format!(
                    "Run:\n  norted-server runtimes list\n  norted-server runtimes clear-selection --model {}",
                    model.id
                )],
            )),
            Ok(_) | Err(_) => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn inspect_model_profiles(
    profiles: &ModelProfilesState,
    settings: &SettingsState,
    models: &[ModelArtifact],
    registry: &EngineRegistry,
    packs: &RuntimePackManager,
    inspection: &RuntimeLocalInspection,
    paths: &AppPaths,
    checks: &mut Vec<DoctorFinding>,
) {
    let models = models
        .iter()
        .map(|model| (&model.id, model))
        .collect::<BTreeMap<_, _>>();
    for profile in profiles.profiles.values() {
        let Some(model) = models.get(&profile.model_id).copied() else {
            checks.push(finding(
                "model_profile.missing_model",
                "model_profiles",
                DoctorStatus::Warning,
                format!(
                    "Model Profile '{}' references missing model `{}`.",
                    profile.id, profile.model_id
                ),
                None,
                &[&format!(
                    "Run:\n  norted-server models list\n  norted-server model-profiles set-model {} <MODEL_ID>",
                    profile.id
                )],
            ));
            continue;
        };
        let Some(adapter) = registry.get(profile.engine_id.as_str()) else {
            checks.push(finding(
                "model_profile.missing_engine",
                "model_profiles",
                DoctorStatus::Warning,
                format!(
                    "Model Profile '{}' references unregistered engine `{}`.",
                    profile.id, profile.engine_id
                ),
                None,
                &[&format!(
                    "Run:\n  norted-server model-profiles set-engine {} <ENGINE>",
                    profile.id
                )],
            ));
            continue;
        };
        if let norted_engine::CompatibilityDecision::Unsupported { reason } =
            adapter.compatibility(model)
        {
            checks.push(finding(
                "model_profile.engine_incompatible",
                "model_profiles",
                DoctorStatus::Warning,
                format!(
                    "Model Profile '{}' binds an engine incompatible with its model.",
                    profile.id
                ),
                Some(reason),
                &[&format!(
                    "Run:\n  norted-server model-profiles set-engine {} <ENGINE>",
                    profile.id
                )],
            ));
            continue;
        }
        let resolved = match settings.resolve(
            &profile.id,
            profile.engine_id.as_str(),
            &profile.overrides,
            &SettingsPatch::default(),
            &paths.data_dir,
        ) {
            Ok(resolved) => resolved,
            Err(error) => {
                checks.push(finding(
                    "model_profile.settings_invalid",
                    "model_profiles",
                    DoctorStatus::Fail,
                    format!(
                        "Model Profile '{}' settings cannot be resolved.",
                        profile.id
                    ),
                    Some(error.to_string()),
                    &[&format!(
                        "Run:\n  norted-server model-profiles show {}",
                        profile.id
                    )],
                ));
                continue;
            }
        };
        match packs.model_settings_schema_for_engine(model, profile.engine_id.as_str()) {
            Ok(schema) => {
                if let Err(error) = schema.validate(&resolved) {
                    checks.push(finding(
                        "model_profile.settings_invalid",
                        "model_profiles",
                        DoctorStatus::Fail,
                        format!(
                            "Model Profile '{}' settings are invalid for its model and engine.",
                            profile.id
                        ),
                        Some(error.to_string()),
                        &[&format!(
                            "Run:\n  norted-server model-profiles show {}",
                            profile.id
                        )],
                    ));
                    continue;
                }
            }
            Err(error) => {
                checks.push(finding(
                    "model_profile.engine_invalid",
                    "model_profiles",
                    DoctorStatus::Warning,
                    format!(
                        "Model Profile '{}' engine compatibility could not be evaluated.",
                        profile.id
                    ),
                    Some(bounded_detail(error.to_string())),
                    &[&format!(
                        "Run:\n  norted-server model-profiles set-engine {} <ENGINE>",
                        profile.id
                    )],
                ));
                continue;
            }
        }
        match packs
            .settings_schema_from_local_inspection(
                model,
                profile.engine_id.as_str(),
                Some(&resolved),
                inspection,
            )
            .await
        {
            Ok((selection, exact_schema)) => {
                if let Err(error) = exact_schema.validate(&resolved) {
                    checks.push(finding(
                        "model_profile.settings_invalid",
                        "model_profiles",
                        DoctorStatus::Fail,
                        format!(
                            "Model Profile '{}' settings are invalid for the selected runtime.",
                            profile.id
                        ),
                        Some(error.to_string()),
                        &[&format!(
                            "Run:\n  norted-server model-profiles show {}",
                            profile.id
                        )],
                    ));
                    continue;
                }
                if selection.source == RuntimeSelectionSource::Fallback
                    && !selection.notices.is_empty()
                {
                    checks.push(finding(
                        "model_profile.runtime_fallback",
                        "model_profiles",
                        DoctorStatus::Warning,
                        format!(
                            "Model Profile '{}' requires automatic runtime fallback.",
                            profile.id
                        ),
                        Some(bounded_detail(selection.notices.join("; "))),
                        &[&format!(
                            "Run:\n  norted-server runtimes list\n  norted-server runtimes search {}",
                            profile.engine_id
                        )],
                    ));
                } else {
                    checks.push(pass(
                        format!("model_profile.usable.{}", profile.id),
                        "model_profiles",
                        format!(
                            "Model Profile '{}' has a compatible installed runtime.",
                            profile.id
                        ),
                        Some(selection.runtime.manifest.runtime_id.to_string()),
                    ));
                }
            }
            Err(error @ RuntimePackError::Adapter(_)) => checks.push(finding(
                "model_profile.runtime_unhealthy",
                "model_profiles",
                DoctorStatus::Fail,
                format!(
                    "Model Profile '{}' selected runtime failed its exact adapter probe.",
                    profile.id
                ),
                Some(bounded_detail(error.to_string())),
                &[RUNTIMES_REMEDIATION],
            )),
            Err(error) => checks.push(finding(
                "model_profile.no_runtime",
                "model_profiles",
                DoctorStatus::Warning,
                format!(
                    "Model Profile '{}' has no compatible installed runtime.",
                    profile.id
                ),
                Some(bounded_detail(error.to_string())),
                &[&format!(
                    "Run:\n  norted-server runtimes search {}",
                    profile.engine_id
                )],
            )),
        }
    }
}

async fn inspect_control_and_bind(
    paths: &AppPaths,
    config: Option<&AppConfig>,
    checks: &mut Vec<DoctorFinding>,
) {
    let mut verified_running = false;
    let mut ownership_uncertain = false;
    match ControlClient::discover_read_only(paths).await {
        Ok(client) => match client.status().await {
            Ok(status) => {
                verified_running = true;
                match status.backend.lifecycle {
                    BackendLifecycle::Failed => checks.push(finding(
                        "control.backend_failed",
                        "control",
                        DoctorStatus::Fail,
                        "The running Server backend is in a failed lifecycle.",
                        status.backend.failure.map(bounded_detail),
                        &["Review recent Server logs and unload or retry the Model Profile explicitly after correcting the reported cause."],
                    )),
                    lifecycle => checks.push(pass(
                        "control.healthy",
                        "control",
                        "A running Norted Server passed authenticated private-control verification.",
                        Some(format!("Backend lifecycle: {lifecycle:?}.")),
                    )),
                }
            }
            Err(error) => {
                ownership_uncertain = true;
                checks.push(finding(
                    "control.unhealthy",
                    "control",
                    DoctorStatus::Fail,
                    "A Norted public endpoint was found, but authenticated private control could not be verified.",
                    Some(bounded_detail(error.to_string())),
                    &["Do not start a competing Server. Inspect the existing process and private runtime state."],
                ));
            }
        },
        Err(ControlClientError::Unavailable) => checks.push(pass(
            "control.stopped",
            "control",
            "No Norted Server is running; this is a normal stopped state.",
            None,
        )),
        Err(error) => {
            ownership_uncertain = true;
            checks.push(finding(
                "control.discovery_failed",
                "control",
                DoctorStatus::Warning,
                "Existing Server ownership could not be safely determined.",
                Some(bounded_detail(error.to_string())),
                &["Inspect the runtime state directory and retry before starting another Server."],
            ));
        }
    }

    let Some(config) = config else {
        return;
    };
    let Ok(address) = config.server.socket_addr() else {
        return;
    };
    if verified_running {
        checks.push(pass(
            "network.bind",
            "network",
            format!("Configured address {address} is owned by the verified Norted Server."),
            None,
        ));
    } else if !ownership_uncertain {
        match tokio::net::TcpListener::bind(address).await {
            Ok(listener) => {
                drop(listener);
                checks.push(pass(
                    "network.bind",
                    "network",
                    format!("Configured address {address} is currently available."),
                    None,
                ));
            }
            Err(error) => checks.push(finding(
                "network.bind_unavailable",
                "network",
                DoctorStatus::Fail,
                format!("Configured address {address} is unavailable to Norted Server."),
                Some(error.to_string()),
                &["Choose an available server.host/server.port or stop the different process occupying this address."],
            )),
        }
    }
}

fn add_basic_host_facts(checks: &mut Vec<DoctorFinding>, host: &norted_core::HostCapabilities) {
    checks.push(pass(
        "host.platform",
        "host",
        format!(
            "Host platform is {} / {}.",
            host.platform, host.architecture
        ),
        None,
    ));
}

fn add_accelerator_facts(checks: &mut Vec<DoctorFinding>, host: &norted_core::HostCapabilities) {
    if host.accelerators.is_empty() {
        checks.push(pass(
            "host.nvidia",
            "host",
            if host.nvidia_gpu_absence_confirmed {
                "No NVIDIA GPU is present; this is not intrinsically a problem."
            } else {
                "No NVIDIA GPU was observed; capability remains unknown and is not intrinsically a problem."
            },
            host.observations.first().cloned(),
        ));
        return;
    }
    for (index, device) in host.accelerators.iter().enumerate() {
        let mut facts = Vec::new();
        if let Some(uuid) = &device.stable_id {
            facts.push(format!("UUID {uuid}"));
        }
        if let Some(vram) = device.vram_bytes {
            facts.push(format!("{} VRAM", format_bytes(vram)));
        }
        if let Some(driver) = &device.driver_version {
            facts.push(format!("driver {driver}"));
        }
        if let Some(compute) = device.compute_capability {
            facts.push(format!("compute {compute}"));
        }
        checks.push(pass(
            format!("host.nvidia.{index}"),
            "host",
            format!(
                "NVIDIA GPU: {}.",
                device.name.as_deref().unwrap_or("unnamed device")
            ),
            (!facts.is_empty()).then(|| facts.join(", ")),
        ));
    }
}

async fn inspect_source_toolchain(
    host: &norted_core::HostCapabilities,
    checks: &mut Vec<DoctorFinding>,
) {
    if host.platform != "linux"
        || host.architecture != "x86_64"
        || !host
            .accelerators
            .iter()
            .any(|device| device.accelerator == "cuda")
    {
        return;
    }
    for (code, label, executable) in [
        ("git", "Git", "git"),
        ("cuda_compiler", "CUDA compiler", "/usr/local/cuda/bin/nvcc"),
        ("cmake", "CMake", "cmake"),
        ("ninja", "Ninja", "ninja"),
        ("make", "Make", "make"),
        ("cxx", "C++ compiler", "g++"),
        ("pkg_config", "pkg-config", "pkg-config"),
    ] {
        match command_version(executable).await {
            Ok(version) => checks.push(pass(
                format!("host.toolchain.{code}"),
                "host_toolchain",
                format!("{label} is available."),
                Some(version),
            )),
            Err(error) => checks.push(finding(
                format!("host.toolchain.{code}_missing"),
                "host_toolchain",
                DoctorStatus::Warning,
                if code == "cuda_compiler" {
                    "CUDA compiler is unavailable at /usr/local/cuda/bin/nvcc.".to_owned()
                } else {
                    format!("{label} is unavailable.")
                },
                Some(bounded_detail(error)),
                &["Managed Linux CUDA source-runtime installation may report Needs Attention. Norted-Utils can prepare a supported host."],
            )),
        }
    }
}

async fn command_version(executable: &str) -> Result<String, String> {
    let mut command = tokio::process::Command::new(executable);
    command.arg("--version").kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(2), command.output())
        .await
        .map_err(|_| format!("`{executable} --version` timed out"))?
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "`{executable} --version` exited with {}",
            output.status
        ));
    }
    let combined = if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    };
    Ok(bounded_detail(
        String::from_utf8_lossy(combined)
            .lines()
            .next()
            .unwrap_or("version observed"),
    ))
}

fn add_terminal_facts(checks: &mut Vec<DoctorFinding>) {
    checks.push(pass(
        "terminal.mode",
        "terminal",
        "Terminal mode is valid for interactive or headless operation.",
        Some(format!(
            "stdin_tty={}, stdout_tty={}",
            std::io::stdin().is_terminal(),
            std::io::stdout().is_terminal()
        )),
    ));
}

fn directory_finding(code: &str, label: &str, path: &Path) -> DoctorFinding {
    match fs::metadata(path) {
        Ok(metadata) if !metadata.is_dir() => finding(
            code,
            "paths",
            DoctorStatus::Fail,
            format!("{label} is not a directory."),
            Some(path.display().to_string()),
            &[
                "Move the conflicting object and let a normal Norted operation create the directory.",
            ],
        ),
        Ok(_) => match directory_write_access(path) {
            Ok(()) => pass(
                code,
                "paths",
                format!("{label} exists and is available for normal writes."),
                Some(path.display().to_string()),
            ),
            Err(error) => finding(
                code,
                "paths",
                DoctorStatus::Fail,
                format!("{label} is not available for normal required writes."),
                Some(format!("{}: {error}", path.display())),
                &[
                    "Restore directory traversal and write access for the account running Norted Server.",
                ],
            ),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match nearest_existing_directory(path).and_then(directory_write_access) {
                Ok(()) => pass(
                    code,
                    "paths",
                    format!("{label} is absent; this is valid fresh state."),
                    Some(format!("{} (not created by Doctor)", path.display())),
                ),
                Err(error) => finding(
                    code,
                    "paths",
                    DoctorStatus::Fail,
                    format!("{label} is absent and cannot be created by a normal operation."),
                    Some(format!("{}: {error}", path.display())),
                    &[
                        "Restore traversal and write access to the nearest existing parent directory.",
                    ],
                ),
            }
        }
        Err(error) => finding(
            code,
            "paths",
            DoctorStatus::Fail,
            format!("{label} is inaccessible."),
            Some(format!("{}: {error}", path.display())),
            &["Restore directory metadata, traversal, and read access."],
        ),
    }
}

fn nearest_existing_directory(path: &Path) -> Result<&Path, String> {
    let mut candidate = Some(path);
    while let Some(current) = candidate {
        match fs::metadata(current) {
            Ok(metadata) if metadata.is_dir() => return Ok(current),
            Ok(_) => return Err(format!("{} is not a directory", current.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                candidate = current.parent();
            }
            Err(error) => return Err(format!("{}: {error}", current.display())),
        }
    }
    Err("no existing parent directory is available".to_owned())
}

#[cfg(unix)]
fn directory_write_access(path: &Path) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;

    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "path contains an interior NUL byte".to_owned())?;
    // SAFETY: `path` is a valid NUL-terminated byte string and access() does
    // not retain the pointer. W_OK|X_OK observes access without writing.
    if unsafe { libc::access(path.as_ptr(), libc::W_OK | libc::X_OK) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

#[cfg(not(unix))]
fn directory_write_access(path: &Path) -> Result<(), String> {
    directory_write_probe(path)
}

#[cfg(any(not(unix), test))]
fn directory_write_probe(path: &Path) -> Result<(), String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_PROBE_ID: AtomicU64 = AtomicU64::new(0);

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    for _ in 0..16 {
        let probe_id = NEXT_PROBE_ID.fetch_add(1, Ordering::Relaxed);
        let probe_path = path.join(format!(
            ".norted-doctor-write-probe-{}-{timestamp}-{probe_id}",
            std::process::id()
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe_path)
        {
            Ok(probe) => {
                drop(probe);
                return fs::remove_file(&probe_path).map_err(|error| {
                    format!(
                        "Doctor created temporary write probe {} but could not remove it: {error}",
                        probe_path.display()
                    )
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "could not create a temporary write probe in {}: {error}",
                    path.display()
                ));
            }
        }
    }
    Err(format!(
        "could not create a uniquely named temporary write probe in {} after 16 attempts",
        path.display()
    ))
}

fn pass(
    code: impl Into<String>,
    category: impl Into<String>,
    message: impl Into<String>,
    detail: Option<String>,
) -> DoctorFinding {
    DoctorFinding {
        code: code.into(),
        category: category.into(),
        status: DoctorStatus::Pass,
        message: message.into(),
        detail,
        remediation: Vec::new(),
    }
}

fn finding(
    code: impl Into<String>,
    category: impl Into<String>,
    status: DoctorStatus,
    message: impl Into<String>,
    detail: Option<String>,
    remediation: &[&str],
) -> DoctorFinding {
    DoctorFinding {
        code: code.into(),
        category: category.into(),
        status,
        message: message.into(),
        detail,
        remediation: remediation.iter().map(|line| (*line).to_owned()).collect(),
    }
}

fn bounded_detail(detail: impl AsRef<str>) -> String {
    const MAX_CHARS: usize = 800;
    let detail = detail.as_ref().trim().replace(['\r', '\n'], " ");
    if detail.chars().count() <= MAX_CHARS {
        detail
    } else {
        format!("{}…", detail.chars().take(MAX_CHARS).collect::<String>())
    }
}

fn format_bytes(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    format!("{:.1} GiB", bytes as f64 / GIB)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn isolated_paths(root: &Path) -> AppPaths {
        AppPaths {
            config_dir: root.join("config"),
            config_file: root.join("config/config.toml"),
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            cache_dir: root.join("cache"),
            log_dir: root.join("logs"),
            runtimes_dir: root.join("data/runtimes"),
            runtime_cache_dir: root.join("cache/runtime-packs"),
            runtime_selections_file: root.join("data/runtime-selections.json"),
            settings_file: root.join("data/settings.json"),
            settings_lock_file: root.join("data/.settings.lock"),
            model_profiles_file: root.join("data/model-profiles.json"),
            model_profiles_lock_file: root.join("data/.model-profiles.lock"),
        }
    }

    #[tokio::test]
    async fn fresh_doctor_run_creates_no_state() {
        let temporary = TempDir::new().expect("temporary directory");
        let paths = isolated_paths(temporary.path());
        let _first = run_with_paths(paths.clone()).await;
        let _second = run_with_paths(paths).await;

        assert_eq!(
            fs::read_dir(temporary.path())
                .expect("read temporary directory")
                .count(),
            0
        );
    }

    #[test]
    fn summary_counts_warning_as_problem_but_not_failure() {
        let report = DoctorReport::new(vec![
            pass("one", "test", "healthy", None),
            finding(
                "two",
                "test",
                DoctorStatus::Warning,
                "attention",
                None,
                &["review"],
            ),
        ]);
        assert_eq!(report.status, DoctorStatus::Warning);
        assert_eq!(report.summary.problems, 1);
        assert!(!report.has_failures());
    }

    #[test]
    fn remediation_commands_have_real_newline_boundaries() {
        let single_command = AUTH_KEY_REMEDIATION;
        let multiple_commands = format!(
            "Run:\n  norted-server runtimes list\n  norted-server runtimes clear-selection --model {}",
            "example"
        );

        assert_eq!(
            single_command.lines().collect::<Vec<_>>(),
            ["Run:", "  norted-server auth keys create --name <LABEL>"]
        );
        assert_eq!(
            multiple_commands.lines().collect::<Vec<_>>(),
            [
                "Run:",
                "  norted-server runtimes list",
                "  norted-server runtimes clear-selection --model example"
            ]
        );
    }

    #[test]
    fn remediation_newlines_serialize_as_json_escapes() {
        let finding = finding(
            "remediation.newlines",
            "test",
            DoctorStatus::Warning,
            "attention",
            None,
            &[AUTH_KEY_REMEDIATION],
        );

        let json = serde_json::to_string(&finding).expect("serialize finding");
        assert!(json.contains(
            r#""remediation":["Run:\n  norted-server auth keys create --name <LABEL>"]"#
        ));
        assert!(!json.contains("Run:norted-server"));
    }

    #[test]
    fn successful_directory_write_probe_leaves_no_file() {
        let temporary = TempDir::new().expect("temporary directory");

        directory_write_probe(temporary.path()).expect("directory should be writable");

        assert_eq!(
            fs::read_dir(temporary.path())
                .expect("read temporary directory")
                .count(),
            0
        );
    }
}
