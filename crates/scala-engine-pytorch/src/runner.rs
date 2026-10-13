//! Independently qualified model-specific serving contracts. Adding a runner
//! requires its own source/dependency, artifact, settings and API qualification.
use super::*;
pub(super) const IMAJEV_RUNNER: &str = "imajev-readout-v1";
pub(super) const IMAJEV_SOURCE: &str = "91729bff7a806187324e287fd0d730839dc32026";
pub(super) const IMAJEV_BASE: &str = "Qwen/Qwen3.5-4B";
pub(super) const IMAJEV_BASE_REVISION: &str = "851bf6e806efd8d0a36b00ddf55e13ccb7b8cd0a";
pub(super) fn qualify_artifact(bundle: &DecisionBundle) -> Result<(), EngineError> {
    let base = bundle
        .sources
        .get("base")
        .ok_or_else(|| invalid("Missing trained base binding"))?;
    if bundle.backend != "torch-readout"
        || base.repository != IMAJEV_BASE
        || base.revision != IMAJEV_BASE_REVISION
    {
        return Err(invalid(
            "The Imajev runner requires its original trained Qwen base revision; a different model requires independent runner qualification",
        ));
    }
    Ok(())
}
pub(super) fn qualify_probe(probe: &Probe) -> bool {
    probe.runner_id.as_deref() == Some(IMAJEV_RUNNER)
        && probe.runner_source_revision.as_deref() == Some(IMAJEV_SOURCE)
}
