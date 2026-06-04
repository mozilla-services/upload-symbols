//! A library for uploading files to the Mozilla Symbols Server.
//!
//! This library provides a [`Client`] to upload a directory of files to the [Mozilla Symbols
//! Server](https://symbols.mozilla.org/).

use reqwest::Url;
use std::{
    fmt::Debug,
    path::{Path, PathBuf},
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
            v1: Default::default(),
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
        };
        info!(monotonic_counter.files_uploaded = summary.uploaded_keys.len());
        info!(monotonic_counter.files_skipped = summary.skipped_keys.len());
        info!(monotonic_counter.files_failed = summary.failed_keys.len());
        info!(monotonic_counter.discovery_errors = summary.discovery_errors.len());
        info!(monotonic_counter.upload_errors = summary.upload_errors.len());
        Ok(summary)
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

/// A configurable builder for a [`Client`].
#[derive(Debug)]
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

    /// Settings for the v1 upload API.
    #[cfg_attr(feature = "clap", command(flatten))]
    v1: v1::Config,
}

impl ClientBuilder {
    /// Build the [`Client`].
    ///
    /// This can fail if no `http_client` was provided and building the default
    /// [`reqwest::Client`] fails.
    pub async fn build(self) -> Result<Client> {
        let client = match self.client {
            Some(client) => client,
            None => reqwest::Client::builder().user_agent(USER_AGENT).build()?,
        };
        let base_url = Self::validate_base_url(self.base_url)?;
        let base = base::Client::new(client, base_url, self.auth_token);
        let auth_info = base.get_auth_info().await?;
        let inner = match (self.upload_api_version, auth_info.upload_api_version) {
            (UploadApiVersion::V1, _) | (UploadApiVersion::Auto, 1) => {
                ClientInner::V1(v1::Client::new(base, self.v1))
            }
            (UploadApiVersion::V2, _) | (UploadApiVersion::Auto, 2) => {
                return Err(Error::NotImplemented(UploadApiVersion::V2));
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

    /// Set the base URL of the symbols server to upload to.
    ///
    /// This defaults to <https://symbols.mozilla.org/>.
    pub fn base_url(mut self, base_url: Url) -> Self {
        self.base_url = Some(base_url);
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
}

#[derive(Debug)]
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
}

static USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"),);

mod base;
pub mod sym_files;
mod v1;

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
