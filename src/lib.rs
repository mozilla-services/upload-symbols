use std::path::{Path, PathBuf};
use url::Url;

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

#[derive(Clone, Debug)]
pub struct Client {
    client: reqwest::Client,
    base_url: Url,
    auth_token: String,
}

impl Client {
    pub fn new<S: Into<String>>(client: reqwest::Client, base_url: Url, auth_token: S) -> Self {
        Self {
            client,
            base_url,
            auth_token: auth_token.into(),
        }
    }

    pub async fn upload_directory<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = std::fs::canonicalize(path.as_ref())?;
        if !path.is_dir() {
            return Err(Error::NotADirectory(path));
        }
        v1::upload_directory(self, &path).await
    }

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
