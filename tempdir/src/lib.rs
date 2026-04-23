//! A temporary directory that gets deleted once it goes out of scope.
//!
//! The [`TempDir`] type in this crate is similar to the one provided by the `tempdir` crate.
//! That crate appears to be unmaintained and pulls in some dependencies we don't need. The
//! functionality is easy enough to re-implement, so we can avoid these dependencies.

use rand::{RngExt, distr::Alphanumeric};
use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct TempDir {
    path: Option<PathBuf>,
}

impl TempDir {
    pub fn new<S: AsRef<OsStr>>(prefix: S) -> io::Result<Self> {
        let tmp_dir = std::fs::canonicalize(std::env::temp_dir())?;
        let mut rng = rand::rng();
        for _ in 0..10 {
            let suffix = unsafe {
                // Safety: alphanumeric ASCII bytes can be safely converted to an OS string
                // on all platforms.
                OsString::from_encoded_bytes_unchecked(
                    (0..12).map(|_| rng.sample(Alphanumeric)).collect(),
                )
            };
            let basename: OsString = [prefix.as_ref(), &suffix].into_iter().collect();
            let path = tmp_dir.join(basename);
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path: Some(path) }),
                Err(ref e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "too many temporary directories already exist",
        ))
    }

    pub fn path(&self) -> &Path {
        self.path.as_ref().unwrap()
    }

    pub fn close(mut self) -> io::Result<()> {
        self.path.take().map(std::fs::remove_dir_all).unwrap()
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        self.path()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        self.path.take().map(std::fs::remove_dir_all);
    }
}
