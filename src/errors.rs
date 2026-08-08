use crate::application_log::redact_log_text;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorCategory {
    Authentication,
    RateLimit,
    Timeout,
    Connection,
    NotFound,
    ModelUnavailable,
    ContextLimit,
    TurnLimit,
    Aborted,
    Interrupted,
    ToolFailure,
    Unknown,
}

impl ErrorCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Authentication => "authentication",
            Self::RateLimit => "rate_limit",
            Self::Timeout => "timeout",
            Self::Connection => "connection",
            Self::NotFound => "not_found",
            Self::ModelUnavailable => "model_unavailable",
            Self::ContextLimit => "context_limit",
            Self::TurnLimit => "turn_limit",
            Self::Aborted => "aborted",
            Self::Interrupted => "interrupted",
            Self::ToolFailure => "tool_failure",
            Self::Unknown => "unknown",
        }
    }
}

pub fn classify_message(message: &str) -> ErrorCategory {
    let message = message.to_ascii_lowercase();
    if [
        "auth",
        "credential",
        "sign-in",
        "sign in",
        "unauthorized",
        "forbidden",
        "token",
    ]
    .iter()
    .any(|needle| message.contains(needle))
    {
        ErrorCategory::Authentication
    } else if message.contains("rate limit")
        || message.contains("too many requests")
        || message.contains("429")
    {
        ErrorCategory::RateLimit
    } else if message.contains("timeout") || message.contains("timed out") {
        ErrorCategory::Timeout
    } else if message.contains("connection") || message.contains("socket") {
        ErrorCategory::Connection
    } else if message.contains("not found") || message.contains("404") {
        ErrorCategory::NotFound
    } else if message.contains("model")
        && (message.contains("unavailable") || message.contains("does not exist"))
    {
        ErrorCategory::ModelUnavailable
    } else if message.contains("context limit") {
        ErrorCategory::ContextLimit
    } else if message.contains("interrupt") {
        ErrorCategory::Interrupted
    } else {
        ErrorCategory::Unknown
    }
}

pub fn public_error(error: Option<&str>) -> Option<String> {
    let error = error?;
    let lowercase = error.to_ascii_lowercase();
    let message = if ["auth", "credential", "token", "unauthorized", "forbidden"]
        .iter()
        .any(|needle| lowercase.contains(needle))
    {
        "Authentication failed"
    } else if lowercase.contains("fastmail email response is invalid") {
        "Fastmail email response is invalid"
    } else if lowercase.contains("rate limit") || lowercase.contains("too many requests") {
        "Rate limited"
    } else if lowercase.contains("timeout") || lowercase.contains("timed out") {
        "Request timed out"
    } else if lowercase.contains("connection reset") {
        "Connection reset"
    } else if lowercase.contains("not found") || lowercase.contains("404") {
        "Resource not found (404)"
    } else if lowercase.contains("model") && lowercase.contains("unavailable") {
        "Model unavailable"
    } else if lowercase.contains("interrupt") {
        "Triage interrupted"
    } else {
        "Operation failed"
    };
    Some(message.into())
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
        public_error(Some(&message)).unwrap_or_else(|| "Operation failed".into())
    } else {
        message
    }
}
