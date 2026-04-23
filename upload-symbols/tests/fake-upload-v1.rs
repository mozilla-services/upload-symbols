//! Integration tests for the upload API v1 client.
//!
//! This test uses a fake Tecken implementation, so it doesn't require a running Tecken
//! development stack.

use crate::common::fake_tecken::FakeTecken;
use std::{collections::HashSet, fs::create_dir_all, os::unix::fs::symlink};
use upload_symbols::{Client, sym_files};

mod common;

static TEST_DATA_PATH: &str = "../tests/data/linux";

#[tokio::test]
async fn upload_directory() {
    // Create a temporary directory with symlinks to half the files in the test data dir.
    let temp_dir = tempdir::TempDir::new("upload-symbols-test-data.").unwrap();
    let temp_path = temp_dir.path();
    for symbols_file in sym_files::discover(TEST_DATA_PATH).take(72) {
        let symbols_file = symbols_file.unwrap();
        let target = temp_path.join(symbols_file.key());
        create_dir_all(target.parent().unwrap()).unwrap();
        symlink(symbols_file.path().canonicalize().unwrap(), target).unwrap();
    }

    // Create a fake symbols server and a client.
    let tecken = FakeTecken::new().await;
    let client = Client::builder("fake_auth_token")
        .base_url(tecken.url())
        .zip_size_threshold_v1(1 << 20) // 1 MiB
        .build()
        .unwrap();

    // Upload the directory with half the files.
    let summary1 = client.upload_directory(&temp_path).await.unwrap();
    let uploaded_keys1: HashSet<String> = summary1.uploaded_keys.into_iter().collect();
    assert_eq!(&uploaded_keys1, &*tecken.uploaded_files());
    assert!(summary1.skipped_keys.is_empty());
    assert!(summary1.failed_keys.is_empty());
    assert!(summary1.discovery_errors.is_empty());
    assert!(summary1.upload_errors.is_empty());

    // Upload the whole test data directory.
    let summary2 = client.upload_directory(TEST_DATA_PATH).await.unwrap();
    let uploaded_keys2: HashSet<String> = summary2.uploaded_keys.into_iter().collect();
    let skipped_keys2: HashSet<String> = summary2.skipped_keys.into_iter().collect();
    assert_eq!(uploaded_keys1, skipped_keys2);
    assert!(uploaded_keys1.is_disjoint(&uploaded_keys2));
    assert_eq!(
        &(&uploaded_keys1 | &uploaded_keys2),
        &*tecken.uploaded_files()
    );
    assert!(summary2.failed_keys.is_empty());
    assert!(summary2.discovery_errors.is_empty());
    assert!(summary2.upload_errors.is_empty());
}
