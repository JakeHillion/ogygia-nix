//! Read-only utilities for querying the Nix store database.
//!
//! `ogygia-irisd` needs fast, index-backed lookups of store-path metadata
//! (the data behind `nix path-info`). Shelling out to `nix path-info --json`
//! per request is slow; this crate provides a [`NixDb`] client — a
//! cheaply-cloneable handle over an async connection pool to
//! `/nix/var/nix/db/db.sqlite` — that answers the same questions directly.
//!
//! The equivalence between [`NixDb`] and the real `nix path-info` command is
//! verified by the testcontainers-based tests in `db.rs`.

pub mod db;
pub mod path_info;
pub mod types;

pub use db::NixDb;
pub use path_info::PathInfo;
pub use path_info::parse_path_info_json;
pub use types::NarHash;
pub use types::StoreHash;
