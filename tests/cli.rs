use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use intake::cli::{
    DashboardOptions, USAGE, parse_dashboard_options, parse_global_options, public_cli_error,
};
use intake::database::{IntakeDatabase, QueueOwnerLock};
use tempfile::TempDir;

#[test]
fn snapshots_usage_and_direct_argument_errors() {
    assert_eq!(
        USAGE,
        "Usage: intake [--config PATH] COMMAND\n\nCommands:\n  watch                 monitor sources and triage continuously\n  check                 poll every source once and drain ready triage events\n  status                show source and queue state\n  dashboard [--host HOST] [--port PORT]\n                        serve the local monitoring dashboard\n  inject FILE           queue one IntakeItem JSON fixture\n  show ID               show one intake event\n  retry ID              queue a retained event for another attempt\n  ignore ID             mark an event handled without action\n  login                 authenticate the ChatGPT subscription provider\n  init                  create private configuration directories and config\n  validate-config       validate YAML, command boundaries, and skill links\n\nLogging:\n  Application logs use state.logs/application and retain eight daily files.\n  Set INTAKE_LOG to off, error, warn, info, debug, or trace.\n"
    );
    let error = parse_global_options(vec!["status".into(), "--config".into()]).unwrap_err();
    assert_eq!(error.to_string(), "--config requires a path");
    let error = parse_dashboard_options(&["--port".into(), "0".into()]).unwrap_err();
    assert_eq!(
        error.to_string(),
        "dashboard port must be between 1 and 65535"
    );
    let error = parse_dashboard_options(&["--bogus".into()]).unwrap_err();
    assert_eq!(error.to_string(), "Unknown dashboard option: --bogus");
    assert_eq!(
        public_cli_error("OAuth token=visible-secret was rejected"),
        "Authentication failed"
    );
    assert_eq!(
        public_cli_error("configuration file is unavailable"),
        "configuration file is unavailable"
    );
}

#[test]
fn accepts_global_config_before_or_after_the_command() {
    for args in [
        vec!["--config", "fixture.yaml", "status"],
        vec!["status", "--config", "fixture.yaml"],
    ] {
        let parsed = parse_global_options(args.into_iter().map(str::to_owned).collect()).unwrap();
        assert_eq!(parsed.config_path.unwrap().to_str(), Some("fixture.yaml"));
        assert_eq!(parsed.args, vec!["status"]);
    }
    assert_eq!(
        parse_dashboard_options(&[
            "--host".into(),
            "0.0.0.0".into(),
            "--port".into(),
            "8080".into(),
            "--allow-non-loopback".into(),
        ])
        .unwrap(),
        DashboardOptions {
            host: Some("0.0.0.0".into()),
            port: Some(8080),
            allow_non_loopback: true,
        }
    );
}

