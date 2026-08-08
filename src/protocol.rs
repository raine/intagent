use std::future::Future;
use std::io;

use chrono::DateTime;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use url::Url;

pub const PROTOCOL_VERSION: u8 = 1;
pub const MAX_STANDARD_INPUT_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("stdin exceeds {MAX_STANDARD_INPUT_BYTES} bytes")]
    InputTooLarge,
    #[error("stdin does not contain one JSON request: {0}")]
    InvalidJson(serde_json::Error),
    #[error("invalid poll request: {0}")]
    InvalidRequest(String),
    #[error("invalid poll response: {0}")]
    InvalidResponse(String),
    #[error("source polling failed: {0}")]
    Source(String),
    #[error("source polling is unavailable: {0}")]
    SourceUnavailable(&'static str),
    #[error("standard stream error: {0}")]
    Io(#[from] io::Error),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PollRequest {
    pub protocol_version: u8,
    pub source: String,
    pub checkpoint: Value,
    pub now: String,
    pub item_limit: usize,
    #[serde(default)]
    pub options: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PollResponse {
    pub protocol_version: u8,
    pub checkpoint: Value,
    pub items: Vec<IntakeItem>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntakeItem {
    pub entity_id: String,
    pub revision_id: String,
    pub kind: IntakeItemKind,
    pub title: String,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub occurred_at: String,
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum IntakeItemKind {
    #[serde(rename = "email")]
    Email,
    #[serde(rename = "github-issue")]
    GithubIssue,
    #[serde(rename = "github-pull-request")]
    GithubPullRequest,
    #[serde(rename = "generic")]
    Generic,
}

impl PollRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(ProtocolError::InvalidRequest(
                "protocolVersion must equal 1".into(),
            ));
        }
        if utf16_len(&self.source) < 1 {
            return Err(ProtocolError::InvalidRequest(
                "source must not be empty".into(),
            ));
        }
        validate_utc_datetime("now", &self.now).map_err(ProtocolError::InvalidRequest)?;
        if !(1..=1000).contains(&self.item_limit) {
            return Err(ProtocolError::InvalidRequest(
                "itemLimit must be an integer between 1 and 1000".into(),
            ));
        }
        Ok(())
    }
}

impl PollResponse {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(ProtocolError::InvalidResponse(
                "protocolVersion must equal 1".into(),
            ));
        }
        if self.items.len() > 1000 {
            return Err(ProtocolError::InvalidResponse(
                "items must contain at most 1000 entries".into(),
            ));
        }
        for (index, item) in self.items.iter().enumerate() {
            item.validate().map_err(|message| {
                ProtocolError::InvalidResponse(format!("items.{index}.{message}"))
            })?;
        }
        Ok(())
    }
}

impl IntakeItem {
    pub fn validate(&self) -> Result<(), String> {
        validate_utf16_range("entityId", &self.entity_id, 1, 1024)?;
        validate_utf16_range("revisionId", &self.revision_id, 1, 1024)?;
        validate_utf16_range("title", &self.title, 0, 4096)?;
        validate_utf16_range("body", &self.body, 0, 1_000_000)?;
        if let Some(url) = &self.url {
            Url::parse(url).map_err(|_| "url: invalid URL".to_string())?;
        }
        validate_utc_datetime("occurredAt", &self.occurred_at)?;
        Ok(())
    }
}

pub fn parse_poll_request(input: &[u8]) -> Result<PollRequest, ProtocolError> {
    let request: PollRequest = serde_json::from_slice(input).map_err(ProtocolError::InvalidJson)?;
    request.validate()?;
    Ok(request)
}

pub fn parse_poll_response(input: &[u8]) -> Result<PollResponse, ProtocolError> {
    let response: PollResponse = serde_json::from_slice(input)
        .map_err(|error| ProtocolError::InvalidResponse(error.to_string()))?;
    response.validate()?;
    Ok(response)
}

