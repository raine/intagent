use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

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
}

impl ToolCallResult {
    pub fn allowed(output: String) -> Self {
        Self {
            output,
            denied: false,
        }
    }

    pub fn denied(reason: String) -> Self {
        let mut bytes = reason.into_bytes();
        bytes.truncate(MAX_DENIAL_BYTES);
        let output = String::from_utf8_lossy(&bytes).into_owned();
        Self {
            output,
            denied: true,
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
