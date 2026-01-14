//! Query irisd for providers of a store path
//!
//! Calls the `/providers/{hash}` endpoint on the local irisd, which
//! checks its own store and all configured peer blooms.

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use clap::Args;
use clap::ValueEnum;
use serde::Deserialize;
use serde::Serialize;

/// Query irisd for providers of a store path
#[derive(Args)]
pub struct ProvidersArgs {
    /// Store path hash (32 chars) or full store path
    #[arg(value_name = "STORE_PATH_OR_HASH")]
    pub input: String,

    /// irisd URL to query
    #[arg(long, default_value = "http://127.0.0.1:35742", env = "OGYGIA_IRISD")]
    pub irisd: String,

    /// Query timeout in seconds
    #[arg(long, default_value = "30")]
    pub timeout: u64,

    /// Show bloom filter candidates alongside confirmed providers
    #[arg(short, long)]
    pub verbose: bool,

    /// Output format
    #[arg(long, default_value = "human", value_enum)]
    pub format: OutputFormat,
}

/// Output format for the providers command
#[derive(Debug, Clone, Copy, ValueEnum, Default)]
pub enum OutputFormat {
    /// Human-readable output
    #[default]
    Human,
    /// JSON output
    Json,
}

/// Response from the irisd /providers/{hash} endpoint
#[derive(Debug, Deserialize, Serialize)]
struct ProvidersResponse {
    hash: String,
    providers: Vec<String>,
    local: bool,
    #[serde(default)]
    bloom_candidates: Option<Vec<String>>,
}

/// Run the providers command
pub fn run(args: &ProvidersArgs) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to create tokio runtime")?;

    rt.block_on(run_async(args))
}

async fn run_async(args: &ProvidersArgs) -> Result<()> {
    let store_path_hash = parse_hash_or_store_path(&args.input)
        .ok_or_else(|| {
            anyhow!(
                "Invalid input '{}': expected 32-character hash or /nix/store/... path",
                args.input
            )
        })?
        .to_string();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(args.timeout))
        .build()
        .context("Failed to build HTTP client")?;

    let mut url = format!(
        "{}/providers/{}",
        args.irisd.trim_end_matches('/'),
        store_path_hash
    );
    if args.verbose {
        url.push_str("?verbose=true");
    }

    let response = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("Failed to query irisd at {}", url))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("irisd returned {}: {}", status, body);
    }

    let result: ProvidersResponse = response
        .json()
        .await
        .context("Failed to parse providers response")?;

    match args.format {
        OutputFormat::Human => print_human(&result),
        OutputFormat::Json => print_json(&result)?,
    }

    Ok(())
}

fn print_human(result: &ProvidersResponse) {
    println!("Store path hash: {}", result.hash);
    println!();

    if result.local {
        println!("Local: yes");
    }

    if let Some(ref candidates) = result.bloom_candidates {
        if !candidates.is_empty() {
            println!("Bloom candidates ({}):", candidates.len());
            for url in candidates {
                let confirmed = result.providers.contains(url);
                let marker = if confirmed {
                    "confirmed"
                } else {
                    "unconfirmed"
                };
                println!("  {} ({})", url, marker);
            }
        } else {
            println!("Bloom candidates: none");
        }
    } else if !result.providers.is_empty() {
        println!("Providers ({}):", result.providers.len());
        for url in &result.providers {
            println!("  {}", url);
        }
    } else if !result.local {
        println!("No providers found.");
    }
}

fn print_json(result: &ProvidersResponse) -> Result<()> {
    let json =
        serde_json::to_string_pretty(result).context("Failed to serialize result to JSON")?;
    println!("{}", json);
    Ok(())
}

/// Parse input that is either a 32-character hash or a full store path.
fn parse_hash_or_store_path(input: &str) -> Option<&str> {
    if input.starts_with("/nix/store/") {
        input
            .strip_prefix("/nix/store/")
            .map(|s| &s[..32.min(s.len())])
    } else if input.len() == 32 && input.chars().all(|c| c.is_ascii_alphanumeric()) {
        Some(input)
    } else {
        None
    }
}
