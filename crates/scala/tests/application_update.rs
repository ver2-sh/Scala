//! Offline CLI contracts: no release server, installer or application startup.
#[cfg(target_os = "linux")]
mod tests {
    use std::{fs, process::Command};

    fn command(root: &std::path::Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_scala"));
        command
            .env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_STATE_HOME", root.join("state"));
        for name in [
            "SCALA_INSTALLER_GITHUB_BASE_URL",
            "SCALA_INSTALLER_GHE_BASE_URL",
            "SCALA_DOWNLOAD_URL",
            "INSTALLER_DOWNLOAD_URL",
            "SCALA_UNMANAGED_INSTALL",
            "SCALA_DISABLE_UPDATE",
            "SCALA_GITHUB_TOKEN",
            "AXOUPDATER_CONFIG_PATH",
            "AXOUPDATER_CONFIG_WORKING_DIR",
        ] {
            command.env_remove(name);
        }
        command
    }

    #[test]
    fn inherited_installer_overrides_are_rejected_without_network_or_config_writes() {
        let dir = tempfile::tempdir().unwrap();
        for key in [
            "SCALA_INSTALLER_GITHUB_BASE_URL",
            "SCALA_INSTALLER_GHE_BASE_URL",
            "SCALA_DOWNLOAD_URL",
            "INSTALLER_DOWNLOAD_URL",
            "SCALA_UNMANAGED_INSTALL",
            "SCALA_DISABLE_UPDATE",
            "SCALA_GITHUB_TOKEN",
            "AXOUPDATER_CONFIG_WORKING_DIR",
            "AXOUPDATER_CONFIG_PATH",
        ] {
            let output = command(dir.path())
                .env(key, "synthetic-test-value")
                .args(["--json", "update", "--check"])
                .output()
                .unwrap();
            assert!(!output.status.success(), "{key}");
            assert!(output.stdout.is_empty(), "{key}");
            let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
            let message = error["error"]["message"].as_str().unwrap();
            assert!(message.contains(key), "{key}: {message}");
            assert!(!message.contains("synthetic-test-value"));
        }
        for path in ["config", "data", "state", "cache"] {
            assert!(!dir.path().join(path).exists(), "{path}");
        }
    }

    #[test]
    fn json_conflict_is_one_structured_error_without_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let output = command(dir.path())
            .args(["update", "--json", "--check", "--yes"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("cannot be used with")
        );
        assert!(!dir.path().join("data").exists());
    }

    #[test]
    fn explicit_check_contends_fails_closed_and_ignores_broken_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config/scala");
        fs::create_dir_all(&config).unwrap();
        fs::write(config.join("config.toml"), "invalid = [").unwrap();
        let cache = dir.path().join("cache/scala");
        fs::create_dir_all(&cache).unwrap();
        fs::write(
            cache.join("app-update.json"),
            r#"{"checked":0,"latest":"999.0.0","error":null}"#,
        )
        .unwrap();
        let lock = fs::File::create(cache.join("app-update.lock")).unwrap();
        fs2::FileExt::lock_exclusive(&lock).unwrap();
        let output = command(dir.path())
            .args(["--json", "update", "--check"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        let message = error["error"]["message"].as_str().unwrap();
        assert!(message.contains("another update check"), "{message}");
        assert!(!message.contains("999.0.0"));
        assert!(!dir.path().join("data").exists());
        assert!(!dir.path().join("state").exists());
        assert_eq!(
            fs::read_to_string(config.join("config.toml")).unwrap(),
            "invalid = ["
        );
    }
}
