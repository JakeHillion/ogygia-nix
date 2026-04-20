use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use enum_map::EnumMap;
use etcd_client::Client;
use etcd_client::EventType;
use etcd_client::GetOptions;
use etcd_client::WatchOptions;
use git2::Oid;
use tokio::sync::Mutex;

use crate::config::Config;
use crate::nixos::CommitState;

#[derive(Default, Debug, Clone)]
pub struct HostStates {
    pub version: usize,
    pub host_states: HashMap<String, EnumMap<CommitState, Option<Oid>>>,
}

pub struct Etcd {
    states: Mutex<Arc<HostStates>>,
}

impl Etcd {
    pub async fn new(config: &Config) -> Result<Arc<Self>> {
        let mut client = Client::connect(&config.etcd.endpoints, None).await?;
        let prefix = config.etcd.prefix.clone();

        let me = Arc::new(Self {
            states: Default::default(),
        });

        // Load initial state
        me.load_all(&mut client, &prefix).await?;

        // Spawn background watcher
        tokio::spawn({
            let weak = Arc::downgrade(&me);
            let endpoints = config.etcd.endpoints.clone();
            let prefix = prefix.clone();
            async move {
                loop {
                    let Some(me) = weak.upgrade() else {
                        break;
                    };

                    if let Err(e) = me.watch_loop(&endpoints, &prefix).await {
                        tracing::error!("etcd watch error: {e}, reconnecting...");
                    }

                    // Brief pause before reconnecting
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

                    let Some(me) = weak.upgrade() else {
                        break;
                    };

                    let mut client = match Client::connect(&endpoints, None).await {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::error!("Failed to reconnect to etcd: {e}");
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            continue;
                        }
                    };

                    // Reload all state after reconnection
                    if let Err(e) = me.load_all(&mut client, &prefix).await {
                        tracing::error!("Failed to reload state after reconnection: {e}");
                    }
                }
            }
        });

        Ok(me)
    }

    pub async fn state(&self) -> Arc<HostStates> {
        self.states.lock().await.clone()
    }

    async fn load_all(&self, client: &mut Client, prefix: &str) -> Result<()> {
        let resp = client
            .get(prefix, Some(GetOptions::new().with_prefix()))
            .await?;

        let mut guard = self.states.lock().await;
        let states = Arc::make_mut(&mut *guard);
        states.host_states.clear();

        for kv in resp.kvs() {
            let key = match kv.key_str() {
                Ok(k) => k,
                Err(e) => {
                    tracing::warn!("Skipping etcd key with invalid UTF-8: {e}");
                    continue;
                }
            };
            let value = match kv.value_str() {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("Skipping etcd key '{key}' with invalid UTF-8 value: {e}");
                    continue;
                }
            };

            if let Some((host, commit_state)) = parse_key(key, prefix) {
                let commit = Oid::from_str(value).ok();
                if commit.is_none() {
                    tracing::warn!(
                        "Invalid git OID for host '{host}' state '{state}' (value: '{value}')",
                        state = commit_state.as_ref()
                    );
                }
                states.host_states.entry(host.to_string()).or_default()[commit_state] = commit;
            }
        }

        states.version += 1;
        tracing::info!(
            "Loaded {} hosts from etcd, version: {}",
            states.host_states.len(),
            states.version
        );

        Ok(())
    }

    async fn watch_loop(&self, endpoints: &[String], prefix: &str) -> Result<()> {
        let mut client = Client::connect(endpoints, None).await?;
        let (_watcher, mut stream) = client
            .watch(prefix, Some(WatchOptions::new().with_prefix()))
            .await?;

        tracing::info!("etcd watch established on prefix: {prefix}");

        while let Some(resp) = stream.message().await? {
            for event in resp.events() {
                let Some(kv) = event.kv() else {
                    continue;
                };

                let key = match kv.key_str() {
                    Ok(k) => k,
                    Err(e) => {
                        tracing::warn!("Skipping etcd watch event with invalid key UTF-8: {e}");
                        continue;
                    }
                };
                let Some((host, commit_state)) = parse_key(key, prefix) else {
                    continue;
                };

                match event.event_type() {
                    EventType::Put => {
                        let value = match kv.value_str() {
                            Ok(v) => v,
                            Err(e) => {
                                tracing::warn!(
                                    "Invalid UTF-8 value for host '{host}' state '{state}': {e}",
                                    state = commit_state.as_ref()
                                );
                                continue;
                            }
                        };
                        let commit = Oid::from_str(value).ok();
                        if commit.is_none() {
                            tracing::warn!(
                                "Invalid git OID for host '{host}' state '{state}' (value: '{value}')",
                                state = commit_state.as_ref()
                            );
                        }

                        let mut guard = self.states.lock().await;
                        let states = Arc::make_mut(&mut *guard);
                        let entry = states.host_states.entry(host.to_string()).or_default();
                        let old = entry[commit_state].take();
                        entry[commit_state] = commit;

                        states.version += 1;

                        let old_str = old.map(|c| c.to_string()[..12].to_string());
                        let new_str = commit.as_ref().map(|c| c.to_string()[..12].to_string());
                        tracing::info!(
                            "Host '{}' state '{}' changed from {} to {}",
                            host,
                            commit_state.as_ref(),
                            old_str.as_deref().unwrap_or("None"),
                            new_str.as_deref().unwrap_or("None (invalid)"),
                        );
                    }
                    EventType::Delete => {
                        let mut guard = self.states.lock().await;
                        let states = Arc::make_mut(&mut *guard);
                        if let Some(host_map) = states.host_states.get_mut(host) {
                            host_map[commit_state] = None;
                        }
                        states.version += 1;
                        tracing::info!("Host '{}' state '{}' deleted", host, commit_state.as_ref());
                    }
                }
            }
        }

        tracing::warn!("etcd watch stream ended");
        Ok(())
    }
}

/// Parse an etcd key like "/ogygia/nixos/versions/hostname/state" into (hostname, CommitState)
fn parse_key<'a>(key: &'a str, prefix: &str) -> Option<(&'a str, CommitState)> {
    let rest = key.strip_prefix(prefix)?.strip_prefix('/')?;
    let (host, state_str) = rest.split_once('/')?;
    let state = state_str.parse::<CommitState>().ok()?;
    Some((host, state))
}
