use std::collections::HashSet;
use std::ffi::CString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::agent::auth::{AuthPaths, authorize};
use crate::agent::command_policy::CommandPolicy;
use crate::agent::rig_runner::{ChatGptTriageRunner, TriageRunnerCore};
use crate::agent::skills::validate_skills;
use crate::application_log::{application_log_path, redact_log_text};
use crate::config::{
    IntakeConfig, canonical_roots, config_directory, default_config_path, expand_path,
    initialize_private_config, load_config, project_registry_path, state_directory,
};
use crate::dashboard::{
    DEFAULT_DASHBOARD_HOST, DEFAULT_DASHBOARD_PORT, DashboardRunLimits, dashboard_bind,
    dashboard_router,
};
use crate::database::{IntakeDatabase, QueueOwnerLock};
use crate::logging::DurableLogStore;
use crate::monitor::IntakeMonitor;
use crate::project_registry::ensure_project_registry;
use crate::protocol::IntakeItem;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Command {
    Watch,
    Check,
    Status,
    Dashboard,
    Inject,
    Show,
    Retry,
    Ignore,
    Login,
    Init,
    ValidateConfig,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Arguments {
    None,
    One,
    Watch,
    Dashboard,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Runtime {
    Login,
    Init,
    ValidateConfig,
    Database {
        queue_owner: bool,
        validate_config: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    command: Command,
    name: &'static str,
    synopsis: &'static str,
    description: &'static str,
    arguments: Arguments,
    runtime: Runtime,
}

impl CommandSpec {
    pub fn name(self) -> &'static str {
        self.name
    }

    pub fn synopsis(self) -> &'static str {
        self.synopsis
    }

    pub fn description(self) -> &'static str {
        self.description
    }
}

pub const COMMAND_SPECS: &[CommandSpec] = &[
    CommandSpec {
        command: Command::Watch,
        name: "watch",
        synopsis: "watch [--dashboard] [--host HOST] [--port PORT]",
        description: "Monitor sources and triage continuously.",
        arguments: Arguments::Watch,
        runtime: Runtime::Database {
            queue_owner: true,
            validate_config: true,
        },
    },
    CommandSpec {
        command: Command::Check,
        name: "check",
        synopsis: "check",
        description: "Poll every source once and drain ready triage events.",
        arguments: Arguments::None,
        runtime: Runtime::Database {
            queue_owner: true,
            validate_config: true,
        },
    },
    CommandSpec {
        command: Command::Status,
        name: "status",
        synopsis: "status",
        description: "Show source and queue state.",
        arguments: Arguments::None,
        runtime: Runtime::Database {
            queue_owner: false,
            validate_config: false,
        },
    },
    CommandSpec {
        command: Command::Dashboard,
        name: "dashboard",
        synopsis: "dashboard [--host HOST] [--port PORT]",
        description: "Serve the local monitoring dashboard.",
        arguments: Arguments::Dashboard,
        runtime: Runtime::Database {
            queue_owner: false,
            validate_config: false,
        },
    },
    CommandSpec {
        command: Command::Inject,
        name: "inject",
        synopsis: "inject FILE",
        description: "Queue one IntakeItem JSON fixture.",
        arguments: Arguments::One,
        runtime: Runtime::Database {
            queue_owner: false,
            validate_config: false,
        },
    },
    CommandSpec {
        command: Command::Show,
        name: "show",
        synopsis: "show ID",
        description: "Show one intake event.",
        arguments: Arguments::One,
        runtime: Runtime::Database {
            queue_owner: false,
            validate_config: false,
        },
    },
    CommandSpec {
        command: Command::Retry,
        name: "retry",
        synopsis: "retry ID",
        description: "Queue a retained event for another attempt.",
        arguments: Arguments::One,
        runtime: Runtime::Database {
            queue_owner: false,
            validate_config: false,
        },
    },
    CommandSpec {
        command: Command::Ignore,
        name: "ignore",
        synopsis: "ignore ID",
        description: "Mark an event handled without action.",
        arguments: Arguments::One,
        runtime: Runtime::Database {
            queue_owner: false,
            validate_config: false,
        },
    },
    CommandSpec {
        command: Command::Login,
        name: "login",
        synopsis: "login",
        description: "Authenticate the ChatGPT subscription provider.",
        arguments: Arguments::None,
        runtime: Runtime::Login,
    },
    CommandSpec {
        command: Command::Init,
        name: "init",
        synopsis: "init",
        description: "Create private configuration directories and config.",
        arguments: Arguments::None,
        runtime: Runtime::Init,
    },
    CommandSpec {
        command: Command::ValidateConfig,
        name: "validate-config",
        synopsis: "validate-config",
        description: "Validate YAML, command boundaries, and skill links.",
        arguments: Arguments::None,
        runtime: Runtime::ValidateConfig,
    },
];

