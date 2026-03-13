//! Read-only access to the Nix SQLite database.
//!
//! The Nix daemon maintains a SQLite database at
//! `/nix/var/nix/db/db.sqlite` (WAL mode) with an indexed `path`
//! column in `ValidPaths`. This module provides fast, index-backed
//! lookups that replace the previous approach of scanning `/nix/store`
//! and shelling out to `nix path-info`.

use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use rusqlite::Connection;
use rusqlite::OpenFlags;

use super::store::PathInfo;

/// Default path to the Nix SQLite database.
const DEFAULT_DB_PATH: &str = "/nix/var/nix/db/db.sqlite";

/// Read-only handle to the Nix store database.
pub struct NixDb {
    conn: Connection,
}

impl NixDb {
    /// Open the Nix database in read-only mode.
    pub fn open() -> Result<Self> {
        Self::open_path(Path::new(DEFAULT_DB_PATH))
    }

    /// Open a Nix database at a specific path (useful for tests).
    pub fn open_path(path: &Path) -> Result<Self> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(path, flags)
            .with_context(|| format!("failed to open Nix database at {}", path.display()))?;
        Ok(Self { conn })
    }

    /// Find a store path by its 32-character hash prefix.
    ///
    /// Uses the unique index on `ValidPaths.path` for an O(log n)
    /// prefix lookup instead of scanning the `/nix/store` directory.
    pub fn find_store_path(&self, hash: &str) -> Result<Option<PathBuf>> {
        let pattern = format!("/nix/store/{}-*", hash);
        let mut stmt = self
            .conn
            .prepare_cached("SELECT path FROM ValidPaths WHERE path GLOB ?1 LIMIT 1")?;
        let path = stmt
            .query_row([&pattern], |row| {
                let p: String = row.get(0)?;
                Ok(PathBuf::from(p))
            })
            .optional()?;
        Ok(path)
    }

    /// Look up full path information by hash prefix.
    ///
    /// Returns the same `PathInfo` that was previously obtained by
    /// shelling out to `nix path-info --json`, but via a direct SQLite
    /// query with index-backed lookup and join.
    ///
    /// The `hash` column in the database is stored as `sha256:<hex>`;
    /// this method converts it to the SRI format `sha256-<base64>`
    /// expected by narinfo consumers.
    pub fn find_path_info(&self, hash: &str) -> Result<Option<PathInfo>> {
        let pattern = format!("/nix/store/{}-*", hash);

        let row = {
            let mut stmt = self.conn.prepare_cached(
                "SELECT id, path, hash, narSize, sigs, ca, deriver \
                 FROM ValidPaths WHERE path GLOB ?1 LIMIT 1",
            )?;
            stmt.query_row([&pattern], |row| {
                Ok(RawPathRow {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    hash: row.get(2)?,
                    nar_size: row.get(3)?,
                    sigs: row.get::<_, Option<String>>(4)?,
                    ca: row.get(5)?,
                    deriver: row.get(6)?,
                })
            })
            .optional()?
        };

        let Some(row) = row else {
            return Ok(None);
        };

        // Fetch references
        let references = {
            let mut stmt = self.conn.prepare_cached(
                "SELECT vp2.path FROM Refs \
                 JOIN ValidPaths vp2 ON vp2.id = Refs.reference \
                 WHERE Refs.referrer = ?1",
            )?;
            let rows = stmt.query_map([row.id], |r| r.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        let nar_hash = hex_hash_to_sri(&row.hash)?;
        let signatures: Vec<String> = row
            .sigs
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| s.split(' ').map(String::from).collect())
            .unwrap_or_default();

        Ok(Some(PathInfo {
            path: row.path,
            nar_hash,
            nar_size: row.nar_size as u64,
            references,
            deriver: row.deriver,
            signatures,
            ca: row.ca,
        }))
    }

    /// Return the 32-character store hashes of all serveable paths.
    ///
    /// A path is serveable if it has signatures or is content-addressed.
    /// Used by the store scanner to populate the bloom filter.
    pub fn serveable_hashes(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT SUBSTR(path, 12, 32) FROM ValidPaths \
             WHERE (sigs IS NOT NULL AND sigs != '') \
                OR (ca IS NOT NULL AND ca != '')",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let hashes = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(hashes)
    }

    /// Check whether a store path is serveable (signed or content-addressed).
    pub fn is_path_serveable(&self, store_path: &str) -> Result<bool> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT 1 FROM ValidPaths \
             WHERE path = ?1 \
               AND ((sigs IS NOT NULL AND sigs != '') \
                 OR (ca IS NOT NULL AND ca != '')) \
             LIMIT 1",
        )?;
        let exists = stmt
            .query_row([store_path], |_| Ok(()))
            .optional()?
            .is_some();
        Ok(exists)
    }
}

/// Intermediate row from the ValidPaths table.
struct RawPathRow {
    id: i64,
    path: String,
    hash: String,
    nar_size: i64,
    sigs: Option<String>,
    ca: Option<String>,
    deriver: Option<String>,
}

/// Convert a Nix database hash (`sha256:<hex>`) to SRI format (`sha256-<base64>`).
fn hex_hash_to_sri(db_hash: &str) -> Result<String> {
    let (algo, hex_str) = db_hash
        .split_once(':')
        .context("invalid hash format in Nix database: missing ':'")?;
    let bytes = hex::decode(hex_str)
        .with_context(|| format!("invalid hex in Nix database hash: {}", hex_str))?;
    let b64 = BASE64.encode(&bytes);
    Ok(format!("{}-{}", algo, b64))
}

/// Extension trait for optional query results.
trait OptionalExt<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalExt<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_hash_to_sri_sha256() {
        let db_hash = "sha256:4b4ca6bc50c269e51f6525b5bdeab4814d765a857cd2a6525dc832965c02cd96";
        let sri = hex_hash_to_sri(db_hash).unwrap();
        assert_eq!(sri, "sha256-S0ymvFDCaeUfZSW1veq0gU12WoV80qZSXcgyllwCzZY=");
    }

    #[test]
    fn hex_hash_to_sri_missing_colon() {
        assert!(hex_hash_to_sri("sha256deadbeef").is_err());
    }

    #[test]
    fn hex_hash_to_sri_invalid_hex() {
        assert!(hex_hash_to_sri("sha256:xyz").is_err());
    }
}
