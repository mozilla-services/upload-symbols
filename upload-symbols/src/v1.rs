//! Client implementation for the original Mozilla Symbols Server upload endpoint.

use crate::{
    Result, UploadSummary,
    base::Retry,
    sym_files::{InvalidKeyError, SymbolsFile},
};
use reqwest::{Method, multipart};
use serde::Deserialize;
use std::{collections::HashSet, io::Seek, path::PathBuf, sync::Arc};
use tokio::{
    sync::mpsc,
    task::{JoinSet, spawn_blocking},
};
use tracing::{Instrument, Span, field, info_span, instrument};
use zip::{CompressionMethod, ZipWriter};

// Update the docstrings of the `ClientBuilder` methods when changing these defaults.
const DEFAULT_MAX_CONNECTIONS_V1: u32 = 3;
const DEFAULT_ZIP_SIZE_THRESHOLD_V1: u64 = 1 << 26; // 64 MiB
const DEFAULT_RETRIES_V1: usize = 5;
const DEFAULT_RETRY_DELAY_SECONDS_V1: u64 = 60;

/// Configuration for the v1 upload client.
#[derive(Debug)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct Config {
    /// The maximum number of concurrent uploads using the v1 upload API.
    #[cfg_attr(feature = "clap", arg(
        long,
        default_value_t = DEFAULT_MAX_CONNECTIONS_V1,
        value_parser = clap::value_parser!(u32).range(1..=16)
    ))]
    pub max_connections_v1: u32,

    /// Set the ZIP archive size threshold in bytes.
    ///
    /// When building ZIP archives for v1 of the upload API, a new archive is started once the
    /// size of the current archive exceeds this threshold. ZIP archives still can get much
    /// bigger than this value since member files can be big.
    #[cfg_attr(feature = "clap", arg(long, default_value_t = DEFAULT_ZIP_SIZE_THRESHOLD_V1))]
    pub zip_size_threshold_v1: u64,

    /// Set the number of retries for the version 1 upload API.
    ///
    /// On retriable status codes, uploading ZIP archives is retried this number of times, in
    /// addition to the original request. A value of 0 disables retrying.
    #[cfg_attr(feature = "clap", arg(long, default_value_t = DEFAULT_RETRIES_V1))]
    pub retries_v1: usize,

    /// Set the delay in seconds between retries for version 1 of the upload API.
    #[cfg_attr(feature = "clap", arg(long, default_value_t = DEFAULT_RETRY_DELAY_SECONDS_V1))]
    pub retry_delay_seconds_v1: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_connections_v1: DEFAULT_MAX_CONNECTIONS_V1,
            zip_size_threshold_v1: DEFAULT_ZIP_SIZE_THRESHOLD_V1,
            retries_v1: DEFAULT_RETRIES_V1,
            retry_delay_seconds_v1: DEFAULT_RETRY_DELAY_SECONDS_V1,
        }
    }
}

/// The v1 upload client.
#[derive(Clone, Debug)]
pub struct Client {
    base: Arc<crate::base::Client>,
    zip_size_threshold: u64,
    retry: Arc<Retry>,
}

impl Client {
    pub fn new(base: crate::base::Client, config: Config) -> Self {
        let retry = Retry::builder()
            .max_connections(config.max_connections_v1)
            .retries(config.retries_v1)
            .delay_seconds(config.retry_delay_seconds_v1)
            .build();
        Self {
            base: Arc::new(base),
            zip_size_threshold: config.zip_size_threshold_v1,
            retry: Arc::new(retry),
        }
    }

