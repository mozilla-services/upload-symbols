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
use url::Url;
use std::{
    collections::HashSet,
    io::Cursor,
    sync::{Arc, Mutex, MutexGuard},
};
use tokio::{net::TcpListener, sync::oneshot};

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
