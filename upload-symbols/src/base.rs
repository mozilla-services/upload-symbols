use crate::{Error, Result};
use reqwest::Url;
use serde::{Deserialize, de::DeserializeOwned};
use std::time::Duration;
use tokio::{sync::Semaphore, time::sleep};
use tracing::{Instrument, debug, debug_span, instrument};

/// The base client with commmon functionality for both versions of the upload API.
#[derive(Clone, Debug)]
pub struct Client {
    pub client: reqwest::Client,
    pub base_url: Url,
    pub auth_token: String,
}

impl Client {
    /// Perform an authenticated request to the symbols server.
    pub fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            // We validate the URL in the builder to make sure it can be used as a base URL.
            // The `path` is a hardcoded string from this library, so `join()` can't return an
            // error here and we can unwrap.
            .request(method, self.base_url.join(path).unwrap())
            .header("auth-token", &self.auth_token)
    }
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
    pub async fn request<F, R>(&self, prepare: F) -> Result<R>
    where
        F: AsyncFn() -> Result<reqwest::RequestBuilder>,
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
