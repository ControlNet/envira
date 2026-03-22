mod apt;
mod dnf;
mod pacman;
mod zypper;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::catalog::TargetBackend;

use super::operation::{ExecutionTarget, OperationSpec};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativePackageRecipe {
    pub packages: Vec<String>,
}

#[derive(Debug, Error)]
pub enum BackendMappingError {
    #[error(
        "native backend `{:?}` is not supported by executor package adapters",
        backend
    )]
    UnsupportedBackend { backend: TargetBackend },
}

pub fn build_native_backend_operations(
    backend: TargetBackend,
    recipe: &NativePackageRecipe,
    target: ExecutionTarget,
) -> Result<Vec<OperationSpec>, BackendMappingError> {
    match backend {
        TargetBackend::Apt => Ok(apt::build(recipe, target)),
        TargetBackend::Pacman => Ok(pacman::build(recipe, target)),
        TargetBackend::Dnf => Ok(dnf::build(recipe, target)),
        TargetBackend::Zypper => Ok(zypper::build(recipe, target)),
        other => Err(BackendMappingError::UnsupportedBackend { backend: other }),
    }
}
