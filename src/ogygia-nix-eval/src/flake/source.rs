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
use gix::objs::FindExt as _;
use gix::objs::tree::EntryKind;
use gix::objs::tree::EntryMode;

use crate::accessor::Accessor;
use crate::accessor::FileType;
use crate::archive;
use crate::archive::MemTree;
use crate::fetch::Fetcher;
use crate::fetch::Method;
use crate::fetch::Request;
use crate::flake::FlakeEnv;
use crate::flake::flakeref;
use crate::flake::flakeref::Attrs;
use crate::git;
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
    objects: Box<dyn gix::objs::Find>,
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
        let mut found = (EntryMode::from(EntryKind::Tree), self.tree);
        let mut buf = Vec::new();
        for name in path.split('/').filter(|n| !n.is_empty()) {
            if !found.0.is_tree() {
                return None;
            }
            let tree = self.objects.find_tree(&found.1, &mut buf).ok()?;
            let entry = tree.entries.iter().find(|e| e.filename == name)?;
            found = (entry.mode, entry.oid.to_owned());
        }
        Some(found)
    }

    fn blob(&self, path: &str) -> std::io::Result<Vec<u8>> {
        match self.entry(path) {
            Some((mode, id)) if mode.is_blob_or_symlink() => {
                let mut buf = Vec::new();
                self.objects
                    .find_blob(&id, &mut buf)
                    .map(|b| b.data.to_vec())
                    .map_err(std::io::Error::other)
            }
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
        let mut buf = Vec::new();
        let tree = self
            .objects
            .find_tree(&id, &mut buf)
            .map_err(std::io::Error::other)?;
        let mut out = Vec::new();
        for entry in tree.entries {
            // Submodules are not fetched.
            let Some(ty) = git_file_type(entry.mode) else {
                continue;
            };
            out.push((String::from_utf8_lossy(entry.filename).into_owned(), ty));
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
        objects: Box::new(repo.objects.clone()),
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

/// The number of commits reachable from `commit` in `objects`, itself
/// included.
fn count_commits(objects: &dyn gix::objs::Find, commit: gix::ObjectId) -> Result<u64> {
    let mut seen = HashSet::from([commit]);
    let mut todo = vec![commit];
    let mut buf = Vec::new();
    while let Some(id) = todo.pop() {
        for parent in objects.find_commit(&id, &mut buf)?.parents() {
            if seen.insert(parent) {
                todo.push(parent);
            }
        }
    }
    Ok(seen.len() as u64)
}

/// Copy the tree of commit `rev` of the remote repository at `url` to the
/// store. A locked input's revision count and modification time are the
/// ones `attrs` gives, if any, and only its commit is fetched; otherwise
/// they come from the repository's history.
fn fetch_git_remote(
    io: &Io,
    env: &FlakeEnv,
    url: &str,
    attrs: &Attrs,
    what: &str,
) -> Result<Source> {
    let Some(rev) = flakeref::get_str(attrs, "rev") else {
        bail!(
            "cannot fetch input '{what}': it has no revision, and this evaluator does not look them up"
        );
    };
    if !url.starts_with("https://") && !url.starts_with("http://") {
        bail!("cannot fetch input '{what}': only HTTP and HTTPS Git repositories can be fetched");
    }
    if flakeref::get_bool(attrs, "submodules") == Some(true) {
        bail!("cannot fetch input '{what}': Git submodules are not supported");
    }
    let id = gix::ObjectId::from_hex(rev.as_bytes())?;
    let nar_hash = flakeref::get_str(attrs, "narHash");
    let objects = git::fetch_commit(fetcher(env, url)?, url, id, nar_hash.is_some())
        .with_context(|| format!("fetching '{url}'"))?;
    let mut buf = Vec::new();
    let commit = objects
        .find_commit(&id, &mut buf)
        .with_context(|| format!("'{url}' has no commit {rev}"))?;
    let tree_id = commit.tree();
    let (rev_count, last_modified) = match nar_hash {
        Some(_) => (
            flakeref::get_int(attrs, "revCount"),
            flakeref::get_int(attrs, "lastModified"),
        ),
        None => (
            Some(count_commits(&objects, id)?),
            Some(commit.committer()?.time()?.seconds.max(0) as u64),
        ),
    };
    let tree = Rc::new(GitTree {
        objects: Box::new(objects),
        tree: tree_id,
    });
    let (path, hash) = add(io, tree, nar_hash, what)?;
    Ok(Source {
        path,
        info: Info {
            nar_hash: Some(hash),
            last_modified,
            rev: Some(id.to_string()),
            rev_count,
            submodules: Some(false),
            ..Info::default()
        },
    })
}

/// The source `locked` names, read from `env`'s store, if it is there.
fn from_store(io: &Io, env: &FlakeEnv, locked: &Attrs) -> Result<Option<Source>> {
    let (Some(store_dir), Some(nar_hash)) = (&env.store, flakeref::get_str(locked, "narHash"))
    else {
        return Ok(None);
    };
    let (_, hash) = store::parse_hash(nar_hash, Some(HashAlgo::Sha256))?;
    let path = store::source_path(&hash, "source");
    let base = path.rsplit('/').next().expect("a store path has a name");
    let physical = store_dir.join(base);
    if std::fs::symlink_metadata(&physical).is_err() {
        return Ok(None);
    }
    io.mount(&path, &physical, None);
    Ok(Some(Source {
        path,
        info: Info {
            nar_hash: Some(nar_hash.to_owned()),
            last_modified: flakeref::get_int(locked, "lastModified"),
            rev: flakeref::get_str(locked, "rev").map(str::to_owned),
            rev_count: flakeref::get_int(locked, "revCount"),
            submodules: (flakeref::get_str(locked, "type") == Some("git"))
                .then(|| flakeref::get_bool(locked, "submodules").unwrap_or(false)),
            ..Info::default()
        },
    }))
}

/// `env`'s fetcher, to fetch `url` with.
fn fetcher<'e>(env: &'e FlakeEnv, url: &str) -> Result<&'e dyn Fetcher> {
    match &env.fetcher {
        Some(fetcher) => Ok(&**fetcher),
        None => bail!("cannot download '{url}': this evaluator has no fetcher"),
    }
}

/// The contents of the file at `url`: read directly for a `file://` URL,
/// and downloaded by `env`'s fetcher otherwise.
fn download(env: &FlakeEnv, url: &str) -> Result<Vec<u8>> {
    if let Some(path) = url.strip_prefix("file://") {
        return std::fs::read(path).with_context(|| format!("reading '{path}'"));
    }
    let request = Request {
        method: Method::Get,
        url,
        headers: &[],
        body: &[],
    };
    let response = fetcher(env, url)?
        .request(&request)
        .with_context(|| format!("downloading '{url}'"))?;
    Ok(response.body)
}

/// Unpack the archive at `url` into the store. Its last modification time
/// is the one `attrs` gives, or, if they are not locked, the archive's.
fn fetch_archive(io: &Io, env: &FlakeEnv, url: &str, attrs: &Attrs, what: &str) -> Result<Source> {
    let unpacked =
        archive::unpack(&download(env, url)?).with_context(|| format!("unpacking '{url}'"))?;
    let nar_hash = flakeref::get_str(attrs, "narHash");
    let (path, hash) = add(io, Rc::new(unpacked.tree), nar_hash, what)?;
    let last_modified = match flakeref::get_int(attrs, "lastModified") {
        Some(t) => Some(t),
        None if nar_hash.is_none() => unpacked.last_modified,
        None => None,
    };
    Ok(Source {
        path,
        info: Info {
            nar_hash: Some(hash),
            last_modified,
            rev: flakeref::get_str(attrs, "rev").map(str::to_owned),
            ..Info::default()
        },
    })
}

/// Add the file at `url` to the store, as it is.
fn fetch_file(io: &Io, env: &FlakeEnv, url: &str, attrs: &Attrs, what: &str) -> Result<Source> {
    let tree = Rc::new(MemTree::file(download(env, url)?));
    let (path, hash) = add(io, tree, flakeref::get_str(attrs, "narHash"), what)?;
    Ok(Source {
        path,
        info: Info {
            nar_hash: Some(hash),
            last_modified: flakeref::get_int(attrs, "lastModified"),
            ..Info::default()
        },
    })
}

/// The URL of the archive of revision `rev` of the GitHub, GitLab or
/// SourceHut repository `attrs` names.
fn forge_archive_url(ty: &str, attrs: &Attrs, rev: &str) -> String {
    let owner = flakeref::get_str(attrs, "owner").unwrap_or("");
    let repo = flakeref::get_str(attrs, "repo").unwrap_or("");
    let host = flakeref::get_str(attrs, "host");
    match ty {
        "github" => format!(
            "https://{}/{owner}/{repo}/archive/{rev}.tar.gz",
            host.unwrap_or("github.com")
        ),
        "gitlab" => format!(
            "https://{}/api/v4/projects/{}%2F{repo}/repository/archive.tar.gz?sha={rev}",
            host.unwrap_or("gitlab.com"),
            owner.replace('/', "%2F")
        ),
        _ => format!(
            "https://{}/{owner}/{repo}/archive/{rev}.tar.gz",
            host.unwrap_or("git.sr.ht")
        ),
    }
}

/// Fetch the source of the input `attrs`, locked or not.
pub fn fetch(io: &Io, env: &FlakeEnv, attrs: &Attrs, what: &str) -> Result<Source> {
    let ty = flakeref::get_str(attrs, "type").unwrap_or("");
    let url = flakeref::get_str(attrs, "url").unwrap_or("");
    match (ty, url.strip_prefix("file://")) {
        ("path", _) => {
            let path = flakeref::get_str(attrs, "path").unwrap_or("");
            return fetch_path(io, Path::new(path), attrs, what);
        }
        ("git", Some(dir)) => {
            return match flakeref::get_str(attrs, "rev") {
                Some(rev) => fetch_git_rev(io, Path::new(dir), rev, attrs, what),
                None => fetch_git_worktree(io, Path::new(dir), what),
            };
        }
        ("indirect", _) => bail!(
            "cannot find flake 'flake:{}' in the flake registries",
            flakeref::get_str(attrs, "id").unwrap_or("")
        ),
        _ => {}
    }
    if let Some(source) = from_store(io, env, attrs)? {
        return Ok(source);
    }
    match ty {
        "tarball" => fetch_archive(io, env, url, attrs, what),
        "file" => fetch_file(io, env, url, attrs, what),
        "github" | "gitlab" | "sourcehut" => {
            let Some(rev) = flakeref::get_str(attrs, "rev") else {
                bail!(
                    "cannot fetch input '{what}': it has no revision, and this evaluator does not look them up"
                );
            };
            fetch_archive(io, env, &forge_archive_url(ty, attrs, rev), attrs, what)
        }
        "git" => fetch_git_remote(io, env, url, attrs, what),
        _ => bail!("input type '{ty}' is unsupported"),
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

    // The flakes below evaluate as `nix eval --json
    // --no-update-lock-file` evaluates them, with lock files written by
    // `nix flake lock` and the expected output printed by Nix 2.36.
    const TARBALL_HASH: &str = "sha256-nnyd4JnnpMA0Lp5sw3G+jnPNNLfc1rJRGYNiHL6ab9c=";
    const FILE_HASH: &str = "sha256-RCJ37DJg+G+SI5m0zPshU3sGVDGrhCq/TeBVi/rLRVY=";
    const REV: &str = "5edf11c44bc78a0d334f6334cdaf7d60d732daab";

    /// A gzipped tarball of `top/`, whose NAR hash is `TARBALL_HASH`.
    fn tarball() -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut b = tar::Builder::new(gz);
        let mut add = |path: &str, ty: tar::EntryType, mode: u32, data: &[u8]| {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(ty);
            h.set_mode(mode);
            h.set_mtime(1000);
            if ty.is_symlink() {
                h.set_size(0);
                b.append_link(&mut h, path, std::str::from_utf8(data).unwrap())
                    .unwrap();
            } else {
                h.set_size(data.len() as u64);
                b.append_data(&mut h, path, data).unwrap();
            }
        };
        use tar::EntryType::*;
        add("top/", Directory, 0o755, b"");
        add("top/a", Regular, 0o644, b"hello\n");
        add("top/empty/", Directory, 0o755, b"");
        add("top/l", Symlink, 0o777, b"a");
        add("top/sub/s", Regular, 0o644, b"s\n");
        add("top/x", Regular, 0o755, b"#!/bin/sh\n");
        b.into_inner().unwrap().finish().unwrap()
    }

    /// A directory holding `tarball()`, a file and a flake with `inputs`,
    /// `outputs` and lock file nodes `nodes`, in which `@DIR@` is the
    /// directory.
    fn flake(inputs: &str, outputs: &str, nodes: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().display().to_string();
        std::fs::write(dir.path().join("t.tar.gz"), tarball()).unwrap();
        std::fs::write(dir.path().join("f.txt"), "data\n").unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let text = format!("{{ inputs = {{ {inputs} }}; outputs = {outputs}; }}");
        std::fs::write(root.join("flake.nix"), text.replace("@DIR@", &d)).unwrap();
        let lock = format!(r#"{{ "nodes": {{ {nodes} }}, "root": "root", "version": 7 }}"#);
        std::fs::write(root.join("flake.lock"), lock.replace("@DIR@", &d)).unwrap();
        dir
    }

    fn eval(dir: &tempfile::TempDir, fetcher: Option<Box<dyn crate::Fetcher>>) -> String {
        eval_installable(dir, &format!("{}/root#x", dir.path().display()), fetcher)
    }

    fn eval_installable(
        dir: &tempfile::TempDir,
        installable: &str,
        fetcher: Option<Box<dyn crate::Fetcher>>,
    ) -> String {
        let env = FlakeEnv {
            cwd: dir.path().to_owned(),
            store: None,
            fetcher,
        };
        crate::eval_flake_json(installable, "x: x", &env, crate::Settings::default())
            .unwrap_or_else(|e| format!("error: {e}"))
    }

    #[test]
    fn evaluates_tarball_and_file_inputs() {
        let dir = flake(
            r#"t = { url = "tarball+file://@DIR@/t.tar.gz"; flake = false; };
               f = { url = "file+file://@DIR@/f.txt"; flake = false; };"#,
            r#"{ self, t, f }: {
                 x = {
                   t = builtins.removeAttrs t [ "outPath" "sourceInfo" ] // { path = t.outPath; };
                   f = builtins.removeAttrs f [ "outPath" "sourceInfo" ] // { path = f.outPath; };
                   files = builtins.readDir t;
                   a = builtins.readFile "${t}/sub/s";
                   c = builtins.readFile f;
                 };
               }"#,
            &format!(
                r#""root": {{ "inputs": {{ "f": "f", "t": "t" }} }},
                   "f": {{ "flake": false,
                     "locked": {{ "narHash": "{FILE_HASH}", "type": "file", "url": "file://@DIR@/f.txt" }},
                     "original": {{ "type": "file", "url": "file://@DIR@/f.txt" }} }},
                   "t": {{ "flake": false,
                     "locked": {{ "lastModified": 1000, "narHash": "{TARBALL_HASH}", "type": "tarball", "url": "file://@DIR@/t.tar.gz" }},
                     "original": {{ "type": "tarball", "url": "file://@DIR@/t.tar.gz" }} }}"#
            ),
        );
        assert_eq!(
            eval(&dir, None),
            concat!(
                r#"{"a":"s\n","c":"data\n","#,
                r#""f":{"narHash":"sha256-RCJ37DJg+G+SI5m0zPshU3sGVDGrhCq/TeBVi/rLRVY=","path":"/nix/store/1bkk5g2aa0vdvxnnbl86xw22j78sfp8p-source"},"#,
                r#""files":{"a":"regular","empty":"directory","l":"symlink","sub":"directory","x":"regular"},"#,
                r#""t":{"lastModified":1000,"lastModifiedDate":"19700101001640","narHash":"sha256-nnyd4JnnpMA0Lp5sw3G+jnPNNLfc1rJRGYNiHL6ab9c=","path":"/nix/store/g9j8h21lmddykkr8h9qjsiym1vrj9gr2-source"}}"#
            )
        );
    }

    /// Serves `tarball()` at every URL, recording the URLs it is asked for.
    struct Fake(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

    impl crate::Fetcher for Fake {
        fn request(&self, request: &crate::Request<'_>) -> anyhow::Result<crate::Response> {
            self.0.lock().unwrap().push(request.url.to_owned());
            Ok(crate::Response {
                headers: Vec::new(),
                body: tarball(),
            })
        }
    }

    fn github_flake(nar_hash: &str) -> tempfile::TempDir {
        flake(
            r#"g = { url = "github:o/r"; flake = false; };"#,
            r#"{ self, g }: { x = builtins.removeAttrs g.sourceInfo [ "outPath" ]; }"#,
            &format!(
                r#""root": {{ "inputs": {{ "g": "g" }} }},
                   "g": {{ "flake": false,
                     "locked": {{ "lastModified": 1000, "narHash": "{nar_hash}", "owner": "o", "repo": "r", "rev": "{REV}", "type": "github" }},
                     "original": {{ "owner": "o", "repo": "r", "type": "github" }} }}"#
            ),
        )
    }

    #[test]
    fn downloads_forge_inputs_through_the_fetcher() {
        let urls = std::sync::Arc::default();
        let dir = github_flake(TARBALL_HASH);
        assert_eq!(
            eval(&dir, Some(Box::new(Fake(std::sync::Arc::clone(&urls))))),
            format!(
                r#"{{"lastModified":1000,"lastModifiedDate":"19700101001640","narHash":"{TARBALL_HASH}","rev":"{REV}","shortRev":"5edf11c"}}"#
            )
        );
        assert_eq!(
            *urls.lock().unwrap(),
            [format!("https://github.com/o/r/archive/{REV}.tar.gz")]
        );
    }

    #[test]
    fn rejects_a_download_with_the_wrong_nar_hash() {
        let dir = github_flake(FILE_HASH);
        let out = eval(&dir, Some(Box::new(Fake(Default::default()))));
        assert!(out.contains("NAR hash mismatch"), "{out}");
    }

    #[test]
    fn cannot_download_without_a_fetcher() {
        let out = eval(&github_flake(TARBALL_HASH), None);
        assert!(out.contains("has no fetcher"), "{out}");
    }

    #[test]
    fn finds_forge_archives() {
        let attrs = |owner: &str, host: Option<&str>| {
            let mut a = Attrs::new();
            a.insert("owner".into(), flakeref::Attr::Str(owner.into()));
            a.insert("repo".into(), flakeref::Attr::Str("r".into()));
            if let Some(h) = host {
                a.insert("host".into(), flakeref::Attr::Str(h.into()));
            }
            a
        };
        assert_eq!(
            forge_archive_url("github", &attrs("o", Some("ghe.example")), "c"),
            "https://ghe.example/o/r/archive/c.tar.gz"
        );
        assert_eq!(
            forge_archive_url("gitlab", &attrs("g/s", None), "c"),
            "https://gitlab.com/api/v4/projects/g%2Fs%2Fr/repository/archive.tar.gz?sha=c"
        );
        assert_eq!(
            forge_archive_url("sourcehut", &attrs("~o", None), "c"),
            "https://git.sr.ht/~o/r/archive/c.tar.gz"
        );
    }

    // A repository made by `git_repo`, and what Nix 2.36 evaluates its
    // flake's `x` to as `git+file://…?rev=GIT_REV`.
    const GIT_REV: &str = "3cea74b699a9351f08629c124f138314aa74cd30";
    const GIT_HASH: &str = "sha256-dv8iUwbOOD9jfPai4FPczVIox7NR2swJyLCdznDt7r8=";
    const GIT_INFO: &str = r#"{"lastModified":2000,"lastModifiedDate":"19700101003320","narHash":"sha256-dv8iUwbOOD9jfPai4FPczVIox7NR2swJyLCdznDt7r8=","rev":"3cea74b699a9351f08629c124f138314aa74cd30","revCount":2,"shortRev":"3cea74b","submodules":false}"#;

    fn git_bin() -> &'static str {
        option_env!("OGYGIA_GIT_BIN").unwrap_or("git")
    }

    /// Run `git` in `dir`, committing at `date`.
    fn git(dir: &Path, date: u64, args: &[&str]) {
        let status = std::process::Command::new(git_bin())
            .args([
                "-c",
                "init.defaultBranch=main",
                "-c",
                "commit.gpgSign=false",
            ])
            .args(args)
            .current_dir(dir)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "a")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_AUTHOR_DATE", "@1000 +0000")
            .env("GIT_COMMITTER_NAME", "a")
            .env("GIT_COMMITTER_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_DATE", format!("@{date} +0000"))
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    /// A repository at `dir` whose `HEAD` is `GIT_REV`, the second of two
    /// commits, holding a flake.
    fn git_repo(dir: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir(dir).unwrap();
        git(dir, 1000, &["init", "-q", "."]);
        std::fs::write(dir.join("a"), "hello\n").unwrap();
        git(dir, 1000, &["add", "a"]);
        git(dir, 1000, &["commit", "-qm", "one"]);
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/s"), "s\n").unwrap();
        std::fs::write(dir.join("x"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(dir.join("x"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("a", dir.join("l")).unwrap();
        std::fs::write(
            dir.join("flake.nix"),
            "{ outputs = { self }: { x = builtins.removeAttrs self.sourceInfo [ \"outPath\" ]; }; }\n",
        )
        .unwrap();
        git(dir, 2000, &["add", "."]);
        git(dir, 2000, &["commit", "-qm", "two"]);
    }

    /// Serves the repository at its path over Git's smart HTTP protocol, as
    /// `git http-backend` does, recording each request's body.
    struct GitServer(PathBuf, std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>);

    impl crate::Fetcher for GitServer {
        fn request(&self, request: &crate::Request<'_>) -> anyhow::Result<crate::Response> {
            use std::io::Write as _;
            self.1.lock().unwrap().push(request.body.to_vec());
            let protocol = request
                .headers
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case("Git-Protocol"))
                .map_or("", |(_, v)| v.as_str());
            let advertise = request.method == Method::Get;
            let mut cmd = std::process::Command::new(git_bin());
            cmd.args(["upload-pack", "--stateless-rpc"]);
            if advertise {
                cmd.arg("--advertise-refs");
            }
            let mut child = cmd
                .arg(&self.0)
                .env("GIT_PROTOCOL", protocol)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()?;
            child.stdin.take().unwrap().write_all(request.body)?;
            let output = child.wait_with_output()?;
            anyhow::ensure!(output.status.success(), "git upload-pack failed");
            let mut body = Vec::new();
            if advertise && !protocol.contains("version=2") {
                body.extend_from_slice(b"001e# service=git-upload-pack\n0000");
            }
            body.extend(output.stdout);
            let kind = if advertise { "advertisement" } else { "result" };
            Ok(crate::Response {
                headers: vec![(
                    "Content-Type".to_owned(),
                    format!("application/x-git-upload-pack-{kind}"),
                )],
                body,
            })
        }
    }

    fn remote_git_flake(lock_extra: &str) -> tempfile::TempDir {
        let dir = flake(
            r#"r = { url = "git+https://git.example/repo"; flake = false; };"#,
            r#"{ self, r }: { x = builtins.removeAttrs r.sourceInfo [ "outPath" ]; }"#,
            &format!(
                r#""root": {{ "inputs": {{ "r": "r" }} }},
                   "r": {{ "flake": false,
                     "locked": {{ {lock_extra} "narHash": "{GIT_HASH}", "rev": "{GIT_REV}", "type": "git", "url": "https://git.example/repo" }},
                     "original": {{ "type": "git", "url": "https://git.example/repo" }} }}"#
            ),
        );
        git_repo(&dir.path().join("repo"));
        dir
    }

    #[test]
    fn fetches_only_the_locked_commit_of_a_remote_git_input() {
        let dir = remote_git_flake(r#""lastModified": 2000, "revCount": 2,"#);
        let bodies = std::sync::Arc::default();
        let server = GitServer(dir.path().join("repo"), std::sync::Arc::clone(&bodies));
        assert_eq!(eval(&dir, Some(Box::new(server))), GIT_INFO);
        let bodies = bodies.lock().unwrap();
        assert!(
            bodies
                .iter()
                .any(|b| b.windows(8).any(|w| w == b"deepen 1")),
            "the fetch was not shallow"
        );
    }

    #[test]
    fn takes_what_the_lock_records_of_a_remote_git_input() {
        let dir = remote_git_flake("");
        let server = GitServer(dir.path().join("repo"), Default::default());
        assert_eq!(
            eval(&dir, Some(Box::new(server))),
            format!(
                r#"{{"narHash":"{GIT_HASH}","rev":"{GIT_REV}","shortRev":"3cea74b","submodules":false}}"#
            )
        );
    }

    #[test]
    fn computes_what_an_unlocked_remote_git_flake_does_not_record() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(&dir.path().join("repo"));
        let server = GitServer(dir.path().join("repo"), Default::default());
        let installable = format!("git+https://git.example/repo?rev={GIT_REV}#x");
        assert_eq!(
            eval_installable(&dir, &installable, Some(Box::new(server))),
            GIT_INFO
        );
    }
}
