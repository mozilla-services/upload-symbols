use crate::AppState;
use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::time::UNIX_EPOCH;

pub(crate) async fn upload(
    State(storage): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<UploadRequest>,
) -> Result<Json<UploadResponse>, StatusCode> {
    let host = headers.get(header::HOST).unwrap().to_str().unwrap();
    let mut files = Vec::new();
    let mut storage = storage.lock().unwrap();
    for file_spec in request.files {
        let key = file_spec.key;
        if storage.uploaded_files().contains(&key) {
            files.push(FileSpecResponse {
                key,
                action: ActionSpec::Skip,
            });
        } else {
            let resumable_upload_id = storage.initiate_file_upload(key.clone());
            files.push(FileSpecResponse {
                key,
                action: ActionSpec::Upload {
                    url: format!("http://{host}/gcs/resumable-upload/{resumable_upload_id}"),
                    content_encoding: None,
                },
            })
        }
    }
    Ok(Json(UploadResponse {
        id: storage.new_upload(),
        created_at: UNIX_EPOCH.elapsed().unwrap().as_secs(),
        user: "user@mozilla.com",
        try_symbols: false,
        upload_protocol: "gcs-resumable",
        files,
    }))
}

pub(crate) async fn resumable_upload(
    State(storage): State<AppState>,
    Path(resumable_upload_id): Path<u32>,
    headers: HeaderMap,
    _body: Bytes,
) -> Result<Response, StatusCode> {
    let content_range = parse_content_range(&headers)?;
    let range = HeaderValue::from_str(&format!("bytes=0-{}", content_range.end)).unwrap();

    if content_range.end + 1 >= content_range.total {
        let mut storage = storage.lock().unwrap();
        let key = storage
            .complete_file_upload(resumable_upload_id)
            .ok_or(StatusCode::NOT_FOUND)?;
        let mut response = Json(GcsObject {
            md5_hash: String::new(),
            name: key,
            size: content_range.total.to_string(),
        })
        .into_response();
        response.headers_mut().insert(header::RANGE, range);
        Ok(response)
    } else {
        if !storage
            .lock()
            .unwrap()
            .pending_uploads()
            .contains_key(&resumable_upload_id)
        {
            return Err(StatusCode::NOT_FOUND);
        }
        let mut response = StatusCode::PERMANENT_REDIRECT.into_response();
        response.headers_mut().insert(header::RANGE, range);
        Ok(response)
    }
}

fn parse_content_range(headers: &HeaderMap) -> Result<ContentRange, StatusCode> {
    let content_range = headers
        .get(header::CONTENT_RANGE)
        .unwrap()
        .to_str()
        .unwrap();
    let content_range = content_range.strip_prefix("bytes ").unwrap();
    let (range, total) = content_range.split_once('/').unwrap();
    let (_, end) = range.split_once('-').unwrap();
    let end = end.parse::<u64>().unwrap();
    let total = total.parse::<u64>().unwrap();
    Ok(ContentRange { end, total })
}

struct ContentRange {
    end: u64,
    total: u64,
}

/// Data for a single symbols file in a upload v2 request payload.
#[derive(Debug, Deserialize)]
#[allow(unused)]
struct FileSpecRequest {
    key: String,
    size: u64,
    md5_hash: String,
}

/// The JSON schema of the upload v2 request payload.
#[derive(Debug, Deserialize)]
pub(crate) struct UploadRequest {
    files: Vec<FileSpecRequest>,
}

/// An action specification for an individual file in the upload v2 response.
#[derive(Debug, Serialize)]
#[allow(dead_code)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ActionSpec {
    Upload {
        url: String,
        content_encoding: Option<&'static str>,
    },
    Skip,
    Error {
        msg: String,
    },
}

/// The response for an individual file in the upload v2 response.
#[derive(Debug, Serialize)]
struct FileSpecResponse {
    key: String,
    action: ActionSpec,
}

/// The JSON schema of the upload v2 response.
#[derive(Debug, Serialize)]
pub(crate) struct UploadResponse {
    id: u32,
    created_at: u64,
    user: &'static str,
    try_symbols: bool,
    upload_protocol: &'static str,
    files: Vec<FileSpecResponse>,
}

/// A minimal GCS object response for a completed resumable upload.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GcsObject {
    md5_hash: String,
    name: String,
    size: String,
}
