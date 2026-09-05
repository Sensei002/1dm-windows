//! Shared HTTP client factory.
//!
//! The downloader reuses the *same* headers and cookies the embedded browser
//! used, so authenticated streams (e.g. logged-in Sony Liv) work out of the
//! box. Callers pass the captured headers from the stream capture layer.

use std::collections::HashMap;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use reqwest::Client;

/// Desktop Chrome UA — many CDNs reject bare programmatic clients.
pub const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// Builds a [`Client`] with the desktop UA plus any captured session headers
/// (e.g. `authorization`, `security_token`, `x-playback-session-id`).
pub fn build_client(extra_headers: &HashMap<String, String>) -> Result<Client, reqwest::Error> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(DEFAULT_USER_AGENT));
    for (key, value) in extra_headers {
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(key.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(name, val);
        }
    }
    Client::builder().default_headers(headers).build()
}