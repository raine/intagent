use std::time::Duration;

use reqwest::Client;

use crate::protocol::ProtocolError;

pub mod fastmail;
pub mod github;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub fn http_client() -> Result<Client, ProtocolError> {
    Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|_| source_error("HTTP client initialization failed"))
}

pub(crate) fn source_error(message: impl Into<String>) -> ProtocolError {
    ProtocolError::Source(message.into())
}
