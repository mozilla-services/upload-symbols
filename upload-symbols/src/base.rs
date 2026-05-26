use crate::Error;
use reqwest::{Method, Url};
use serde::{Deserialize, Deserializer, de::DeserializeOwned};
use std::{
    collections::HashMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::Semaphore, time::sleep};
use tracing::{Instrument, debug, debug_span, instrument};

/// The base client with commmon functionality for both versions of the upload API.
#[derive(Clone, Debug)]
pub struct Client {
    client: reqwest::Client,
    base_url: Url,
    auth_token: String,
}

impl Client {
    pub fn new(client: reqwest::Client, base_url: Url, auth_token: String) -> Self {
        Self {
            client,
            base_url,
            auth_token,
        }
    }

    /// Perform an authenticated request to the symbols server.
    pub fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            // We validate the URL in the builder to make sure it can be used as a base URL.
            // The `path` is a hardcoded string from this library, so `join()` can't return an
            // error here and we can unwrap.
            .request(method, self.base_url.join(path).unwrap())
            .header("auth-token", &self.auth_token)
    }

    pub async fn get_auth_info(&self) -> crate::Result<AuthInfo> {
        Retry::builder()
            .delay_seconds(2)
            .delay_factor(1.5)
            .build()
            .request(async move || Ok(self.request(Method::POST, "/upload/auth_info/")))
            .await
    }
}

#[derive(Clone, Debug, Deserialize)]
#[allow(unused)]
pub struct AuthInfo {
    pub email: String,
    pub try_symbols: bool,
    #[serde(deserialize_with = "deserialize_system_time")]
    pub token_expires_at: SystemTime,
    pub upload_api_version: u32,
    pub opentelemetry: Option<OpenTelemetryConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[allow(unused)]
pub struct OpenTelemetryConfig {
    pub endpoint: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub resource_attributes: HashMap<String, String>,
    #[serde(deserialize_with = "deserialize_log_level")]
    pub log_level: tracing::Level,
}

fn deserialize_system_time<'de, D>(deserializer: D) -> Result<SystemTime, D::Error>
where
    D: Deserializer<'de>,
{
    let timestamp = u64::deserialize(deserializer)?;
    UNIX_EPOCH
        .checked_add(Duration::from_secs(timestamp))
        .ok_or_else(|| serde::de::Error::custom("Unix timestamp is out of range"))
}

fn deserialize_log_level<'de, D>(deserializer: D) -> Result<tracing::Level, D::Error>
where
    D: Deserializer<'de>,
{
    let level = String::deserialize(deserializer)?;
    level.parse().map_err(serde::de::Error::custom)
}

#[derive(Debug)]
pub struct Retry {
    retries: usize,
    delay: Duration,
    delay_factor: f64,
    conn_limit: Option<Semaphore>,
}

impl Retry {
    pub fn builder() -> RetryBuilder {
        RetryBuilder {
            retries: 4,
            delay_seconds: 2,
            delay_factor: 1.0,
            max_connections: None,
        }
    }

    #[instrument(level = "debug", skip(self, prepare))]
    pub async fn request<F, R>(&self, prepare: F) -> crate::Result<R>
    where
        F: AsyncFn() -> crate::Result<reqwest::RequestBuilder>,
        R: DeserializeOwned,
    {
        let mut remaining_retries = self.retries;
        let mut delay = self.delay;
        loop {
            let request = prepare().await?;
            let permit = match self.conn_limit {
                // We know the semaphore hasn't been closed, so we can unwrap.
                Some(ref semaphore) => Some(semaphore.acquire().await.unwrap()),
                None => None,
            };
            let response = request
                .send()
                .instrument(debug_span!("Symbols Server request"))
                .await?;
            let status = response.status().as_u16();
            debug!("Symbols Server request status {status}");
            match status {
                429 | 502 | 503 | 504 => {
                    if remaining_retries == 0 {
                        return Err(response.error_for_status().unwrap_err().into());
                    }
                    drop(permit);
                    sleep(delay).await;
                    remaining_retries -= 1;
                    delay = delay.mul_f64(self.delay_factor);
                    continue;
                }
                400..500 => {
                    // For 400s, the symbols server returns an error message.
                    let server_error: ServerError = response.json().await?;
                    let msg = server_error.error;
                    return Err(Error::SymbolsServer4xx { status, msg });
                }
                _ => {}
            }
            let json_response: R = response.error_for_status()?.json().await?;
            debug!(
                "Symbols Server request successful after {} attempt(s)",
                self.retries + 1 - remaining_retries
            );
            return Ok(json_response);
        }
    }
}

#[derive(Deserialize)]
pub struct ServerError {
    error: String,
}

#[derive(Clone, Debug)]
pub struct RetryBuilder {
    retries: usize,
    delay_seconds: u64,
    delay_factor: f64,
    max_connections: Option<u32>,
}

impl RetryBuilder {
    pub fn build(self) -> Retry {
        Retry {
            retries: self.retries,
            delay: Duration::from_secs(self.delay_seconds),
            delay_factor: self.delay_factor,
            conn_limit: self.max_connections.map(|m| Semaphore::new(m as _)),
        }
    }

    pub fn retries(mut self, retries: usize) -> Self {
        self.retries = retries;
        self
    }

    pub fn delay_seconds(mut self, delay_seconds: u64) -> Self {
        self.delay_seconds = delay_seconds;
        self
    }

    #[allow(unused)]
    pub fn delay_factor(mut self, delay_factor: f64) -> Self {
        self.delay_factor = delay_factor;
        self
    }

    pub fn max_connections(mut self, max_connections: u32) -> Self {
        self.max_connections = Some(max_connections);
        self
    }
}
