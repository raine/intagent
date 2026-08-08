use intake::database::ErrorCategory as DatabaseErrorCategory;
use intake::errors::{ErrorCategory, classify_message, public_cli_error, public_error};

#[test]
fn error_categories_have_stable_storage_names() {
    for (category, name) in [
        (ErrorCategory::Authentication, "authentication"),
        (ErrorCategory::RateLimit, "rate_limit"),
        (ErrorCategory::Timeout, "timeout"),
        (ErrorCategory::Connection, "connection"),
        (ErrorCategory::NotFound, "not_found"),
        (ErrorCategory::ModelUnavailable, "model_unavailable"),
        (ErrorCategory::ContextLimit, "context_limit"),
        (ErrorCategory::TurnLimit, "turn_limit"),
        (ErrorCategory::Aborted, "aborted"),
        (ErrorCategory::Interrupted, "interrupted"),
        (ErrorCategory::ToolFailure, "tool_failure"),
        (ErrorCategory::Unknown, "unknown"),
    ] {
        assert_eq!(category.as_str(), name);
    }
    assert_eq!(
        DatabaseErrorCategory::Authentication,
        ErrorCategory::Authentication
    );
}

#[test]
fn message_classification_has_safe_explicit_precedence() {
    for (message, expected) in [
        (
            "authentication timed out after a rate limit",
            ErrorCategory::Authentication,
        ),
        ("too many requests before timeout", ErrorCategory::RateLimit),
        ("socket timeout", ErrorCategory::Timeout),
        ("socket closed", ErrorCategory::Connection),
        ("model not found", ErrorCategory::NotFound),
        ("model does not exist", ErrorCategory::ModelUnavailable),
        ("context limit reached", ErrorCategory::ContextLimit),
        ("triage interrupted", ErrorCategory::Interrupted),
        ("opaque provider failure", ErrorCategory::Unknown),
    ] {
        assert_eq!(classify_message(message), expected, "{message}");
    }
}

#[test]
fn public_redaction_keeps_safe_messages_and_precedence() {
    assert_eq!(
        public_error(Some("token rejected: Fastmail email response is invalid")).as_deref(),
        Some("Authentication failed")
    );
    assert_eq!(
        public_error(Some("HTTP 429: upstream detail")).as_deref(),
        Some("Operation failed")
    );
    assert_eq!(
        public_cli_error("prompt payload included token=visible-secret"),
        "Authentication failed"
    );
}
