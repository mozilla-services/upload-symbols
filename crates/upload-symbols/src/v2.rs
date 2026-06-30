use crate::{
    Result, UploadSummary,
    base::{Retry, deserialize_system_time},
    sym_files::{InvalidKeyError, SymbolsFile},
};
use md5::Digest;
use reqwest::{Body, Method, Url, header::HeaderMap};
use serde::{Deserialize, Deserializer, Serialize};
use std::{
    collections::HashMap,
    fmt::Write,
    io::{Read, SeekFrom},
    num::ParseIntError,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::mpsc,
    task::{self, JoinSet},
    time::sleep,
};
use tokio_util::io::ReaderStream;
use tracing::{Instrument, Span, instrument};

// Update the docstrings of the `ClientBuilder` methods when changing these defaults.
const DEFAULT_RETRIES: usize = 2;
const DEFAULT_RETRY_DELAY_SECONDS: u64 = 30;
const DEFAULT_BATCH_SIZE: usize = 128;
const DEFAULT_MAX_FILE_UPLOADS: u32 = 16;
const DEFAULT_FILE_UPLOAD_RETRIES: usize = 10;
const DEFAULT_FILE_UPLOAD_DELAY_SECONDS: u64 = 1;
const DEFAULT_CHUNK_SIZE: u64 = 16 * 1024 * 1024;

/// Configuration for the v2 upload client.
#[derive(Debug)]
#[cfg_attr(feature = "clap", derive(clap::Args), group(id = "config-v2"))]
pub struct Config {
    /// The number of retries for Symbols Server requests.
    ///
    /// On retriable status codes, Symbols Server requests are retried this number of times, in
    /// addition to the original request. A value of 0 disables retrying.
    #[cfg_attr(feature = "clap", arg(long, default_value_t = DEFAULT_RETRIES))]
    pub retries: usize,

    /// The delay in seconds between Symbols Server request retries.
    #[cfg_attr(feature = "clap", arg(long, default_value_t = DEFAULT_RETRY_DELAY_SECONDS))]
    pub retry_delay_seconds: u64,

    /// The number of symbols files per request to the Symbols Server.
    #[cfg_attr(feature = "clap", arg(long, default_value_t = DEFAULT_BATCH_SIZE))]
    pub batch_size: usize,

    /// The maximum number of concurrent file uploads to GCS.
    #[cfg_attr(feature = "clap", arg(
        long,
        default_value_t = DEFAULT_MAX_FILE_UPLOADS,
        value_parser = clap::value_parser!(u32).range(1..=64),
    ))]
    pub max_file_uploads: u32,

    /// The number of retries for individual file uploads to GCS.
    ///
    /// The number of retriable status codes that are accepted before bailing out for each file
    /// upload. A value of 0 disables retrying.
    #[cfg_attr(feature = "clap", arg(long, default_value_t = DEFAULT_FILE_UPLOAD_RETRIES))]
    pub file_upload_retries: usize,

    /// The retry delay in seconds between GCS upload request retries.
    #[cfg_attr(feature = "clap", arg(long, default_value_t = DEFAULT_FILE_UPLOAD_DELAY_SECONDS))]
    pub file_upload_delay_seconds: u64,

    /// The chunk size for file uploads to GCS.
    #[cfg_attr(feature = "clap", arg(
        long,
        default_value_t = DEFAULT_CHUNK_SIZE,
        value_parser = clap::value_parser!(u64).range(262_144..),
    ))]
    pub chunk_size: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            retries: DEFAULT_RETRIES,
            retry_delay_seconds: DEFAULT_RETRY_DELAY_SECONDS,
            batch_size: DEFAULT_BATCH_SIZE,
            max_file_uploads: DEFAULT_MAX_FILE_UPLOADS,
            file_upload_retries: DEFAULT_FILE_UPLOAD_RETRIES,
            file_upload_delay_seconds: DEFAULT_FILE_UPLOAD_DELAY_SECONDS,
            chunk_size: DEFAULT_CHUNK_SIZE,
        }
    }
}

/// The v2 upload client.
#[derive(Clone, Debug)]
pub struct Client {
    base: Arc<crate::base::Client>,
    retry: Arc<Retry>,
    batch_size: usize,
    max_file_uploads: u32,
    file_upload_retries: usize,
    file_upload_delay: Duration,
    chunk_size: u64,
}

