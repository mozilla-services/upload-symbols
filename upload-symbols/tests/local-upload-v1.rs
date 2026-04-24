//! Integration tests for the upload API v1 client.
//!
//! This test requires running the Tecken development stack, creating a token with upload
//! permissions and storing that token in the LOCAL_AUTH_TOKEN environment variable.

#![cfg(feature = "local-upload-test")]

use std::collections::HashSet;

use upload_symbols::Client;
use url::Url;

#[tokio::test]
async fn upload_directory_locally() {
    let auth_token = std::env::var("LOCAL_AUTH_TOKEN").unwrap();
    let client = Client::builder(auth_token)
        .base_url(Url::parse("http://localhost:8000/").unwrap())
        .zip_size_threshold_v1(1 << 20) // 1 MiB
        .build()
        .unwrap();
    let summary = client
        .upload_directory("../tests/data/linux")
        .await
        .unwrap();
    let successful_keys: HashSet<String> = summary
        .uploaded_keys
        .into_iter()
        .chain(summary.skipped_keys)
        .collect();
    assert_eq!(successful_keys.len(), 144);
    assert!(summary.failed_keys.is_empty());
    assert!(summary.discovery_errors.is_empty());
    assert!(summary.upload_errors.is_empty());
}
