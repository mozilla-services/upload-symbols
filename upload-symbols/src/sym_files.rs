//! Utilities to discover and handle symbols files.

use std::{
    ffi::OsStr,
    fs::File,
    io,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

/// A reference to a symbols file on the filesystem.
#[derive(Clone, Debug)]
pub struct SymbolsFile {
    /// The filesystem path of the underlying file.
    path: PathBuf,
    /// The key that should be used for the file on the symbols server.
    key: String,
}

impl SymbolsFile {
    pub fn new<P, S>(path: P, key: S) -> Self
    where
        P: Into<PathBuf>,
        S: Into<String>,
    {
        Self {
            path: path.into(),
            key: key.into(),
        }
    }

    // Return the filesystem path for this symbols file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Return the storage key for this symbols file.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Return the storage key for this symbols file.
    pub fn into_key(self) -> String {
        self.key
    }

    /// Return whether the file is compressed. We currently assume that all file types except
    /// for Breakpad symobls files are already compressed.
    pub fn is_compressed(&self) -> bool {
        self.path
            .extension()
            .map(OsStr::to_ascii_lowercase)
            .as_deref()
            != Some(OsStr::new("sym"))
    }

    /// Open the symbols file and return the `File` instance.
    pub fn open(&self) -> io::Result<File> {
        File::open(&self.path)
    }
}

/// Errors for invalid keys found during symbols file discovery.
#[derive(Debug, thiserror::Error)]
pub enum InvalidKeyError {
    #[error("path does not have exactly three components: {0}")]
    NotThreeComponents(PathBuf),
    #[error("path not valid UTF-8: {0}")]
    PathNotValidUtf8(PathBuf),
    #[error("debug_id must be a hex string: {0}")]
    InvalidDebugId(String),
    #[error("key contains invalid characters: {0}")]
    InvalidCharacters(String),
    #[error("error while traversing diretory tree")]
    WalkDirError(#[from] walkdir::Error),
}

/// Discover all symbols files in a directory.
///
/// The symbols files must be laid out in a way that their eventual storage key on the symbols
/// server can be derived from their path relative to the root directory. The relative path for
/// each file should be of the form
/// ```text
/// <debug_file>/<debug_id>/<sym_file>
/// ```
/// The iterator will return `SymbolFile` instances for all regular files found. Entries in the
/// directory tree that aren't regular files are silently ignored, unless they are symlinks
/// pointing to regular files. For files with paths that aren't valid symbols files keys
/// [`InvalidKeyError`] is returned. Iteration continues normally after errors.
pub fn discover<P: Into<PathBuf>>(
    root: P,
) -> impl Iterator<Item = Result<SymbolsFile, InvalidKeyError>> {
    Discovery::new(root.into())
}

struct Discovery {
    root: PathBuf,
    walker: walkdir::IntoIter,
}

impl Discovery {
    fn new(root: PathBuf) -> Self {
        let walker = WalkDir::new(&root).follow_links(true).into_iter();
        Self { root, walker }
    }
}

impl Iterator for Discovery {
    type Item = Result<SymbolsFile, InvalidKeyError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.walker.next()? {
                Ok(entry) => {
                    if !entry.file_type().is_file() {
                        // Silently skip everything that's not a regular file. Since we set
                        // `follow_links`, symlinks resolving to regular files will also be
                        // considered files and not skipped.
                        continue;
                    }
                    // Paths of entries are guaranteed to start with `root`, so we can unwrap.
                    // This is also true for symlinks; `entry.path()` still returns the path of
                    // the link, not the path of the target.
                    let rel_path = entry.path().strip_prefix(&self.root).unwrap();
                    if entry.depth() != 3 {
                        // Everything that isn't exactly at depth 3 is not of the form
                        // `<debug_file>/<debug_id>/<symbols_file>`.
                        return Some(Err(InvalidKeyError::NotThreeComponents(rel_path.into())));
                    }
                    let Some(key) = rel_path.to_str() else {
                        return Some(Err(InvalidKeyError::PathNotValidUtf8(rel_path.into())));
                    };
                    // We know the path must contain two slashes, so we can unwrap.
                    let (_, rest) = key.split_once('/').unwrap();
                    let (debug_id, _) = rest.split_once('/').unwrap();
                    if !debug_id.chars().all(|c| c.is_ascii_hexdigit()) {
                        // The debug_id is not a hex string; ignore the file.
                        return Some(Err(InvalidKeyError::InvalidDebugId(key.into())));
                    }
                    if key.chars().any(is_invalid_char) {
                        return Some(Err(InvalidKeyError::InvalidCharacters(key.into())));
                    }
                    let symbols_file = SymbolsFile::new(entry.path(), key);
                    return Some(Ok(symbols_file));
                }
                Err(e) => return Some(Err(InvalidKeyError::from(e))),
            }
        }
    }
}

/// Check for characters we consider invalid in symbol file keys.
///
/// This is taken from `tecken/base/utils.py`; these are the chracters we currently reject on
/// the server side, both during upload and during download. Originally this was inspired by
/// the restriction S3 places on object keys.
fn is_invalid_char(c: char) -> bool {
    !c.is_ascii()
        || c.is_ascii_control()
        || matches!(
            c,
            '\\' | '^' | '`' | '<' | '>' | '{' | '}' | '[' | ']' | '#' | '%' | '"' | '\'' | '|'
        )
}
