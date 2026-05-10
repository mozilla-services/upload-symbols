use reqwest::Url;

/// The base client with commmon functionality for both version of the upload API.
#[derive(Clone, Debug)]
pub struct Client {
    pub client: reqwest::Client,
    pub base_url: Url,
    pub auth_token: String,
}

impl Client {
    /// Perform an authenticated request to the symbols server.
    pub fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            // We validate the URL in the builder to make sure it can be used as a base URL.
            // The `path` is a hardcoded string from this library, so `join()` can't return an
            // error here and we can unwrap.
            .request(method, self.base_url.join(path).unwrap())
            .header("auth-token", &self.auth_token)
    }
}
