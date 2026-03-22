use super::NativePackageRecipe;
use crate::executor::operation::{CommandOperation, ExecutionTarget, OperationSpec};

pub fn build(recipe: &NativePackageRecipe, target: ExecutionTarget) -> Vec<OperationSpec> {
    vec![OperationSpec::Command(
        CommandOperation::new("pacman")
            .with_args(install_args(recipe))
            .with_target(target),
    )]
}

fn install_args(recipe: &NativePackageRecipe) -> Vec<String> {
    let mut args = vec!["-Sy".to_string(), "--noconfirm".to_string()];
    args.extend(recipe.packages.iter().cloned());
    args
}
