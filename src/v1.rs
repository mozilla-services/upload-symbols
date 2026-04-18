//! Client implementation for the original Mozilla Symbols Server upload endpoint.

use crate::{Client, Result};
use reqwest::{Method, multipart};
use std::{
    io::Seek,
    path::{Path, PathBuf},
    time::Duration,
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
    let temp_dir = crate::tmpdir::TempDir::new("upload-symbols")?;
    let temp_path = temp_dir.path().to_path_buf();
    let create_zip_handle = spawn_blocking(|| create_zip_archives(tx, path, temp_path));

    // Upload ZIP archives as they get created.
    let mut set = JoinSet::new();
    while let Some(zip_archive_path) = rx.recv().await {
        set.spawn(upload_zip_archive(client.clone(), zip_archive_path));
    }

    // Unwrap the outer JoinError. This will basically propagate panics.
    create_zip_handle.await.unwrap()?;

    // TODO(smarnach) Instead of just returning the first error, we should do something more
    // reasonable. We should probably also collect the information about skipped files and pass
    // it back to the caller.
    let result = set.join_all().await.into_iter().collect();

    // Explicitly close temp_dir so we can propagate any errors.
    temp_dir.close()?;

    result
}

/// The file size threshold after which to start a new ZIP archive.
const ZIP_FILE_SIZE_THRESHOLD: u64 = 2 << 29; // 0.5 GiB

/// Create ZIP archives for all symbols files in the given directory.
fn create_zip_archives(tx: mpsc::Sender<PathBuf>, root: PathBuf, temp_path: PathBuf) -> Result<()> {
    let mut current_zip_writer = None;
    let mut zip_path_iter = (0..).map(|i| temp_path.join(format!("symbols-{i}.zip")));
    let mut current_zip_path = None;
    for sym_file in crate::sym_files::discover(&root) {
        // TODO(smarnach): Add tracing events for ignored files instead of erroring out.
        let sym_file = sym_file?;
        let zip_writer = match current_zip_writer {
            Some(ref mut zip_writer) => zip_writer,
            None => {
                let zip_path = zip_path_iter.next().unwrap();
                let zip_file = std::fs::File::create_new(&zip_path)?;
                let zip_writer = ZipWriter::new(zip_file);
                current_zip_writer = Some(zip_writer);
                current_zip_path = Some(zip_path);
                current_zip_writer.as_mut().unwrap()
            }
        };
        let options = zip::write::SimpleFileOptions::default().compression_method(
            if sym_file.is_compressed() {
                CompressionMethod::Stored
            } else {
                CompressionMethod::Deflated
            },
        );
        zip_writer.start_file(sym_file.key(), options)?;
        std::io::copy(&mut sym_file.open()?, zip_writer)?;
        // We know the ZipWriter isn't closed yet, so we can unwrap.
        if zip_writer.get_ref().unwrap().stream_position()? >= ZIP_FILE_SIZE_THRESHOLD {
            current_zip_writer.take().unwrap().finish()?;
            // We know the receiver hasn't hung up yet, so we can unwrap.
            tx.blocking_send(current_zip_path.take().unwrap()).unwrap();
        }
    }
    if let Some(zip_writer) = current_zip_writer {
        zip_writer.finish()?;
        // We know the receiver hasn't hung up yet, so we can unwrap.
        tx.blocking_send(current_zip_path.unwrap()).unwrap();
    }
    Ok(())
}

async fn upload_zip_archive(client: Client, path: PathBuf) -> Result<()> {
    let mut retries = 2; // TODO(smarnach): Make configurable
    loop {
        let form = multipart::Form::new().file("file", &path).await?;
        let response = client
            .request(Method::POST, "/upload/")?
            .multipart(form)
            .send()
            .await?;
        if let 429 | 502 | 503 | 504 = response.status().as_u16() {
            if retries == 0 {
                return Err(response.error_for_status().unwrap_err().into());
            }
            retries -= 1;
            sleep(Duration::from_secs(120)).await; // TODO(smarnach): Make configurable
            continue;
        }
        response.error_for_status_ref()?;
        // TODO(smarnach): Extract skipped keys from response.
        break;
    }
    Ok(())
}
