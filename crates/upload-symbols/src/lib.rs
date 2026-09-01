//! A library for uploading files to the Mozilla Symbols Server.
//!
//! This library provides a [`Client`] to upload a directory of files to the [Mozilla Symbols
//! Server](https://symbols.mozilla.org/).

use reqwest::{Url, header::HeaderValue, tls};
use std::{
    fmt::Debug,
    path::{Path, PathBuf},
    time::Duration,
};
use tracing::{info, instrument};

/// Errors that may occur while uploading symbols.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a directory: {0}")]
    NotADirectory(PathBuf),
    #[error("URL must have http or https scheme: {0}")]
    InvalidBaseUrlScheme(Url),
    #[error("I/O error: {0}")]
    IOError(#[from] std::io::Error),
    #[error("ZIP archiver error: {0}")]
    ZipError(#[from] zip::result::ZipError),
    #[error("error sending HTTP request: {0}")]
    ReqwestError(#[from] reqwest::Error),
    #[error("status {status} response from symbols server: {msg}")]
    SymbolsServer4xx { status: u16, msg: String },
    #[error("upload client not implemented: {0:?}")]
    NotImplemented(UploadApiVersion),
    #[error("auth token must contain only hex digits")]
    InvalidAuthToken,
    #[error("missing range header in GCS response")]
    MissingRangeHeader,
    #[error("invalid range header in GCS response: {0:?}")]
    InvalidRangeHeader(HeaderValue),
    #[error("status {status} response from GCS: {msg}")]
    GcsError { status: u16, msg: String },
    #[error("invalid Symbols Server response: {msg}")]
    InvalidSymbolsServerResponse { msg: String },
}

type Result<T> = std::result::Result<T, Error>;

/// The Mozilla Symbols Server upload client.
///
/// The main functionality is provided by the [`Client::upload_directory`] method.
///
/// Clients are relatively cheap to clone. Clones will share the underlying [`reqwest::Client`]
/// (which uses `Arc` internally) and the limit on concurrent connections to the server.
#[derive(Clone, Debug)]
pub struct Client {
    inner: ClientInner,
    auth_info: AuthInfo,
}

#[derive(Clone, Debug)]
enum ClientInner {
    V1(v1::Client),
    V2(v2::Client),
}

pub use base::{AuthInfo, OpenTelemetryConfig};

impl Client {
    /// Return a [`ClientBuilder`] instance with a default configuration.
    pub fn builder<S: Into<String>>(auth_token: S) -> ClientBuilder {
        ClientBuilder {
            client: None,
            base_url: None,
            auth_token: auth_token.into(),
            upload_api_version: UploadApiVersion::Auto,
            connect_timeout_seconds: DEFAULT_CONNECT_TIMEOUT_SECONDS,
            read_timeout_seconds: DEFAULT_READ_TIMEOUT_SECONDS,
            v1: Default::default(),
            v2: Default::default(),
        }
    }

    /// Upload a directory on the filesystem to the symbols server.
    ///
    /// The files to be uploaded are discovered using [`sym_files::discover`].
    #[instrument(skip(self))]
    pub async fn upload_directory<P>(&self, path: P) -> Result<UploadSummary>
    where
        P: AsRef<Path> + Debug,
    {
        let path = std::fs::canonicalize(path.as_ref())?;
        if !path.is_dir() {
            return Err(Error::NotADirectory(path));
        }
        let summary = match self.inner {
            ClientInner::V1(ref inner) => inner.upload_directory(path).await?,
            ClientInner::V2(ref inner) => inner.upload_directory(path).await?,
        };
        info!(monotonic_counter.files_uploaded = summary.uploaded_keys.len());
        info!(monotonic_counter.files_skipped = summary.skipped_keys.len());
        info!(monotonic_counter.files_failed = summary.failed_keys.len());
        info!(monotonic_counter.discovery_errors = summary.discovery_errors.len());
        info!(monotonic_counter.upload_errors = summary.upload_errors.len());
        Ok(summary)
    }

    pub fn upload_api_version(&self) -> UploadApiVersion {
        match self.inner {
            ClientInner::V1(_) => UploadApiVersion::V1,
            ClientInner::V2(_) => UploadApiVersion::V2,
        }
    }

    pub fn auth_info(&self) -> &AuthInfo {
        &self.auth_info
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum UploadApiVersion {
    Auto,
    V1,
    V2,
}

impl std::fmt::Display for UploadApiVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UploadApiVersion::Auto => f.write_str("auto"),
            UploadApiVersion::V1 => f.write_str("1"),
            UploadApiVersion::V2 => f.write_str("2"),
        }
    }
}

const DEFAULT_CONNECT_TIMEOUT_SECONDS: u64 = 10;
const DEFAULT_READ_TIMEOUT_SECONDS: u64 = 600;

/// A configurable builder for a [`Client`].
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct ClientBuilder {
    #[cfg_attr(feature = "clap", arg(skip))]
    client: Option<reqwest::Client>,

    /// Set the base URL of the symbols server to upload to.
    ///
    /// This defaults to <https://symbols.mozilla.org/>.
    #[cfg_attr(feature = "clap", arg(long = "server-url"))]
    base_url: Option<Url>,

    /// A Mozilla Symbols Server authentication token with upload permissions.
    #[cfg_attr(
        feature = "clap",
        arg(long, required = true, env = "SYMBOLS_AUTH_TOKEN")
    )]
    auth_token: String,

    /// The upload API version to use.
    ///
    /// By default, the version is automatically detected by asking the server.
    #[cfg_attr(feature = "clap", arg(long, value_enum, default_value_t = UploadApiVersion::Auto))]
    upload_api_version: UploadApiVersion,

    /// Set the connect timeout for HTTP connections.
    ///
    /// This timeout only covers establishing a TCP connection to the server, and not
    /// transferring any data. The default is 10 seconds.
    #[cfg_attr(feature = "clap", arg(long, default_value_t = DEFAULT_CONNECT_TIMEOUT_SECONDS))]
    connect_timeout_seconds: u64,

    /// Set the socket read timeout for HTTP connections.
    ///
    /// This is the timeout for individual read operations on the socket. It does not establish
    /// a total timeout for reading the entire response.
    ///
    /// The default value is 600 seconds to account for the long processing times upload API v1
    /// requires to process uploaded ZIP archives.
    #[cfg_attr(feature = "clap", arg(long, default_value_t = DEFAULT_READ_TIMEOUT_SECONDS))]
    read_timeout_seconds: u64,

    /// Settings for the v1 upload API.
    #[cfg_attr(feature = "clap", command(flatten))]
    v1: v1::Config,

    /// Settings for the v2 upload API.
    #[cfg_attr(feature = "clap", command(flatten))]
    v2: v2::Config,
}

