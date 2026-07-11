//! Utilities for reading Nix store metadata and data formats, replacing
//! ad-hoc string surgery and shelling out to the Nix CLIs.
//!
//! `ogygia-irisd` needs fast, index-backed lookups of store-path metadata
//! (the data behind `nix path-info`). Shelling out to `nix path-info --json`
//! per request is slow; this crate provides a [`NixDb`] client — a
//! cheaply-cloneable handle over an async connection pool to
//! `/nix/var/nix/db/db.sqlite` — that answers the same questions directly.
//! Alongside it live the strongly-typed store hashes ([`StoreHash`],
//! [`NarHash`]), the [`PathInfo`] metadata record, and [`NarInfo`] narinfo
//! parsing and serialization.
//!
//! The equivalence between [`NixDb`] and the real `nix path-info` command is
//! verified by the testcontainers-based tests in `db.rs`.

pub mod db;
pub mod narinfo;
pub mod path_info;
pub mod types;

pub use db::NixDb;
pub use narinfo::Compression;
pub use narinfo::NarInfo;
pub use path_info::PathInfo;
pub use path_info::parse_path_info_json;
pub use types::NarHash;
pub use types::StoreHash;
