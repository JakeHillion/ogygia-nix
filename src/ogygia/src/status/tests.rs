//! Unit tests for status module.

use super::display::trim_domain_suffix;
use super::display::truncate_revision;
use super::state::HostMatcher;
use super::state::HostState;
use super::state::STATE_COUNT;
use super::state::empty_state_values;
use super::state::join_etcd_path;
use super::state::join_zk_path;
use super::state::normalize_domain;
use super::state::normalize_namespace;

// ============================================================================
// HostMatcher tests
// ============================================================================

#[test]
fn test_hostname_matcher_exact_match() {
    let matcher = HostMatcher::new("web01.example.com");
    assert!(matcher.matches("web01.example.com"));
}

#[test]
fn test_hostname_matcher_short_match() {
    let matcher = HostMatcher::new("web01.example.com");
    assert!(matcher.matches("web01"));
}

#[test]
fn test_hostname_matcher_case_insensitive() {
    let matcher = HostMatcher::new("web01.example.com");
    assert!(matcher.matches("WEB01.EXAMPLE.COM"));
    assert!(matcher.matches("WEB01"));
    assert!(matcher.matches("Web01"));
}

#[test]
fn test_hostname_matcher_short_hostname_matching_fqdn() {
    let matcher = HostMatcher::new("web01");
    assert!(matcher.matches("web01.example.com"));
}

#[test]
fn test_hostname_matcher_no_match() {
    let matcher = HostMatcher::new("web01.example.com");
    assert!(!matcher.matches("web02"));
    assert!(!matcher.matches("web02.example.com"));
    assert!(!matcher.matches("db01.example.com"));
}

#[test]
fn test_hostname_matcher_whitespace_trimmed() {
    let matcher = HostMatcher::new("  web01.example.com  ");
    assert!(matcher.matches("  web01  "));
    assert!(matcher.matches("web01.example.com"));
}

#[test]
fn test_hostname_matcher_empty_short_name() {
    // Edge case: hostname that is just a dot
    let matcher = HostMatcher::new(".");
    assert!(matcher.matches("."));
}

// ============================================================================
// ZooKeeper path manipulation tests
// ============================================================================

#[test]
fn test_join_zk_path_normal() {
    assert_eq!(join_zk_path("/foo", "bar"), "/foo/bar");
}

#[test]
fn test_join_zk_path_with_trailing_slash() {
    assert_eq!(join_zk_path("/foo/", "bar"), "/foo/bar");
}

#[test]
fn test_join_zk_path_with_leading_slash() {
    assert_eq!(join_zk_path("/foo", "/bar"), "/foo/bar");
}

#[test]
fn test_join_zk_path_both_slashes() {
    assert_eq!(join_zk_path("/foo/", "/bar"), "/foo/bar");
}

#[test]
fn test_join_zk_path_root_prefix() {
    assert_eq!(join_zk_path("/", "bar"), "/bar");
    assert_eq!(join_zk_path("/", "/bar"), "/bar");
}

#[test]
fn test_join_zk_path_multi_level() {
    assert_eq!(join_zk_path("/a/b/c", "d/e"), "/a/b/c/d/e");
}

// ============================================================================
// etcd path manipulation tests
// ============================================================================

#[test]
fn test_join_etcd_path_normal() {
    assert_eq!(join_etcd_path("/foo", "bar"), "/foo/bar");
}

#[test]
fn test_join_etcd_path_with_trailing_slash() {
    assert_eq!(join_etcd_path("/foo/", "bar"), "/foo/bar");
}

#[test]
fn test_join_etcd_path_with_leading_slash() {
    assert_eq!(join_etcd_path("/foo", "/bar"), "/foo/bar");
}

#[test]
fn test_join_etcd_path_both_slashes() {
    assert_eq!(join_etcd_path("/foo/", "/bar"), "/foo/bar");
}

#[test]
fn test_join_etcd_path_root_prefix() {
    assert_eq!(join_etcd_path("/", "bar"), "/bar");
    assert_eq!(join_etcd_path("/", "/bar"), "/bar");
}

#[test]
fn test_join_etcd_path_multi_level() {
    assert_eq!(join_etcd_path("/a/b/c", "d/e"), "/a/b/c/d/e");
}

// ============================================================================
// Revision truncation tests
// ============================================================================

#[test]
fn test_truncate_revision_long_hash() {
    let hash = "abcdef1234567890abcdef";
    assert_eq!(truncate_revision(hash), "abcdef123456");
    assert_eq!(truncate_revision(hash).len(), 12);
}

#[test]
fn test_truncate_revision_exact_length() {
    let hash = "abcdef123456";
    assert_eq!(truncate_revision(hash), "abcdef123456");
}

#[test]
fn test_truncate_revision_short() {
    let hash = "abc123";
    assert_eq!(truncate_revision(hash), "abc123");
}

