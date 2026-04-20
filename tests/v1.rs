//! Integration tests for the upload API v1 client.
//!
//! This test requires running the Tecken development stack, creating a token with upload
//! permissions and storing that token in the LOCAL_AUTH_TOKEN environment variable.

#![cfg(feature = "local-upload-test")]

use tokio::runtime::Runtime;
use upload_symbols::Client;

#[test]
fn upload_directory_locally() {
    let req_client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .unwrap();
    let auth_token = std::env::var("LOCAL_AUTH_TOKEN").unwrap();
    let client = Client::new(
        req_client,
        "http://localhost:8000/".try_into().unwrap(),
        auth_token,
    );
    Runtime::new()
        .unwrap()
        .block_on(client.upload_directory("tests/data/linux"))
        .unwrap();
}

static USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"),);
