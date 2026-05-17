use super::operation::{CommandOperation, ExecutionTarget, OperationSpec};
pub fn plan_shell_operations(
    shell: &str,
    command: &str,
    target: ExecutionTarget,
) -> Vec<OperationSpec> {
    vec![CommandOperation::shell(shell, command)
        .with_target(target)
        .into()]
}
