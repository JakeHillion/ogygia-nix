//! Filesystem and store access for the evaluator.
//!
//! Paths the evaluator sees are logical: a flake input is `/nix/store/…-source`
//! whether or not it has been realised there. [`Io`] reads store paths from
//! its store and other logical paths from physical directories ("mounts"),
//! optionally restricted to a set of files (the tracked files of a Git
//! checkout).

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io::ErrorKind;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::bail;
use sha2::Digest;
use sha2::Sha256;

use crate::accessor::Accessor;
use crate::accessor::FileType;
use crate::nar;
use crate::path::join;
use crate::store;
use crate::store::MemoryStore;
use crate::store::Store;
use crate::store::StorePath;

struct Mount {
    logical: String,
    physical: PathBuf,
    /// If set, only these paths (relative to the mount, `/`-separated, with
    /// every ancestor directory included) exist.
    filter: Option<HashSet<String>>,
}

// The directory holding the files Nix ships with itself, which `<nix/…>`
// paths resolve within, as `nix/…`. Set at build time; without it they do
// not exist.
const DEFAULT_INCLUDE_PATH: Option<&str> = option_env!("OGYGIA_NIX_EVAL_DEFAULT_INCLUDE_PATH");

/// A `builtins.path` filter: whether to keep a path of the given type.
pub type KeepFn<'f> = &'f dyn Fn(&str, FileType) -> Result<bool>;

/// `logical` relative to `root`, if it is `root` or inside it.
fn relative<'p>(logical: &'p str, root: &str) -> Option<&'p str> {
    if logical == root {
        return Some("");
    }
    logical
        .strip_prefix(root.trim_end_matches('/'))?
        .strip_prefix('/')
}

/// The filesystem as seen through the mounts, addressed by logical path.
#[derive(Default)]
struct Fs {
    mounts: RefCell<Vec<Mount>>,
}

impl Fs {
    /// Whether `logical` is a mount or inside one.
    fn in_mount(&self, logical: &str) -> bool {
        self.mounts
            .borrow()
            .iter()
            .any(|m| relative(logical, &m.logical).is_some())
    }

    /// The physical path behind `logical`, or `NotFound` if a mount filter
    /// hides it.
    fn physical(&self, logical: &str) -> std::io::Result<PathBuf> {
        if let Some(rest) = logical.strip_prefix(crate::path::COREPKGS) {
            let Some(include) = DEFAULT_INCLUDE_PATH else {
                return Err(ErrorKind::NotFound.into());
            };
            return Ok(Path::new(include)
                .join("nix")
                .join(rest.trim_start_matches('/')));
        }
        let mounts = self.mounts.borrow();
        let found = mounts
            .iter()
            .find_map(|m| relative(logical, &m.logical).map(|rest| (m, rest)));
        let Some((m, rest)) = found else {
            return Ok(PathBuf::from(logical));
        };
        if let Some(f) = &m.filter
            && !rest.is_empty()
            && !f.contains(rest)
        {
            return Err(ErrorKind::NotFound.into());
        }
        if rest.is_empty() {
            Ok(m.physical.clone())
        } else {
            Ok(m.physical.join(rest))
        }
    }
}

impl Accessor for Fs {
    fn file_type(&self, path: &str) -> Option<FileType> {
        let p = self.physical(path).ok()?;
        std::fs::symlink_metadata(&p)
            .ok()
            .map(|m| FileType::of(m.file_type()))
    }

    fn is_dir(&self, path: &str) -> bool {
        self.physical(path).map(|p| p.is_dir()).unwrap_or(false)
    }

    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
        std::fs::read(self.physical(path)?)
    }

    fn read_dir(&self, path: &str) -> std::io::Result<Vec<(String, FileType)>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(self.physical(path)?)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            // Entries hidden by a mount filter do not exist.
            if self.physical(&join(path, &name)).is_err() {
                continue;
            }
            out.push((name, FileType::of(entry.file_type()?)));
        }
        out.sort();
        Ok(out)
    }

    fn read_link(&self, path: &str) -> std::io::Result<Vec<u8>> {
        Ok(std::fs::read_link(self.physical(path)?)?
            .into_os_string()
            .into_vec())
    }

    fn is_executable(&self, path: &str) -> std::io::Result<bool> {
        let meta = std::fs::symlink_metadata(self.physical(path)?)?;
        Ok(meta.permissions().mode() & 0o100 != 0)
    }
}

/// The tree at `root` in `base`, holding only the entries in `kept`
/// (relative to `root`) if that is set.
struct Subtree {
    base: Rc<dyn Accessor>,
    root: String,
    kept: Option<HashSet<String>>,
}

impl Subtree {
    /// `path` in `base`, or `NotFound` if it is not kept.
    fn path(&self, path: &str) -> std::io::Result<String> {
        match &self.kept {
            Some(kept) if !path.is_empty() && !kept.contains(path) => {
                Err(ErrorKind::NotFound.into())
            }
            _ => Ok(join(&self.root, path)),
        }
    }
}