#[test]
fn test_truncate_revision_empty() {
    assert_eq!(truncate_revision(""), "");
}

// ============================================================================
// Domain suffix trimming tests
// ============================================================================

#[test]
fn test_trim_domain_suffix_basic() {
    assert_eq!(
        trim_domain_suffix("web01.example.com", Some("example.com")),
        "web01"
    );
}

#[test]
fn test_trim_domain_suffix_case_insensitive() {
    assert_eq!(
        trim_domain_suffix("WEB01.EXAMPLE.COM", Some("example.com")),
        "WEB01"
    );
    assert_eq!(
        trim_domain_suffix("web01.example.com", Some("EXAMPLE.COM")),
        "web01"
    );
}

#[test]
fn test_trim_domain_suffix_no_match() {
    assert_eq!(
        trim_domain_suffix("web01.other.com", Some("example.com")),
        "web01.other.com"
    );
}

#[test]
fn test_trim_domain_suffix_none() {
    assert_eq!(
        trim_domain_suffix("web01.example.com", None),
        "web01.example.com"
    );
}

#[test]
fn test_trim_domain_suffix_empty_domain() {
    assert_eq!(
        trim_domain_suffix("web01.example.com", Some("")),
        "web01.example.com"
    );
}

#[test]
fn test_trim_domain_suffix_with_dots() {
    assert_eq!(
        trim_domain_suffix("web01.example.com", Some(".example.com.")),
        "web01"
    );
}

#[test]
fn test_trim_domain_suffix_short_hostname() {
    assert_eq!(trim_domain_suffix("web01", Some("example.com")), "web01");
}

#[test]
fn test_trim_domain_suffix_whitespace() {
    // Host whitespace is trimmed, but domain whitespace is not automatically trimmed
    // Domain normalization should be done by normalize_domain() before passing here
    assert_eq!(
        trim_domain_suffix("  web01.example.com  ", Some("example.com")),
        "web01"
    );
}

// ============================================================================
// Domain normalization tests
// ============================================================================

#[test]
fn test_normalize_domain_basic() {
    assert_eq!(
        normalize_domain("example.com"),
        Some("example.com".to_string())
    );
}

#[test]
fn test_normalize_domain_with_dots() {
    assert_eq!(
        normalize_domain(".example.com."),
        Some("example.com".to_string())
    );
}

#[test]
fn test_normalize_domain_whitespace() {
    assert_eq!(
        normalize_domain("  example.com  "),
        Some("example.com".to_string())
    );
}

#[test]
fn test_normalize_domain_empty() {
    assert_eq!(normalize_domain(""), None);
    assert_eq!(normalize_domain("   "), None);
    assert_eq!(normalize_domain("..."), None);
}

// ============================================================================
// Namespace normalization tests
// ============================================================================

#[test]
fn test_normalize_namespace_basic() {
    assert_eq!(normalize_namespace("/nixos/versions"), "/nixos/versions");
}

#[test]
fn test_normalize_namespace_no_leading_slash() {
    assert_eq!(normalize_namespace("nixos/versions"), "/nixos/versions");
}

#[test]
fn test_normalize_namespace_trailing_slash() {
    assert_eq!(normalize_namespace("/nixos/versions/"), "/nixos/versions");
}

#[test]
fn test_normalize_namespace_both_slashes() {
    assert_eq!(normalize_namespace("nixos/versions/"), "/nixos/versions");
}

#[test]
fn test_normalize_namespace_root() {
    assert_eq!(normalize_namespace("/"), "/");
}

#[test]
fn test_normalize_namespace_empty() {
    assert_eq!(normalize_namespace(""), "/nixos/versions");
    assert_eq!(normalize_namespace("   "), "/nixos/versions");
}

#[test]
fn test_normalize_namespace_whitespace() {
    assert_eq!(
        normalize_namespace("  /nixos/versions  "),
        "/nixos/versions"
    );
}

// ============================================================================
// HostState display tests
// ============================================================================

#[test]
fn test_host_state_display_local() {
    let state = HostState {
        host: "web01".to_string(),
        values: [None, None, None],
        is_local: true,
    };
    // With domain trimming
    assert_eq!(trim_domain_suffix(&state.host, None), "web01");
}

#[test]
fn test_host_state_display_remote() {
    let state = HostState {
        host: "web01".to_string(),
        values: [None, None, None],
        is_local: false,
    };
    assert_eq!(trim_domain_suffix(&state.host, None), "web01");
}

// ============================================================================
// Empty state values tests
// ============================================================================

#[test]
fn test_empty_state_values() {
    let values = empty_state_values();
    assert_eq!(values.len(), STATE_COUNT);
    assert!(values.iter().all(|v| v.is_none()));
}