#[test]
fn init_is_idempotent_and_queue_commands_preserve_exit_behavior() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config/config.yaml");
    let state = root.path().join("state");
    let skills = root.path().join("application-skills");
    fs::create_dir(&skills).unwrap();

    let first = intake(
        &root,
        [
            "init".to_string(),
            "--config".to_string(),
            config.display().to_string(),
        ],
    );
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        String::from_utf8_lossy(&first.stdout).starts_with("Created private configuration at ")
    );
    assert_eq!(
        fs::metadata(&config).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(config.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    let second = intake(
        &root,
        [
            "--config".to_string(),
            config.display().to_string(),
            "init".to_string(),
        ],
    );
    assert!(second.status.success());
    assert!(
        String::from_utf8_lossy(&second.stdout).starts_with("Private configuration exists at ")
    );
    let configured_logs = root.path().join("configured-logs");
    let configured = fs::read_to_string(&config)
        .unwrap()
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("logs:") {
                format!("  logs: {}", configured_logs.display())
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&config, format!("{configured}\n")).unwrap();

    let database_path = state.join("intake/intake.sqlite");
    let owner = QueueOwnerLock::acquire(&database_path).unwrap();
    let overlapping = command(&root, &config, "check", None);
    assert_eq!(overlapping.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&overlapping.stderr)
            .contains("another intake watch or check owns the queue")
    );
    let readable = command(&root, &config, "status", None);
    assert!(readable.status.success());
    drop(owner);
    let checked = command(&root, &config, "check", None);
    assert!(checked.status.success());
    assert_eq!(
        String::from_utf8_lossy(&checked.stdout),
        "Observed 0; handled 0; errors 0.\n"
    );

    let fixture = root.path().join("fixture.json");
    fs::write(
        &fixture,
        r#"{"entityId":"fixture:1","revisionId":"revision:1","kind":"generic","title":"Fixture","body":"content","occurredAt":"2026-08-08T12:00:00.000Z","metadata":{}}"#,
    )
    .unwrap();
    let injected = command(
        &root,
        &config,
        "inject",
        Some(&fixture.display().to_string()),
    );
    assert!(
        injected.status.success(),
        "{}",
        String::from_utf8_lossy(&injected.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&injected.stdout),
        format!("Queued fixture from {}.\n", fixture.display())
    );

    let status = command(&root, &config, "status", None);
    assert!(status.status.success());
    let status_output = String::from_utf8_lossy(&status.stdout);
    assert!(status_output.contains("Queue:\n  pending: 1\n"));
    assert!(status_output.contains("Recent events:\n  1 pending manual-injection: Fixture\n"));

    let shown = command(&root, &config, "show", Some("1"));
    assert!(shown.status.success());
    assert!(String::from_utf8_lossy(&shown.stdout).contains("\"entityId\": \"fixture:1\""));

    let retried = command(&root, &config, "retry", Some("1"));
    assert!(retried.status.success());
    assert_eq!(
        String::from_utf8_lossy(&retried.stdout),
        "Event 1 is queued for retry.\n"
    );

    let ignored = command(&root, &config, "ignore", Some("1"));
    assert!(ignored.status.success());
    assert_eq!(
        String::from_utf8_lossy(&ignored.stdout),
        "Event 1 is ignored.\n"
    );

    let unavailable = command(&root, &config, "retry", Some("1"));
    assert_eq!(unavailable.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&unavailable.stderr),
        "intake: Event 1 has no retained content to retry\n"
    );

    assert!(state.join("intake/intake.sqlite").exists());
    let application_logs = configured_logs.join("application");
    assert_eq!(
        fs::metadata(&application_logs)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let application_log = fs::read_dir(&application_logs)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("intake."))
        })
        .expect("application log");
    assert_eq!(
        fs::metadata(&application_log).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let application_log = fs::read_to_string(application_log).unwrap();
    assert!(application_log.contains("intake::lifecycle"));
    assert!(!application_log.contains("rusqlite"));
}

#[test]
fn watch_handles_graceful_and_forced_signals() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config/config.yaml");
    fs::create_dir(root.path().join("application-skills")).unwrap();
    let initialized = intake(
        &root,
        [
            "init".to_string(),
            "--config".to_string(),
            config.display().to_string(),
        ],
    );
    assert!(initialized.status.success());

    let mut graceful = spawn_intake(&root, &config, "watch");
    wait_for_watch_start(&mut graceful);
    send_signal(&graceful, libc::SIGINT);
    assert_eq!(
        wait_for_exit(&mut graceful, Duration::from_secs(3)),
        Some(0)
    );

    let marker = root.path().join("source-started");
    let source = root.path().join("source");
    fs::write(
        &source,
        format!("#!/bin/sh\ntouch '{}'\nsleep 2\n", marker.display()),
    )
    .unwrap();
    let mut permissions = fs::metadata(&source).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&source, permissions).unwrap();
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        contents.replace(
            "sources: []",
            &format!(
                "sources:\n- name: sleeper\n  command: {}\n  interval_seconds: 10\n  timeout_seconds: 60\n  item_limit: 1",
                source.display()
            ),
        ),
    )
    .unwrap();
    let mut forced = spawn_intake(&root, &config, "watch");
    wait_for_watch_start(&mut forced);
    wait_for_path(&marker, Duration::from_secs(3));
    send_signal(&forced, libc::SIGTERM);
    thread::sleep(Duration::from_millis(100));
    send_signal(&forced, libc::SIGTERM);
    assert_eq!(
        wait_for_exit(&mut forced, Duration::from_secs(3)),
        Some(130)
    );
}