impl Client {
    pub fn new(base: crate::base::Client, config: Config) -> Self {
        let retry = Retry::builder()
            .retries(config.retries)
            .delay_seconds(config.retry_delay_seconds)
            .build();
        Self {
            base: Arc::new(base),
            retry: Arc::new(retry),
            batch_size: config.batch_size,
            max_file_uploads: config.max_file_uploads,
            file_upload_retries: config.file_upload_retries,
            file_upload_delay: Duration::from_secs(config.file_upload_delay_seconds),
            chunk_size: config.chunk_size,
        }
    }

    /// Upload a directory of files to the Mozilla Symbols Server.
    ///
    /// This function uses `crate::sym_files::discover()` to find symbols files under the given
    /// `root` directory and uploads them to the Mozilla Symbols Server using the given client
    /// to perform the HTTP requests. Only regular files are inlcuded.
    ///
    /// This operation is performed as a pipeline of three concurrent tasks.
    /// ```text
    /// collect_file_specs() -> collect_upload_jobs() -> upload_directory()
    /// ```
    /// `collect_file_specs()` discovers symbols files, collects information including the MD5
    /// digest about them and groups them into batches.
    ///
    /// `collect_upload_jobs()` sends a request to the Symbols Server for each batch, records
    /// skips and errors and sends information about files that should be uploaded to GCS on to
    /// this function.
    ///
    /// This function, `upload_directory()`, spawns a task for each upload to GCS and gathers
    /// all infromation.
    #[instrument(level = "debug", skip(self))]
    pub async fn upload_directory(&self, root: PathBuf) -> Result<UploadSummary> {
        let temp_dir = tempdir::TempDir::new("upload-symbols.")?;
        let temp_path = temp_dir.path().to_path_buf();

        let (batch_tx, batch_rx) = mpsc::channel(16);
        let batch_size = self.batch_size;
        let span = Span::current();
        let collect_file_specs_handle = task::spawn_blocking(move || {
            span.in_scope(|| collect_file_specs(batch_tx, root, batch_size))
        });

        let (job_tx, mut job_rx) = mpsc::channel(256);
        let span = Span::current();
        let client = self.clone();
        let collect_upload_jobs_handle = task::spawn(
            async move { client.collect_upload_jobs(batch_rx, job_tx).await }.instrument(span),
        );

        let mut summary = UploadSummary::default();
        let max_file_uploads = self.max_file_uploads as usize;
        let mut uploads = JoinSet::new();
        while let Some(job) = job_rx.recv().await {
            while uploads.len() >= max_file_uploads {
                let (key, result) = uploads.join_next().await.unwrap().unwrap();
                summary.record_upload(key, result);
            }
            let key = job.sym_file.key().to_string();
            let client = self.clone();
            let temp_path = temp_path.clone();
            uploads.spawn(async move {
                let result = client.upload_file_to_gcs(job, temp_path).await;
                (key, result)
            });
        }
        while let Some(upload_result) = uploads.join_next().await {
            let (key, result) = upload_result.unwrap();
            summary.record_upload(key, result);
        }

        // Unwrap the outer JoinError. This will basically propagate panics.
        summary.discovery_errors = collect_file_specs_handle.await.unwrap()?;
        summary.merge(collect_upload_jobs_handle.await.unwrap()?);
        if let Err(e) = temp_dir.close() {
            summary.upload_errors.push(e.into());
        }
        Ok(summary)
    }

    /// Request uploads from the Symbols Server for each batch of symbols files.
    #[instrument(level = "debug", skip(self))]
    async fn collect_upload_jobs(
        self,
        mut rx: mpsc::Receiver<FileBatch>,
        tx: mpsc::Sender<UploadJob>,
    ) -> Result<UploadSummary> {
        let mut summary = UploadSummary::default();
        while let Some(mut batch) = rx.recv().await {
            let data = UploadRequest {
                files: batch.file_specs,
            };
            let base = self.base.clone();
            let upload_response: UploadResponse = self
                .retry
                .request(async move || {
                    let request = base.request(Method::POST, "upload/v2/").json(&data);
                    Ok(request)
                })
                .await?;
            for file_spec in upload_response.files {
                match file_spec.action {
                    ActionSpec::Upload {
                        url,
                        content_encoding,
                    } => {
                        let Some(sym_file) = batch.sym_files.remove(&file_spec.key) else {
                            // The Symbols Server returned a spec for a key we don't know
                            // about. This can only happen due to a bug.
                            summary.upload_errors.push(
                                crate::Error::InvalidSymbolsServerResponse {
                                    msg: format!("unknown key {}", file_spec.key),
                                },
                            );
                            summary.failed_keys.push(file_spec.key);
                            continue;
                        };
                        tx.send(UploadJob {
                            sym_file,
                            session_url: url,
                            content_encoding,
                        })
                        .await
                        .unwrap();
                    }
                    ActionSpec::Skip => summary.skipped_keys.push(file_spec.key),
                    ActionSpec::Error { msg } => {
                        // The only error we return from the server is a key validation error.
                        // Since we have the same key validation on the client side, we can
                        // only reach this point due to a bug.
                        summary.failed_keys.push(file_spec.key);
                        summary
                            .upload_errors
                            .push(crate::Error::InvalidSymbolsServerResponse { msg });
                    }
                };
            }
        }
        Ok(summary)
    }

