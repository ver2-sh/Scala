//! Shared dispatch; adapters continue to own the selected dependency policy.
use super::*;
use crate::linux_dependencies::LinuxDependencyRecipe;
use std::{future::Future, pin::Pin};
type NativeFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, RuntimeInstallError>> + Send + 'a>>;
pub(super) enum Prepared {
    Windows(managed_source::Prepared),
    Linux(linux_dependencies::LinuxPrepared),
}
impl RuntimeInstaller {
    pub(super) fn linux_source_recipe(
        &self,
        plan: &RuntimeSourceBuildPlan,
    ) -> Result<Option<LinuxDependencyRecipe>, RuntimeInstallError> {
        if windows_source::is_msvc_plan(plan) {
            return Ok(None);
        }
        let Some(authority) = self.authorities.get(&plan.source.source_provider) else {
            return Ok(None);
        };
        let Some(adapter) = self.registry.get(&authority.engine_id) else {
            return Ok(None);
        };
        adapter
            .managed_linux_source_dependencies(plan)
            .map_err(RuntimeInstallError::InvalidMetadata)
    }
    pub(super) fn prepare_native_dependencies<'a>(
        &'a self,
        available: &'a AvailableRuntime,
        plan: &'a RuntimeSourceBuildPlan,
    ) -> NativeFuture<'a, Option<Prepared>> {
        Box::pin(async move {
            if let Some(recipe) = self.linux_source_recipe(plan)? {
                let prepared =
                    Box::pin(self.prepare_linux_dependencies(available, &recipe)).await?;
                prepared.validate_plan(plan).await?;
                return Ok(Some(Prepared::Linux(prepared)));
            }
            self.prepare_managed_source(available, plan)
                .await
                .map(|p| p.map(Prepared::Windows))
        })
    }
}
impl Prepared {
    pub(super) fn checked(
        &self,
        plan: &RuntimeSourceBuildPlan,
    ) -> Result<CheckedSourceBuildPrerequisites, RuntimeInstallError> {
        match self {
            Self::Windows(p) => p.checked(plan),
            Self::Linux(p) => p.checked(plan),
        }
    }
    pub(super) fn stage_runtime_files<'a>(
        &'a self,
        root: &'a Path,
        entrypoint: &'a Path,
        toolchain: &'a mut RuntimeSourceBuildToolchain,
    ) -> NativeFuture<'a, ()> {
        Box::pin(async move {
            match self {
                Self::Windows(p) => p.stage_runtime_files(root, entrypoint, toolchain).await,
                Self::Linux(p) => p.stage_runtime_files(root, toolchain).await,
            }
        })
    }
}
pub(super) fn run<'a>(
    prepared: Option<&'a Prepared>,
    label: &'a str,
    program: &'a str,
    args: &'a [&'a str],
    cwd: Option<&'a Path>,
    rejected: &'a [&'a str],
) -> NativeFuture<'a, ()> {
    Box::pin(async move {
        match prepared {
            Some(Prepared::Windows(p)) => {
                managed_source::run(Some(p), label, program, args, cwd, rejected).await
            }
            Some(Prepared::Linux(p)) => p.run(program, args, cwd).await,
            None => managed_source::run(None, label, program, args, cwd, rejected).await,
        }
    })
}
pub(super) fn text<'a>(
    prepared: Option<&'a Prepared>,
    program: &'a str,
    args: &'a [&'a str],
    cwd: Option<&'a Path>,
) -> NativeFuture<'a, String> {
    Box::pin(async move {
        match prepared {
            Some(Prepared::Windows(p)) => managed_source::text(Some(p), program, args, cwd).await,
            Some(Prepared::Linux(p)) => p.text(program, args, cwd).await,
            None => managed_source::text(None, program, args, cwd).await,
        }
    })
}
