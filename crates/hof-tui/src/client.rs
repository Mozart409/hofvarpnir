//! HTTP client for the Hofvarpnir REST API.
//!
//! Authenticates with an API key as `Authorization: Bearer hof_sk_...`
//! (see `hof-api/src/auth.rs`). Also consumes the JSON SSE progress stream
//! and forwards parsed events to the UI over `tokio::sync::mpsc`
//! (GOALS.md: "SSE stream consumption via `tokio::sync::mpsc`").

use std::time::Duration;

use futures::StreamExt;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::search::ActivityFilter;
use crate::sse::SseParser;
use crate::types::{
    ActivityListResponse, ApiErrorResponse, PauseModule, PauseSummaryResponse, ProfileResponse,
    ProgressEvent, SettingsResponse, SourceResponse, SystemStatusResponse, VideoResponse,
    VideoStatus, WhoAmIResponse,
};

/// First delay before reconnecting a dropped progress stream. Doubles on
/// each consecutive failure up to [`SSE_RECONNECT_MAX`], and resets once a
/// connection is established again.
const SSE_RECONNECT_INITIAL: Duration = Duration::from_secs(1);

/// Upper bound for the reconnect backoff.
const SSE_RECONNECT_MAX: Duration = Duration::from_mins(5);

/// Message produced by the background progress-stream task.
#[derive(Debug)]
pub enum ProgressMsg {
    /// A parsed `event: progress` payload.
    Progress(ProgressEvent),
    /// Stream connectivity changed (drives the status-bar indicator).
    Connected(bool),
    /// The stream is down; the next attempt starts after this delay.
    Retrying(Duration),
}

/// API client errors.
#[derive(Debug, thiserror::Error)]
pub enum ApiClientError {
    #[error("request failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("invalid API token for Authorization header: {0}")]
    InvalidToken(String),
    #[error("API error (HTTP {status}): {message}")]
    Api { status: StatusCode, message: String },
}

/// Request body for `POST /api/v1/system/pause`.
#[derive(serde::Serialize)]
struct PauseBody {
    module: &'static str,
}

/// Response envelope of the pause/resume endpoints.
#[derive(serde::Deserialize)]
struct PauseEnvelope {
    pause: PauseSummaryResponse,
}

/// Thin authenticated client over `api/v1`.
#[derive(Debug, Clone)]
pub struct ApiClient {
    http: reqwest::Client,
    /// Server base URL without trailing slash, e.g. `http://localhost:8080`.
    base: String,
}

impl ApiClient {
    /// Build a client that sends `Authorization: Bearer <token>` on every
    /// request.
    ///
    /// # Errors
    ///
    /// Returns an error if the token contains characters that are not valid
    /// in an HTTP header value.
    pub fn new(base: &str, token: &str) -> Result<Self, ApiClientError> {
        let mut headers = HeaderMap::new();
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| ApiClientError::InvalidToken(e.to_string()))?;
        headers.insert(AUTHORIZATION, value);

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .build()?;