#[test]
fn help_survives_unavailable_application_logging() {
    let root = tempfile::tempdir().unwrap();
    let blocked = root.path().join("state/intake/logs/application");
    fs::create_dir_all(blocked.parent().unwrap()).unwrap();
    fs::write(&blocked, "not a directory").unwrap();

    let output = intake(&root, ["--help".to_string()]);

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("Usage: intake"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Warning: application logging is unavailable"));
    assert!(!stderr.contains("not a directory"));
}

#[test]
fn log_filter_suppresses_lower_priority_and_dependency_events() {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_intake"))
        .arg("--help")
        .env("HOME", root.path())
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env("INTAKE_LOG", "error")
        .output()
        .unwrap();
    assert!(output.status.success());
    let directory = root.path().join("state/intake/logs/application");
    let log = fs::read_dir(directory)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(fs::read_to_string(log).unwrap().is_empty());
}

#[test]
fn source_help_uses_default_application_log_directory() {
    for (executable, prefix) in [
        (
            env!("CARGO_BIN_EXE_intake-fastmail-source"),
            "intake-fastmail-source.",
        ),
        (
            env!("CARGO_BIN_EXE_intake-github-source"),
            "intake-github-source.",
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let output = Command::new(executable)
            .arg("--help")
            .env("HOME", root.path())
            .env("XDG_STATE_HOME", root.path().join("state"))
            .output()
            .unwrap();
        assert!(output.status.success());
        let directory = root.path().join("state/intake/logs/application");
        let log = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(prefix))
            })
            .expect("source application log");
        assert_eq!(
            fs::metadata(directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(log).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn queue_owner_lock_uses_the_canonical_database_identity() {
    let root = tempfile::tempdir().unwrap();
    let database_path = root.path().join("intake.sqlite");
    let database = IntakeDatabase::open(&database_path).await.unwrap();
    database.shutdown().await.unwrap();
    let alias = root.path().join("database-alias.sqlite");
    std::os::unix::fs::symlink(&database_path, &alias).unwrap();

    let owner = QueueOwnerLock::acquire(&database_path).unwrap();
    let observer = IntakeDatabase::open(&alias).await.unwrap();
    assert!(observer.readers().status().await.unwrap().is_empty());
    let error = QueueOwnerLock::acquire(&alias).unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("another intake watch or check owns the queue for ")
    );
    observer.shutdown().await.unwrap();
    drop(observer);
    drop(owner);
    QueueOwnerLock::acquire(&alias).unwrap();
}

fn command(
    root: &TempDir,
    config: &std::path::Path,
    name: &str,
    value: Option<&str>,
) -> std::process::Output {
    let mut args = vec![name.to_string()];
    if let Some(value) = value {
        args.push(value.to_string());
    }
    args.push("--config".into());
    args.push(config.display().to_string());
    intake(root, args)
}

fn intake(root: &TempDir, args: impl IntoIterator<Item = String>) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_intake"))
        .args(args)
        .env("HOME", root.path())
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env("INTAKE_SKILLS_DIR", root.path().join("application-skills"))
        .output()
        .unwrap()
}

fn spawn_intake(root: &TempDir, config: &std::path::Path, command: &str) -> Child {
    Command::new(env!("CARGO_BIN_EXE_intake"))
        .args([command, "--config", &config.display().to_string()])
        .env("HOME", root.path())
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env("INTAKE_SKILLS_DIR", root.path().join("application-skills"))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn wait_for_watch_start(child: &mut Child) {
    let output = child.stdout.take().expect("watch stdout");
    let mut line = String::new();
    BufReader::new(output)
        .read_line(&mut line)
        .expect("read watch startup");
    assert!(line.contains("Watching"), "unexpected watch output: {line}");
    assert!(!line.contains("\x1b["));
    assert!(line.as_bytes().get(..8).is_some_and(|time| {
        time.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 2 | 5) {
                *byte == b':'
            } else {
                byte.is_ascii_digit()
            }
        })
    }));
}

fn wait_for_path(path: &std::path::Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn send_signal(child: &Child, signal: i32) {
    assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> Option<i32> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status.code();
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let _ = child.wait();
            return None;
        }
        thread::sleep(Duration::from_millis(20));
    }
}