// Add custom Debug implementation to redact the auth_token.
impl Debug for ClientBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientBuilder")
            .field("client", &self.client)
            .field("base_url", &self.base_url)
            .field("auth_token", &"<redacted>")
            .field("upload_api_version", &self.upload_api_version)
            .field("connect_timeout_seconds", &self.connect_timeout_seconds)
            .field("read_timeout_seconds", &self.read_timeout_seconds)
            .field("v1", &self.v1)
            .field("v2", &self.v2)
            .finish()
    }
}

impl ClientBuilder {
    /// Build the [`Client`].
    ///
    /// This can fail if no `http_client` was provided and building the default
    /// [`reqwest::Client`] fails.
    pub async fn build(self) -> Result<Client> {
        let client = match self.client {
            Some(client) => client,
            None => reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .connect_timeout(Duration::from_secs(self.connect_timeout_seconds))
                .read_timeout(Duration::from_secs(self.read_timeout_seconds))
                .tls_version_min(tls::Version::TLS_1_2)
                .build()?,
        };
        let base_url = Self::validate_base_url(self.base_url)?;
        if self.auth_token.chars().any(|c| !c.is_ascii_hexdigit()) {
            return Err(Error::InvalidAuthToken);
        }
        // We already know the auth token only contains hex digits, so we can unwrap.
        let mut auth_token: HeaderValue = self.auth_token.try_into().unwrap();
        auth_token.set_sensitive(true);
        let base = base::Client::new(client, base_url, auth_token);
        let auth_info = base.get_auth_info().await?;
        let inner = match (self.upload_api_version, auth_info.upload_api_version) {
            (UploadApiVersion::V1, _) | (UploadApiVersion::Auto, 1) => {
                ClientInner::V1(v1::Client::new(base, self.v1))
            }
            (UploadApiVersion::V2, _) | (UploadApiVersion::Auto, 2) => {
                ClientInner::V2(v2::Client::new(base, self.v2))
            }
            _ => unreachable!("invalid API version returned by Symbols Server"),
        };
        Ok(Client { inner, auth_info })
    }

