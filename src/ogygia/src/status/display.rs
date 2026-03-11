//! Display and rendering logic for status output.
//!
//! This module handles all formatting, truncation, and table rendering.
//! It takes raw state data and formats it for human-readable display.

use super::state::HostState;
use super::state::STATE_COUNT;
use super::state::StateValues;

/// Number of characters to display from revision hashes (e.g., git SHA).
const REVISION_DISPLAY_LENGTH: usize = 12;

/// Display label for a system state (includes emoji).
pub struct SystemStateLabel {
    /// Human-readable label with emoji for table display.
    pub label: &'static str,
}

/// Display labels for the three system states.
pub const SYSTEM_STATE_LABELS: [SystemStateLabel; STATE_COUNT] = [
    SystemStateLabel {
        label: "⚡ current",
    },
    SystemStateLabel {
        label: "🥾 booted"
    },
    SystemStateLabel {
        label: "🔜 next boot",
    },
];

/// Prints a formatted table of host states to stdout.
///
/// Calculates column widths dynamically based on content to ensure proper
/// alignment. Local host is marked with an asterisk and sorted first.
///
/// Applies display transformations:
/// - Truncates revision hashes to 12 characters
/// - Trims domain suffix from hostnames if provided
/// - Shows "unknown" for missing revisions
pub fn print_host_table(rows: &[HostState], domain_suffix: Option<&str>) {
    // Calculate display labels for each row
    let display_rows: Vec<_> = rows
        .iter()
        .map(|row| {
            let display_host = if row.is_local {
                format!("* {} (local)", trim_domain_suffix(&row.host, domain_suffix))
            } else {
                trim_domain_suffix(&row.host, domain_suffix)
            };

            let display_values: StateValues = row.values.clone().map(|opt| {
                opt.map(|rev| truncate_revision(&rev))
                    .or_else(|| Some("unknown".to_string()))
            });

            (display_host, display_values)
        })
        .collect();

    // Calculate column widths
    let mut host_width = "Host".len();
    let mut column_widths = [0usize; STATE_COUNT];

    for (host, values) in &display_rows {
        host_width = host_width.max(host.len());
        for (idx, value) in values.iter().enumerate() {
            let len = value.as_deref().unwrap_or("unknown").len();
            column_widths[idx] = column_widths[idx].max(len);
        }
    }

    for (idx, label) in SYSTEM_STATE_LABELS.iter().enumerate() {
        column_widths[idx] = column_widths[idx].max(label.label.len());
    }

    // Print header
    let mut header = format!("{:<host_width$}", "Host", host_width = host_width);
    for (idx, label) in SYSTEM_STATE_LABELS.iter().enumerate() {
        header.push(' ');
        header.push_str(&format!(
            "{:<width$}",
            label.label,
            width = column_widths[idx]
        ));
    }
    println!("{header}");
    println!("{}", "-".repeat(header.len()));

    // Print rows
    for (host, values) in display_rows {
        let mut line = format!("{:<host_width$}", host, host_width = host_width);
        for (idx, value) in values.iter().enumerate() {
            line.push(' ');
            line.push_str(&format!(
                "{:<width$}",
                value.as_deref().unwrap_or("unknown"),
                width = column_widths[idx]
            ));
        }
        println!("{line}");
    }
}

/// Truncates a revision string to the display length for readability.
///
/// Git SHAs and similar revision identifiers are typically long hashes.
/// This function shortens them to the first 12 characters for cleaner
/// table display while remaining unique enough for identification.
pub fn truncate_revision(revision: &str) -> String {
    if revision.len() > REVISION_DISPLAY_LENGTH {
        revision[..REVISION_DISPLAY_LENGTH].to_string()
    } else {
        revision.to_string()
    }
}

/// Trims a domain suffix from a hostname for cleaner display.
///
/// For example, `web01.example.com` with domain `example.com` becomes `web01`.
/// Matching is case-insensitive.
pub fn trim_domain_suffix(host: &str, domain: Option<&str>) -> String {
    let trimmed_host = host.trim();
    let Some(domain) = domain else {
        return trimmed_host.to_string();
    };

    let sanitized = domain.trim_matches('.');
    if sanitized.is_empty() {
        return trimmed_host.to_string();
    }

    let suffix = format!(".{}", sanitized);
    if trimmed_host.len() <= suffix.len() {
        return trimmed_host.to_string();
    }

    if trimmed_host
        .to_ascii_lowercase()
        .ends_with(&suffix.to_ascii_lowercase())
    {
        let cutoff = trimmed_host.len() - suffix.len();
        trimmed_host[..cutoff].trim_end_matches('.').to_string()
    } else {
        trimmed_host.to_string()
    }
}
