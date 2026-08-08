use std::collections::HashSet;
use std::ffi::CString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use chrono::Utc;
use serde_json::json;

use crate::agent::auth::{AuthPaths, authorize};
use crate::agent::command_policy::CommandPolicy;
use crate::agent::rig_runner::{ChatGptTriageRunner, TriageRunnerCore};
use crate::agent::skills::validate_skills;
use crate::config::{
    IntakeConfig, canonical_roots, config_directory, default_config_path, expand_path,
    initialize_private_config, load_config, project_registry_path,
};
use crate::dashboard::{DashboardRunLimits, dashboard_bind, dashboard_router};
use crate::database::{IntakeDatabase, QueueOwnerLock};
use crate::logging::DurableLogStore;
use crate::monitor::IntakeMonitor;
use crate::project_registry::ensure_project_registry;
use crate::protocol::IntakeItem;
use crate::terminal::stderr_line;

pub const USAGE: &str = "Usage: intake [--config PATH] COMMAND\n\nCommands:\n  watch                 monitor sources and triage continuously\n  check                 poll every source once and drain ready triage events\n  status                show source and queue state\n  dashboard [--host HOST] [--port PORT]\n                        serve the local monitoring dashboard\n  inject FILE           queue one IntakeItem JSON fixture\n  show ID               show one intake event\n  retry ID              queue a retained event for another attempt\n  ignore ID             mark an event handled without action\n  login                 authenticate the ChatGPT subscription provider\n  init                  create private configuration directories and config\n  validate-config       validate YAML, command boundaries, and skill links\n";

pub async fn run(argv: Vec<String>) -> Result<i32> {
    let parsed = parse_global_options(argv)?;
    let mut args = parsed.args;
    let command = args.first().cloned();
    if command
        .as_deref()
        .is_none_or(|command| matches!(command, "help" | "--help" | "-h"))
    {
        print!("{USAGE}");
        return Ok(0);
    }
    let command = args.remove(0);

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

    match command.as_str() {
        "status" => print_status(&database).await?,
        "dashboard" => dashboard(&database, &config, &args).await?,
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
            if !run_monitor(command == "watch", config, database.clone()).await? {
                drop(queue_lock);
                database.shutdown().await?;
                return Ok(1);
            }
        }
        _ => bail!("Unknown command: {command}"),
    }

    drop(queue_lock);
    database.shutdown().await?;
    Ok(0)
}

async fn run_monitor(watch: bool, config: IntakeConfig, database: IntakeDatabase) -> Result<bool> {
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
        logs.clone(),
        std::io::stdout(),
        registry,
    );
    let runner = ChatGptTriageRunner::new(core, auth_paths()?);
    let monitor = IntakeMonitor::new(config, database, runner, logs);

    if !watch {
        let result = monitor.check().await?;
        println!(
            "Observed {}; handled {}; errors {}.",
            result.observed,
            result.handled,
            result.errors.len()
        );
        if !result.errors.is_empty() {
            eprintln!("{}", result.errors.join("\n"));
            return Ok(false);
        }
        return Ok(true);
    }

    let signal_task = spawn_monitor_signals(monitor.clone())?;
    let result = monitor.watch().await;
    signal_task.abort();
    result.map(|()| true)
}

async fn dashboard(
    database: &IntakeDatabase,
    config: &IntakeConfig,
    args: &[String],
) -> Result<()> {
    let options = parse_dashboard_options(args)?;
    let bind = dashboard_bind(
        options.host.as_deref(),
        options.port,
        options.allow_non_loopback,
    )?;
    if let Some(warning) = bind.warning() {
        eprintln!("{warning}");
    }
    let listener = tokio::net::TcpListener::bind((bind.host(), bind.port())).await?;
    let address = listener.local_addr()?;
    println!("Intake dashboard: http://{address}/");
    axum::serve(
        listener,
        dashboard_router(
            database.readers(),
            DashboardRunLimits {
                max_turns: Some(config.triage.max_turns as u32),
                wall_timeout_ms: Some(config.triage.timeout_minutes.saturating_mul(60_000)),
            },
        ),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    Ok(())
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
fn spawn_monitor_signals<R>(monitor: IntakeMonitor<R>) -> Result<tokio::task::JoinHandle<()>>
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
        stderr_line("Stopping schedules and waiting for active triage.");
        monitor.stop();
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
        stderr_line("Forced shutdown requested.");
        std::process::exit(130);
    }))
}

#[cfg(not(unix))]
fn spawn_monitor_signals<R>(monitor: IntakeMonitor<R>) -> Result<tokio::task::JoinHandle<()>>
where
    R: crate::agent::rig_runner::TriageRunner + Send + Sync + 'static,
{
    Ok(tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        stderr_line("Stopping schedules and waiting for active triage.");
        monitor.stop();
        let _ = tokio::signal::ctrl_c().await;
        stderr_line("Forced shutdown requested.");
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

pub fn write_error(error: &anyhow::Error) {
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "intake: {error}");
}
