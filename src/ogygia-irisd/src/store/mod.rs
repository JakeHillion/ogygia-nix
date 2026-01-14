//! Store management for local /nix/store
//!
//! This module provides functionality for:
//! - Watching /nix/store for new paths (watcher)
//! - Scanning existing store paths at startup (scanner)

pub mod scanner;
pub mod watcher;