impl Accessor for Subtree {
    fn file_type(&self, path: &str) -> Option<FileType> {
        self.base.file_type(&self.path(path).ok()?)
    }

    fn is_dir(&self, path: &str) -> bool {
        self.path(path).is_ok_and(|p| self.base.is_dir(&p))
    }

    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
        self.base.read_file(&self.path(path)?)
    }

    fn read_dir(&self, path: &str) -> std::io::Result<Vec<(String, FileType)>> {
        let mut entries = self.base.read_dir(&self.path(path)?)?;
        entries.retain(|(name, _)| self.path(&join(path, name)).is_ok());
        Ok(entries)
    }

    fn read_link(&self, path: &str) -> std::io::Result<Vec<u8>> {
        self.base.read_link(&self.path(path)?)
    }

    fn is_executable(&self, path: &str) -> std::io::Result<bool> {
        self.base.is_executable(&self.path(path)?)
    }
}

pub struct Io {
    pub(crate) pure: bool,
    fs: Rc<Fs>,
    store: Box<dyn Store>,
    /// Store paths pure evaluation may read.
    allowed: RefCell<HashSet<StorePath>>,
    store_paths: RefCell<HashMap<String, String>>,
}

impl Default for Io {
    fn default() -> Io {
        Io {
            pure: false,
            fs: Rc::default(),
            store: Box::new(MemoryStore::default()),
            allowed: RefCell::default(),
            store_paths: RefCell::default(),
        }
    }
}

impl Io {
    /// An [`Io`] that, like Nix in pure evaluation mode, forbids access to
    /// any path outside a mount or a store path it did not add.
    pub fn pure() -> Io {
        Io {
            pure: true,
            ..Io::default()
        }
    }

    /// Fail if `logical` may not be accessed at all.
    pub fn check_access(&self, logical: &str) -> Result<()> {
        if self.pure
            && !logical.starts_with(crate::path::COREPKGS)
            && !self.fs.in_mount(logical)
            && !StorePath::split(logical).is_some_and(|(sp, _)| self.allowed.borrow().contains(&sp))
        {
            bail!(
                "access to absolute path '{logical}' is forbidden in pure evaluation mode (use '--impure' to override)"
            );
        }
        Ok(())
    }

    /// Make the logical directory `logical` show the contents of `physical`,
    /// restricted to `filter` if given.
    pub fn mount(&self, logical: &str, physical: &Path, filter: Option<HashSet<String>>) {
        let mut mounts = self.fs.mounts.borrow_mut();
        mounts.retain(|m| m.logical != logical);
        mounts.push(Mount {
            logical: logical.to_owned(),
            physical: physical.to_owned(),
            filter,
        });
        // Longest prefix first, so nested mounts win.
        mounts.sort_by_key(|m| std::cmp::Reverse(m.logical.len()));
    }

