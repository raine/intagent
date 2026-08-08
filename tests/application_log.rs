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
fn initializes_once_with_private_filtered_durable_log() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("application.log");
    unsafe {
        std::env::set_var("INTAKE_LOG", "warn");
    }
    let initialized = initialize_tracing(Some(&path)).unwrap();
    unsafe {
        std::env::remove_var("INTAKE_LOG");
    }
    assert_eq!(initialized.path.as_deref(), Some(path.as_path()));
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

    let duplicate = initialize_tracing(Some(&path)).unwrap_err();
    assert!(matches!(duplicate, TracingInitError::AlreadyInitialized));
    assert_eq!(
        fs::metadata(root.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let contents = fs::read_to_string(path).unwrap();
    assert!(contents.contains("recorded failure"));
    assert!(contents.contains("safe=\"durable\""));
    assert!(contents.contains("[REDACTED]"));
    assert!(!contents.contains("visible-token"));
    assert!(!contents.contains("private payload"));
    assert!(!contents.contains("filtered information"));
    assert!(!contents.contains("dependency failure"));
    assert!(!contents.contains("private terminal title"));
}
