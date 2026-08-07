use std::ffi::OsStr;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub struct ProcessOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessFailure {
    Cancelled,
    TimedOut,
}

#[derive(Clone, Debug)]
pub struct ProcessOptions<'a> {
    pub cwd: &'a Path,
    pub environment: &'a [(String, String)],
    pub stdin: Option<Vec<u8>>,
    pub output_limit: usize,
    pub timeout: Duration,
    pub cancellation: CancellationToken,
}

pub async fn run_process<I, S>(
    program: &Path,
    arguments: I,
    options: ProcessOptions<'_>,
) -> Result<ProcessOutput>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let ProcessOptions {
        cwd,
        environment,
        stdin,
        output_limit,
        timeout,
        cancellation,
    } = options;
    let mut command = Command::new(program);
    command
        .args(arguments)
        .current_dir(cwd)
        .env_clear()
        .envs(environment.iter().map(|(key, value)| (key, value)))
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_process_group(&mut command);

    let mut child = command
        .spawn()
        .with_context(|| format!("spawn {}", program.display()))?;
    let pid = child.id().context("spawned process has no process ID")? as i32;
    let mut guard = ProcessGroupGuard::new(pid);
    let stdout = child.stdout.take().context("capture process stdout")?;
    let stderr = child.stderr.take().context("capture process stderr")?;
    let mut stdout_task = tokio::spawn(read_bounded(stdout, output_limit));
    let mut stderr_task = tokio::spawn(read_bounded(stderr, output_limit));
    let mut stdin_task = child.stdin.take().map(|mut pipe| {
        tokio::spawn(async move {
            if let Some(bytes) = stdin {
                pipe.write_all(&bytes).await?;
            }
            pipe.shutdown().await
        })
    });

    let failure = {
        let completion = async {
            let status = child.wait().await.context("wait for process")?;
            if let Some(task) = stdin_task.as_mut() {
                match task.await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) if error.kind() == std::io::ErrorKind::BrokenPipe => {}
                    Ok(Err(error)) => return Err(error).context("write process stdin"),
                    Err(error) => return Err(error).context("join stdin writer"),
                }
            }
            let (stdout, stdout_truncated) =
                (&mut stdout_task).await.context("join stdout reader")??;
            let (stderr, stderr_truncated) =
                (&mut stderr_task).await.context("join stderr reader")??;
            Ok(ProcessOutput {
                status,
                stdout,
                stderr,
                stdout_truncated,
                stderr_truncated,
            })
        };
        tokio::pin!(completion);
        tokio::select! {
            result = &mut completion => {
                if result.is_ok() {
                    guard.disarm();
                }
                return result;
            }
            () = cancellation.cancelled() => ProcessFailure::Cancelled,
            () = tokio::time::sleep(timeout) => ProcessFailure::TimedOut,
        }
    };

    terminate_and_reap(&mut child, pid, Duration::from_secs(1)).await?;
    guard.disarm();
    stdout_task.abort();
    stderr_task.abort();
    if let Some(task) = stdin_task {
        task.abort();
    }
    match failure {
        ProcessFailure::Cancelled => bail!("process cancelled"),
        ProcessFailure::TimedOut => bail!("process timed out"),
    }
}

pub async fn supervise_process<I, S>(
    program: &Path,
    arguments: I,
    cancellation: CancellationToken,
    termination_grace: Duration,
) -> Result<ExitStatus>
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
        .env_clear()
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
            terminate_and_reap(&mut child, pid, termination_grace).await?;
            guard.disarm();
            bail!("supervised process canceled")
        }
    }
}

async fn read_bounded(
    mut stream: impl AsyncRead + Unpin,
    limit: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let count = stream.read(&mut buffer).await?;
        if count == 0 {
            return Ok((bytes, truncated));
        }
        let remaining = limit.saturating_sub(bytes.len());
        let accepted = remaining.min(count);
        bytes.extend_from_slice(&buffer[..accepted]);
        truncated |= accepted < count;
    }
}

async fn terminate_and_reap(child: &mut Child, pid: i32, grace: Duration) -> Result<()> {
    terminate_group(pid, libc::SIGTERM);
    tokio::time::sleep(grace).await;
    terminate_group(pid, libc::SIGKILL);
    child.wait().await.context("reap terminated process")?;
    Ok(())
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