        Ok(Self {
            http,
            base: base.to_string(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Map a non-success response to an error, decoding the API's JSON error
    /// body when present.
    async fn api_error(resp: reqwest::Response) -> ApiClientError {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let parsed = serde_json::from_str::<ApiErrorResponse>(&body).ok();
        let message = parsed
            .and_then(|e| {
                if e.message.is_empty() {
                    (!e.error.is_empty()).then_some(e.error)
                } else {
                    Some(e.message)
                }
            })
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| body.trim().to_string());
        ApiClientError::Api { status, message }
    }

    async fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, ApiClientError> {
        let resp = self
            .http
            .request(method, self.url(path))
            .query(query)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(Self::api_error(resp).await);
        }
        Ok(resp.json::<T>().await?)
    }

    /// Send a bodyless action request (POST/DELETE) that may return an empty
    /// or non-JSON success body. Returns a short human-readable confirmation.
    async fn action(&self, method: Method, path: &str) -> Result<String, ApiClientError> {
        let resp = self.http.request(method, self.url(path)).send().await?;
        if !resp.status().is_success() {
            return Err(Self::api_error(resp).await);
        }
        // Some endpoints return a JSON message body; use it when readable.
        let body = resp.text().await.unwrap_or_default();
        let parsed = serde_json::from_str::<ApiErrorResponse>(&body).ok();
        Ok(parsed.map(|e| e.message).unwrap_or_default())
    }

    /// `GET /api/v1/system/status`.
    pub async fn system_status(&self) -> Result<SystemStatusResponse, ApiClientError> {
        self.request(Method::GET, "/api/v1/system/status", &[])
            .await
    }

    /// `GET /api/v1/system/whoami`: the key's own scopes.
    pub async fn whoami(&self) -> Result<WhoAmIResponse, ApiClientError> {
        self.request(Method::GET, "/api/v1/system/whoami", &[])
            .await
    }

    /// `GET /api/v1/downloads`, optionally filtered by status.
    pub async fn list_downloads(
        &self,
        status: Option<VideoStatus>,
    ) -> Result<Vec<VideoResponse>, ApiClientError> {
        let query: Vec<(&str, String)> = status
            .map(|s| ("status", s.as_str().to_string()))
            .into_iter()
            .collect();
        self.request(Method::GET, "/api/v1/downloads", &query).await
    }

    /// `POST /api/v1/downloads/{id}/retry`.
    pub async fn retry_download(&self, id: &str) -> Result<String, ApiClientError> {
        self.action(Method::POST, &format!("/api/v1/downloads/{id}/retry"))
            .await
    }

    /// `POST /api/v1/downloads/{id}/cancel`.
    pub async fn cancel_download(&self, id: &str) -> Result<String, ApiClientError> {
        self.action(Method::POST, &format!("/api/v1/downloads/{id}/cancel"))
            .await
    }

    /// `DELETE /api/v1/downloads/{id}`.
    pub async fn delete_download(&self, id: &str) -> Result<String, ApiClientError> {
        self.action(Method::DELETE, &format!("/api/v1/downloads/{id}"))
            .await
    }

    /// `GET /api/v1/sources`.
    pub async fn list_sources(&self) -> Result<Vec<SourceResponse>, ApiClientError> {
        self.request(Method::GET, "/api/v1/sources", &[]).await
    }

    /// `POST /api/v1/sources/{id}/index` — trigger a manual index.
    pub async fn trigger_index(&self, id: &str) -> Result<String, ApiClientError> {
        self.action(Method::POST, &format!("/api/v1/sources/{id}/index"))
            .await
    }

    /// `DELETE /api/v1/sources/{id}`.
    pub async fn delete_source(&self, id: &str) -> Result<String, ApiClientError> {
        self.action(Method::DELETE, &format!("/api/v1/sources/{id}"))
            .await
    }

    /// `GET /api/v1/profiles`.
    pub async fn list_profiles(&self) -> Result<Vec<ProfileResponse>, ApiClientError> {
        self.request(Method::GET, "/api/v1/profiles", &[]).await
    }

    /// `DELETE /api/v1/profiles/{id}`.
    pub async fn delete_profile(&self, id: &str) -> Result<String, ApiClientError> {
        self.action(Method::DELETE, &format!("/api/v1/profiles/{id}"))
            .await
    }

    /// `GET /api/v1/activity?limit=N&offset=M`, newest first, narrowed by
    /// `filter`.
    pub async fn list_activity(
        &self,
        limit: i64,
        offset: usize,
        filter: &ActivityFilter,
    ) -> Result<ActivityListResponse, ApiClientError> {
        let mut query = vec![("limit", limit.to_string()), ("offset", offset.to_string())];
        if let Some(severity) = filter.severity {
            query.push(("severity", severity.as_query().to_string()));
        }
        if let Some(search) = &filter.search {
            query.push(("search", search.clone()));
        }
        self.request(Method::GET, "/api/v1/activity", &query).await
    }

    /// `POST /api/v1/system/pause` — pause indexing and downloads
    /// indefinitely.
    pub async fn pause_all(&self) -> Result<PauseSummaryResponse, ApiClientError> {
        self.pause(PauseModule::All).await
    }

    /// `DELETE /api/v1/system/pause` — resume everything.
    pub async fn resume_all(&self) -> Result<PauseSummaryResponse, ApiClientError> {
        self.resume(PauseModule::All).await
    }

    /// `POST /api/v1/system/pause` — pause one module (or all) indefinitely.
    pub async fn pause(&self, module: PauseModule) -> Result<PauseSummaryResponse, ApiClientError> {
        let resp = self
            .http
            .post(self.url("/api/v1/system/pause"))
            .json(&PauseBody {
                module: module.as_str(),
            })
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(Self::api_error(resp).await);
        }
        Ok(resp.json::<PauseEnvelope>().await?.pause)
    }

