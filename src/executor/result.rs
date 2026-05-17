use serde::{Deserialize, Serialize};

use crate::executor::operation::CommandOperation;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Pending,
    Running,
    Success,
    Failure,
    Skipped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionDisposition {
    Success,
    Failure,
    Skipped,
}

impl ExecutionDisposition {
    pub fn is_success(self) -> bool {
        matches!(self, Self::Success)
    }

    pub fn is_failure(self) -> bool {
        matches!(self, Self::Failure)
    }

    pub fn is_skipped(self) -> bool {
        matches!(self, Self::Skipped)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OutputSummary {
    pub line_count: u64,
    pub byte_count: u64,
    pub tail: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapturedOutput {
    pub evidence: String,
    pub summary: OutputSummary,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandExecutionSummary {
    pub disposition: ExecutionDisposition,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub stdout: OutputSummary,
    pub stderr: OutputSummary,
    pub message: String,
}

impl CommandExecutionSummary {
    pub fn state(&self) -> OperationState {
        match self.disposition {
            ExecutionDisposition::Success => OperationState::Success,
            ExecutionDisposition::Failure => OperationState::Failure,
            ExecutionDisposition::Skipped => OperationState::Skipped,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandExecution {
    pub operation: CommandOperation,
    pub stdout: CapturedOutput,
    pub stderr: CapturedOutput,
    pub summary: CommandExecutionSummary,
}

impl CommandExecution {
    pub fn disposition(&self) -> ExecutionDisposition {
        self.summary.disposition
    }

    pub fn state(&self) -> OperationState {
        self.summary.state()
    }

    pub fn succeeded(&self) -> bool {
        self.disposition().is_success()
    }

    pub fn failed(&self) -> bool {
        self.disposition().is_failure()
    }

    pub fn skipped(&self) -> bool {
        self.disposition().is_skipped()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum CommandEvent {
    Started(CommandStartedEvent),
    Stdout(CommandOutputEvent),
    Stderr(CommandOutputEvent),
    Finished(CommandFinishedEvent),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandStartedEvent {
    pub pid: u32,
    pub operation: CommandOperation,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandOutputEvent {
    pub text: String,
    pub line_number: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandFinishedEvent {
    pub summary: CommandExecutionSummary,
}