    /// The tree holding `logical`: the store object it is in, if the store
    /// has it, else the filesystem.
    fn locate<'p>(&self, logical: &'p str) -> Result<(Rc<dyn Accessor>, Cow<'p, str>)> {
        if let Some((sp, rest)) = StorePath::split(logical)
            && let Some(acc) = self.store.accessor(&sp)?
        {
            return Ok((acc, Cow::Borrowed(rest)));
        }
        Ok((self.fs.clone(), Cow::Borrowed(logical)))
    }

    pub fn read(&self, logical: &str) -> Result<Vec<u8>> {
        self.check_access(logical)?;
        let (acc, path) = self.locate(logical)?;
        acc.read_file(&path)
            .with_context(|| format!("reading file '{logical}'"))
    }

    pub fn read_to_string(&self, logical: &str) -> Result<String> {
        let bytes = self.read(logical)?;
        String::from_utf8(bytes).with_context(|| format!("file '{logical}' is not UTF-8"))
    }

    /// The type of `logical` without following a final symlink, or `None` if
    /// it does not exist.
    pub fn file_type(&self, logical: &str) -> Option<FileType> {
        self.check_access(logical).ok()?;
        let (acc, path) = self.locate(logical).ok()?;
        acc.file_type(&path)
    }

    /// Whether `logical` exists, following symlinks.
    pub fn exists(&self, logical: &str) -> bool {
        self.file_type(logical).is_some()
    }

    pub fn is_dir(&self, logical: &str) -> bool {
        self.check_access(logical).is_ok()
            && self
                .locate(logical)
                .is_ok_and(|(acc, path)| acc.is_dir(&path))
    }

    /// The entries of a directory, sorted by name.
    pub fn read_dir(&self, logical: &str) -> Result<Vec<(String, FileType)>> {
        self.check_access(logical)?;
        let (acc, path) = self.locate(logical)?;
        acc.read_dir(&path)
            .with_context(|| format!("reading directory '{logical}'"))
    }

    /// The file `import` reads for `logical`: the path itself, or its
    /// `default.nix` if it is a directory.
    pub fn resolve_import(&self, logical: &str) -> Result<String> {
        self.check_access(logical)?;
        if !self.exists(logical) {
            bail!("path '{logical}' does not exist");
        }
        if self.is_dir(logical) {
            return Ok(format!("{}/default.nix", logical.trim_end_matches('/')));
        }
        Ok(logical.to_owned())
    }

    /// Copy `logical` to the store, as `"${path}"` does, and return its store
    /// path.
    pub fn add_path_to_store(&self, logical: &str) -> Result<String> {
        if let Some(p) = self.store_paths.borrow().get(logical) {
            return Ok(p.clone());
        }
        let name = crate::path::store_name(logical);
        let sp = self.add_filtered_to_store(logical, &name, None)?;
        self.store_paths
            .borrow_mut()
            .insert(logical.to_owned(), sp.clone());
        Ok(sp)
    }

    /// Copy `logical` to the store under `name`, keeping only entries
    /// `keep(path, type)` accepts (`builtins.path`), and return its store
    /// path.
    pub fn add_filtered_to_store(
        &self,
        logical: &str,
        name: &str,
        keep: Option<KeepFn<'_>>,
    ) -> Result<String> {
        store::check_name(name)?;
        self.check_access(logical)?;
        if !self.exists(logical) {
            bail!("path '{logical}' does not exist");
        }
        let (base, root) = self.locate(logical)?;
        let kept = match keep {
            None => None,
            Some(keep) => {
                let mut kept = HashSet::new();
                select(&*base, &root, logical, "", keep, &mut kept)?;
                Some(kept)
            }
        };
        let tree = Rc::new(Subtree {
            base,
            root: root.into_owned(),
            kept,
        });
        let sp = self.store.add_source(name, tree)?;
        self.allow(sp)
    }

    /// Add a file holding `contents` to the store, as `builtins.toFile` does,
    /// and return its store path.
    pub fn add_text(&self, name: &str, contents: &[u8], refs: &[&str]) -> Result<String> {
        let sp = self.store.add_text(name, contents, refs)?;
        self.allow(sp)
    }

    /// Write a derivation's `.drv` file to the store and return its store
    /// path. Unlike other additions, pure evaluation may not read it.
    pub fn add_derivation(&self, name: &str, contents: &[u8], refs: &[&str]) -> Result<String> {
        let sp = self.store.add_text(name, contents, refs)?;
        Ok(sp.as_str().to_owned())
    }

    fn allow(&self, sp: StorePath) -> Result<String> {
        let path = sp.as_str().to_owned();
        self.allowed.borrow_mut().insert(sp);
        Ok(path)
    }

    /// SHA-256 of the NAR serialisation of `logical`.
    pub fn nar_hash(&self, logical: &str) -> Result<[u8; 32]> {
        self.check_access(logical)?;
        if !self.exists(logical) {
            bail!("path '{logical}' does not exist");
        }
        let (acc, path) = self.locate(logical)?;
        let mut h = Sha256::new();
        nar::dump(&mut h, &*acc, &path)?;
        Ok(h.finalize().into())
    }
}

/// Record in `kept` each entry below `rel` in the directory `root` of `base`
/// that `keep` accepts, in NAR order, not descending into rejected
/// directories. `keep` sees entries by their logical path below `logical`.
fn select(
    base: &dyn Accessor,
    root: &str,
    logical: &str,
    rel: &str,
    keep: KeepFn<'_>,
    kept: &mut HashSet<String>,
) -> Result<()> {
    let path = join(root, rel);
    if base.file_type(&path) != Some(FileType::Directory) {
        return Ok(());
    }
    let entries = base
        .read_dir(&path)
        .with_context(|| format!("reading directory '{}'", join(logical, rel)))?;
    for (name, ty) in entries {
        let child = join(rel, &name);
        if keep(&join(logical, &child), ty)? {
            select(base, root, logical, &child, keep, kept)?;
            kept.insert(child);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    // Expected results are those of `nix-instantiate --eval --strict
    // --read-write-mode`.

    fn eval(expr: &str, pure: bool) -> Result<String, crate::Error> {
        let settings = crate::Settings {
            pure,
            ..crate::Settings::default()
        };
        crate::eval_to_string(expr, "/", settings)
    }

    #[test]
    fn added_files_are_readable() {
        let expr =
            r#"builtins.readFile (builtins.unsafeDiscardStringContext (builtins.toFile "x" "y"))"#;
        assert_eq!(eval(expr, true).unwrap(), r#""y""#);
    }

    #[test]
    fn derivation_files_are_readable_only_when_impure() {
        let expr = r#"builtins.readFile (derivation { name = "d"; system = "x"; builder = "/b"; }).drvPath"#;
        assert!(eval(expr, false).unwrap().starts_with(r#""Derive("#));
        assert!(eval(expr, true).is_err());
    }
}
