use std::env;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use tracing::level_filters::LevelFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

pub const APPLICATION_LOG_FILE: &str = "application.log";
pub const APPLICATION_LOG_PATH_ENV: &str = "INTAKE_LOG_PATH";
pub const DEFAULT_LOG_LEVEL: LevelFilter = LevelFilter::INFO;

#[derive(Debug)]
pub struct TracingInit {
    pub path: Option<PathBuf>,
    pub warning: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum TracingInitError {
    #[error("tracing is already initialized")]
    AlreadyInitialized,
}

pub fn initialize_tracing(path: Option<&Path>) -> Result<TracingInit, TracingInitError> {
    set_private_umask();
    let (level, level_warning) = configured_level();
    let stdout_color = std::io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none();
    let stderr_color = std::io::stderr().is_terminal() && env::var_os("NO_COLOR").is_none();
    let terminal_output = tracing_subscriber::fmt::layer()
        .compact()
        .with_ansi(stdout_color)
        .with_level(false)
        .with_target(false)
        .with_timer(LocalTime)
        .with_writer(RedactingMakeWriter::new(std::io::stdout))
        .with_filter(tracing_subscriber::filter::filter_fn(move |metadata| {
            metadata.target() == "intake::terminal" && *metadata.level() <= level
        }));
    let terminal_diagnostics = tracing_subscriber::fmt::layer()
        .compact()
        .with_ansi(stderr_color)
        .with_level(false)
        .with_target(false)
        .with_timer(LocalTime)
        .with_writer(RedactingMakeWriter::new(std::io::stderr))
        .with_filter(tracing_subscriber::filter::filter_fn(move |metadata| {
            metadata.target() == "intake::terminal::error" && *metadata.level() <= level
        }));

    let mut warning = level_warning;
    let mut installed_path = None;
    let appender = path.and_then(|path| match private_appender(path) {
        Ok(appender) => {
            installed_path = Some(path.to_path_buf());
            Some(appender)
        }
        Err(error) => {
            warning = Some(format!(
                "application logging is unavailable for {}: {}",
                path.display(),
                safe_io_error(&error)
            ));
            None
        }
    });
    let file_filter = Targets::new()
        .with_target("intake::terminal", LevelFilter::OFF)
        .with_target("intake", level)
        .with_default(LevelFilter::OFF);
    let file = appender.map(|appender| {
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_target(true)
            .with_writer(RedactingMakeWriter::new(appender))
            .with_filter(file_filter)
    });

    tracing_subscriber::registry()
        .with(terminal_output)
        .with(terminal_diagnostics)
        .with(file)
        .try_init()
        .map_err(|_| TracingInitError::AlreadyInitialized)?;

    Ok(TracingInit {
        path: installed_path,
        warning,
    })
}

pub fn application_log_path(logs: impl AsRef<Path>) -> PathBuf {
    logs.as_ref().join(APPLICATION_LOG_FILE)
}

pub fn source_application_log_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os(APPLICATION_LOG_PATH_ENV) {
        return Some(PathBuf::from(path));
    }
    crate::config::state_directory()
        .ok()
        .map(|state| application_log_path(state.join("logs")))
}

pub fn redact_log_text(value: &str) -> String {
    let mut output = redact_bearer(value);
    for keyword in [
        "authorization",
        "command_output",
        "cookie",
        "oauth",
        "password",
        "payload",
        "prompt",
        "secret",
        "token",
    ] {
        output = redact_named_value(&output, keyword);
    }
    output
}

fn private_appender(path: &Path) -> Result<File, io::Error> {
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "log path has no parent"))?;
    fs::create_dir_all(directory)?;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

fn configured_level() -> (LevelFilter, Option<String>) {
    let Some(value) = env::var_os("INTAKE_LOG") else {
        return (DEFAULT_LOG_LEVEL, None);
    };
    let value = value.to_string_lossy();
    let level = match value.to_ascii_lowercase().as_str() {
        "off" => LevelFilter::OFF,
        "error" => LevelFilter::ERROR,
        "warn" => LevelFilter::WARN,
        "info" => LevelFilter::INFO,
        "debug" => LevelFilter::DEBUG,
        "trace" => LevelFilter::TRACE,
        _ => {
            return (
                DEFAULT_LOG_LEVEL,
                Some(
                    "ignoring invalid INTAKE_LOG value; expected off, error, warn, info, debug, or trace"
                        .into(),
                ),
            );
        }
    };
    (level, None)
}

