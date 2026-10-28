//! The sources of flakes: copying a flake's files to the store, and what
//! the flake learns about where they came from.

use std::collections::HashSet;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::bail;
use gix::objs::tree::EntryKind;
use gix::objs::tree::EntryMode;

use crate::accessor::Accessor;
use crate::accessor::FileType;
use crate::flake::FlakeEnv;
use crate::flake::flakeref;
use crate::flake::flakeref::Attrs;
use crate::io::Io;
use crate::store;
use crate::store::HashAlgo;
use crate::store::HashFormat;

/// What a flake knows about its source, besides where it is.
#[derive(Clone, Debug, Default)]
pub struct Info {
    pub nar_hash: Option<String>,
    pub last_modified: Option<u64>,
    pub rev: Option<String>,
    pub rev_count: Option<u64>,
    pub dirty_rev: Option<String>,
    pub submodules: Option<bool>,
}

/// A source in the store.
#[derive(Clone, Debug)]
pub struct Source {
    /// Its store path.
    pub path: String,
    pub info: Info,
}

/// The files under a directory, or only those in `keep` (relative paths,
/// every ancestor directory included), if set.
struct Dir {
    root: PathBuf,
    keep: Option<HashSet<String>>,
}

impl Dir {
    fn physical(&self, path: &str) -> std::io::Result<PathBuf> {
        if let Some(keep) = &self.keep
            && !path.is_empty()
            && !keep.contains(path)
        {
            return Err(std::io::ErrorKind::NotFound.into());
        }
        Ok(if path.is_empty() {
            self.root.clone()
        } else {
            self.root.join(path)
        })
    }
}

impl Accessor for Dir {
    fn file_type(&self, path: &str) -> Option<FileType> {
        let p = self.physical(path).ok()?;
        std::fs::symlink_metadata(p)
            .ok()
            .map(|m| FileType::of(m.file_type()))
    }

    fn is_dir(&self, path: &str) -> bool {
        self.physical(path).is_ok_and(|p| p.is_dir())
    }

    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
        std::fs::read(self.physical(path)?)
    }

    fn read_dir(&self, path: &str) -> std::io::Result<Vec<(String, FileType)>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(self.physical(path)?)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if self.physical(&crate::path::join(path, &name)).is_err() {
                continue;
            }
            out.push((name, FileType::of(entry.file_type()?)));
        }
        out.sort();
        Ok(out)
    }

    fn read_link(&self, path: &str) -> std::io::Result<Vec<u8>> {
        Ok(std::fs::read_link(self.physical(path)?)?
            .as_os_str()
            .as_bytes()
            .to_vec())
    }

    fn is_executable(&self, path: &str) -> std::io::Result<bool> {
        let meta = std::fs::symlink_metadata(self.physical(path)?)?;
        Ok(meta.permissions().mode() & 0o100 != 0)
    }
}

/// The tree of a Git commit.
struct GitTree {
    repo: gix::Repository,
    tree: gix::ObjectId,
}

/// The type of a Git tree entry, or `None` for a submodule.
fn git_file_type(mode: EntryMode) -> Option<FileType> {
    match mode.kind() {
        EntryKind::Tree => Some(FileType::Directory),
        EntryKind::Link => Some(FileType::Symlink),
        EntryKind::Blob | EntryKind::BlobExecutable => Some(FileType::Regular),
        EntryKind::Commit => None,
    }
}

impl GitTree {
    fn entry(&self, path: &str) -> Option<(EntryMode, gix::ObjectId)> {
        if path.is_empty() {
            return Some((EntryKind::Tree.into(), self.tree));
        }
        let tree = self.repo.find_tree(self.tree).ok()?;
        let entry = tree.lookup_entry_by_path(path).ok()??;
        Some((entry.mode(), entry.object_id()))
    }

    fn blob(&self, path: &str) -> std::io::Result<Vec<u8>> {
        match self.entry(path) {
            Some((mode, id)) if mode.is_blob_or_symlink() => self
                .repo
                .find_blob(id)
                .map(|mut b| b.take_data())
                .map_err(std::io::Error::other),
            Some(_) => Err(std::io::ErrorKind::InvalidInput.into()),
            None => Err(std::io::ErrorKind::NotFound.into()),
        }
    }
}

impl Accessor for GitTree {
    fn file_type(&self, path: &str) -> Option<FileType> {
        let (mode, _) = self.entry(path)?;
        git_file_type(mode)
    }