pub async fn read_poll_request<R>(reader: R) -> Result<PollRequest, ProtocolError>
where
    R: AsyncRead + Unpin,
{
    let mut input = Vec::new();
    reader
        .take(MAX_STANDARD_INPUT_BYTES + 1)
        .read_to_end(&mut input)
        .await?;
    if input.len() as u64 > MAX_STANDARD_INPUT_BYTES {
        return Err(ProtocolError::InputTooLarge);
    }
    parse_poll_request(&input)
}

pub async fn write_poll_response<W>(
    writer: &mut W,
    response: &PollResponse,
) -> Result<(), ProtocolError>
where
    W: AsyncWrite + Unpin,
{
    response.validate()?;
    let mut output = serde_json::to_vec(response)
        .map_err(|error| ProtocolError::InvalidResponse(error.to_string()))?;
    output.push(b'\n');
    writer.write_all(&output).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn run_source<R, W, E, H, F>(
    reader: R,
    mut writer: W,
    mut diagnostics: E,
    handler: H,
) -> bool
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    E: AsyncWrite + Unpin,
    H: FnOnce(PollRequest) -> F,
    F: Future<Output = Result<PollResponse, ProtocolError>>,
{
    let result = async {
        let request = read_poll_request(reader).await?;
        let response = handler(request).await?;
        write_poll_response(&mut writer, &response).await
    }
    .await;
    match result {
        Ok(()) => true,
        Err(error) => {
            tracing::error!(
                target: "intake::source",
                failure_category = protocol_error_category(&error),
                "source protocol failed"
            );
            let _ = diagnostics.write_all(format!("{error}\n").as_bytes()).await;
            let _ = diagnostics.flush().await;
            false
        }
    }
}

pub async fn source_main<H, F>(handler: H) -> bool
where
    H: FnOnce(PollRequest) -> F,
    F: Future<Output = Result<PollResponse, ProtocolError>>,
{
    run_source(
        tokio::io::stdin(),
        tokio::io::stdout(),
        tokio::io::stderr(),
        handler,
    )
    .await
}

fn protocol_error_category(error: &ProtocolError) -> &'static str {
    match error {
        ProtocolError::InputTooLarge => "input_limit",
        ProtocolError::InvalidJson(_) | ProtocolError::InvalidRequest(_) => "invalid_request",
        ProtocolError::InvalidResponse(_) => "invalid_response",
        ProtocolError::Source(_) | ProtocolError::SourceUnavailable(_) => "source",
        ProtocolError::Io(_) => "io",
    }
}

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn validate_utf16_range(
    name: &str,
    value: &str,
    minimum: usize,
    maximum: usize,
) -> Result<(), String> {
    let length = utf16_len(value);
    if (minimum..=maximum).contains(&length) {
        Ok(())
    } else {
        Err(format!(
            "{name}: must contain between {minimum} and {maximum} UTF-16 code units"
        ))
    }
}

fn validate_utc_datetime(name: &str, value: &str) -> Result<(), String> {
    if value.as_bytes().get(10) != Some(&b'T') || !value.ends_with('Z') {
        return Err(format!("{name}: must be a UTC date-time ending in Z"));
    }
    let time = value
        .split_once('T')
        .map(|(_, time)| time)
        .ok_or_else(|| format!("{name}: invalid date-time"))?;
    let core = &time[..time.len() - 1];
    let valid_shape = match core.as_bytes() {
        [_, _, b':', _, _] => true,
        [_, _, b':', _, _, b':', _, _] => true,
        bytes
            if bytes.len() > 9
                && bytes.get(2) == Some(&b':')
                && bytes.get(5) == Some(&b':')
                && bytes.get(8) == Some(&b'.')
                && bytes[9..].iter().all(u8::is_ascii_digit) =>
        {
            true
        }
        _ => false,
    };
    if !valid_shape {
        return Err(format!("{name}: invalid UTC date-time"));
    }
    let parsed = if core.len() == 5 {
        let expanded = format!("{}:00Z", &value[..value.len() - 1]);
        DateTime::parse_from_rfc3339(&expanded)
    } else {
        DateTime::parse_from_rfc3339(value)
    };
    parsed
        .map(|_| ())
        .map_err(|_| format!("{name}: invalid UTC date-time"))
}
