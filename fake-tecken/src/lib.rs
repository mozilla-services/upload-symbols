//! A fake implementation of Tecken for integration tests.
//!
//! The implementation keeps track of uploaded files and correctly returns `skipped_keys`.

use axum::{
    Json, Router,
    extract::{Multipart, State},
    http::StatusCode,
    routing::post,
};
use serde::Serialize;
use std::{
    collections::HashSet,
    io::Cursor,
    sync::{Arc, Mutex, MutexGuard},
    time::UNIX_EPOCH,
};
use tokio::{net::TcpListener, sync::oneshot};
use url::Url;

type UploadedFiles = Arc<Mutex<HashSet<String>>>;

pub struct FakeTecken {
    uploaded_files: UploadedFiles,
    port: u16,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl FakeTecken {
    pub async fn new() -> Self {
        let uploaded_files = Arc::new(Mutex::new(HashSet::new()));
        let app = Router::new()
            .route("/upload/", post(upload))
            .route("/upload/auth_info/", post(auth_info))
            .with_state(Arc::clone(&uploaded_files));
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        tokio::spawn(
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .into_future(),
        );
        Self {
            uploaded_files,
            port,
            shutdown_tx: Some(shutdown_tx),
        }
    }

    pub fn url(&self) -> Url {
        Url::parse(&format!("http://localhost:{}/", self.port)).unwrap()
    }

    pub fn uploaded_files(&self) -> MutexGuard<'_, HashSet<String>> {
        self.uploaded_files.lock().unwrap()
    }
}

impl Drop for FakeTecken {
    fn drop(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
    }
}

async fn upload(
    State(uploaded_files): State<UploadedFiles>,
    mut multipart: Multipart,
) -> Result<Json<UploadResponse>, StatusCode> {
    let field = multipart
        .next_field()
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?
        .ok_or(StatusCode::BAD_REQUEST)?;
    let bytes = field.bytes().await.map_err(|_| StatusCode::BAD_REQUEST)?;

    let archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| StatusCode::BAD_REQUEST)?;
    let mut uploaded_files = uploaded_files.lock().unwrap();
    let mut skipped_keys = Vec::new();
    for file_name in archive.file_names() {
        if uploaded_files.contains(file_name) {
            skipped_keys.push(file_name.to_owned());
        } else {
            uploaded_files.insert(file_name.to_owned());
        }
    }

    Ok(Json(UploadResponse {
        upload: Upload { skipped_keys },
    }))
}

#[derive(Serialize)]
struct UploadResponse {
    upload: Upload,
}

#[derive(Serialize)]
struct Upload {
    skipped_keys: Vec<String>,
}

async fn auth_info() -> Result<Json<AuthInfo>, StatusCode> {
    Ok(Json(AuthInfo {
        email: "user@mozilla.com".to_string(),
        try_symbols: false,
        token_expires_at: UNIX_EPOCH.elapsed().unwrap().as_secs() + 86_400,
        upload_api_version: 1,
    }))
}

#[derive(Serialize)]
pub struct AuthInfo {
    email: String,
    try_symbols: bool,
    token_expires_at: u64,
    upload_api_version: u32,
}