    /// Upload a single file to a GCS resumable upload session URL.
    ///
    /// Documentation of the protocol:
    /// https://docs.cloud.google.com/storage/docs/performing-resumable-uploads#upload-data
    async fn upload_file_to_gcs(&self, mut job: UploadJob, temp_path: PathBuf) -> Result<()> {
        let mut content_encoding_header = HeaderMap::new();
        if let Some(ContentEncoding::Gzip) = job.content_encoding {
            job = task::spawn_blocking(move || -> Result<UploadJob> {
                job.sym_file.gzip_compress(temp_path)?;
                Ok(job)
            })
            .await
            .unwrap()?;
            content_encoding_header.insert("content-encoding", "gzip".parse().unwrap());
        }
        let content_encoding_header = content_encoding_header;

        let mut remaining_retries = self.file_upload_retries;
        let mut delay = self.file_upload_delay;
        let file = job.sym_file.async_open().await?;
        let file_size = file.metadata().await?.len();
        let mut transferred = 0;
        loop {
            let chunk_size = std::cmp::min(file_size - transferred, self.chunk_size);
            let chunk_end = (transferred + chunk_size).saturating_sub(1);
            let mut chunk_file = file.try_clone().await?;
            chunk_file.seek(SeekFrom::Start(transferred)).await?;
            let response = self
                .base
                .http_client()
                .put(job.session_url.clone())
                .header("content-length", chunk_size)
                .header(
                    "content-range",
                    format!("bytes {transferred}-{chunk_end}/{file_size}"),
                )
                .headers(content_encoding_header.clone())
                .body(Body::wrap_stream(ReaderStream::new(
                    chunk_file.take(chunk_size),
                )))
                .send()
                .await?;
            let status = response.status().as_u16();
            match status {
                200 | 201 => {
                    // These status codes mean we are done uploading.
                    let _object: GcsObject = response.json().await?;
                    // TODO(smarnach): validate size, key and MD5 hash.
                    break;
                }
                308 => {
                    // Status code 308 is used by Google to indicate that the upload isn't
                    // complete yet. The range header indicates how many bytes have already
                    // been successfully transferred.
                    let Some(range) = response.headers().get("range") else {
                        return Err(crate::Error::MissingRangeHeader);
                    };
                    let Ok(Some(Ok(new_transferred))) = range.to_str().map(|s| {
                        s.strip_prefix("bytes=0-")
                            .map(|s| Ok::<_, ParseIntError>(s.parse::<u64>()? + 1))
                    }) else {
                        return Err(crate::Error::InvalidRangeHeader(range.clone()));
                    };
                    transferred = new_transferred;
                }
                429 | 500 | 502 | 503 | 504 => {
                    if remaining_retries == 0 {
                        return Err(response.error_for_status().unwrap_err().into());
                    }
                    sleep(delay).await;
                    remaining_retries -= 1;
                    delay = delay.mul_f64(1.5);
                }
                _ => {
                    // Something unexpected must have happened if we get here.
                    response.error_for_status()?;
                    // It's even more unexpected if the previous line did not bail out. If we
                    // get here, we've got no idea how to recover, so let's just panic to get a
                    // stack trace in Sentry.
                    panic!("unexpected response from GCS: {status}");
                }
            }
        }
        Ok(())
    }
}

