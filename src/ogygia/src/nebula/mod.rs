//! Nebula certificate management module.
//!
//! Provides CLI commands for managing Nebula overlay network certificates
//! with content-addressed storage (similar to agenix-rekey).
//!
//! This module enables content-addressed Nebula certificate management
//! where certificate paths are computed from (pubKey + IP + groups + FQDN).

#[cfg(feature = "nebula")]
pub mod cli;

#[cfg(feature = "nebula")]
pub mod cert;
