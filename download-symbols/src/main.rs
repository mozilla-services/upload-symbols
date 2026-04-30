//! Download symbols files from the Mozilla Symbols server.

use anyhow::Result;
use clap::Parser;
use reqwest::Url;
use serde::Deserialize;
use std::{path::PathBuf, process::ExitCode};
use tokio::{io::AsyncWriteExt, task::JoinSet};

const MAX_CONCURRENT_DOWNLOADS: usize = 6;

/// Download symbols files from the Mozilla Symbols server.
///
/// This program is mainly intended to generate test data for the upload-symbols library and
/// CLI. It downloads all symbols files that were uploaded together in the uploads identified
/// by the upload IDs passed on the command-line.
///
/// You need to set the `DOWNLOAD_SYMBOLS_AUTH_TOKEN` environment variable to a token with the
/// "view all symbols uploads" permission.
#[derive(Debug, Parser)]
struct Args {
    /// IDs of uploads whose files should be downloaded.
    #[arg(required = true, value_name = "UPLOAD_ID", num_args = 1..)]
    upload_ids: Vec<u64>,

    /// The target directory to download the files to.
    #[arg(required = true, value_name = "TARGET_DIRECTORY")]
    target_directory: PathBuf,

    /// A Mozilla Symbols Server authentication token with upload permissions.
    #[arg(long, required = true, env = "DOWNLOAD_SYMBOLS_AUTH_TOKEN")]
    auth_token: String,

    /// The base URL of the symbols server.
    #[arg(long, default_value = "https://symbols.mozilla.org/")]
    server_url: Url,
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let args = Args::parse();
    let client = reqwest::Client::builder().gzip(true).build()?;
    let mut downloads = JoinSet::new();
    for upload_id in args.upload_ids {
        let upload_url = args
            .server_url
            .join(&format!("api/uploads/upload/{upload_id}"))?;
        let upload_response = client
            .get(upload_url)
            .header("Auth-Token", &args.auth_token)
            .send()
            .await?
            .error_for_status()?
            .json::<UploadResponse>()
            .await?;
        for key in upload_response.upload.into_keys() {
            if downloads.len() >= MAX_CONCURRENT_DOWNLOADS {
                downloads.join_next().await.unwrap()??;
            }
            downloads.spawn(download_symbols_file(
                client.clone(),
                args.target_directory.clone(),
                key,
            ));
        }
    }
    while let Some(result) = downloads.join_next().await {
        result??;
    }

    Ok(ExitCode::SUCCESS)
}

#[derive(Debug, Deserialize)]
struct UploadResponse {
    upload: Upload,
}

#[derive(Debug, Deserialize)]
struct Upload {
    skipped_keys: Vec<String>,
    file_uploads: Vec<FileUpload>,
}

impl Upload {
    fn into_keys(self) -> impl Iterator<Item = String> {
        self.skipped_keys
            .into_iter()
            .chain(self.file_uploads.into_iter().map(|file| file.key))
    }
}

#[derive(Debug, Deserialize)]
struct FileUpload {
    key: String,
}

async fn download_symbols_file(
    client: reqwest::Client,
    target_directory: PathBuf,
    key: String,
) -> Result<()> {
    let mut response = client
        .get(format!("https://symbols.mozilla.org/try/{key}"))
        .send()
        .await?
        .error_for_status()?;
    let path = target_directory.join(&key);
    tokio::fs::create_dir_all(path.parent().unwrap()).await?;
    let mut file = tokio::fs::File::create(path).await?;
    while let Some(chunk) = response.chunk().await? {
        file.write_all(&chunk).await?;
    }
    Ok(())
}
