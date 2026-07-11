//! A handle to the local Nix installation.
//!
//! [`Nix`] is the entry point for asking questions about the local store. It is
//! deliberately named for the domain, not the backend: today its store-metadata
//! queries are answered by the Nix SQLite database ([`NixDb`]), but a caller
//! writes `nix.find_path_info(..)` regardless of how that answer is produced, so
//! individual operations can move between backends (a direct query, shelling out
//! to a `nix` command, a Rust reimplementation) without touching call sites.
//!
//! Backends are opened lazily on first use and cached for the life of the
//! handle. A caller that only exercises operations which don't need the database
//! never opens it — and never has to declare up front whether it will.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::OnceCell;

use crate::db::NixDb;
use crate::path_info::PathInfo;
use crate::types::StoreHash;

/// A handle to the local Nix installation.
///
/// Cloning is cheap and all clones share one lazily-opened database: the first
/// database-backed call anywhere opens the connection pool, and every clone sees
/// it thereafter. The pool then stays open for the life of the handle rather
/// than being torn down between calls, so repeated lookups reuse connections.
#[derive(Clone)]
pub struct Nix {
    /// Shared so that cloning before the first query can't open several pools;
    /// the `Arc` makes "open once" hold across every clone, not once per clone.
    db: Arc<OnceCell<NixDb>>,
}

impl Nix {
    /// A handle backed by an empty in-memory database, for tests that need a
    /// [`Nix`] but no real store data: every lookup returns "not found".
    pub async fn in_memory() -> Result<Self> {
        let db = NixDb::open_in_memory().await?;
        Ok(Self {
            db: Arc::new(OnceCell::new_with(Some(db))),
        })
    }

    /// The store database, opened on first use and cached for the process.
    async fn db(&self) -> Result<&NixDb> {
        self.db.get_or_try_init(NixDb::open).await
    }

    /// Find a store path by its store-path hash.
    pub async fn find_store_path(&self, hash: &StoreHash) -> Result<Option<PathBuf>> {
        self.db().await?.find_store_path(hash).await
    }

    /// Look up full path information by store-path hash.
    pub async fn find_path_info(&self, hash: &StoreHash) -> Result<Option<PathInfo>> {
        self.db().await?.find_path_info(hash).await
    }

    /// Return the 32-character store hashes of all serveable paths.
    pub async fn serveable_hashes(&self) -> Result<Vec<StoreHash>> {
        self.db().await?.serveable_hashes().await
    }

    /// Check whether a store path is serveable (signed or content-addressed).
    pub async fn is_path_serveable(&self, store_path: &str) -> Result<bool> {
        self.db().await?.is_path_serveable(store_path).await
    }
}

impl Default for Nix {
    /// A handle to the Nix installation at its default location.
    fn default() -> Self {
        Self {
            db: Arc::new(OnceCell::new()),
        }
    }
}
