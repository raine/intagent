use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

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

pub async fn supervise_process<I, S>(
    program: &Path,
    arguments: I,
    cancellation: CancellationToken,
    termination_grace: Duration,
) -> Result<std::process::ExitStatus>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(program);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    configure_process_group(&mut command);

    let mut child = command.spawn().context("spawn supervised process")?;
    let pid = child.id().context("supervised process has no process ID")? as i32;
    let mut guard = ProcessGroupGuard::new(pid);

    tokio::select! {
        status = child.wait() => {
            let status = status.context("wait for supervised process")?;
            guard.disarm();
            Ok(status)
        }
        () = cancellation.cancelled() => {
            terminate_group(pid, libc::SIGTERM);
            tokio::time::sleep(termination_grace).await;
            terminate_group(pid, libc::SIGKILL);
            child.wait().await.context("reap canceled supervised process")?;
            guard.disarm();
            bail!("supervised process canceled")
        }
    }
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
