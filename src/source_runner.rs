use std::collections::HashMap;
use std::env;
use std::io;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

use crate::application_log::{APPLICATION_LOG_PATH_ENV, application_log_path, redact_log_text};
use crate::config::{IntakeConfig, SourceConfig, expand_path};
use crate::database::{DatabaseError, IntakeDatabase};
use crate::protocol::{
    MAX_STANDARD_INPUT_BYTES, PROTOCOL_VERSION, PollRequest, parse_poll_response,
};

pub const SOURCE_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
pub const SOURCE_DIAGNOSTIC_LIMIT: usize = 65_536;
const TERMINATION_GRACE: Duration = Duration::from_secs(1);

#[derive(Debug, Error)]
pub enum SourceRunnerError {
    #[error("source request exceeded {MAX_STANDARD_INPUT_BYTES} bytes")]
    RequestTooLarge,
    #[error("source failed to start: {0}")]
    Spawn(String),
    #[error("source poll timed out")]
    Timeout,
    #[error("source exited {status}: {diagnostics}")]
    Exit { status: String, diagnostics: String },
    #[error("source standard {stream} exceeded {limit} bytes")]
    OutputTooLarge { stream: &'static str, limit: usize },
    #[error("source standard stream error: {0}")]
    Stream(String),
    #[error("source stdout is not valid UTF-8: {0}")]
    Utf8(String),
    #[error("source stdout is not one JSON response: {0}")]
    Json(String),
    #[error("source response failed validation: {0}")]
    Schema(String),
    #[error("source returned {actual} items for a limit of {limit}")]
    ItemLimit { actual: usize, limit: usize },
    #[error("source database operation failed: {0}")]
    Database(#[from] DatabaseError),
    #[error("{0}")]
    Poll(String),
    #[error("source poll failed and its failure could not be recorded: {poll}; {database}")]
    FailureRecording { poll: String, database: String },
}

pub async fn poll_source(
    source: &SourceConfig,
    config: &IntakeConfig,
    database: &IntakeDatabase,
    now: DateTime<Utc>,
) -> Result<usize, SourceRunnerError> {
    let environment = source_environment(source, config);
    let secret_values = source
        .environment
        .iter()
        .filter_map(|name| environment.get(name).cloned())
        .collect::<Vec<_>>();
    let result = run_source_process(source, config, database, now, environment).await;
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            let message = redact_source_error(&error.to_string(), &secret_values);
            match database
                .source_failed(source.name.clone(), message.clone(), now)
                .await
            {
                Ok(()) => Err(SourceRunnerError::Poll(message)),
                Err(database_error) => Err(SourceRunnerError::FailureRecording {
                    poll: message,
                    database: redact_source_error(&database_error.to_string(), &secret_values),
                }),
            }
        }
    }
}