    /// Upload a directory of files to the Mozilla Symbols Server.
    ///
    /// This function uses `crate::sym_files::discover()` to find symbols files under the given
    /// `root` directory and uploads them to the Mozilla Symbols Server using the given client
    /// to perform the HTTP requests. Only regular files are inlcuded.
    ///
    /// Since the original version of the upload API only supports uploading ZIP archives, we
    /// first need to create ZIP archives in a temporary directory before sending the actual
    /// HTTP requests.
    #[instrument(level = "debug", skip(self))]
    pub async fn upload_directory(&self, root: PathBuf) -> Result<UploadSummary> {
        // Create ZIP archives in a background thread so we can start uploading the first
        // archive as soon as it is ready.
        let (tx, mut rx) = mpsc::channel(64);
        let temp_dir = tempdir::TempDir::new("upload-symbols.")?;
        let temp_path = temp_dir.path().to_path_buf();
        let zip_size_threshold = self.zip_size_threshold;
        let span = Span::current();
        let create_zip_handle = spawn_blocking(move || {
            span.in_scope(|| create_zip_archives(tx, root, temp_path, zip_size_threshold))
        });

        // Upload ZIP archives as they get created.
        let mut set = JoinSet::new();
        while let Some((zip_archive_path, zip_keys)) = rx.recv().await {
            let client = self.clone();
            let span = Span::current();
            set.spawn(
                async move { (client.upload_zip_archive(zip_archive_path).await, zip_keys) }
                    .instrument(span),
            );
        }

        // Unwrap the outer JoinError. This will basically propagate panics.
        let discovery_errors = create_zip_handle.await.unwrap()?;

        let mut uploaded_keys = vec![];
        let mut skipped_keys = vec![];
        let mut failed_keys = vec![];
        let mut upload_errors = vec![];
        while let Some(join_result) = set.join_next().await {
            // Unwrap the outer result to propagate panics.
            let (upload_result, zip_keys) = join_result.unwrap();
            match upload_result {
                Ok(UploadResponse { upload }) => {
                    let not_skipped = zip_keys
                        .into_iter()
                        .filter(|key| !upload.skipped_keys.contains(key));
                    uploaded_keys.extend(not_skipped);
                    skipped_keys.extend(upload.skipped_keys);
                }
                Err(e) => {
                    failed_keys.extend(zip_keys);
                    upload_errors.push(e);
                }
            }
        }

        // Explicitly close temp_dir so we can propagate any errors. We don't want to return any
        // errors in this operation directly, since then the caller wouldn't get any information
        // about the uploads that were performed, so we add any potential error to `upload_errors`.
        if let Err(e) = temp_dir.close() {
            upload_errors.push(e.into());
        }

        let summary = UploadSummary {
            uploaded_keys,
            skipped_keys,
            failed_keys,
            discovery_errors,
            upload_errors,
        };
        Ok(summary)
    }
}

/// Create ZIP archives for all symbols files in the given directory.
#[instrument(level = "debug", skip(tx))]
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

#[derive(Debug)]
struct ZipArchive {
    path: PathBuf,
    writer: ZipWriter<std::fs::File>,
    keys: Vec<String>,
    span: tracing::span::EnteredSpan,
}

impl ZipArchive {
    fn new(path: PathBuf) -> std::io::Result<Self> {
        let span = info_span!("ZipArchive", ?path, size = field::Empty).entered();
        let file = std::fs::File::create_new(&path)?;
        Ok(Self {
            path,
            writer: ZipWriter::new(file),
            keys: vec![],
            span,
        })
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
        let mut file = self.writer.finish()?;
        self.span.record("size", file.stream_position()?);
        self.span.exit();
        // We know the receiver hasn't hung up yet, so we can unwrap.
        tx.blocking_send((self.path, self.keys)).unwrap();
        Ok(())
    }
}

impl Client {
    #[instrument(skip(self), fields(upload_id = field::Empty))]
    async fn upload_zip_archive(self, path: PathBuf) -> Result<UploadResponse> {
        // We know the file name is of the form `symbols-{i}.zip`. So we can unwrap the result of
        // `file_name()`, as there must be a file name. We can also unwrap the result of to_str(),
        // since the file name only contain ASCII characters.
        let file_name = String::from(path.file_name().unwrap().to_str().unwrap());
        let upload_response: UploadResponse = self
            .retry
            .request(async move || {
                let form = multipart::Form::new()
                    .file(file_name.clone(), &path)
                    .await?;
                let request = self.base.request(Method::POST, "upload/").multipart(form);
                Ok(request)
            })
            .await?;
        Span::current().record("upload_id", upload_response.upload.id);
        Ok(upload_response)
    }
}

#[derive(Deserialize)]
struct UploadResponse {
    upload: Upload,
}

#[derive(Deserialize)]
struct Upload {
    id: u32,
    skipped_keys: HashSet<String>,
}
