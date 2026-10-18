//! Immutable, verified compiler binding for the Python runtime's child only.
use super::*;
impl LinuxPrepared {
    pub(in crate::installer) async fn write_python_binding(
        &self,
        root: &Path,
    ) -> Result<(), RuntimeInstallError> {
        let mut environment = BTreeMap::new();
        self.add_jit_environment(&mut environment)?;
        environment.insert("PATH".into(), self.environment["PATH"].clone());
        let binding = serde_json::json!({"schema_version": 1, "reference": self.reference, "environment": environment});
        publish(
            &root.join("build-toolchain.json"),
            &serde_json::to_vec_pretty(&binding).map_err(io)?,
        )?;
        publish(
            &root.join("linux-dependencies.json"),
            &serde_json::to_vec_pretty(&self.reference).map_err(io)?,
        )?;
        Ok(())
    }
}