    fn is_dir(&self, path: &str) -> bool {
        self.file_type(path) == Some(FileType::Directory)
    }

    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
        self.blob(path)
    }

    fn read_dir(&self, path: &str) -> std::io::Result<Vec<(String, FileType)>> {
        let Some((mode, id)) = self.entry(path) else {
            return Err(std::io::ErrorKind::NotADirectory.into());
        };
        if !mode.is_tree() {
            return Err(std::io::ErrorKind::NotADirectory.into());
        }
        let tree = self.repo.find_tree(id).map_err(std::io::Error::other)?;
        let mut out = Vec::new();
        for entry in tree.iter() {
            let entry = entry.map_err(std::io::Error::other)?;
            // Submodules are not fetched.
            let Some(ty) = git_file_type(entry.mode()) else {
                continue;
            };
            out.push((String::from_utf8_lossy(entry.filename()).into_owned(), ty));
        }
        out.sort();
        Ok(out)
    }

    fn read_link(&self, path: &str) -> std::io::Result<Vec<u8>> {
        self.blob(path)
    }

    fn is_executable(&self, path: &str) -> std::io::Result<bool> {
        match self.entry(path) {
            Some((mode, _)) => Ok(mode.kind() == EntryKind::BlobExecutable),
            None => Err(std::io::ErrorKind::NotFound.into()),
        }
    }
}

/// Add `tree` to the store as a source, failing if its NAR hash is not
/// `expected`.
fn add(
    io: &Io,
    tree: Rc<dyn Accessor>,
    expected: Option<&str>,
    what: &str,
) -> Result<(String, String)> {
    let (path, digest) = io.add_tree("source", tree)?;
    let hash = store::format_hash(HashAlgo::Sha256, &digest, HashFormat::Sri);
    if let Some(expected) = expected {
        let (_, want) = store::parse_hash(expected, Some(HashAlgo::Sha256))?;
        if want != digest {
            bail!("NAR hash mismatch in input '{what}', expected '{expected}' but got '{hash}'");
        }
    }
    Ok((path, hash))
}

/// The latest modification time of anything under `path`, itself included.
fn latest_mtime(path: &Path) -> Result<u64> {
    let meta = std::fs::symlink_metadata(path)
        .with_context(|| format!("getting status of '{}'", path.display()))?;
    let mut latest = meta.mtime().max(0) as u64;
    if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            latest = latest.max(latest_mtime(&entry?.path())?);
        }
    }
    Ok(latest)
}

/// Copy the directory `dir` to the store, as a `path:` flake.
fn fetch_path(io: &Io, dir: &Path, locked: &Attrs, what: &str) -> Result<Source> {
    if !dir.exists() {
        bail!("path '{}' does not exist", dir.display());
    }
    let tree = Rc::new(Dir {
        root: dir.to_owned(),
        keep: None,
    });
    let (path, hash) = add(io, tree, flakeref::get_str(locked, "narHash"), what)?;
    let last_modified = match flakeref::get_int(locked, "lastModified") {
        Some(t) => t,
        None => latest_mtime(dir)?,
    };
    Ok(Source {
        path,
        info: Info {
            nar_hash: Some(hash),
            last_modified: Some(last_modified),
            ..Info::default()
        },
    })
}

fn open_repo(dir: &Path) -> Result<gix::Repository> {
    gix::open(dir).with_context(|| format!("opening Git repository '{}'", dir.display()))
}

fn commit_count(repo: &gix::Repository, commit: gix::ObjectId) -> Result<u64> {
    let mut count = 0;
    for info in repo.rev_walk([commit]).all()? {
        info?;
        count += 1;
    }
    Ok(count)
}

/// Whether the tracked files of the repository's working tree, or its
/// index, differ from `HEAD`. Submodules are not considered.
fn is_dirty(repo: &gix::Repository) -> Result<bool> {
    use gix::status::Item;
    use gix::status::index_worktree;
    let submodule = gix::index::entry::Mode::COMMIT;
    let status = repo
        .status(gix::progress::Discard)?
        .untracked_files(gix::status::UntrackedFiles::None)
        .index_worktree_submodules(None)
        .into_iter(None)?;
    for item in status {
        match item? {
            Item::TreeIndex(change) if change.entry_mode() == submodule => {}
            Item::IndexWorktree(index_worktree::Item::Modification { entry, .. })
                if entry.mode == submodule => {}
            Item::IndexWorktree(item) if item.summary().is_none() => {}
            _ => return Ok(true),
        }
    }
    Ok(false)
}

/// Copy the files Git tracks in the working tree of the repository at
/// `dir` to the store, as an unlocked `git+file:` flake.
fn fetch_git_worktree(io: &Io, dir: &Path, what: &str) -> Result<Source> {
    let repo = open_repo(dir)?;
    let mut keep = HashSet::new();
    let index = repo.index_or_empty()?;
    for entry in index.entries() {
        // Submodules are not fetched.
        if entry.mode == gix::index::entry::Mode::COMMIT {
            continue;
        }
        let path = String::from_utf8_lossy(entry.path(&index)).into_owned();
        let mut at = path.as_str();
        keep.insert(at.to_owned());
        while let Some((parent, _)) = at.rsplit_once('/') {
            keep.insert(parent.to_owned());
            at = parent;
        }
    }
    let dirty = is_dirty(&repo)?;
    let head = repo.head_commit().ok();
    let tree = Rc::new(Dir {
        root: repo
            .workdir()
            .context("a Git flake needs a working tree")?
            .to_owned(),
        keep: Some(keep),
    });
    let (path, hash) = add(io, tree, None, what)?;
    let mut info = Info {
        nar_hash: Some(hash),
        last_modified: Some(match &head {
            Some(c) => c.time()?.seconds.max(0) as u64,
            None => 0,
        }),
        submodules: Some(false),
        ..Info::default()
    };
    if let Some(head) = head {
        let rev = head.id.to_string();
        if dirty {
            info.dirty_rev = Some(format!("{rev}-dirty"));
        } else {
            info.rev_count = Some(commit_count(&repo, head.id)?);
            info.rev = Some(rev);
        }
    }
    Ok(Source { path, info })
}

