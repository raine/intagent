use std::fs;
use std::os::unix::fs::PermissionsExt;

use intake::application_log::{TracingInitError, initialize_tracing, redact_log_text};

#[test]
fn redacts_sensitive_fields_and_bearer_credentials() {
    let redacted = redact_log_text(
        "token=visible password: 'hunter two' payload=\"private body\" Authorization: Bearer oauth-value safe=value",
    );
    assert!(!redacted.contains("visible"));
    assert!(!redacted.contains("hunter two"));
    assert!(!redacted.contains("private body"));
    assert!(!redacted.contains("oauth-value"));
    assert!(redacted.contains("safe=value"));
    assert!(redacted.matches("[REDACTED]").count() >= 4);
}

#[test]
fn initializes_once_with_private_filtered_durable_logs() {
    let root = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("INTAKE_LOG", "warn");
    }
    let initialized = initialize_tracing("test-intake", Some(root.path())).unwrap();
    unsafe {
        std::env::remove_var("INTAKE_LOG");
    }
    assert_eq!(initialized.directory.as_deref(), Some(root.path()));
    assert!(initialized.warning.is_none());

    tracing::info!(target: "intake::test", "filtered information");
    tracing::error!(
        target: "intake::test",
        token = "visible-token",
        payload = "private payload",
        safe = "durable",
        "recorded failure"
    );
    tracing::error!(target: "dependency::test", "dependency failure");
    tracing::error!(target: "intake::terminal::error", "private terminal title");

    let duplicate = initialize_tracing("test-intake", Some(root.path())).unwrap_err();
    assert!(matches!(duplicate, TracingInitError::AlreadyInitialized));
    assert_eq!(
        fs::metadata(root.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let log = fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|extension| extension == "log"))
        .expect("application log");
    assert_eq!(
        fs::metadata(&log).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let contents = fs::read_to_string(log).unwrap();
    assert!(contents.contains("recorded failure"));
    assert!(contents.contains("safe=\"durable\""));
    assert!(contents.contains("[REDACTED]"));
    assert!(!contents.contains("visible-token"));
    assert!(!contents.contains("private payload"));
    assert!(!contents.contains("filtered information"));
    assert!(!contents.contains("dependency failure"));
    assert!(!contents.contains("private terminal title"));
}