async fn run_source_process(
    source: &SourceConfig,
    config: &IntakeConfig,
    database: &IntakeDatabase,
    now: DateTime<Utc>,
    environment: HashMap<String, String>,
) -> Result<usize, SourceRunnerError> {
    let request = PollRequest {
        protocol_version: PROTOCOL_VERSION,
        source: source.name.clone(),
        checkpoint: database
            .readers()
            .source_checkpoint(source.name.clone())
            .await?,
        now: crate::database::timestamp(now),
        item_limit: source.item_limit,
        options: request_options(source, config),
    };
    request
        .validate()
        .map_err(|error| SourceRunnerError::Schema(error.to_string()))?;
    let input = serde_json::to_vec(&request)
        .map_err(|error| SourceRunnerError::Schema(error.to_string()))?;
    if input.len() as u64 > MAX_STANDARD_INPUT_BYTES {
        return Err(SourceRunnerError::RequestTooLarge);
    }

    let mut command = Command::new(&source.command);
    command
        .args(&source.args)
        .env_clear()
        .envs(environment)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| SourceRunnerError::Spawn(error.to_string()))?;
    let pid = child
        .id()
        .ok_or_else(|| SourceRunnerError::Spawn("source process has no process ID".into()))?
        as i32;
    tracing::debug!(
        target: "intake::source_runner",
        source = source.name,
        pid,
        "source process started"
    );
    let mut guard = ProcessGroupGuard::new(pid);

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| SourceRunnerError::Spawn("source stdin is unavailable".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| SourceRunnerError::Spawn("source stdout is unavailable".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| SourceRunnerError::Spawn("source stderr is unavailable".into()))?;

    let mut input_task = tokio::spawn(write_input(stdin, input));
    let mut stdout_task = tokio::spawn(read_bounded(stdout, SOURCE_OUTPUT_LIMIT, "output"));
    let mut stderr_task =
        tokio::spawn(read_bounded(stderr, SOURCE_DIAGNOSTIC_LIMIT, "diagnostics"));
    let process_result = supervise_io(
        &mut child,
        pid,
        source.timeout_seconds,
        &mut input_task,
        &mut stdout_task,
        &mut stderr_task,
    )
    .await;
    let (status, stdout, stderr) = match process_result {
        Ok(result) => result,
        Err(error) => {
            input_task.abort();
            stdout_task.abort();
            stderr_task.abort();
            terminate_and_reap(&mut child, pid).await;
            return Err(error);
        }
    };
    guard.disarm();

    if !status.success() {
        return Err(SourceRunnerError::Exit {
            status: exit_status(&status),
            diagnostics: diagnostic_text(&stderr),
        });
    }
    let stdout = String::from_utf8(stdout)
        .map_err(|error| SourceRunnerError::Utf8(error.utf8_error().to_string()))?;
    let value: Value = serde_json::from_str(stdout.trim())
        .map_err(|error| SourceRunnerError::Json(error.to_string()))?;
    let encoded =
        serde_json::to_vec(&value).map_err(|error| SourceRunnerError::Schema(error.to_string()))?;
    let response = parse_poll_response(&encoded)
        .map_err(|error| SourceRunnerError::Schema(error.to_string()))?;
    if response.items.len() > source.item_limit {
        return Err(SourceRunnerError::ItemLimit {
            actual: response.items.len(),
            limit: source.item_limit,
        });
    }
    database
        .source_succeeded(
            source.name.clone(),
            response.checkpoint,
            response.items,
            now,
        )
        .await
        .map_err(SourceRunnerError::Database)
}

async fn supervise_io(
    child: &mut Child,
    pid: i32,
    timeout_seconds: u64,
    input_task: &mut JoinHandle<Result<(), io::Error>>,
    stdout_task: &mut JoinHandle<Result<Vec<u8>, SourceRunnerError>>,
    stderr_task: &mut JoinHandle<Result<Vec<u8>, SourceRunnerError>>,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), SourceRunnerError> {
    let timeout = tokio::time::sleep(Duration::from_secs(timeout_seconds));
    tokio::pin!(timeout);
    let mut status: Option<ExitStatus> = None;
    let mut input_result: Option<Result<(), String>> = None;
    let mut stdout: Option<Vec<u8>> = None;
    let mut stderr: Option<Vec<u8>> = None;

    loop {
        if status.is_some() && input_result.is_some() && stdout.is_some() && stderr.is_some() {
            let status = status.take().expect("checked source status");
            if !status.success() {
                return Ok((
                    status,
                    stdout.take().expect("checked source stdout"),
                    stderr.take().expect("checked source stderr"),
                ));
            }
            input_result
                .take()
                .expect("checked source input")
                .map_err(SourceRunnerError::Stream)?;
            return Ok((
                status,
                stdout.take().expect("checked source stdout"),
                stderr.take().expect("checked source stderr"),
            ));
        }
        tokio::select! {
            biased;
            result = child.wait(), if status.is_none() => {
                status = Some(result.map_err(|error| SourceRunnerError::Stream(error.to_string()))?);
            }
            result = &mut *input_task, if input_result.is_none() => {
                input_result = Some(result
                    .map_err(|error| SourceRunnerError::Stream(error.to_string()))?
                    .map_err(|error| error.to_string()));
            }
            result = &mut *stdout_task, if stdout.is_none() => {
                stdout = Some(result
                    .map_err(|error| SourceRunnerError::Stream(error.to_string()))??);
            }
            result = &mut *stderr_task, if stderr.is_none() => {
                stderr = Some(result
                    .map_err(|error| SourceRunnerError::Stream(error.to_string()))??);
            }
            () = &mut timeout => {
                terminate_group(pid, libc::SIGTERM);
                return Err(SourceRunnerError::Timeout);
            }
        }
    }
}

