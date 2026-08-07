use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use rig_core::message::ToolCall;
use serde::Deserialize;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use super::command_policy::CommandPolicy;
use super::read_policy::{ReadInput, ReadPolicy};
use crate::database::IntakeDatabase;
use crate::project_registry::{replace_project_registry, validate_project_registry_write};

pub use super::process::supervise_process;

const MAX_DENIAL_BYTES: usize = 512;

#[derive(Clone, Debug)]
pub struct CountingTool {
    name: &'static str,
    allowed_value: &'static str,
    executions: Arc<AtomicUsize>,
}

impl CountingTool {
    pub fn new(name: &'static str, allowed_value: &'static str) -> Self {
        Self {
            name,
            allowed_value,
            executions: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn call(&self, value: &str) -> ToolCallResult {
        if value != self.allowed_value {
            return ToolCallResult::denied(format!(
                "{} denied value that failed its enforcement policy",
                self.name
            ));
        }
        self.executions.fetch_add(1, Ordering::SeqCst);
        ToolCallResult::allowed(format!("{} executed", self.name))
    }

    pub fn executions(&self) -> usize {
        self.executions.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolCallResult {
    pub output: String,
    pub denied: bool,
    pub failed: bool,
}

impl ToolCallResult {
    pub fn allowed(output: String) -> Self {
        Self {
            output,
            denied: false,
            failed: false,
        }
    }

    pub fn failed(output: String) -> Self {
        Self {
            output,
            denied: false,
            failed: true,
        }
    }

    pub fn denied(reason: String) -> Self {
        let mut bytes = reason.into_bytes();
        bytes.truncate(MAX_DENIAL_BYTES);
        let output = String::from_utf8_lossy(&bytes).into_owned();
        Self {
            output,
            denied: true,
            failed: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RecordingExecutableTools {
    executable: Arc<std::path::PathBuf>,
    executions: Arc<AtomicUsize>,
}

impl RecordingExecutableTools {
    pub fn new(executable: impl Into<std::path::PathBuf>) -> Result<Self> {
        let executable = executable.into();
        let metadata = std::fs::metadata(&executable)
            .with_context(|| format!("inspect recording executable {}", executable.display()))?;
        if !executable.is_absolute() || !metadata.is_file() {
            bail!("recording executable must be an absolute regular file");
        }
        Ok(Self {
            executable: Arc::new(executable),
            executions: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub fn executions(&self) -> usize {
        self.executions.load(Ordering::SeqCst)
    }

    pub async fn call(
        &self,
        name: &str,
        arguments: &serde_json::Value,
        cancellation: CancellationToken,
    ) -> ToolCallResult {
        if !matches!(name, "bash" | "read" | "write") {
            return ToolCallResult::denied("tool is outside the intake capability set".to_string());
        }
        let mut command = Command::new(self.executable.as_ref());
        command
            .arg(name)
            .arg(arguments.to_string())
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = match command.spawn() {
            Ok(child) => {
                self.executions.fetch_add(1, Ordering::SeqCst);
                child
            }
            Err(error) => {
                return ToolCallResult::denied(format!("recording executable failed: {error}"));
            }
        };
        let output = tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                return ToolCallResult::denied("recording executable canceled".to_string());
            }
            output = child.wait_with_output() => output,
        };
        match output {
            Ok(output) if output.status.success() => {
                let mut bytes = output.stdout;
                bytes.truncate(4096);
                ToolCallResult::allowed(String::from_utf8_lossy(&bytes).into_owned())
            }
            Ok(output) => ToolCallResult::denied(format!(
                "recording executable exited with {}",
                output.status
            )),
            Err(error) => ToolCallResult::denied(format!("recording executable failed: {error}")),
        }
    }
}

#[derive(Clone)]
pub struct ProductionTools {
    command: Arc<CommandPolicy>,
    read: Arc<ReadPolicy>,
    database: IntakeDatabase,
    event_id: i64,
    default_cwd: Arc<PathBuf>,
    registry_path: Arc<PathBuf>,
    project_roots: Arc<Vec<String>>,
    recording_failed: Arc<AtomicBool>,
}

impl ProductionTools {
    pub fn new(
        command: Arc<CommandPolicy>,
        read: ReadPolicy,
        database: IntakeDatabase,
        event_id: i64,
        default_cwd: PathBuf,
        registry_path: PathBuf,
        project_roots: Vec<String>,
    ) -> Self {
        Self {
            command,
            read: Arc::new(read),
            database,
            event_id,
            default_cwd: Arc::new(default_cwd),
            registry_path: Arc::new(registry_path),
            project_roots: Arc::new(project_roots),
            recording_failed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn recording_failed(&self) -> bool {
        self.recording_failed.load(Ordering::SeqCst)
    }

    pub async fn authorize(&self, call: &ToolCall) -> Result<()> {
        match call.function.name.as_str() {
            "bash" => {
                let input: BashInput = parse_arguments(call)?;
                self.command
                    .parse_and_authorize(&input.command, self.cwd(input.cwd.as_deref()))?;
            }
            "read" => {
                let input: ReadArguments = parse_arguments(call)?;
                self.read
                    .authorize(&input.into_policy_input(), &self.default_cwd)?;
            }
            "write" => {
                let input: WriteInput = parse_arguments(call)?;
                validate_project_registry_write(
                    Path::new(&input.path),
                    &input.content,
                    &self.registry_path,
                    &self.project_roots,
                )
                .await?;
            }
            _ => bail!("tool is outside the intake capability set"),
        }
        Ok(())
    }

    pub async fn execute(
        &self,
        call: &ToolCall,
        cancellation: CancellationToken,
    ) -> ToolCallResult {
        match call.function.name.as_str() {
            "bash" => self.execute_bash(call, cancellation).await,
            "read" => self.execute_read(call),
            "write" => self.execute_write(call).await,
            _ => ToolCallResult::denied("tool is outside the intake capability set".into()),
        }
    }

    async fn execute_bash(
        &self,
        call: &ToolCall,
        cancellation: CancellationToken,
    ) -> ToolCallResult {
        let input: BashInput = match parse_arguments(call) {
            Ok(input) => input,
            Err(error) => return self.denied(error),
        };
        let cwd = self.cwd(input.cwd.as_deref()).to_path_buf();
        let result = match self
            .command
            .execute(&input.command, &cwd, cancellation, input.stdin.as_deref())
            .await
        {
            Ok(result) => result,
            Err(error) => return self.denied(error),
        };
        let combined = [
            format!("exit code: {}", result.exit_code),
            if result.stdout.is_empty() {
                "stdout: (empty)".into()
            } else {
                format!("stdout:\n{}", result.stdout)
            },
            if result.stderr.is_empty() {
                "stderr: (empty)".into()
            } else {
                format!("stderr:\n{}", result.stderr)
            },
            if result.truncated {
                "output was truncated".into()
            } else {
                String::new()
            },
        ]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
        if self
            .database
            .record_command(
                self.event_id,
                input.command,
                result.exit_code,
                combined.clone(),
                Utc::now(),
            )
            .await
            .is_err()
        {
            self.recording_failed.store(true, Ordering::SeqCst);
        }
        if result.exit_code == 0 {
            ToolCallResult::allowed(combined)
        } else {
            ToolCallResult::failed(combined)
        }
    }

    fn execute_read(&self, call: &ToolCall) -> ToolCallResult {
        let input: ReadArguments = match parse_arguments(call) {
            Ok(input) => input,
            Err(error) => return self.denied(error),
        };
        match self
            .read
            .read(&input.into_policy_input(), &self.default_cwd)
        {
            Ok(result) => ToolCallResult::allowed(result.text),
            Err(error) => self.denied(error),
        }
    }

    async fn execute_write(&self, call: &ToolCall) -> ToolCallResult {
        let input: WriteInput = match parse_arguments(call) {
            Ok(input) => input,
            Err(error) => return self.denied(error),
        };
        match replace_project_registry(
            Path::new(&input.path),
            &input.content,
            &self.registry_path,
            &self.project_roots,
        )
        .await
        {
            Ok(()) => ToolCallResult::allowed("project registry updated".into()),
            Err(error) => self.denied(error),
        }
    }

    fn cwd<'a>(&'a self, requested: Option<&'a Path>) -> &'a Path {
        requested.unwrap_or(&self.default_cwd)
    }

    fn denied(&self, error: impl std::fmt::Display) -> ToolCallResult {
        ToolCallResult::denied(self.command.filter(&error.to_string()))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BashInput {
    command: String,
    cwd: Option<PathBuf>,
    stdin: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArguments {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

impl ReadArguments {
    fn into_policy_input(self) -> ReadInput {
        ReadInput {
            path: self.path,
            offset: self.offset,
            limit: self.limit,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteInput {
    path: String,
    content: String,
}

fn parse_arguments<T: for<'de> Deserialize<'de>>(call: &ToolCall) -> Result<T> {
    serde_json::from_value(call.function.arguments.clone())
        .with_context(|| format!("invalid {} tool arguments", call.function.name))
}

#[derive(Clone, Debug)]
pub struct CountingTools {
    tools: Arc<Mutex<BTreeMap<&'static str, CountingTool>>>,
}

impl Default for CountingTools {
    fn default() -> Self {
        let tools = [
            CountingTool::new("bash", "allowed"),
            CountingTool::new("read", "allowed"),
            CountingTool::new("write", "allowed"),
        ]
        .into_iter()
        .map(|tool| (tool.name, tool))
        .collect();
        Self {
            tools: Arc::new(Mutex::new(tools)),
        }
    }
}

impl CountingTools {
    pub fn call(&self, name: &str, value: &str) -> ToolCallResult {
        let tools = match self.tools.lock() {
            Ok(tools) => tools,
            Err(poisoned) => poisoned.into_inner(),
        };
        match tools.get(name) {
            Some(tool) => tool.call(value),
            None => ToolCallResult::denied(format!("tool `{name}` is unavailable")),
        }
    }

    pub fn executions(&self, name: &str) -> usize {
        let tools = match self.tools.lock() {
            Ok(tools) => tools,
            Err(poisoned) => poisoned.into_inner(),
        };
        tools.get(name).map_or(0, CountingTool::executions)
    }
}
