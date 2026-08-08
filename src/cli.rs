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

pub static USAGE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "Usage: intake [--config PATH] COMMAND\n\nCommands:\n  watch [--dashboard] [--host HOST] [--port PORT]\n                        monitor sources and triage continuously\n  check                 poll every source once and drain ready triage events\n  status                show source and queue state\n  dashboard [--host HOST] [--port PORT]\n                        serve the local monitoring dashboard\n  inject FILE           queue one IntakeItem JSON fixture\n  show ID               show one intake event\n  retry ID              queue a retained event for another attempt\n  ignore ID             mark an event handled without action\n  login                 authenticate the ChatGPT subscription provider\n  init                  create private configuration directories and config\n  validate-config       validate YAML, command boundaries, and skill links\n\nWatch option:\n  --dashboard           serve the dashboard with watch\n\nDashboard options for watch --dashboard and dashboard:\n  --host HOST           bind host (default: {DEFAULT_DASHBOARD_HOST})\n  --port PORT           bind port (default: {DEFAULT_DASHBOARD_PORT})\n  --allow-non-loopback  acknowledge unauthenticated non-loopback access\n\nLogging:\n  Application logs append to state.logs/application.log.\n  Set INTAKE_LOG to off, error, warn, info, debug, or trace.\n"
    )
});

fn command_help(command: &str) -> Option<String> {
    let dashboard_options = || {
        format!(
            "  --host HOST           bind host (default: {DEFAULT_DASHBOARD_HOST})\n  --port PORT           bind port (default: {DEFAULT_DASHBOARD_PORT})\n  --allow-non-loopback  acknowledge unauthenticated non-loopback access\n"
        )
    };
    let help = match command {
        "watch" => format!(
            "Usage: intake [--config PATH] watch [--dashboard] [--host HOST] [--port PORT] [--allow-non-loopback]\n\nMonitor sources and triage continuously.\n\nOptions:\n  --dashboard           serve the dashboard with watch\n{}",
            dashboard_options()
        ),
        "check" => "Usage: intake [--config PATH] check\n\nPoll every source once and drain ready triage events.\n".into(),
        "status" => "Usage: intake [--config PATH] status\n\nShow source and queue state.\n".into(),
        "dashboard" => format!(
            "Usage: intake [--config PATH] dashboard [--host HOST] [--port PORT] [--allow-non-loopback]\n\nServe the local monitoring dashboard.\n\nOptions:\n{}",
            dashboard_options()
        ),
        "inject" => "Usage: intake [--config PATH] inject FILE\n\nQueue one IntakeItem JSON fixture.\n".into(),
        "show" => "Usage: intake [--config PATH] show ID\n\nShow one intake event.\n".into(),
        "retry" => "Usage: intake [--config PATH] retry ID\n\nQueue a retained event for another attempt.\n".into(),
        "ignore" => "Usage: intake [--config PATH] ignore ID\n\nMark an event handled without action.\n".into(),
        "login" => "Usage: intake login\n\nAuthenticate the ChatGPT subscription provider.\n".into(),
        "init" => "Usage: intake [--config PATH] init\n\nCreate private configuration directories and config.\n".into(),
        "validate-config" => "Usage: intake [--config PATH] validate-config\n\nValidate YAML, command boundaries, and skill links.\n".into(),
        _ => return None,
    };
    Some(help)
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
    if command.is_none_or(|command| matches!(command, "help" | "--help" | "-h" | "login" | "init"))
    {
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
    let command = args.remove(0);
    let watch_options = (command == "watch")
        .then(|| parse_watch_options(&args))
        .transpose()?;
    let dashboard_options = (command == "dashboard")
        .then(|| parse_dashboard_options(&args))
        .transpose()?;
    validate_other_command_options(&command, &args)?;
    tracing::info!(
        target: "intake::cli",
        command = safe_cli_command(&command),
        "command started"
    );

    if command == "login" {
        let auth = auth_paths()?;
        authorize(&auth, true).await?;
        return Ok(0);
    }

    let config_path = match parsed.config_path {
        Some(path) => expand_path(path)?,
        None => default_config_path()?,
    };
    if command == "init" {
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
    if command == "validate-config" {
        let diagnostics = validate_configuration(&config)?;
        if !diagnostics.is_empty() {
            bail!(diagnostics.join("\n"));
        }
        println!("Configuration is valid: {}", config_path.display());
        return Ok(0);
    }

    let database_path = expand_path(&config.state.database)?;
    let queue_lock = if matches!(command.as_str(), "watch" | "check") {
        Some(QueueOwnerLock::acquire(&database_path)?)
    } else {
        None
    };
    let database = IntakeDatabase::open(&database_path).await?;

    let operation = async {
        match command.as_str() {
            "status" => print_status(&database).await?,
            "dashboard" => {
                dashboard(
                    &database,
                    &config,
                    dashboard_options.expect("dashboard options parsed"),
                )
                .await?
            }
            "inject" => inject(&database, args.first()).await?,
            "show" => show(&database, args.first()).await?,
            "retry" => retry(&database, args.first()).await?,
            "ignore" => ignore(&database, args.first()).await?,
            "check" | "watch" => {
                let diagnostics = validate_configuration(&config)?;
                if !diagnostics.is_empty() {
                    bail!(
                        "Configuration validation failed:\n{}",
                        diagnostics.join("\n")
                    );
                }
                let succeeded = if command == "watch" {
                    run_watch(
                        config,
                        database.clone(),
                        watch_options.expect("watch options parsed").dashboard,
                    )
                    .await?;
                    true
                } else {
                    run_check(config, database.clone()).await?
                };
                if !succeeded {
                    return Ok(1);
                }
            }
            _ => bail!("Unknown command: {command}"),
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
                event_status_name(event.status),
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

fn validate_other_command_options(command: &str, args: &[String]) -> Result<()> {
    let positional_count = match command {
        "check" | "status" | "login" | "init" | "validate-config" => 0,
        "inject" | "show" | "retry" | "ignore" => 1,
        "watch" | "dashboard" => return Ok(()),
        _ => return Ok(()),
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

fn safe_cli_command(command: &str) -> &'static str {
    match command {
        "watch" => "watch",
        "check" => "check",
        "status" => "status",
        "dashboard" => "dashboard",
        "inject" => "inject",
        "show" => "show",
        "retry" => "retry",
        "ignore" => "ignore",
        "login" => "login",
        "init" => "init",
        "validate-config" => "validate-config",
        _ => "unknown",
    }
}

fn event_status_name(status: crate::database::EventStatus) -> &'static str {
    match status {
        crate::database::EventStatus::Pending => "pending",
        crate::database::EventStatus::Processing => "processing",
        crate::database::EventStatus::Retryable => "retryable",
        crate::database::EventStatus::Succeeded => "succeeded",
        crate::database::EventStatus::Failed => "failed",
        crate::database::EventStatus::Ignored => "ignored",
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
