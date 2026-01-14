//! HTTP client for communicating with irisd.

use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use reqwest::Client;
use serde::Deserialize;
use serde::Serialize;

/// Response from the rescan endpoint
#[derive(Debug, Deserialize)]
pub struct RescanResponse {
    pub rescanned: usize,
    pub errors: usize,
}

/// Request body for rescan endpoint
#[derive(Debug, Serialize)]
struct RescanRequest {
    paths: Vec<PathBuf>,
}

/// Client for communicating with irisd
pub struct IrisdClient {
    base_url: String,
    client: Client,
}

impl IrisdClient {
    /// Create a new client from a URL.
    pub fn new(url: &str) -> Result<Self> {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(anyhow!(
                "Invalid irisd URL: {}. Expected http://host:port",
                url
            ));
        }

        let client = Client::builder()
            .build()
            .context("Failed to build HTTP client")?;

        Ok(Self {
            base_url: url.trim_end_matches('/').to_string(),
            client,
        })
    }

    /// Request irisd to rescan specified store paths for updated signatures.
    ///
    /// This is called after signing paths with `nix store sign` to notify
    /// irisd to index the paths in its bloom filter.
    pub async fn rescan<I, P>(&self, paths: I) -> Result<RescanResponse>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let paths: Vec<PathBuf> = paths
            .into_iter()
            .map(|p| p.as_ref().to_path_buf())
            .collect();
        let request = RescanRequest { paths };

        let url = format!("{}/rescan", self.base_url);
        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .with_context(|| format!("Failed to send POST request to {}", url))?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown error".to_string());
            return Err(anyhow!(
                "irisd returned error {}: {}",
                status.as_u16(),
                body
            ));
        }

        response
            .json()
            .await
            .context("Failed to parse rescan response")
    }
}