pub static USAGE: LazyLock<String> = LazyLock::new(|| {
    let mut commands = String::new();
    for spec in COMMAND_SPECS {
        let description = lowercase_first(spec.description.trim_end_matches('.'));
        if spec.synopsis.len() > 22 {
            commands.push_str(&format!(
                "  {}\n                        {description}\n",
                spec.synopsis
            ));
        } else {
            commands.push_str(&format!("  {:<22}{description}\n", spec.synopsis));
        }
    }
    format!(
        "Usage: intake [--config PATH] COMMAND\n\nCommands:\n{commands}\nWatch option:\n  --dashboard           serve the dashboard with watch\n\nDashboard options for watch --dashboard and dashboard:\n  --host HOST           bind host (default: {DEFAULT_DASHBOARD_HOST})\n  --port PORT           bind port (default: {DEFAULT_DASHBOARD_PORT})\n  --allow-non-loopback  acknowledge unauthenticated non-loopback access\n\nLogging:\n  Application logs append to state.logs/application.log.\n  Set INTAKE_LOG to off, error, warn, info, debug, or trace.\n"
    )
});

fn lowercase_first(value: &str) -> String {
    let mut characters = value.chars();
    characters
        .next()
        .map(|first| first.to_lowercase().chain(characters).collect())
        .unwrap_or_default()
}

fn command_spec(name: &str) -> Option<&'static CommandSpec> {
    COMMAND_SPECS.iter().find(|spec| spec.name == name)
}

fn command_help(command: &str) -> Option<String> {
    let spec = command_spec(command)?;
    let config = if spec.command == Command::Login {
        ""
    } else {
        " [--config PATH]"
    };
    let synopsis = match spec.arguments {
        Arguments::Watch => {
            "watch [--dashboard] [--host HOST] [--port PORT] [--allow-non-loopback]"
        }
        Arguments::Dashboard => "dashboard [--host HOST] [--port PORT] [--allow-non-loopback]",
        Arguments::None | Arguments::One => spec.synopsis,
    };
    let options = match spec.arguments {
        Arguments::Watch => format!(
            "\nOptions:\n  --dashboard           serve the dashboard with watch\n{}",
            dashboard_options_help()
        ),
        Arguments::Dashboard => format!("\nOptions:\n{}", dashboard_options_help()),
        Arguments::None | Arguments::One => String::new(),
    };
    Some(format!(
        "Usage: intake{config} {synopsis}\n\n{}\n{options}",
        spec.description
    ))
}

fn dashboard_options_help() -> String {
    format!(
        "  --host HOST           bind host (default: {DEFAULT_DASHBOARD_HOST})\n  --port PORT           bind port (default: {DEFAULT_DASHBOARD_PORT})\n  --allow-non-loopback  acknowledge unauthenticated non-loopback access\n"
    )
}

fn requested_command_help(args: &[String]) -> Option<String> {
    (args.len() == 2 && matches!(args[1].as_str(), "--help" | "-h"))
        .then(|| command_help(&args[0]))
        .flatten()
}

