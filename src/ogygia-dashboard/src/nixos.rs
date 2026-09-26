use chrono::DateTime;
use chrono::Utc;
use enum_map::Enum;
use serde::Deserialize;
use serde::Serialize;
use strum::AsRefStr;
use strum::EnumIter;
use strum::EnumString;

#[derive(EnumIter, AsRefStr, Enum, EnumString, Debug, Copy, Clone)]
#[strum(serialize_all = "lowercase")]
pub enum CommitState {
    Booted,
    Current,
    NextBoot,
}

/// Information about a commit relevant to host states
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CommitInfo {
    Missing(String), // Just the hash
    Complete {
        hash: String,
        message: String,
        author: String,
        timestamp: DateTime<Utc>,
        branch: String,
        hosts_using: Vec<String>, // Which hosts are using this commit
        /// Nearest ancestors among the commits being shown, real parents first.
        parents: Vec<GraphParent>,
    },
}

/// An edge from a shown commit to one of its nearest shown ancestors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphParent {
    pub hash: String,
    /// Whether commits that aren't shown lie between the two.
    pub elided: bool,
}

impl CommitInfo {
    pub fn hash(&self) -> &str {
        match self {
            CommitInfo::Missing(hash) => hash,
            CommitInfo::Complete { hash, .. } => hash,
        }
    }

    pub fn parents(&self) -> &[GraphParent] {
        match self {
            CommitInfo::Missing(_) => &[],
            CommitInfo::Complete { parents, .. } => parents,
        }
    }

    /// Get the short version of the commit hash (first 7 characters)
    pub fn short_hash(&self) -> &str {
        let hash = self.hash();
        &hash[..7.min(hash.len())]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_commit_info_creation() {
        let commit_info = CommitInfo::Complete {
            hash: "abcdef123456".to_string(),
            message: "Test commit".to_string(),
            author: "Test Author".to_string(),
            timestamp: Utc::now(),
            branch: "main".to_string(),
            hosts_using: vec!["host1".to_string(), "host2".to_string()],
            parents: Vec::new(),
        };

        assert_eq!(commit_info.short_hash(), "abcdef1");

        if let CommitInfo::Complete {
            hash,
            message,
            author,
            branch,
            hosts_using,
            ..
        } = &commit_info
        {
            assert_eq!(hash, "abcdef123456");
            assert_eq!(message, "Test commit");
            assert_eq!(author, "Test Author");
            assert_eq!(branch, "main");
            assert_eq!(hosts_using.len(), 2);
        } else {
            panic!("Expected Complete variant");
        }
    }
}