/// Copy the tree of commit `rev` of the repository at `dir` to the store.
fn fetch_git_rev(io: &Io, dir: &Path, rev: &str, locked: &Attrs, what: &str) -> Result<Source> {
    let repo = open_repo(dir)?;
    let (id, last_modified, tree_id) = {
        let commit = repo
            .find_commit(gix::ObjectId::from_hex(rev.as_bytes())?)
            .with_context(|| format!("Git repository '{}' has no commit {rev}", dir.display()))?;
        let time = commit.time()?.seconds.max(0) as u64;
        (commit.id, time, commit.tree_id()?.detach())
    };
    let rev_count = match flakeref::get_int(locked, "revCount") {
        Some(n) => n,
        None => commit_count(&repo, id)?,
    };
    let tree = Rc::new(GitTree {
        repo,
        tree: tree_id,
    });
    let (path, hash) = add(io, tree, flakeref::get_str(locked, "narHash"), what)?;
    Ok(Source {
        path,
        info: Info {
            nar_hash: Some(hash),
            last_modified: Some(flakeref::get_int(locked, "lastModified").unwrap_or(last_modified)),
            rev: Some(id.to_string()),
            rev_count: Some(rev_count),
            submodules: Some(false),
            ..Info::default()
        },
    })
}

/// The source `locked` names, read from `env`'s store, where it must
/// already be.
fn fetch_from_store(io: &Io, env: &FlakeEnv, locked: &Attrs, what: &str) -> Result<Source> {
    let Some(nar_hash) = flakeref::get_str(locked, "narHash") else {
        bail!(
            "cannot fetch input '{what}': it has no NAR hash, and this evaluator only reads inputs from the store"
        );
    };
    let (_, hash) = store::parse_hash(nar_hash, Some(HashAlgo::Sha256))?;
    let path = store::source_path(&hash, "source");
    let base = path.rsplit('/').next().expect("a store path has a name");
    let physical = env.store.join(base);
    if std::fs::symlink_metadata(&physical).is_err() {
        bail!(
            "cannot fetch input '{what}': this evaluator only reads inputs from the store, which does not have '{path}'"
        );
    }
    io.mount(&path, &physical, None);
    Ok(Source {
        path,
        info: Info {
            nar_hash: Some(nar_hash.to_owned()),
            last_modified: flakeref::get_int(locked, "lastModified"),
            rev: flakeref::get_str(locked, "rev").map(str::to_owned),
            rev_count: flakeref::get_int(locked, "revCount"),
            ..Info::default()
        },
    })
}

/// Fetch the source of the input `attrs`, locked or not.
pub fn fetch(io: &Io, env: &FlakeEnv, attrs: &Attrs, what: &str) -> Result<Source> {
    match flakeref::get_str(attrs, "type") {
        Some("path") => {
            let path = flakeref::get_str(attrs, "path").unwrap_or("");
            fetch_path(io, Path::new(path), attrs, what)
        }
        Some("git") => {
            let url = flakeref::get_str(attrs, "url").unwrap_or("");
            match url.strip_prefix("file://") {
                Some(dir) => match flakeref::get_str(attrs, "rev") {
                    Some(rev) => fetch_git_rev(io, Path::new(dir), rev, attrs, what),
                    None => fetch_git_worktree(io, Path::new(dir), what),
                },
                None => fetch_from_store(io, env, attrs, what),
            }
        }
        Some("indirect") => bail!(
            "cannot find flake 'flake:{}' in the flake registries",
            flakeref::get_str(attrs, "id").unwrap_or("")
        ),
        _ => fetch_from_store(io, env, attrs, what),
    }
}

/// `secs` since the epoch as `YYYYMMDDHHMMSS` in UTC.
pub fn date(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// The first seven characters of `rev`, as `shortRev` holds them.
pub fn short_rev(rev: &str) -> &str {
    &rev[..rev.len().min(7)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_dates_as_nix_does() {
        assert_eq!(date(1_600_000_000), "20200913122640");
        assert_eq!(date(1000), "19700101001640");
        assert_eq!(date(1_500_000_100), "20170714024140");
    }
}