pub fn tracing_log_path(argv: &[String]) -> Option<PathBuf> {
    let default = state_directory()
        .ok()
        .map(|state| application_log_path(state.join("logs")));
    let parsed = match parse_global_options(argv.to_vec()) {
        Ok(parsed) => parsed,
        Err(_) => return default,
    };
    if requested_command_help(&parsed.args).is_some() {
        return None;
    }
    let command = parsed.args.first().map(String::as_str);
    if command.is_none_or(|command| {
        matches!(command, "help" | "--help" | "-h")
            || command_spec(command)
                .is_some_and(|spec| matches!(spec.runtime, Runtime::Login | Runtime::Init))
    }) {
        return default;
    }
    let config_path = parsed
        .config_path
        .map(expand_path)
        .transpose()
        .ok()
        .flatten()
        .or_else(|| default_config_path().ok());
    let configured = config_path
        .and_then(|path| load_config(path).ok())
        .and_then(|config| expand_path(config.state.logs).ok())
        .map(application_log_path);
    configured.or(default)
}

pub async fn run(argv: Vec<String>) -> Result<i32> {
    let parsed = parse_global_options(argv)?;
    let mut args = parsed.args;
    let command = args.first().cloned();
    if command
        .as_deref()
        .is_none_or(|command| matches!(command, "help" | "--help" | "-h"))
    {
        print!("{}", *USAGE);
        return Ok(0);
    }
    if let Some(help) = requested_command_help(&args) {
        print!("{help}");
        return Ok(0);
    }
    let command_name = args.remove(0);
    let spec = command_spec(&command_name).copied();
    let watch_options = spec
        .is_some_and(|spec| spec.arguments == Arguments::Watch)
        .then(|| parse_watch_options(&args))
        .transpose()?;
    let dashboard_options = spec
        .is_some_and(|spec| spec.arguments == Arguments::Dashboard)
        .then(|| parse_dashboard_options(&args))
        .transpose()?;
    validate_other_command_options(spec.as_ref(), &command_name, &args)?;
    tracing::info!(
        target: "intake::cli",
        command = spec.map_or("unknown", |spec| spec.name),
        "command started"
    );

    if spec.is_some_and(|spec| spec.runtime == Runtime::Login) {
        let auth = auth_paths()?;
        authorize(&auth, true).await?;
        return Ok(0);
    }

    let config_path = match parsed.config_path {
        Some(path) => expand_path(path)?,
        None => default_config_path()?,
    };
    if spec.is_some_and(|spec| spec.runtime == Runtime::Init) {
        let environment = std::env::vars().collect();
        let result = initialize_private_config(&config_path, &environment)?;
        if result.created.is_empty() {
            println!(
                "Private configuration exists at {}",
                result.config_path.display()
            );
        } else {
            println!(
                "Created private configuration at {}",
                result.config_path.display()
            );
        }
        return Ok(0);
    }

    let config = load_config(&config_path)?;
    if spec.is_some_and(|spec| spec.runtime == Runtime::ValidateConfig) {
        let diagnostics = validate_configuration(&config)?;
        if !diagnostics.is_empty() {
            bail!(diagnostics.join("\n"));
        }
        println!("Configuration is valid: {}", config_path.display());
        return Ok(0);
    }

    let database_path = expand_path(&config.state.database)?;
    let queue_lock = spec
        .and_then(|spec| match spec.runtime {
            Runtime::Database {
                queue_owner: true, ..
            } => Some(QueueOwnerLock::acquire(&database_path)),
            _ => None,
        })
        .transpose()?;
    let database = IntakeDatabase::open(&database_path).await?;

    let operation = async {
        if spec.is_some_and(|spec| {
            matches!(
                spec.runtime,
                Runtime::Database {
                    validate_config: true,
                    ..
                }
            )
        }) {
            let diagnostics = validate_configuration(&config)?;
            if !diagnostics.is_empty() {
                bail!(
                    "Configuration validation failed:\n{}",
                    diagnostics.join("\n")
                );
            }
        }
        match spec.map(|spec| spec.command) {
            Some(Command::Status) => print_status(&database).await?,
            Some(Command::Dashboard) => {
                dashboard(
                    &database,
                    &config,
                    dashboard_options.expect("dashboard options parsed"),
                )
                .await?
            }
            Some(Command::Inject) => inject(&database, args.first()).await?,
            Some(Command::Show) => show(&database, args.first()).await?,
            Some(Command::Retry) => retry(&database, args.first()).await?,
            Some(Command::Ignore) => ignore(&database, args.first()).await?,
            Some(Command::Check) => {
                if !run_check(config, database.clone()).await? {
                    return Ok(1);
                }
            }
            Some(Command::Watch) => {
                run_watch(
                    config,
                    database.clone(),
                    watch_options.expect("watch options parsed").dashboard,
                )
                .await?;
            }
            Some(Command::Login | Command::Init | Command::ValidateConfig) => {
                unreachable!("non-database command handled before database startup")
            }
            None => bail!("Unknown command: {command_name}"),
        }
        Ok(0)
    }
    .await;

    drop(queue_lock);
    let shutdown = database.shutdown().await;
    match operation {
        Ok(code) => {
            shutdown?;
            Ok(code)
        }
        Err(error) => {
            if let Err(shutdown_error) = shutdown {
                tracing::error!(
                    target: "intake::database",
                    error = %shutdown_error,
                    "database shutdown failed"
                );
            }
            Err(error)
        }
    }
}

