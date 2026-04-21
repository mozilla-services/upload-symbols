//! Client implementation for the original Mozilla Symbols Server upload endpoint.

use crate::{
    Client, Error, Result,
    sym_files::{InvalidKeyError, SymbolsFile},
};
use reqwest::{Method, multipart};
use serde::Deserialize;
use std::{
    io::Seek,
    path::{Path, PathBuf},
};
use tokio::{
    sync::mpsc,
    task::{JoinSet, spawn_blocking},
    time::sleep,
};
use zip::{CompressionMethod, ZipWriter};

/// Upload a directory of files to the Mozilla Symbols Server.
///
/// This function uses `crate::sym_files::discover()` to find symbols files under the given
/// `root` directory and uploads them to the Mozilla Symbols Server using the given client to
/// perform the HTTP requests. Only regular files are inlcuded.
///
/// Since the original version of the upload API only supports uploading ZIP archives, we first
/// need to create ZIP archives in a temporary directory before sending the actual HTTP
/// requests.
pub async fn upload_directory(client: &Client, root: &Path) -> Result<()> {
    // Create ZIP archives in a background thread so we can start uploading the first
    // archive as soon as it is ready.
    let (tx, mut rx) = mpsc::channel(64);
    let path = root.to_path_buf();
    let temp_dir = crate::tmpdir::TempDir::new("upload-symbols.")?;
    let temp_path = temp_dir.path().to_path_buf();
    let zip_size_threshold = client.zip_size_threshold_v1;
    let create_zip_handle =
        spawn_blocking(move || create_zip_archives(tx, path, temp_path, zip_size_threshold));

    // Upload ZIP archives as they get created.
    let mut set = JoinSet::new();
    while let Some((zip_archive_path, keys)) = rx.recv().await {
        let client = client.clone();
        set.spawn(async move { (upload_zip_archive(client, zip_archive_path).await, keys) });
    }

    // Unwrap the outer JoinError. This will basically propagate panics.
    create_zip_handle.await.unwrap()?;

    let mut result = Ok(());
    while let Some(join_result) = set.join_next().await {
        // Unwrap the outer result to propagate panics.
        let (upload_result, _keys) = join_result.unwrap();
        // TODO(smarnach) Instead of just returning the first error, we should do something
        // more reasonable. We should probably also collect the information about skipped files
        // and pass it back to the caller.
        result = result.and(upload_result);
    }

    // Explicitly close temp_dir so we can propagate any errors.
    temp_dir.close()?;

    result
}

/// Create ZIP archives for all symbols files in the given directory.
fn create_zip_archives(
    tx: mpsc::Sender<(PathBuf, Vec<String>)>,
    root: PathBuf,
    temp_path: PathBuf,
    file_size_threshold: u64,
) -> Result<Vec<InvalidKeyError>> {
    let mut zip_path_iter = (0..).map(|i| temp_path.join(format!("symbols-{i}.zip")));
    let mut current_zip_archive = None;
    let mut errors = vec![];
    for sym_file in crate::sym_files::discover(&root) {
        let Ok(sym_file) = sym_file else {
                errors.push(sym_file.unwrap_err());
                continue;
        };
        let zip_archive = if let Some(ref mut zip_archive) = current_zip_archive {
            zip_archive
        } else {
            let zip_path = zip_path_iter.next().unwrap();
            current_zip_archive = Some(ZipArchive::new(zip_path)?);
            current_zip_archive.as_mut().unwrap()
        };
        zip_archive.add_sym_file(sym_file)?;
        if zip_archive.size()? >= file_size_threshold {
            current_zip_archive.take().unwrap().finish(&tx)?;
        }
    }
    if let Some(zip_archive) = current_zip_archive {
        zip_archive.finish(&tx)?;
    }
    Ok(errors)
}

struct ZipArchive {
    path: PathBuf,
    writer: ZipWriter<std::fs::File>,
    keys: Vec<String>,
}

impl ZipArchive {
    fn new(path: PathBuf) -> std::io::Result<Self> {
        let file = std::fs::File::create_new(&path)?;
        let writer = ZipWriter::new(file);
        let keys = vec![];
        Ok(Self { path, writer, keys })
    }

    fn add_sym_file(&mut self, sym_file: SymbolsFile) -> Result<()> {
        let options = zip::write::SimpleFileOptions::default().compression_method(
            if sym_file.is_compressed() {
                CompressionMethod::Stored
            } else {
                CompressionMethod::Deflated
            },
        );
        self.writer.start_file(sym_file.key(), options)?;
        std::io::copy(&mut sym_file.open()?, &mut self.writer)?;
        self.keys.push(sym_file.into_key());
        Ok(())
    }

    fn size(&self) -> std::io::Result<u64> {
        // We know the ZipWriter isn't closed yet, so we can unwrap.
        self.writer.get_ref().unwrap().stream_position()
    }

    fn finish(self, tx: &mpsc::Sender<(PathBuf, Vec<String>)>) -> zip::result::ZipResult<()> {
        self.writer.finish()?;
        // We know the receiver hasn't hung up yet, so we can unwrap.
        tx.blocking_send((self.path, self.keys)).unwrap();
        Ok(())
    }
}

async fn upload_zip_archive(client: Client, path: PathBuf) -> Result<()> {
    let mut remaining_retries = client.retries_v1;
    // We know the file name is of the form `symbols-{i}.zip`. So we can unwrap the result of
    // `file_name()`, as there must be a file name. We can also unwrap the result of to_str(),
    // since the file name only contain ASCII characters.
    let file_name = String::from(path.file_name().unwrap().to_str().unwrap());
    loop {
        let form = multipart::Form::new()
            .file(file_name.clone(), &path)
            .await?;
        // We know the semaphore hasn't been closed, so we can unwrap.
        let permit = client.conn_limit_upload_v1.acquire().await.unwrap();
        let response = client
            .request(Method::POST, "/upload/")?
            .multipart(form)
            .send()
            .await?;
        match response.status().as_u16() {
            429 | 502 | 503 | 504 => {
                if remaining_retries == 0 {
                    return Err(response.error_for_status().unwrap_err().into());
                }
                remaining_retries -= 1;
                drop(permit);
                sleep(client.retry_delay_v1).await;
                continue;
            }
            400 => {
                // For 400s, the symbols server returns an error message.
                let server_error = response.json::<ServerError>().await?;
                return Err(Error::SymbolsServerBadRequest(server_error.error));
            }
            _ => {
                response.error_for_status_ref()?;
            }
        }
        // TODO(smarnach): Extract skipped keys from response.
        break;
    }
    Ok(())
}

#[derive(Deserialize)]
struct ServerError {
    error: String,
}
