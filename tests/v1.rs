//! Integration tests for the upload API v1 client.
//!
//! This test requires running the Tecken development stack, creating a token with upload
//! permissions and storing that token in the LOCAL_AUTH_TOKEN environment variable.

#![cfg(feature = "local-upload-test")]

use tokio::runtime::Runtime;
use upload_symbols::Client;
use url::Url;

#[test]
fn upload_directory_locally() {
    let auth_token = std::env::var("LOCAL_AUTH_TOKEN").unwrap();
    let client = Client::builder(auth_token)
        .base_url(Url::parse("http://localhost:8000/").unwrap())
        .build()
        .unwrap();
    Runtime::new()
        .unwrap()
        .block_on(client.upload_directory("tests/data/linux"))
        .unwrap();
}
