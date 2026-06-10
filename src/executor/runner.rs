use std::collections::VecDeque;
use std::env;
use std::io::{self, BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;
use users::get_effective_uid;

use crate::executor::operation::{CommandOperation, ExecutionTarget};
use crate::executor::result::{
    CapturedOutput, CommandEvent, CommandExecution, CommandExecutionSummary, CommandFinishedEvent,
    CommandOutputEvent, CommandStartedEvent, ExecutionDisposition, OutputSummary, StreamKind,
};

#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("failed to spawn `{program}`: {source}")]
    Spawn {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error("`{program}` did not expose a piped {stream:?} handle")]
    MissingPipe { program: String, stream: StreamKind },
    #[error("failed while waiting for `{program}`: {source}")]
    Wait {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error("failed to terminate `{program}` after timeout: {source}")]
    Kill {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error("failed to read {stream:?} for `{program}`: {message}")]
    StreamRead {
        program: String,
        stream: StreamKind,
        message: String,
    },
    #[error("reader thread for {stream:?} on `{program}` terminated unexpectedly")]
    ReaderJoin { program: String, stream: StreamKind },
    #[error(
        "execution target `target_user` for `{program}` requires a non-empty SUDO_USER context"
    )]
    MissingTargetUser { program: String },
}

#[derive(Clone, Debug)]
pub struct CommandRunner {
    tail_line_limit: usize,
    poll_interval: Duration,
}

impl Default for CommandRunner {
    fn default() -> Self {
        Self {
            tail_line_limit: 5,
            poll_interval: Duration::from_millis(10),
        }
    }
}

impl CommandRunner {
    pub fn new(tail_line_limit: usize) -> Self {
        Self {
            tail_line_limit: tail_line_limit.max(1),
            ..Self::default()
        }
    }

    pub fn execute(&self, operation: &CommandOperation) -> Result<CommandExecution, ExecutorError> {
        self.execute_with_events(operation, |_| {})
    }

    pub fn execute_with_events<F>(
        &self,
        operation: &CommandOperation,
        mut on_event: F,
    ) -> Result<CommandExecution, ExecutorError>
    where
        F: FnMut(CommandEvent),
    {
        let prepared = prepare_command(operation)?;
        let mut command = Command::new(&prepared.program);
        command.args(&prepared.args);
        command.envs(&operation.env);
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        if let Some(cwd) = &operation.cwd {
            command.current_dir(cwd);
        }

        let mut child = command.spawn().map_err(|source| ExecutorError::Spawn {
            program: prepared.program.clone(),
            source,
        })?;
        let pid = child.id();
        let started_at = Instant::now();

        on_event(CommandEvent::Started(CommandStartedEvent {
            pid,
            operation: operation.clone(),
        }));

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ExecutorError::MissingPipe {
                program: prepared.program.clone(),
                stream: StreamKind::Stdout,
            })?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ExecutorError::MissingPipe {
                program: prepared.program.clone(),
                stream: StreamKind::Stderr,
            })?;
        let (sender, receiver) = mpsc::channel();

        let stdout_handle = spawn_reader(stdout, StreamKind::Stdout, sender.clone());
        let stderr_handle = spawn_reader(stderr, StreamKind::Stderr, sender);

        let mut stdout_capture = StreamCapture::new(self.tail_line_limit);
        let mut stderr_capture = StreamCapture::new(self.tail_line_limit);
        let mut stdout_closed = false;
        let mut stderr_closed = false;
        let mut timed_out = false;
        let mut exit_status = None;
        let mut stream_error = None;

        loop {
            if exit_status.is_none() {
                if let Some(timeout_ms) = operation.timeout_ms {
                    if started_at.elapsed() >= Duration::from_millis(timeout_ms) {
                        timed_out = true;
                        child.kill().map_err(|source| ExecutorError::Kill {
                            program: prepared.program.clone(),
                            source,
                        })?;
                        exit_status = Some(child.wait().map_err(|source| ExecutorError::Wait {
                            program: prepared.program.clone(),
                            source,
                        })?);
                    }
                }

                if exit_status.is_none() {
                    if let Some(status) =
                        child.try_wait().map_err(|source| ExecutorError::Wait {
                            program: prepared.program.clone(),
                            source,
                        })?
                    {
                        exit_status = Some(status);
                    }
                }
            }

            match recv_internal_event(&receiver, self.poll_interval) {
                Ok(Some(InternalEvent::Line { stream, text })) => match stream {
                    StreamKind::Stdout => {
                        let line_number = stdout_capture.push_line(text.clone());
                        on_event(CommandEvent::Stdout(CommandOutputEvent {
                            text,
                            line_number,
                        }));
                    }
                    StreamKind::Stderr => {
                        let line_number = stderr_capture.push_line(text.clone());
                        on_event(CommandEvent::Stderr(CommandOutputEvent {
                            text,
                            line_number,
                        }));
                    }
                },
                Ok(Some(InternalEvent::Closed(stream))) => match stream {
                    StreamKind::Stdout => stdout_closed = true,
                    StreamKind::Stderr => stderr_closed = true,
                },
                Ok(Some(InternalEvent::ReadError { stream, message })) => {
                    stream_error = Some(ExecutorError::StreamRead {
                        program: prepared.program.clone(),
                        stream,
                        message,
                    });
                    if exit_status.is_none() {
                        let _ = child.kill();
                        exit_status = Some(child.wait().map_err(|source| ExecutorError::Wait {
                            program: prepared.program.clone(),
                            source,
                        })?);
                    }
                }
                Ok(None) => {
                    stdout_closed = true;
                    stderr_closed = true;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    stdout_closed = true;
                    stderr_closed = true;
                }
            }

            if exit_status.is_some() && stdout_closed && stderr_closed {
                break;
            }
        }

        join_reader(stdout_handle, &prepared.program, StreamKind::Stdout)?;
        join_reader(stderr_handle, &prepared.program, StreamKind::Stderr)?;

        if let Some(error) = stream_error {
            return Err(error);
        }

        let exit_code = exit_status.and_then(|status| status.code());
        let duration_ms = duration_ms(started_at.elapsed());
        let stdout = stdout_capture.finish();
        let stderr = stderr_capture.finish();
        let disposition = if timed_out || !exit_status_is_success(exit_code) {
            ExecutionDisposition::Failure
        } else {
            ExecutionDisposition::Success
        };
        let summary = CommandExecutionSummary {
            disposition,
            exit_code,
            timed_out,
            duration_ms,
            stdout: stdout.summary.clone(),
            stderr: stderr.summary.clone(),
            message: summarize(
                operation,
                disposition,
                exit_code,
                timed_out,
                &stdout,
                &stderr,
            ),
        };
        let execution = CommandExecution {
            operation: operation.clone(),
            stdout,
            stderr,
            summary,
        };

        on_event(CommandEvent::Finished(CommandFinishedEvent {
            summary: execution.summary.clone(),
        }));

        Ok(execution)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PreparedCommand {
    program: String,
    args: Vec<String>,
}

