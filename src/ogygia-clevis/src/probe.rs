//! Finds which Tang servers in the spec can be bound to right now.

use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures::future::join_all;
use reqwest::Client;
use serde_json::Value;
use tracing::debug;
use tracing::warn;

use crate::blob;
use crate::config::Pin;

/// A pin whose server answered with an advertisement carrying the pinned
/// signing key.
pub struct Reachable {
    pub pin: Pin,
    /// The signed advertisement, handed to clevis so it binds to exactly
    /// what was checked here rather than fetching again.
    pub adv: Value,
}

pub fn client() -> Result<Client> {
    Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .context("building HTTP client")
}

/// Probes every pin concurrently. Unreachable pins are logged and dropped;
/// the rest keep their order.
pub async fn probe_all(client: &Client, pins: &[Pin]) -> Vec<Reachable> {
    let results = join_all(pins.iter().map(|pin| probe(client, pin))).await;
    pins.iter()
        .zip(results)
        .filter_map(|(pin, result)| match result {
            Ok(adv) => {
                debug!(%pin, "reachable");
                Some(Reachable {
                    pin: pin.clone(),
                    adv,
                })
            }
            Err(err) => {
                warn!(%pin, "unreachable: {err:#}");
                None
            }
        })
        .collect()
}

async fn probe(client: &Client, pin: &Pin) -> Result<Value> {
    let url = format!("{}/adv/{}", pin.url.trim_end_matches('/'), pin.thp);
    let body = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let jws: Value = serde_json::from_str(&body).context("advertisement is not JSON")?;
    let payload = jws
        .get("payload")
        .and_then(Value::as_str)
        .context("advertisement has no payload")?;
    let payload = URL_SAFE_NO_PAD
        .decode(payload)
        .context("decoding advertisement payload")?;
    let jwks: Value = serde_json::from_slice(&payload).context("parsing advertisement payload")?;
    // Tang keeps answering for a rotated-out signing key, but advertises
    // only its current keys, and clevis then refuses to trust them under the
    // old thumbprint. Apply the same rule so such a pin is merely
    // unreachable instead of failing the whole run at encryption time.
    let advertised = blob::verify_keys(&jwks)?
        .map(blob::thumbprint)
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        advertised.contains(&pin.thp),
        "advertised signing keys {advertised:?} do not include the pinned one"
    );
    Ok(jws)
}
