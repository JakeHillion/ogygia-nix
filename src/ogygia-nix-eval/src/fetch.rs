//! Downloads, done by whoever embeds the evaluator.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

/// An HTTP request.
pub struct Request<'a> {
    pub method: Method,
    pub url: &'a str,
    /// Header names and values.
    pub headers: &'a [(String, String)],
    /// Empty for a `GET`.
    pub body: &'a [u8],
}

/// The response to a successful request.
pub struct Response {
    /// Header names and values.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Makes the requests that fetch flake inputs from the network.
pub trait Fetcher: Sync {
    /// Make `request`, whose URL is not a `file://` URL, failing unless the
    /// server reports success.
    fn request(&self, request: &Request<'_>) -> anyhow::Result<Response>;
}