fn prepare_command(operation: &CommandOperation) -> Result<PreparedCommand, ExecutorError> {
    prepare_command_for_uid(operation, get_effective_uid())
}

fn prepare_command_for_uid(
    operation: &CommandOperation,
    effective_uid: u32,
) -> Result<PreparedCommand, ExecutorError> {
    match operation.target {
        ExecutionTarget::CurrentProcess => Ok(PreparedCommand {
            program: operation.program.clone(),
            args: operation.args.clone(),
        }),
        ExecutionTarget::System => {
            if effective_uid == 0 {
                Ok(PreparedCommand {
                    program: operation.program.clone(),
                    args: operation.args.clone(),
                })
            } else {
                Ok(PreparedCommand {
                    program: "sudo".to_string(),
                    args: wrap_args(None, operation),
                })
            }
        }
        ExecutionTarget::TargetUser => {
            let target_user = merged_env_value(operation, "SUDO_USER")
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| ExecutorError::MissingTargetUser {
                    program: operation.program.clone(),
                })?;
            Ok(PreparedCommand {
                program: "sudo".to_string(),
                args: wrap_args(Some(target_user), operation),
            })
        }
    }
}

fn wrap_args(target_user: Option<String>, operation: &CommandOperation) -> Vec<String> {
    let mut args = preserved_env_args(operation);
    if let Some(target_user) = target_user {
        args.push("-u".to_string());
        args.push(target_user);
    }
    args.push("--".to_string());
    args.push(operation.program.clone());
    args.extend(operation.args.iter().cloned());
    args
}

fn preserved_env_args(operation: &CommandOperation) -> Vec<String> {
    if operation.env.is_empty() {
        return Vec::new();
    }

    let preserved = operation
        .env
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(",");
    vec![format!("--preserve-env={preserved}")]
}

fn merged_env_value(operation: &CommandOperation, key: &str) -> Option<String> {
    operation
        .env
        .get(key)
        .cloned()
        .or_else(|| env::var(key).ok())
}

fn recv_internal_event(
    receiver: &Receiver<InternalEvent>,
    poll_interval: Duration,
) -> Result<Option<InternalEvent>, RecvTimeoutError> {
    match receiver.recv_timeout(poll_interval) {
        Ok(event) => Ok(Some(event)),
        Err(RecvTimeoutError::Disconnected) => Ok(None),
        Err(other) => Err(other),
    }
}

fn join_reader(
    handle: thread::JoinHandle<()>,
    program: &str,
    stream: StreamKind,
) -> Result<(), ExecutorError> {
    handle.join().map_err(|_| ExecutorError::ReaderJoin {
        program: program.to_string(),
        stream,
    })
}

