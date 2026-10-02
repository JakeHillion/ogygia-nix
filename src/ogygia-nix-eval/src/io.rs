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
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::bail;
use sha2::Digest;
use sha2::Sha256;

use crate::store;

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

    fn of(ft: std::fs::FileType) -> FileType {
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

/// A `builtins.path` filter: whether to keep a path of the given type.
pub type KeepFn<'f> = &'f dyn Fn(&str, FileType) -> Result<bool>;

#[derive(Default)]
pub struct Io {
    mounts: RefCell<Vec<Mount>>,
    store_paths: RefCell<HashMap<String, String>>,
}

/// `logical` relative to `root`, if it is `root` or inside it.
fn relative<'p>(logical: &'p str, root: &str) -> Option<&'p str> {
    if logical == root {
        return Some("");
    }
    logical
        .strip_prefix(root.trim_end_matches('/'))?
        .strip_prefix('/')
}

impl Io {
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

    /// Make the logical directory `logical` show the contents of `physical`,
    /// restricted to `filter` if given.
    pub fn mount(&self, logical: &str, physical: &Path, filter: Option<HashSet<String>>) {
        self.add_mount(logical, Target::Physical(physical.to_owned()), filter);
    }

    pub fn is_mounted(&self, logical: &str) -> bool {
        self.mounts.borrow().iter().any(|m| m.logical == logical)
    }

    /// The physical path behind `logical`, or an error if a mount filter
    /// hides it.
    pub fn physical(&self, logical: &str) -> Result<PathBuf> {
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
                bail!("path '{logical}' does not exist");
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

    pub fn read(&self, logical: &str) -> Result<Vec<u8>> {
        let p = self.physical(logical)?;
        std::fs::read(&p).with_context(|| format!("reading file '{logical}'"))
    }

    pub fn read_to_string(&self, logical: &str) -> Result<String> {
        let bytes = self.read(logical)?;
        String::from_utf8(bytes).with_context(|| format!("file '{logical}' is not UTF-8"))
    }

    /// The type of `logical` without following a final symlink, or `None` if
    /// it does not exist.
    pub fn file_type(&self, logical: &str) -> Option<FileType> {
        let p = self.physical(logical).ok()?;
        std::fs::symlink_metadata(&p)
            .ok()
            .map(|m| FileType::of(m.file_type()))
    }

    /// Whether `logical` exists, following symlinks.
    pub fn exists(&self, logical: &str) -> bool {
        match self.physical(logical) {
            Ok(p) => p.exists() || std::fs::symlink_metadata(&p).is_ok(),
            Err(_) => false,
        }
    }

    pub fn is_dir(&self, logical: &str) -> bool {
        self.physical(logical).map(|p| p.is_dir()).unwrap_or(false)
    }

    /// The entries of a directory, sorted by name.
    pub fn read_dir(&self, logical: &str) -> Result<Vec<(String, FileType)>> {
        let p = self.physical(logical)?;
        let base = logical.trim_end_matches('/');
        let mut out = Vec::new();
        for entry in
            std::fs::read_dir(&p).with_context(|| format!("reading directory '{logical}'"))?
        {
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

    /// The file `import` reads for `logical`: the path itself, or its
    /// `default.nix` if it is a directory.
    pub fn resolve_import(&self, logical: &str) -> Result<String> {
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
        let name = crate::path::base_name_of(logical.as_bytes());
        let name = String::from_utf8_lossy(name).into_owned();
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
        if !self.exists(logical) {
            bail!("path '{logical}' does not exist");
        }
        let mut kept = HashSet::new();
        let accept_all = |_: &str, _: FileType| Ok(true);
        let keep = keep.unwrap_or(&accept_all);
        let mut h = Sha256::new();
        store::nar_str(&mut h, b"nix-archive-1")?;
        self.dump(&mut h, logical, logical, keep, &mut kept)?;
        let hash: [u8; 32] = h.finalize().into();
        let sp = store::source_path(&hash, name);
        if !self.is_mounted(&sp) {
            self.add_mount(&sp, Target::Logical(logical.to_owned()), Some(kept));
        }
        Ok(sp)
    }

    /// SHA-256 of the NAR serialisation of `logical`.
    pub fn nar_hash(&self, logical: &str) -> Result<[u8; 32]> {
        if !self.exists(logical) {
            bail!("path '{logical}' does not exist");
        }
        let mut h = Sha256::new();
        store::nar_str(&mut h, b"nix-archive-1")?;
        self.dump(
            &mut h,
            logical,
            logical,
            &|_, _| Ok(true),
            &mut HashSet::new(),
        )?;
        Ok(h.finalize().into())
    }

    /// Write the NAR serialisation of `logical`, recording in `kept` every
    /// entry included, relative to `root`.
    fn dump(
        &self,
        w: &mut impl Write,
        root: &str,
        logical: &str,
        keep: &dyn Fn(&str, FileType) -> Result<bool>,
        kept: &mut HashSet<String>,
    ) -> Result<()> {
        let p = self.physical(logical)?;
        let meta = std::fs::symlink_metadata(&p).with_context(|| format!("reading '{logical}'"))?;
        store::nar_str(w, b"(")?;
        store::nar_str(w, b"type")?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            store::nar_str(w, b"symlink")?;
            store::nar_str(w, b"target")?;
            let target = std::fs::read_link(&p)?;
            store::nar_str(w, target.as_os_str().as_bytes())?;
        } else if ft.is_dir() {
            store::nar_str(w, b"directory")?;
            let mut entries = self.read_dir(logical)?;
            entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
            for (name, ty) in entries {
                let child = format!("{}/{name}", logical.trim_end_matches('/'));
                if !keep(&child, ty)? {
                    continue;
                }
                if let Some(rel) = relative(&child, root) {
                    kept.insert(rel.to_owned());
                }
                store::nar_str(w, b"entry")?;
                store::nar_str(w, b"(")?;
                store::nar_str(w, b"name")?;
                store::nar_str(w, name.as_bytes())?;
                store::nar_str(w, b"node")?;
                self.dump(w, root, &child, keep, kept)?;
                store::nar_str(w, b")")?;
            }
        } else if ft.is_file() {
            store::nar_str(w, b"regular")?;
            if meta.permissions().mode() & 0o100 != 0 {
                store::nar_str(w, b"executable")?;
                store::nar_str(w, b"")?;
            }
            store::nar_str(w, b"contents")?;
            let contents = std::fs::read(&p)?;
            store::nar_str(w, &contents)?;
        } else {
            bail!("file '{logical}' has an unsupported type");
        }
        store::nar_str(w, b")")?;
        Ok(())
    }
}