fn build_monitor(
    config: IntakeConfig,
    database: IntakeDatabase,
) -> Result<IntakeMonitor<ChatGptTriageRunner>> {
    let roots = canonical_roots(&config.project_roots)?;
    let policy = Arc::new(CommandPolicy::new(&config, roots)?);
    let filter = policy.clone();
    let logs = DurableLogStore::new(expand_path(&config.state.logs)?, move |value| {
        filter.filter(value)
    });
    let registry = project_registry_path()?;
    ensure_project_registry(&registry)?;
    let core = TriageRunnerCore::new(
        config.clone(),
        database.clone(),
        policy,
        logs,
        std::io::stdout(),
        registry,
    );
    let runner = ChatGptTriageRunner::new(core, auth_paths()?);
    Ok(IntakeMonitor::new(config, database, runner))
}

async fn run_check(config: IntakeConfig, database: IntakeDatabase) -> Result<bool> {
    let monitor = build_monitor(config, database)?;
    let result = monitor.check().await?;
    println!(
        "Observed {}; handled {}; errors {}.",
        result.observed,
        result.handled,
        result.errors.len()
    );
    if !result.errors.is_empty() {
        for error in &result.errors {
            eprintln!(
                "{}",
                crate::dashboard::public_error(Some(error))
                    .unwrap_or_else(|| "Operation failed".into())
            );
        }
        return Ok(false);
    }
    Ok(true)
}

async fn run_watch(
    config: IntakeConfig,
    database: IntakeDatabase,
    dashboard_options: Option<DashboardOptions>,
) -> Result<()> {
    let monitor = build_monitor(config.clone(), database.clone())?;
    let Some(options) = dashboard_options else {
        let signal_task = spawn_monitor_signals(monitor.clone(), None)?;
        let result = monitor.watch().await;
        signal_task.abort();
        return result;
    };

    let listener = bind_dashboard(&options).await?;
    let dashboard_shutdown = CancellationToken::new();
    let signal_task = spawn_monitor_signals(monitor.clone(), Some(dashboard_shutdown.clone()))?;
    if let Err(error) = announce_dashboard(&listener) {
        signal_task.abort();
        return Err(error);
    }
    let dashboard = axum::serve(
        listener,
        dashboard_router(database.readers(), dashboard_limits(&config)),
    )
    .with_graceful_shutdown(dashboard_shutdown.cancelled_owned());
    let result = tokio::try_join!(monitor.watch(), async {
        dashboard.await.context("dashboard server failed")
    });
    signal_task.abort();
    result.map(|_| ())
}