fn spawn_reader<R>(
    reader: R,
    stream: StreamKind,
    sender: Sender<InternalEvent>,
) -> thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buffered = BufReader::new(reader);
        loop {
            let mut chunk = Vec::new();
            match buffered.read_until(b'\n', &mut chunk) {
                Ok(0) => break,
                Ok(_) => {
                    let text = normalize_output_line(&chunk);
                    if sender.send(InternalEvent::Line { stream, text }).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = sender.send(InternalEvent::ReadError {
                        stream,
                        message: error.to_string(),
                    });
                    let _ = sender.send(InternalEvent::Closed(stream));
                    return;
                }
            }
        }

        let _ = sender.send(InternalEvent::Closed(stream));
    })
}

fn normalize_output_line(chunk: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(chunk).into_owned();
    while text.ends_with('\n') || text.ends_with('\r') {
        text.pop();
    }
    text
}

fn summarize(
    operation: &CommandOperation,
    disposition: ExecutionDisposition,
    exit_code: Option<i32>,
    timed_out: bool,
    stdout: &CapturedOutput,
    stderr: &CapturedOutput,
) -> String {
    if disposition.is_success() {
        return format!("command `{}` exited successfully", operation.program);
    }

    if timed_out {
        return match operation.timeout_ms {
            Some(timeout_ms) => {
                format!(
                    "command `{}` timed out after {timeout_ms}ms",
                    operation.program
                )
            }
            None => format!("command `{}` timed out", operation.program),
        };
    }

    let evidence = if !stderr.summary.tail.is_empty() {
        stderr.summary.tail.join("; ")
    } else if !stdout.summary.tail.is_empty() {
        stdout.summary.tail.join("; ")
    } else if let Some(code) = exit_code {
        format!("exit code {code}")
    } else {
        "terminated without an exit code".to_string()
    };

    format!("command `{}` failed: {evidence}", operation.program)
}

fn exit_status_is_success(exit_code: Option<i32>) -> bool {
    matches!(exit_code, Some(0))
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

enum InternalEvent {
    Line { stream: StreamKind, text: String },
    Closed(StreamKind),
    ReadError { stream: StreamKind, message: String },
}

struct StreamCapture {
    evidence: String,
    line_count: u64,
    byte_count: u64,
    tail: VecDeque<String>,
    tail_line_limit: usize,
}

impl StreamCapture {
    fn new(tail_line_limit: usize) -> Self {
        Self {
            evidence: String::new(),
            line_count: 0,
            byte_count: 0,
            tail: VecDeque::new(),
            tail_line_limit,
        }
    }

    fn push_line(&mut self, text: String) -> u64 {
        if !self.evidence.is_empty() {
            self.evidence.push('\n');
        }
        self.evidence.push_str(&text);
        self.line_count += 1;
        self.byte_count += text.len() as u64;
        self.tail.push_back(text);
        if self.tail.len() > self.tail_line_limit {
            self.tail.pop_front();
        }
        self.line_count
    }

    fn finish(self) -> CapturedOutput {
        CapturedOutput {
            evidence: self.evidence,
            summary: OutputSummary {
                line_count: self.line_count,
                byte_count: self.byte_count,
                tail: self.tail.into_iter().collect(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{prepare_command_for_uid, CommandOperation, ExecutionTarget};

    #[test]
    fn wrapped_system_execution_preserves_declared_env_names_in_sudo_args() {
        let operation = CommandOperation::new("apt")
            .with_args(["update"])
            .with_env([("DEBIAN_FRONTEND", "noninteractive"), ("PATH", "/tmp/bin")])
            .with_target(ExecutionTarget::System);

        let prepared =
            prepare_command_for_uid(&operation, 1000).expect("system target should prepare");

        assert_eq!(prepared.program, "sudo");
        assert_eq!(
            prepared.args,
            vec![
                "--preserve-env=DEBIAN_FRONTEND,PATH".to_string(),
                "--".to_string(),
                "apt".to_string(),
                "update".to_string(),
            ]
        );
    }

    #[test]
    fn wrapped_target_user_execution_preserves_declared_env_names_in_sudo_args() {
        let operation = CommandOperation::new("python3")
            .with_args(["-c", "print('hi')"])
            .with_env([
                ("ENVIRA_EXECUTOR_TEST", "available"),
                ("PATH", "/tmp/bin"),
                ("SUDO_USER", "alice"),
            ])
            .with_target(ExecutionTarget::TargetUser);

        let prepared =
            prepare_command_for_uid(&operation, 0).expect("target-user target should prepare");

        assert_eq!(prepared.program, "sudo");
        assert_eq!(
            prepared.args,
            vec![
                "--preserve-env=ENVIRA_EXECUTOR_TEST,PATH,SUDO_USER".to_string(),
                "-u".to_string(),
                "alice".to_string(),
                "--".to_string(),
                "python3".to_string(),
                "-c".to_string(),
                "print('hi')".to_string(),
            ]
        );
    }
}
