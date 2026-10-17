//! A store held in memory for the life of the evaluator.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::rc::Rc;

use anyhow::Result;
use sha2::Digest;
use sha2::Sha256;

use super::Store;
use super::StorePath;
use crate::accessor::Accessor;
use crate::accessor::FileType;
use crate::nar;

/// Holds every object added to it, and nothing else. Copied sources are not
/// copied: they read through to the tree they were added from.
#[derive(Default)]
pub struct MemoryStore {
    objects: RefCell<HashMap<StorePath, Rc<dyn Accessor>>>,
}

impl MemoryStore {
    fn insert(&self, path: StorePath, object: impl FnOnce() -> Rc<dyn Accessor>) -> StorePath {
        self.objects
            .borrow_mut()
            .entry(path.clone())
            .or_insert_with(object);
        path
    }
}

impl Store for MemoryStore {
    fn accessor(&self, path: &StorePath) -> Result<Option<Rc<dyn Accessor>>> {
        Ok(self.objects.borrow().get(path).cloned())
    }

    fn add_text(&self, name: &str, contents: &[u8], refs: &[&str]) -> Result<StorePath> {
        super::check_name(name)?;
        let path = StorePath(super::text_path(contents, name, refs));
        Ok(self.insert(path, || Rc::new(File(contents.to_vec()))))
    }

    fn add_source(&self, name: &str, tree: Rc<dyn Accessor>) -> Result<StorePath> {
        super::check_name(name)?;
        let mut h = Sha256::new();
        nar::dump(&mut h, &*tree, "")?;
        let path = StorePath(super::source_path(&h.finalize(), name));
        Ok(self.insert(path, || tree))
    }
}

/// A store object that is a single, non-executable file.
struct File(Vec<u8>);

impl File {
    fn root(path: &str) -> std::io::Result<()> {
        if path.is_empty() {
            Ok(())
        } else {
            Err(ErrorKind::NotFound.into())
        }
    }
}

impl Accessor for File {
    fn file_type(&self, path: &str) -> Option<FileType> {
        path.is_empty().then_some(FileType::Regular)
    }

    fn is_dir(&self, _: &str) -> bool {
        false
    }

    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
        File::root(path)?;
        Ok(self.0.clone())
    }

    fn read_dir(&self, path: &str) -> std::io::Result<Vec<(String, FileType)>> {
        File::root(path)?;
        Err(ErrorKind::NotADirectory.into())
    }

    fn read_link(&self, path: &str) -> std::io::Result<Vec<u8>> {
        File::root(path)?;
        Err(ErrorKind::InvalidInput.into())
    }

    fn is_executable(&self, path: &str) -> std::io::Result<bool> {
        File::root(path)?;
        Ok(false)
    }
}