async fn dashboard(
    database: &IntakeDatabase,
    config: &IntakeConfig,
    options: DashboardOptions,
) -> Result<()> {
    let listener = bind_dashboard(&options).await?;
    announce_dashboard(&listener)?;
    axum::serve(
        listener,
        dashboard_router(database.readers(), dashboard_limits(config)),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("dashboard server failed")?;
    tracing::info!(target: "intake::dashboard", "dashboard stopped");
    Ok(())
}

async fn bind_dashboard(options: &DashboardOptions) -> Result<tokio::net::TcpListener> {
    let bind = dashboard_bind(
        options.host.as_deref(),
        options.port,
        options.allow_non_loopback,
    )?;
    if let Some(warning) = bind.warning() {
        eprintln!("{warning}");
    }
    tokio::net::TcpListener::bind((bind.host(), bind.port()))
        .await
        .with_context(|| {
            format!(
                "failed to bind dashboard at {}:{}",
                bind.host(),
                bind.port()
            )
        })
}

fn announce_dashboard(listener: &tokio::net::TcpListener) -> Result<()> {
    let address = listener.local_addr()?;
    tracing::info!(
        target: "intake::dashboard",
        host = %address.ip(),
        port = address.port(),
        "dashboard started"
    );
    println!("Intake dashboard: http://{address}/");
    Ok(())
}

fn dashboard_limits(config: &IntakeConfig) -> DashboardRunLimits {
    DashboardRunLimits {
        max_turns: Some(config.triage.max_turns as u32),
        max_attempts: Some(config.triage.max_attempts as u32),
        wall_timeout_ms: Some(config.triage.timeout_minutes.saturating_mul(60_000)),
    }
}

async fn inject(database: &IntakeDatabase, path: Option<&String>) -> Result<()> {
    let path = path.ok_or_else(|| anyhow!("inject requires an IntakeItem JSON file"))?;
    let expanded = expand_path(path)?;
    let bytes = tokio::fs::read(&expanded).await?;
    let fixture: IntakeItem = serde_json::from_slice(&bytes)?;
    fixture.validate().map_err(anyhow::Error::msg)?;
    let now = Utc::now();
    let queued = database
        .source_succeeded(
            "manual-injection".into(),
            json!({ "injected_at": crate::database::timestamp(now) }),
            vec![fixture],
            now,
        )
        .await?;
    if queued == 0 {
        bail!("fixture entity and revision are already queued");
    }
    println!("Queued fixture from {path}.");
    Ok(())
}

async fn show(database: &IntakeDatabase, value: Option<&String>) -> Result<()> {
    let id = parse_event_id(value)?;
    let event = database
        .readers()
        .event(id)
        .await?
        .ok_or_else(|| anyhow!("Unknown event {id}"))?;
    println!("{}", serde_json::to_string_pretty(&event)?);
    Ok(())
}

async fn retry(database: &IntakeDatabase, value: Option<&String>) -> Result<()> {
    let id = parse_event_id(value)?;
    if !database.retry(id, Utc::now()).await? {
        bail!("Event {id} has no retained content to retry");
    }
    println!("Event {id} is queued for retry.");
    Ok(())
}

async fn ignore(database: &IntakeDatabase, value: Option<&String>) -> Result<()> {
    let id = parse_event_id(value)?;
    if !database.ignore(id, Utc::now()).await? {
        bail!("Unknown event {id}");
    }
    println!("Event {id} is ignored.");
    Ok(())
}

async fn print_status(database: &IntakeDatabase) -> Result<()> {
    println!("Queue:");
    let statuses = database.readers().status().await?;
    if statuses.is_empty() {
        println!("  empty");
    }
    for status in [
        "failed",
        "ignored",
        "pending",
        "processing",
        "retryable",
        "succeeded",
    ] {
        if let Some(count) = statuses.get(status) {
            println!("  {status}: {count}");
        }
    }
    println!("Sources:");
    let sources = database.readers().source_statuses().await?;
    if sources.is_empty() {
        println!("  unchecked");
    }
    for source in sources {
        println!("  {}", serde_json::to_string(&source)?);
    }
    let recent = database.readers().list_events(10).await?;
    if !recent.is_empty() {
        println!("Recent events:");
        for event in recent {
            println!(
                "  {} {} {}: {}",
                event.id,
                event.status.as_str(),
                event.source,
                event.title
            );
        }
    }
    Ok(())
}

pub fn validate_configuration(config: &IntakeConfig) -> Result<Vec<String>> {
    let mut diagnostics = Vec::new();
    let mut names = HashSet::new();
    for source in &config.sources {
        if !names.insert(&source.name) {
            diagnostics.push(format!("duplicate source name: {}", source.name));
        }
    }
    let mut executables = HashSet::new();
    for rule in &config.commands.rules {
        if !executables.insert(&rule.executable) {
            diagnostics.push(format!("duplicate command rule: {}", rule.executable));
        }
    }
    for path in &config.commands.path {
        let expanded = expand_path(path)?;
        if !is_executable_directory(&expanded) {
            diagnostics.push(format!("command PATH directory is unavailable: {path}"));
        }
    }
    diagnostics.extend(validate_skills(config)?.diagnostics);
    Ok(diagnostics)
}

fn is_executable_directory(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    let Ok(path) = CString::new(path.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 }
}

fn auth_paths() -> Result<AuthPaths> {
    let directory = config_directory()?.join("agent");
    Ok(AuthPaths {
        cache: directory.join("rig-auth.json"),
        directory,
    })
}

fn parse_event_id(value: Option<&String>) -> Result<i64> {
    let id = value.and_then(|value| value.parse::<i64>().ok());
    id.filter(|id| *id > 0)
        .ok_or_else(|| anyhow!("A positive event ID is required"))
}

#[derive(Debug, Eq, PartialEq)]
pub struct GlobalOptions {
    pub config_path: Option<PathBuf>,
    pub args: Vec<String>,
}

pub fn parse_global_options(mut args: Vec<String>) -> Result<GlobalOptions> {
    let config_path = if let Some(index) = args.iter().position(|argument| argument == "--config") {
        let value = args
            .get(index + 1)
            .filter(|value| !value.is_empty())
            .cloned()
            .ok_or_else(|| anyhow!("--config requires a path"))?;
        args.drain(index..=index + 1);
        Some(PathBuf::from(value))
    } else {
        None
    };
    Ok(GlobalOptions { config_path, args })
}

fn validate_other_command_options(
    spec: Option<&CommandSpec>,
    command: &str,
    args: &[String],
) -> Result<()> {
    let positional_count = match spec.map(|spec| spec.arguments) {
        Some(Arguments::None) => 0,
        Some(Arguments::One) => 1,
        Some(Arguments::Watch | Arguments::Dashboard) | None => return Ok(()),
    };
    for (index, argument) in args.iter().enumerate() {
        if argument.starts_with('-') {
            bail!("Unknown {command} option: {argument}");
        }
        if index >= positional_count {
            bail!("Unexpected {command} argument: {argument}");
        }
    }
    Ok(())
}

#[derive(Debug, Default, Eq, PartialEq)]
pub struct WatchOptions {
    pub dashboard: Option<DashboardOptions>,
}

pub fn parse_watch_options(args: &[String]) -> Result<WatchOptions> {
    let dashboard_enabled = args.iter().any(|argument| argument == "--dashboard");
    let dashboard_args = args
        .iter()
        .filter(|argument| argument.as_str() != "--dashboard")
        .cloned()
        .collect::<Vec<_>>();
    if !dashboard_enabled {
        if let Some(option) = dashboard_args.first() {
            bail!("Unknown watch option: {option}");
        }
        return Ok(WatchOptions::default());
    }
    if args
        .iter()
        .filter(|argument| argument.as_str() == "--dashboard")
        .count()
        > 1
    {
        bail!("--dashboard may only be specified once");
    }
    Ok(WatchOptions {
        dashboard: Some(parse_dashboard_options(&dashboard_args)?),
    })
}

#[derive(Debug, Default, Eq, PartialEq)]
pub struct DashboardOptions {
    pub host: Option<String>,
    pub port: Option<u32>,
    pub allow_non_loopback: bool,
}

pub fn parse_dashboard_options(args: &[String]) -> Result<DashboardOptions> {
    let mut options = DashboardOptions::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--host" if index + 1 < args.len() => {
                options.host = Some(args[index + 1].clone());
                index += 2;
            }
            "--port" if index + 1 < args.len() => {
                options.port = Some(
                    args[index + 1]
                        .parse()
                        .ok()
                        .filter(|port: &u32| (1..=65_535).contains(port))
                        .ok_or_else(|| anyhow!("dashboard port must be between 1 and 65535"))?,
                );
                index += 2;
            }
            "--allow-non-loopback" => {
                options.allow_non_loopback = true;
                index += 1;
            }
            option => bail!("Unknown dashboard option: {option}"),
        }
    }
    Ok(options)
}