    // This function ensures that the base URL actually is an absolute URL with an http(s)
    // scheme. The [`url`] crate ensures that the host of such URLs is non-empty. We also add
    // a trailing slash to the path if it doesn't have one.
    fn validate_base_url(base_url: Option<Url>) -> Result<Url> {
        match base_url {
            Some(mut base_url) => {
                if base_url.scheme() != "http" && base_url.scheme() != "https" {
                    return Err(Error::InvalidBaseUrlScheme(base_url));
                }
                if !base_url.path().ends_with('/') {
                    // We already know the URL is an absolute http(s) URL, so
                    // `path_segments_mut()` can't return an error.
                    base_url.path_segments_mut().unwrap().push("");
                }
                Ok(base_url)
            }
            None => Ok(Url::parse("https://symbols.mozilla.org/").unwrap()),
        }
    }

    /// Provide a custom [`reqwest::Client`] to perform HTTP requests.
    ///
    /// The client should have a meaningful custom user agent string.
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.client = Some(client);
        self
    }

    /// Set the connect timeout for HTTP connections.
    ///
    /// This timeout only covers establishing a TCP connection to the server, and not
    /// transferring any data. The default is 10 seconds.
    pub fn connect_timeout_seconds(mut self, connect_timeout_seconds: u64) -> Self {
        self.connect_timeout_seconds = connect_timeout_seconds;
        self
    }

    /// Set the socket read timeout for HTTP connections.
    ///
    /// This is the timeout for individual read operations on the socket. It does not establish
    /// a total timeout for reading the entire response.
    pub fn read_timeout_seconds(mut self, read_timeout_seconds: u64) -> Self {
        self.read_timeout_seconds = read_timeout_seconds;
        self
    }

    /// Set the base URL of the symbols server to upload to.
    ///
    /// This defaults to <https://symbols.mozilla.org/>.
    pub fn base_url(mut self, base_url: Url) -> Self {
        self.base_url = Some(base_url);
        self
    }

    /// Set the upload API version to use.
    ///
    /// The default is [`UploadApiVersion::Auto`], which asks the symbols server which version
    /// to use.
    pub fn upload_api_version(mut self, upload_api_version: UploadApiVersion) -> Self {
        self.upload_api_version = upload_api_version;
        self
    }

    /// Set the maximum number of concurrent uploads using the v1 upload API.
    ///
    /// The default is 3. Panics if `max_connections` is 0.
    pub fn max_connections_v1(mut self, max_connections_v1: u32) -> Self {
        assert_ne!(max_connections_v1, 0, "must allow at least one connection");
        self.v1.max_connections_v1 = max_connections_v1;
        self
    }

    /// Set the ZIP archive size threshold in bytes.
    ///
    /// When building ZIP archives for v1 of the upload API, a new archive is started once the
    /// size of the current archive exceeds this threshold. ZIP archives still can get much
    /// bigger than this value since member files can be big.
    ///
    /// The default is 64 MiB.
    pub fn zip_size_threshold_v1(mut self, zip_size_threshold_v1: u64) -> Self {
        self.v1.zip_size_threshold_v1 = zip_size_threshold_v1;
        self
    }

    /// Set the number of retries for the version 1 upload API.
    ///
    /// On retriable status codes, uploading ZIP archives is retried this number of times, in
    /// addition to the original request. A value of 0 disables retrying.
    ///
    /// The default is 5.
    pub fn retries_v1(mut self, retries_v1: usize) -> Self {
        self.v1.retries_v1 = retries_v1;
        self
    }

    /// Set the delay in seconds between retries for version 1 of the upload API.
    ///
    /// The default is 60 seconds.
    pub fn retry_delay_v1_seconds(mut self, retry_delay_v1_seconds: u64) -> Self {
        self.v1.retry_delay_seconds_v1 = retry_delay_v1_seconds;
        self
    }

    /// Set the number of retries for Symbols Server requests.
    ///
    /// On retriable status codes, Symbols Server requests are retried this number of times, in
    /// addition to the original request. A value of 0 disables retrying.
    ///
    /// The default is 2.
    pub fn retries(mut self, retries: usize) -> Self {
        self.v2.retries = retries;
        self
    }

    /// Set the delay in seconds between Symbols Server request retries.
    ///
    /// The default is 30 seconds.
    pub fn retry_delay_seconds(mut self, retry_delay_seconds: u64) -> Self {
        self.v2.retry_delay_seconds = retry_delay_seconds;
        self
    }

    /// Set the number of symbols files per request to the Symbols Server.
    ///
    /// The default is 128.
    pub fn batch_size(mut self, batch_size: usize) -> Self {
        assert_ne!(batch_size, 0, "the batch size needs to be at least one");
        self.v2.batch_size = batch_size;
        self
    }

    /// Set the maximum number of concurrent file uploads to GCS.
    ///
    /// The default is 16. Panics if `max_file_uploads` is 0.
    pub fn max_file_uploads(mut self, max_file_uploads: u32) -> Self {
        assert_ne!(max_file_uploads, 0, "must allow at least one file upload");
        self.v2.max_file_uploads = max_file_uploads;
        self
    }

    /// Set the number of retries for individual file uploads to GCS.
    ///
    /// The number of retriable status codes that are accepted before bailing out for each file
    /// upload. A value of 0 disables retrying.
    ///
    /// The default is 10.
    pub fn file_upload_retries(mut self, file_upload_retries: usize) -> Self {
        self.v2.file_upload_retries = file_upload_retries;
        self
    }

    /// Set the retry delay in seconds between GCS upload request retries.
    ///
    /// The default is 1 second.
    pub fn file_upload_delay_seconds(mut self, file_upload_delay_seconds: u64) -> Self {
        self.v2.file_upload_delay_seconds = file_upload_delay_seconds;
        self
    }

    /// Set the chunk size for file uploads to GCS.
    ///
    /// The value will be rounded down to the next multiple of 256 KiB and must be positive.
    ///
    /// The default is 16 MiB.
    pub fn chunk_size(mut self, chunk_size: u64) -> Self {
        const MIN_CHUNK_SIZE: u64 = 1 << 18;
        assert!(
            chunk_size >= MIN_CHUNK_SIZE,
            "chunk_size must be at least 256 KiB"
        );
        self.v2.chunk_size = chunk_size & !(MIN_CHUNK_SIZE - 1);
        self
    }
}

