//! System state reading for ogygiad.
//!
//! This module provides functions to read NixOS build revisions from
//! the standard system state paths.

use std::fs;
use std::path::Path;

use ogygia_common::{REVISION_RELATIVE_PATH, STATE_COUNT, SYSTEM_STATE_DATA};

/// Array of optional revision strings for the three tracked states.
pub type StateValues = [Option<String>; STATE_COUNT];

/// Reads a build revision from a system state directory.
///
/// Returns the full revision string (not truncated). Returns `None` if the file
/// doesn't exist.
pub fn read_revision(base_path: &Path) -> Option<String> {
    let revision_path = base_path.join(REVISION_RELATIVE_PATH);

    match fs::read_to_string(&revision_path) {
        Ok(contents) => Some(contents.trim().to_string()),
        Err(_) => None,
    }
}

/// Collects current system state by reading from all tracked filesystem paths.
///
/// Returns an array of optional revision strings for current, booted, and nextboot states.
pub fn collect_all_revisions() -> StateValues {
    let mut values: StateValues = [None, None, None];
    for (idx, state) in SYSTEM_STATE_DATA.iter().enumerate() {
        values[idx] = read_revision(Path::new(state.base_path));
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collect_all_revisions_returns_array() {
        let values = collect_all_revisions();
        assert_eq!(values.len(), STATE_COUNT);
    }
}
