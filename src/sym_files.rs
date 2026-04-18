//! Utilities to discover and handle symbols files.

use crate::{Error, Result};
use std::{ffi::OsStr, fs::File, io, path::PathBuf};
use walkdir::WalkDir;

/// A reference to a symbols file on the filesystem.
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

    /// Return the storage key for this symbols file.
    pub fn key(&self) -> &str {
        &self.key
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

/// Discover all symbols files in a directory.
///
/// The symbols files must be laid out in a way that their eventual storage key on the symbols
/// server can be derived from their path relative to the root directory. The relative path for
/// each file should be of the form
///
///     <debug_file>/<debug_id>/<sym_file>
///
/// The iterator will return SymbolFile instances for all regular files found. Entries in the
/// directory tree that aren't regular files are silently ignored, unless they are symlinks
/// pointing to regular files. For files that don't have the above path structure, and
/// Error::IgnoredFile error is returned. For files with non-UTF8 paths Error::PathNotValidUtf8
/// is returned.
pub fn discover<P: Into<PathBuf>>(root: P) -> impl Iterator<Item = Result<SymbolsFile>> {
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
    type Item = Result<SymbolsFile>;

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
                    if entry.depth() != 3 {
                        // Everything that isn't exactly at depth 3 is not of the form
                        // `<debug_file>/<debug_id>/<symbols_file>`, so we can ignore it.
                        return Some(Err(Error::IgnoredFile(entry.into_path())));
                    }
                    // Paths of entries are guaranteed to start with `root`, so we can unwrap.
                    // This is also true for symlinks; `entry.path()` still returns the path of
                    // the link, not the path of the target.
                    let rel_path = entry.path().strip_prefix(&self.root).unwrap();
                    let Some(key) = rel_path.to_str() else {
                        return Some(Err(Error::PathNotValidUtf8(entry.into_path())));
                    };
                    // We know the path must contain two slashes, so we can unwrap.
                    let (_, rest) = key.split_once('/').unwrap();
                    let (debug_id, _) = rest.split_once('/').unwrap();
                    if !debug_id.chars().all(|c| c.is_ascii_hexdigit()) {
                        // The debug_id is not a hex string; ignore the file.
                        return Some(Err(Error::IgnoredFile(entry.into_path())));
                    }
                    // TODO(smarnach): Check for other undesirable characters in the key.
                    let symbols_file = SymbolsFile::new(entry.path(), key);
                    return Some(Ok(symbols_file));
                }
                Err(e) => return Some(Err(Error::from(e))),
            }
        }
    }
}