async fn write_input(
    mut stdin: tokio::process::ChildStdin,
    input: Vec<u8>,
) -> Result<(), io::Error> {
    stdin.write_all(&input).await?;
    stdin.shutdown().await
}

async fn read_bounded<R>(
    reader: R,
    limit: usize,
    stream: &'static str,
) -> Result<Vec<u8>, SourceRunnerError>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| SourceRunnerError::Stream(error.to_string()))?;
    if bytes.len() > limit {
        return Err(SourceRunnerError::OutputTooLarge { stream, limit });
    }
    Ok(bytes)
}

fn request_options(source: &SourceConfig, config: &IntakeConfig) -> Map<String, Value> {
    let mut options = Map::from_iter([(
        "project_roots".into(),
        Value::Array(
            config
                .project_roots
                .iter()
                .cloned()
                .map(Value::String)
                .collect(),
        ),
    )]);
    options.extend(source.options.clone());
    options
}

fn source_environment(source: &SourceConfig, config: &IntakeConfig) -> HashMap<String, String> {
    let mut environment = HashMap::new();
    for name in &source.environment {
        if let Ok(value) = env::var(name) {
            environment.insert(name.clone(), value);
        }
    }
    environment.extend([
        ("PATH".into(), config.commands.path.join(":")),
        ("HOME".into(), env::var("HOME").unwrap_or_default()),
        ("LANG".into(), "C.UTF-8".into()),
        ("LC_ALL".into(), "C.UTF-8".into()),
        ("NO_COLOR".into(), "1".into()),
    ]);
    if let Ok(logs) = expand_path(&config.state.logs) {
        environment.insert(
            APPLICATION_LOG_PATH_ENV.into(),
            application_log_path(logs).to_string_lossy().into_owned(),
        );
    }
    environment
}

fn redact_source_error(message: &str, secret_values: &[String]) -> String {
    let mut values = secret_values
        .iter()
        .filter(|value| !value.is_empty())
        .cloned()
        .collect::<Vec<_>>();
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    values.dedup();
    let mut redacted = message.to_string();
    for value in values {
        redacted = redacted.replace(&value, "[REDACTED]");
    }
    redact_log_text(&redacted)
}

fn diagnostic_text(bytes: &[u8]) -> String {
    let value = String::from_utf8_lossy(bytes);
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "no diagnostics".into()
    } else {
        trimmed.into()
    }
}

fn exit_status(status: &ExitStatus) -> String {
    status
        .code()
        .map_or_else(|| status.to_string(), |code| code.to_string())
}

async fn terminate_and_reap(child: &mut Child, pid: i32) {
    terminate_group(pid, libc::SIGTERM);
    tokio::time::sleep(TERMINATION_GRACE).await;
    terminate_group(pid, libc::SIGKILL);
    let _ = child.wait().await;
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn terminate_group(pid: i32, signal: i32) {
    unsafe {
        libc::kill(-pid, signal);
    }
}

#[cfg(not(unix))]
fn terminate_group(_pid: i32, _signal: i32) {}

struct ProcessGroupGuard {
    pid: i32,
    armed: bool,
}

impl ProcessGroupGuard {
    fn new(pid: i32) -> Self {
        Self { pid, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if self.armed {
            terminate_group(self.pid, libc::SIGKILL);
        }
    }
}
