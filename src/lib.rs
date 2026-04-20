//! A library for uploading files to the Mozilla Symbols Server.
//!
//! This library provides a [`Client`] to upload a directory of files to the [Mozilla Symbols
//! Server](https://symbols.mozilla.org/).

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Semaphore;
use url::Url;

/// Errors that may occur while uploading symbols.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a directory: {0}")]
    NotADirectory(PathBuf),
    #[error("invalid base URL")]
    UrlParseError(#[from] url::ParseError),
    #[error("I/O error")]
    IOError(#[from] std::io::Error),
    #[error("ZIP archiver error")]
    ZipError(#[from] zip::result::ZipError),
    #[error("error sending HTTP request")]
    ReqwestError(#[from] reqwest::Error),
    #[error("error while traversing diretory tree")]
    WalkDirError(#[from] walkdir::Error),
    #[error("ignored file: {0}")]
    IgnoredFile(PathBuf),
    #[error("path not valid UTF-8: {0}")]
    PathNotValidUtf8(PathBuf),
    #[error("bad request to symbols server: {0}")]
    SymbolsServerBadRequest(String),
}

type Result<T> = std::result::Result<T, Error>;

/// The Mozill Symbols Server upload client.
///
/// The main functionality is provided by the [`Client::upload_directory`] method.
///
/// Clients are relatively cheap to clone. Clones will share the underlying [`reqwest::Client`]
/// (which uses [`Arc`] internally) and the limit on concurrent connections to the server.
#[derive(Clone, Debug)]
pub struct Client {
    client: reqwest::Client,
    base_url: Url,
    auth_token: String,
    /// The current upload API doesn't handle load spikes gracefully, so we limit the number
    /// of concurrent connections.
    conn_limit_upload_v1: Arc<Semaphore>,
    zip_size_threshold_v1: u64,
    retries_v1: usize,
    retry_delay_v1: Duration,
}

impl Client {
    /// Return a [`ClientBuilder`] instance with a default configuration.
    pub fn builder<S: Into<String>>(auth_token: S) -> ClientBuilder {
        ClientBuilder {
            client: None,
            base_url: None,
            auth_token: auth_token.into(),
            max_connections_v1: 3,
            zip_size_threshold_v1: 1 << 26, // 64 MiB
            retries_v1: 2,
            retry_delay_v1: Duration::from_secs(120),
        }
    }

    /// Upload a directory on the filesystem to the symbols server.
    ///
    /// The files to be uploaded are discovered using [`sym_files::discover`].
    pub async fn upload_directory<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = std::fs::canonicalize(path.as_ref())?;
        if !path.is_dir() {
            return Err(Error::NotADirectory(path));
        }
        v1::upload_directory(self, &path).await
    }

    /// Perform an authenticated request to the symbols server.
    fn request(&self, method: reqwest::Method, path: &str) -> Result<reqwest::RequestBuilder> {
        let builder = self
            .client
            .request(method, self.base_url.join(path)?)
            .header("auth-token", &self.auth_token);
        Ok(builder)
    }
}

/// A configurable builder for a [`Client`].
#[derive(Debug)]
pub struct ClientBuilder {
    client: Option<reqwest::Client>,
    base_url: Option<Url>,
    auth_token: String,
    max_connections_v1: usize,
    zip_size_threshold_v1: u64,
    retries_v1: usize,
    retry_delay_v1: Duration,
}

impl ClientBuilder {
    /// Build the [`Client`].
    ///
    /// This can fail if no `http_client` was provided and building the default
    /// [`reqwest::Client`] fails.
    pub fn build(self) -> Result<Client> {
        let client = Client {
            client: match self.client {
                Some(client) => client,
                None => reqwest::Client::builder().user_agent(USER_AGENT).build()?,
            },
            base_url: self
                .base_url
                .unwrap_or_else(|| Url::parse("https://symbols.mozilla.org/").unwrap()),
            auth_token: self.auth_token,
            conn_limit_upload_v1: Arc::new(Semaphore::new(self.max_connections_v1)),
            zip_size_threshold_v1: self.zip_size_threshold_v1,
            retries_v1: self.retries_v1,
            retry_delay_v1: self.retry_delay_v1,
        };
        Ok(client)
    }

    /// Provide a custom [`reqwest::Client`] to perform HTTP requests.
    ///
    /// The client should have a meaningful custom user agent string.
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.client = Some(client);
        self
    }

    /// Set the base URL of the symbols server to upload to.
    ///
    /// This defaults to <https://symbols.mozilla.org/>.
    pub fn base_url(mut self, base_url: Url) -> Self {
        self.base_url = Some(base_url);
        self
    }

    /// Set the maximum number of concurrent uploads using the v1 upload API.
    ///
    /// The default is 3.
    pub fn max_connections_v1(mut self, max_connections_v1: usize) -> Self {
        self.max_connections_v1 = max_connections_v1;
        self
    }

    /// Set the ZIP archive size threshold.
    ///
    /// When building ZIP archives for v1 of the upload API, a new archive is started once the
    /// size of the current archive exceeds this threshold. ZIP archives still can get much
    /// bigger than this value since member files can be big.
    ///
    /// The default is 64 MiB.
    pub fn zip_size_threshold_v1(mut self, zip_size_threshold_v1: u64) -> Self {
        self.zip_size_threshold_v1 = zip_size_threshold_v1;
        self
    }

    /// Set the number of retries for the version 1 upload API.
    ///
    /// On retriable status codes, uploading ZIP archives is retried this number of times, in
    /// addition to the original request. A value of 0 disables retrying.
    ///
    /// The default is 2.
    pub fn retries_v1(mut self, retries_v1: usize) -> Self {
        self.retries_v1 = retries_v1;
        self
    }

    /// Set the delay between retries for version 1 of the upload API.
    ///
    /// The default is 120 seconds.
    pub fn retry_delay_v1_seconds(mut self, retry_delay_v1_seconds: u64) -> Self {
        self.retry_delay_v1 = Duration::from_secs(retry_delay_v1_seconds);
        self
    }
}

static USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"),);

pub mod sym_files;
mod tmpdir;
mod v1;
