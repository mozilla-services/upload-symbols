//! A library for uploading files to the Mozilla Symbols Server.
//!
//! This library provides a [`Client`] to upload a directory of files to the [Mozilla Symbols
//! Server](https://symbols.mozilla.org/).

use std::{
    path::{Path, PathBuf},
    sync::Arc,
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
}

impl Client {
    /// Create a new upload client.
    ///
    /// The [`reqwest::Client`] should have a meaningful, custom user agent. The `base_url` of
    /// the production Mozilla Symbols Server is <https://symbols.mozilla.org/>. You can can
    /// obtain an `auth_token` from the web interface of the symbols server (provided you have
    /// an account with upload permissions).
    pub fn new<S: Into<String>>(client: reqwest::Client, base_url: Url, auth_token: S) -> Self {
        Self {
            client,
            base_url,
            auth_token: auth_token.into(),
            conn_limit_upload_v1: Arc::new(Semaphore::new(3)), // TODO(smarnach): Make configurable
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

pub mod sym_files;
mod tmpdir;
mod v1;

#[cfg(test)]
mod tests {}