fn safe_io_error(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::NotFound => "path not found",
        io::ErrorKind::PermissionDenied => "permission denied",
        io::ErrorKind::AlreadyExists => "path already exists",
        io::ErrorKind::InvalidInput => "invalid path",
        io::ErrorKind::ReadOnlyFilesystem => "read-only filesystem",
        _ => "I/O error",
    }
}

fn set_private_umask() {
    unsafe {
        libc::umask(0o077);
    }
}

fn redact_named_value(value: &str, keyword: &str) -> String {
    let mut output = value.to_string();
    let mut offset = 0;
    loop {
        let lowercase = output[offset..].to_ascii_lowercase();
        let Some(relative) = lowercase.find(keyword) else {
            break;
        };
        let start = offset + relative;
        let key_end = start + keyword.len();
        if start > 0 && output.as_bytes()[start - 1].is_ascii_alphanumeric() {
            offset = key_end;
            continue;
        }
        let mut cursor = key_end;
        while output
            .as_bytes()
            .get(cursor)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            cursor += 1;
        }
        while output
            .as_bytes()
            .get(cursor)
            .is_some_and(u8::is_ascii_whitespace)
        {
            cursor += 1;
        }
        if !output
            .as_bytes()
            .get(cursor)
            .is_some_and(|byte| matches!(byte, b':' | b'='))
        {
            offset = key_end;
            continue;
        }
        cursor += 1;
        while output
            .as_bytes()
            .get(cursor)
            .is_some_and(u8::is_ascii_whitespace)
        {
            cursor += 1;
        }
        let quote = output
            .as_bytes()
            .get(cursor)
            .copied()
            .filter(|byte| matches!(byte, b'"' | b'\''));
        if quote.is_some() {
            cursor += 1;
        }
        let value_end = match quote {
            Some(quote) => output.as_bytes()[cursor..]
                .iter()
                .position(|byte| *byte == quote)
                .map_or(output.len(), |length| cursor + length),
            None => output.as_bytes()[cursor..]
                .iter()
                .position(|byte| byte.is_ascii_whitespace() || matches!(byte, b',' | b'}' | b']'))
                .map_or(output.len(), |length| cursor + length),
        };
        if value_end == cursor {
            offset = cursor;
            continue;
        }
        output.replace_range(cursor..value_end, "[REDACTED]");
        offset = cursor + "[REDACTED]".len();
    }
    output
}

fn redact_bearer(value: &str) -> String {
    let mut output = value.to_string();
    let mut offset = 0;
    loop {
        let lowercase = output[offset..].to_ascii_lowercase();
        let Some(relative) = lowercase.find("bearer ") else {
            break;
        };
        let start = offset + relative + "bearer ".len();
        let end = output.as_bytes()[start..]
            .iter()
            .position(|byte| {
                byte.is_ascii_whitespace() || matches!(byte, b'"' | b'\'' | b',' | b'}')
            })
            .map_or(output.len(), |length| start + length);
        output.replace_range(start..end, "[REDACTED]");
        offset = start + "[REDACTED]".len();
    }
    output
}

#[derive(Clone)]
struct RedactingMakeWriter<M> {
    inner: M,
}

impl<M> RedactingMakeWriter<M> {
    fn new(inner: M) -> Self {
        Self { inner }
    }
}

impl<'a, M> MakeWriter<'a> for RedactingMakeWriter<M>
where
    M: MakeWriter<'a>,
{
    type Writer = RedactingWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter {
            inner: self.inner.make_writer(),
        }
    }

    fn make_writer_for(&'a self, metadata: &tracing::Metadata<'_>) -> Self::Writer {
        RedactingWriter {
            inner: self.inner.make_writer_for(metadata),
        }
    }
}

struct RedactingWriter<W> {
    inner: W,
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.write_all(buffer)?;
        Ok(buffer.len())
    }

    fn write_all(&mut self, buffer: &[u8]) -> io::Result<()> {
        let value = String::from_utf8_lossy(buffer);
        self.inner.write_all(redact_log_text(&value).as_bytes())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

struct LocalTime;

impl FormatTime for LocalTime {
    fn format_time(&self, writer: &mut Writer<'_>) -> fmt::Result {
        write!(writer, "{}", chrono::Local::now().format("%H:%M:%S"))
    }
}