#[cfg(unix)]
fn spawn_monitor_signals<R>(
    monitor: IntakeMonitor<R>,
    dashboard_shutdown: Option<CancellationToken>,
) -> Result<tokio::task::JoinHandle<()>>
where
    R: crate::agent::rig_runner::TriageRunner + Send + Sync + 'static,
{
    use tokio::signal::unix::{SignalKind, signal};

    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    Ok(tokio::spawn(async move {
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
        tracing::info!(
            target: "intake::terminal::error",
            "Stopping schedules and waiting for active triage."
        );
        monitor.stop();
        if let Some(shutdown) = dashboard_shutdown {
            shutdown.cancel();
        }
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
        tracing::warn!(target: "intake::terminal::error", "Forced shutdown requested.");
        std::process::exit(130);
    }))
}

#[cfg(not(unix))]
fn spawn_monitor_signals<R>(
    monitor: IntakeMonitor<R>,
    dashboard_shutdown: Option<CancellationToken>,
) -> Result<tokio::task::JoinHandle<()>>
where
    R: crate::agent::rig_runner::TriageRunner + Send + Sync + 'static,
{
    Ok(tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!(
            target: "intake::terminal::error",
            "Stopping schedules and waiting for active triage."
        );
        monitor.stop();
        if let Some(shutdown) = dashboard_shutdown {
            shutdown.cancel();
        }
        let _ = tokio::signal::ctrl_c().await;
        tracing::warn!(target: "intake::terminal::error", "Forced shutdown requested.");
        std::process::exit(130);
    }))
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut interrupt = signal(SignalKind::interrupt()).expect("install SIGINT handler");
        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

pub fn public_cli_error(error: &str) -> String {
    let message = redact_log_text(error);
    let lowercase = message.to_ascii_lowercase();
    if [
        "auth",
        "bearer",
        "credential",
        "oauth",
        "password",
        "payload",
        "prompt",
        "secret",
        "token",
    ]
    .iter()
    .any(|keyword| lowercase.contains(keyword))
    {
        crate::dashboard::public_error(Some(&message)).unwrap_or_else(|| "Operation failed".into())
    } else {
        message
    }
}

pub fn write_error(error: &anyhow::Error) {
    let public = public_cli_error(&error.to_string());
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "intake: {public}");
}