/// Discover files and compute their MD5 hashes.
///
/// This operation is performed in a synchronous task. It's only performing disk I/O and MD5
/// computation, so it doesn't gain anything from asynchronicity.
/// TODO(smarnach): Measure whether this can be sped up using rayon.
#[instrument(level = "debug", skip(tx))]
fn collect_file_specs(
    tx: mpsc::Sender<FileBatch>,
    root: PathBuf,
    batch_size: usize,
) -> Result<Vec<InvalidKeyError>> {
    let mut errors = vec![];
    let mut batch = FileBatch::default();
    for sym_file in crate::sym_files::discover(&root) {
        let Ok(sym_file) = sym_file else {
            errors.push(sym_file.unwrap_err());
            continue;
        };
        batch.file_specs.push(file_spec(&sym_file)?);
        batch.sym_files.insert(sym_file.key().to_string(), sym_file);
        if batch.file_specs.len() >= batch_size
            && tx.blocking_send(std::mem::take(&mut batch)).is_err()
        {
            // If the receiver has hung up, it must have errored out. The actual error will be
            // returned by that task, so here we can simply return success.
            return Ok(errors);
        }
    }
    if !batch.file_specs.is_empty() {
        tx.blocking_send(batch).ok();
    }
    Ok(errors)
}

/// Populate a `FileSpec` for the given `SymbolsFile` instance.
fn file_spec(sym_file: &SymbolsFile) -> Result<FileSpecRequest> {
    let key = sym_file.key().to_string();
    let mut file = sym_file.open()?;
    let mut hasher = md5::Md5::new();
    let mut buf = vec![0_u8; 65_536];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        hasher.update(&buf[..n]);
    }
    let md5_hash = encode_hex(hasher.finalize().as_slice());
    Ok(FileSpecRequest {
        key,
        size,
        md5_hash,
    })
}

/// Format the given bytes as a hex string.
///
/// This is used to format the MD5 hex digest for files.
fn encode_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        write!(&mut s, "{b:02x}").unwrap();
    }
    s
}

/// A batch of symbols files together with their `FileSpec` instances
///
/// This is used to communicate a batch of files from `collect_file_specs()` to
/// `collect_upload_jobs()`.
#[derive(Debug, Default)]
struct FileBatch {
    sym_files: HashMap<String, SymbolsFile>,
    file_specs: Vec<FileSpecRequest>,
}

/// A single symbols file together with an upload URL.
///
/// This is used to communicate files to be uploaded from `collect_upload_jobs()` back to
/// `upload_directory()`.
#[derive(Debug)]
struct UploadJob {
    sym_file: SymbolsFile,
    session_url: Url,
    content_encoding: Option<ContentEncoding>,
}

/// Data for a single symbols file in a upload v2 request payload.
#[derive(Debug, Serialize)]
struct FileSpecRequest {
    key: String,
    size: u64,
    md5_hash: String,
}

/// The JSON schema of the upload v2 request payload.
#[derive(Debug, Serialize)]
struct UploadRequest {
    files: Vec<FileSpecRequest>,
}

/// The content encoding for the upload, returned by the server.
///
/// Only "gzip" and no content encoding at all are supported.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ContentEncoding {
    Gzip,
}

/// An action specification for an individual file in the upload v2 response.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ActionSpec {
    Upload {
        #[serde(deserialize_with = "deserialize_url")]
        url: Url,
        content_encoding: Option<ContentEncoding>,
    },
    Skip,
    Error {
        msg: String,
    },
}

/// The response for an individual file in the upload v2 response.
#[derive(Debug, Deserialize)]
struct FileSpecResponse {
    key: String,
    action: ActionSpec,
}

/// The upload protocol specifier. Only "gcs-resumable" is supported.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum UploadProtocol {
    GcsResumable,
}

/// The JSON schema of the upload v2 response.
#[derive(Debug, Deserialize)]
#[allow(unused)]
struct UploadResponse {
    id: u32,
    #[serde(deserialize_with = "deserialize_system_time")]
    created_at: SystemTime,
    user: String,
    try_symbols: bool,
    upload_protocol: UploadProtocol,
    files: Vec<FileSpecResponse>,
}

/// The oject returned by the final PUT request to upload a file.
///
/// We only include the few fields we are interested in for verification.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(unused)]
struct GcsObject {
    md5_hash: String,
    name: String,
    // For whatever reason, the size is returned as a string containing a decimal integer by
    // GCS, so we need a custom function to deserialize it.
    #[serde(deserialize_with = "deserialize_u64")]
    size: u64,
}

fn deserialize_url<'de, D>(deserializer: D) -> std::result::Result<Url, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    Url::parse(&s).map_err(serde::de::Error::custom)
}

fn deserialize_u64<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    s.parse::<u64>().map_err(serde::de::Error::custom)
}
