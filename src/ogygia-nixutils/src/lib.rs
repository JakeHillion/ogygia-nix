//! Utilities for reading Nix store metadata and data formats, replacing
//! ad-hoc string surgery and shelling out to the Nix CLIs.
//!
//! `ogygia-irisd` needs fast, index-backed lookups of store-path metadata
//! (the data behind `nix path-info`). Shelling out to `nix path-info --json`
//! per request is slow; this crate provides a [`Nix`] handle — a
//! cheaply-cloneable entry point to the local Nix installation — whose
//! store-metadata queries are answered directly from `/nix/var/nix/db/db.sqlite`
//! via an async connection pool opened on first use. Alongside it live the
//! strongly-typed store hashes ([`StoreHash`], [`NarHash`]) and signatures
//! ([`Signature`]), the [`PathInfo`] metadata record, and [`NarInfo`] narinfo
//! parsing and serialization.
//!
//! The equivalence between [`Nix`]'s queries and the real `nix path-info`
//! command is verified by the testcontainers-based tests in `db.rs`.

mod db;
pub mod narinfo;
mod nix;
pub mod path_info;
mod signature;
pub mod types;

pub use narinfo::Compression;
pub use narinfo::NarInfo;
pub use nix::Nix;
pub use path_info::PathInfo;
pub use path_info::parse_path_info_json;
pub use signature::Signature;
pub use types::NarHash;
pub use types::StoreHash;
