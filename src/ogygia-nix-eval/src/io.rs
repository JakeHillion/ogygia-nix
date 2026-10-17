//! Filesystem access for the evaluator.
//!
//! Paths the evaluator sees are logical: a flake input is `/nix/store/…-source`
//! whether or not it has been realised there. [`Io`] maps logical paths onto
//! physical directories ("mounts"), optionally restricted to a set of files
//! (the tracked files of a Git checkout), and computes store paths of copied
//! sources without writing to a store.

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
use crate::store;

enum Target {
    Physical(PathBuf),
    /// Another logical path, as for a store path computed from a source.
    Logical(String),
}

struct Mount {
    logical: String,
    target: Target,
    /// If set, only these paths (relative to the mount, `/`-separated, with
    /// every ancestor directory included) exist.
    filter: Option<Rc<HashSet<String>>>,
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
    fn add_mount(&self, logical: &str, target: Target, filter: Option<HashSet<String>>) {
        let mut mounts = self.mounts.borrow_mut();
        mounts.retain(|m| m.logical != logical);
        mounts.push(Mount {
            logical: logical.to_owned(),
            target,
            filter: filter.map(Rc::new),
        });
        // Longest prefix first, so nested mounts win.
        mounts.sort_by_key(|m| std::cmp::Reverse(m.logical.len()));
    }

    fn is_mounted(&self, logical: &str) -> bool {
        self.mounts.borrow().iter().any(|m| m.logical == logical)
    }

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
        let next = {
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
            match &m.target {
                Target::Physical(p) if rest.is_empty() => return Ok(p.clone()),
                Target::Physical(p) => return Ok(p.join(rest)),
                Target::Logical(l) if rest.is_empty() => l.clone(),
                Target::Logical(l) => format!("{}/{rest}", l.trim_end_matches('/')),
            }
        };
        self.physical(&next)
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
        let base = path.trim_end_matches('/');
        let mut out = Vec::new();
        for entry in std::fs::read_dir(self.physical(path)?)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            // Entries hidden by a mount filter do not exist.
            if self.physical(&format!("{base}/{name}")).is_err() {
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

#[derive(Default)]
pub struct Io {
    pub(crate) pure: bool,
    fs: Fs,
    store_paths: RefCell<HashMap<String, String>>,
}

impl Io {
    /// An [`Io`] that, like Nix in pure evaluation mode, forbids access to
    /// any path outside a mount.
    pub fn pure() -> Io {
        Io {
            pure: true,
            ..Io::default()
        }
    }

    /// Fail if `logical` may not be accessed at all.
    pub fn check_access(&self, logical: &str) -> Result<()> {
        if self.pure && !logical.starts_with(crate::path::COREPKGS) && !self.fs.in_mount(logical) {
            bail!(
                "access to absolute path '{logical}' is forbidden in pure evaluation mode (use '--impure' to override)"
            );
        }
        Ok(())
    }

    /// Make the logical directory `logical` show the contents of `physical`,
    /// restricted to `filter` if given.
    pub fn mount(&self, logical: &str, physical: &Path, filter: Option<HashSet<String>>) {
        self.fs
            .add_mount(logical, Target::Physical(physical.to_owned()), filter);
    }

    pub fn read(&self, logical: &str) -> Result<Vec<u8>> {
        self.check_access(logical)?;
        self.fs
            .read_file(logical)
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
        self.fs.file_type(logical)
    }

    /// Whether `logical` exists, following symlinks.
    pub fn exists(&self, logical: &str) -> bool {
        self.file_type(logical).is_some()
    }

    pub fn is_dir(&self, logical: &str) -> bool {
        self.check_access(logical).is_ok() && self.fs.is_dir(logical)
    }

    /// The entries of a directory, sorted by name.
    pub fn read_dir(&self, logical: &str) -> Result<Vec<(String, FileType)>> {
        self.check_access(logical)?;
        self.fs
            .read_dir(logical)
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

    /// The store path `logical` would be copied to, as `"${path}"` does. The
    /// store path then reads as a copy of `logical`.
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

    /// The store path of `logical` copied under `name`, keeping only entries
    /// `keep(path, type)` accepts (`builtins.path`). The store path then
    /// reads as that filtered copy.
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
        let mut kept = HashSet::new();
        let mut h = Sha256::new();
        nar::dump(&mut h, &self.fs, logical, &mut |child, ty| {
            if let Some(keep) = keep
                && !keep(child, ty)?
            {
                return Ok(false);
            }
            if let Some(rel) = relative(child, logical) {
                kept.insert(rel.to_owned());
            }
            Ok(true)
        })?;
        let hash: [u8; 32] = h.finalize().into();
        let sp = store::source_path(&hash, name);
        if !self.fs.is_mounted(&sp) {
            self.fs
                .add_mount(&sp, Target::Logical(logical.to_owned()), Some(kept));
        }
        Ok(sp)
    }

    /// SHA-256 of the NAR serialisation of `logical`.
    pub fn nar_hash(&self, logical: &str) -> Result<[u8; 32]> {
        self.check_access(logical)?;
        if !self.exists(logical) {
            bail!("path '{logical}' does not exist");
        }
        let mut h = Sha256::new();
        nar::dump(&mut h, &self.fs, logical, &mut |_, _| Ok(true))?;
        Ok(h.finalize().into())
    }
}
