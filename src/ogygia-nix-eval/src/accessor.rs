//! Read-only access to trees of files, wherever they live.

use std::io::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FileType {
    Regular,
    Directory,
    Symlink,
    Unknown,
}

impl FileType {
    pub fn name(self) -> &'static str {
        match self {
            FileType::Regular => "regular",
            FileType::Directory => "directory",
            FileType::Symlink => "symlink",
            FileType::Unknown => "unknown",
        }
    }

    pub(crate) fn of(ft: std::fs::FileType) -> FileType {
        if ft.is_symlink() {
            FileType::Symlink
        } else if ft.is_dir() {
            FileType::Directory
        } else if ft.is_file() {
            FileType::Regular
        } else {
            FileType::Unknown
        }
    }
}

/// A tree of files. Errors do not name the path; callers add it.
pub trait Accessor {
    /// The type of `path` without following a final symlink, or `None` if it
    /// does not exist. Fails if resolving `path` loops.
    fn file_type(&self, path: &str) -> Result<Option<FileType>>;

    /// The type of what `path` resolves to, following symlinks, or `None` if
    /// it does not resolve. Fails if resolving `path` loops.
    fn resolved_type(&self, path: &str) -> Result<Option<FileType>>;

    /// The contents of the file at `path`, following symlinks.
    fn read_file(&self, path: &str) -> Result<Vec<u8>>;

    /// The entries of the directory at `path`, sorted by name.
    fn read_dir(&self, path: &str) -> Result<Vec<(String, FileType)>>;

    /// The target of the symlink at `path`.
    fn read_link(&self, path: &str) -> Result<Vec<u8>>;

    /// Whether the regular file at `path` is executable.
    fn is_executable(&self, path: &str) -> Result<bool>;

    /// `path` with every symlink in it, its last component included,
    /// followed. A tree that does not follow its symlinks returns `path`.
    fn real_path(&self, path: &str) -> Result<String> {
        Ok(path.to_owned())
    }
}

/// The error of resolving a path through too many symlinks.
#[derive(Debug, thiserror::Error)]
#[error("infinite symlink recursion")]
pub struct SymlinkLoop;

impl SymlinkLoop {
    pub(crate) fn error() -> std::io::Error {
        std::io::Error::other(SymlinkLoop)
    }

    /// Whether `e` is a [`SymlinkLoop`].
    pub(crate) fn is(e: &std::io::Error) -> bool {
        e.get_ref().is_some_and(|e| e.is::<SymlinkLoop>())
    }
}