    /// `DELETE /api/v1/system/pause?module=...` — resume one module (or all).
    pub async fn resume(
        &self,
        module: PauseModule,
    ) -> Result<PauseSummaryResponse, ApiClientError> {
        let resp = self
            .http
            .delete(self.url("/api/v1/system/pause"))
            .query(&[("module", module.as_str())])
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(Self::api_error(resp).await);
        }
        Ok(resp.json::<PauseEnvelope>().await?.pause)
    }

    /// `GET /api/v1/system/settings`.
    pub async fn get_settings(&self) -> Result<SettingsResponse, ApiClientError> {
        self.request(Method::GET, "/api/v1/system/settings", &[])
            .await
    }

    /// `PATCH /api/v1/system/settings` with a single field. `None` sends an
    /// explicit JSON `null`, which resets the knob to its env/default value.
    pub async fn patch_setting(
        &self,
        field: &str,
        value: Option<u64>,
    ) -> Result<SettingsResponse, ApiClientError> {
        let mut body = serde_json::Map::new();
        body.insert(
            field.to_string(),
            value.map_or(serde_json::Value::Null, serde_json::Value::from),
        );
        let resp = self
            .http
            .patch(self.url("/api/v1/system/settings"))
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(Self::api_error(resp).await);
        }
        Ok(resp.json::<SettingsResponse>().await?)
    }

    /// Consume `GET /api/v1/downloads/progress` forever, reconnecting after
    /// drops with a fixed backoff. Terminates when the receiver is dropped
    /// (i.e. the app is shutting down).
    pub async fn run_progress_stream(&self, tx: mpsc::Sender<ProgressMsg>) {
        let mut delay = SSE_RECONNECT_INITIAL;
        loop {
            let mut connected = false;
            if let Err(e) = self.consume_progress_stream(&tx, &mut connected).await {
                debug!(error = %e, "progress stream ended with error");
            }
            if connected {
                // A stream that came up and later dropped is a fresh outage,
                // not a continuation of the previous one.
                delay = SSE_RECONNECT_INITIAL;
            }
            if tx.send(ProgressMsg::Connected(false)).await.is_err()
                || tx.send(ProgressMsg::Retrying(delay)).await.is_err()
            {
                return;
            }
            debug!(
                retry_in_secs = delay.as_secs(),
                "progress stream reconnect scheduled"
            );
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = tx.closed() => return,
            }
            delay = next_backoff(delay);
        }
    }

    /// One connection attempt against the SSE endpoint: reads the byte stream,
    /// splits it into lines (splitting raw bytes on `\n` is UTF-8 safe — the
    /// newline byte never occurs inside a multi-byte sequence), parses frames
    /// and forwards progress events. Returns when the stream ends or fails.
    async fn consume_progress_stream(
        &self,
        tx: &mpsc::Sender<ProgressMsg>,
        connected: &mut bool,
    ) -> Result<(), ApiClientError> {
        let resp = self
            .http
            .get(self.url("/api/v1/downloads/progress"))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(Self::api_error(resp).await);
        }
        *connected = true;
        if tx.send(ProgressMsg::Connected(true)).await.is_err() {
            return Ok(());
        }
        debug!("progress stream connected");

        let mut stream = resp.bytes_stream();
        let mut parser = SseParser::new();
        let mut buf: Vec<u8> = Vec::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            buf.extend_from_slice(&chunk);
            while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line);
                let line = line.trim_end_matches('\n');
                if let Some(data) = parser.feed(line) {
                    match serde_json::from_str::<ProgressEvent>(&data) {
                        Ok(event) => {
                            if tx.send(ProgressMsg::Progress(event)).await.is_err() {
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, data = %data, "unparseable progress event");
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// Double `delay`, capped at [`SSE_RECONNECT_MAX`].
fn next_backoff(delay: Duration) -> Duration {
    delay.saturating_mul(2).min(SSE_RECONNECT_MAX)
}