#[derive(Debug, Default)]
pub struct UploadSummary {
    /// Keys of files that were successfully uploaded.
    pub uploaded_keys: Vec<String>,
    /// Keys of files that were skipped because they were already known to the server.
    pub skipped_keys: Vec<String>,
    /// Keys of files that were not successfully uploaded.
    pub failed_keys: Vec<String>,
    /// Errors during symbols file discovery.
    pub discovery_errors: Vec<sym_files::InvalidKeyError>,
    /// Errors during uploads.
    pub upload_errors: Vec<Error>,
}

impl UploadSummary {
    /// Indicate whether the upload completed successfully without any errors.
    pub fn success(&self) -> bool {
        self.discovery_errors.is_empty() && self.upload_errors.is_empty()
    }

    fn record_upload(&mut self, key: String, result: Result<()>) {
        match result {
            Ok(()) => self.uploaded_keys.push(key),
            Err(e) => {
                self.upload_errors.push(e);
                self.failed_keys.push(key);
            }
        }
    }

    fn merge(&mut self, other: UploadSummary) {
        self.uploaded_keys.extend(other.uploaded_keys);
        self.skipped_keys.extend(other.skipped_keys);
        self.failed_keys.extend(other.failed_keys);
        self.discovery_errors.extend(other.discovery_errors);
        self.upload_errors.extend(other.upload_errors);
    }
}

static USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

mod base;
pub mod sym_files;
mod v1;
mod v2;

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::Url;

    #[test]
    fn test_validate_base_url() {
        for (base_url, expected) in [
            (None, Ok("https://symbols.mozilla.org/")),
            (
                Some("https://symbols.allizom.org/"),
                Ok("https://symbols.allizom.org/"),
            ),
            (
                Some("https://symbols.mozilla.org/v1"),
                Ok("https://symbols.mozilla.org/v1/"),
            ),
            (Some("ftp://ftp.mozilla.org/"), Err(())),
        ] {
            let actual = ClientBuilder::validate_base_url(base_url.map(|u| Url::parse(u).unwrap()));
            match actual {
                Ok(base_url) => assert_eq!(Ok(base_url.as_str()), expected),
                Err(e) => {
                    if let Error::InvalidBaseUrlScheme(_) = e {
                        assert_eq!(Err(()), expected);
                    } else {
                        panic!("expected InvalidBaseUrl error");
                    }
                }
            }
        }
    }
}
