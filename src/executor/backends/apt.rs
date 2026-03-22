use super::NativePackageRecipe;
use crate::executor::operation::{CommandOperation, ExecutionTarget, OperationSpec};

pub fn build(recipe: &NativePackageRecipe, target: ExecutionTarget) -> Vec<OperationSpec> {
    vec![
        OperationSpec::Command(
            CommandOperation::new("apt")
                .with_args(["update"])
                .with_env([("DEBIAN_FRONTEND", "noninteractive")])
                .with_target(target),
        ),
        OperationSpec::Command(
            CommandOperation::new("apt")
                .with_args(install_args(recipe))
                .with_env([("DEBIAN_FRONTEND", "noninteractive")])
                .with_target(target),
        ),
    ]
}

fn install_args(recipe: &NativePackageRecipe) -> Vec<String> {
    let mut args = vec!["install".to_string(), "-y".to_string()];
    args.extend(recipe.packages.iter().cloned());
    args
}
