//! A fake implementation of Tecken for integration tests.
//!
//! The implementation keeps track of uploaded files and correctly returns `skipped_keys`.

use axum::{
    Json, Router,
    extract::{Multipart, State},
    http::StatusCode,
    routing::{post, put},
};
use serde::Serialize;
use std::{
    io::Cursor,
    sync::{Arc, Mutex, MutexGuard},
    time::UNIX_EPOCH,
};
pub use storage::SymbolsStorage;
use tokio::{net::TcpListener, sync::oneshot};
use url::Url;

mod storage;
mod v2;

type AppState = Arc<Mutex<SymbolsStorage>>;

pub struct FakeTecken {
    storage: AppState,
    port: u16,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl FakeTecken {
    pub async fn new(upload_api_version: u32) -> Self {
        let storage = Arc::new(Mutex::new(SymbolsStorage::new()));
        let app = Router::new()
            .route("/upload/", post(upload))
            .route("/upload/v2/", post(v2::upload))
            .route(
                "/gcs/resumable-upload/{resumable_upload_id}",
                put(v2::resumable_upload),
            )
            .with_state(Arc::clone(&storage))
            .route("/upload/auth_info/", post(auth_info))
            .with_state(upload_api_version);
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
            storage,
            port,
            shutdown_tx: Some(shutdown_tx),
        }
    }

    pub fn url(&self) -> Url {
        Url::parse(&format!("http://localhost:{}/", self.port)).unwrap()
    }

    pub fn symbols_storage(&self) -> MutexGuard<'_, SymbolsStorage> {
        self.storage.lock().unwrap()
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
    State(storage): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<UploadResponse>, StatusCode> {
    let field = multipart
        .next_field()
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?
        .ok_or(StatusCode::BAD_REQUEST)?;
    let bytes = field.bytes().await.map_err(|_| StatusCode::BAD_REQUEST)?;

    let archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| StatusCode::BAD_REQUEST)?;
    let mut storage = storage.lock().unwrap();
    let id = storage.new_upload();
    let mut skipped_keys = Vec::new();
    for file_name in archive.file_names() {
        if storage.uploaded_files().contains(file_name) {
            skipped_keys.push(file_name.to_owned());
        } else {
            storage.upload_file(file_name.to_owned());
        }
    }

    Ok(Json(UploadResponse {
        upload: Upload { id, skipped_keys },
    }))
}

#[derive(Serialize)]
struct UploadResponse {
    upload: Upload,
}

#[derive(Serialize)]
struct Upload {
    id: u32,
    skipped_keys: Vec<String>,
}

async fn auth_info(State(upload_api_version): State<u32>) -> Result<Json<AuthInfo>, StatusCode> {
    Ok(Json(AuthInfo {
        email: "user@mozilla.com".to_string(),
        try_symbols: false,
        token_expires_at: UNIX_EPOCH.elapsed().unwrap().as_secs() + 86_400,
        upload_api_version,
    }))
}

#[derive(Serialize)]
pub struct AuthInfo {
    email: String,
    try_symbols: bool,
    token_expires_at: u64,
    upload_api_version: u32,
}
