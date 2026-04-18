use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("error while traversing diretory tree")]
    WalkDirError(#[from] walkdir::Error),
    #[error("ignored file: {0}")]
    IgnoredFile(PathBuf),
    #[error("path not valid UTF-8: {0}")]
    PathNotValidUtf8(PathBuf),
}

type Result<T> = std::result::Result<T, Error>;

pub mod sym_files;
mod tmpdir;

#[cfg(test)]
mod tests {}
