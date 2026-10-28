//! Archives, unpacked into memory as Nix unpacks tarball inputs.

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::io::Read;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::bail;

use crate::accessor::Accessor;
use crate::accessor::FileType;
use crate::path::join;

enum Node {
    Regular { contents: Vec<u8>, executable: bool },
    Directory(BTreeSet<String>),
    Symlink(Vec<u8>),
}

/// A tree of files held in memory.
pub struct MemTree {
    /// Every node, by its path relative to the root, which is the empty path.
    nodes: HashMap<String, Node>,
}

impl MemTree {
    /// A tree that is a single non-executable file holding `contents`.
    pub fn file(contents: Vec<u8>) -> MemTree {
        let root = Node::Regular {
            contents,
            executable: false,
        };
        MemTree {
            nodes: HashMap::from([(String::new(), root)]),
        }
    }

    fn empty() -> MemTree {
        MemTree {
            nodes: HashMap::from([(String::new(), Node::Directory(BTreeSet::new()))]),
        }
    }

    fn node(&self, path: &str) -> std::io::Result<&Node> {
        self.nodes
            .get(path)
            .ok_or_else(|| ErrorKind::NotFound.into())
    }

    /// Add `node` at the canonical relative path `path`, replacing whatever
    /// is there unless both are directories, and creating any missing parent
    /// directories.
    fn insert(&mut self, path: &str, node: Node) {
        if path.is_empty() {
            return;
        }
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        if !matches!(self.nodes.get(parent), Some(Node::Directory(_))) {
            self.insert(parent, Node::Directory(BTreeSet::new()));
        }
        if let Some(Node::Directory(children)) = self.nodes.get_mut(parent) {
            children.insert(name.to_owned());
        }
        match (self.nodes.get(path), &node) {
            (Some(Node::Directory(_)), Node::Directory(_)) => {}
            (Some(_), _) => {
                self.remove(path);
                self.nodes.insert(path.to_owned(), node);
            }
            (None, _) => {
                self.nodes.insert(path.to_owned(), node);
            }
        }
    }

    /// Remove the node at `path` and everything below it, leaving its parent
    /// listing it.
    fn remove(&mut self, path: &str) {
        if let Some(Node::Directory(children)) = self.nodes.remove(path) {
            for child in children {
                self.remove(&join(path, &child));
            }
        }
    }

    /// The tree rooted at the sole entry of the root directory, if there is
    /// exactly one and it is not a regular file.
    fn strip_top_level(mut self) -> MemTree {
        let top = match self.nodes.get("") {
            Some(Node::Directory(children)) if children.len() == 1 => {
                children.first().expect("one child").clone()
            }
            _ => return self,
        };
        if matches!(self.nodes.get(&top), Some(Node::Regular { .. })) {
            return self;
        }
        let nodes = self
            .nodes
            .drain()
            .filter_map(|(path, node)| {
                let rest = if path == top {
                    ""
                } else {
                    path.strip_prefix(&top)?.strip_prefix('/')?
                };
                Some((rest.to_owned(), node))
            })
            .collect();
        MemTree { nodes }
    }
}

impl Accessor for MemTree {
    fn file_type(&self, path: &str) -> Option<FileType> {
        Some(match self.nodes.get(path)? {
            Node::Regular { .. } => FileType::Regular,
            Node::Directory(_) => FileType::Directory,
            Node::Symlink(_) => FileType::Symlink,
        })
    }

    fn is_dir(&self, path: &str) -> bool {
        // Symlinks are not followed: an archive's links can point anywhere.
        matches!(self.nodes.get(path), Some(Node::Directory(_)))
    }

    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
        match self.node(path)? {
            Node::Regular { contents, .. } => Ok(contents.clone()),
            Node::Directory(_) => Err(ErrorKind::IsADirectory.into()),
            Node::Symlink(_) => Err(ErrorKind::InvalidInput.into()),
        }
    }

    fn read_dir(&self, path: &str) -> std::io::Result<Vec<(String, FileType)>> {
        let Node::Directory(children) = self.node(path)? else {
            return Err(ErrorKind::NotADirectory.into());
        };
        Ok(children
            .iter()
            .map(|name| {
                let ty = self
                    .file_type(&join(path, name))
                    .expect("a directory's children exist");
                (name.clone(), ty)
            })
            .collect())
    }

    fn read_link(&self, path: &str) -> std::io::Result<Vec<u8>> {
        match self.node(path)? {
            Node::Symlink(target) => Ok(target.clone()),
            _ => Err(ErrorKind::InvalidInput.into()),
        }
    }

    fn is_executable(&self, path: &str) -> std::io::Result<bool> {
        Ok(matches!(
            self.node(path)?,
            Node::Regular {
                executable: true,
                ..
            }
        ))
    }
}

/// An archive's files and when they were last modified.
pub struct Unpacked {
    pub tree: MemTree,
    /// The latest modification time of any entry, or `None` if the archive
    /// has none.
    pub last_modified: Option<u64>,
}

/// Unpack a tar archive, compressed or not, or a zip archive. A single
/// top-level entry that is not a regular file becomes the root.
pub fn unpack(data: &[u8]) -> Result<Unpacked> {
    let unpacked = if data.starts_with(b"PK\x03\x04") || data.starts_with(b"PK\x05\x06") {
        unpack_zip(data)?
    } else {
        unpack_tar(&decompress(data)?)?
    };
    Ok(Unpacked {
        tree: unpacked.tree.strip_top_level(),
        ..unpacked
    })
}

/// `data`, decompressed according to its magic bytes, or as it is if it has
/// none.
fn decompress(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut reader: Box<dyn Read + '_> = if data.starts_with(&[0x1f, 0x8b]) {
        Box::new(flate2::read::MultiGzDecoder::new(data))
    } else if data.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0]) {
        Box::new(liblzma::read::XzDecoder::new_multi_decoder(data))
    } else if data.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        Box::new(zstd::stream::read::Decoder::new(data)?)
    } else if data.starts_with(b"BZh") {
        Box::new(bzip2::read::MultiBzDecoder::new(data))
    } else {
        return Ok(data.to_vec());
    };
    reader
        .read_to_end(&mut out)
        .context("decompressing archive")?;
    Ok(out)
}

/// The path of an archive entry relative to the root, made canonical as
/// [`crate::path::canon_path`] does.
fn entry_path(raw: &[u8]) -> String {
    let path = crate::path::canon_path(&format!("/{}", String::from_utf8_lossy(raw)));
    path[1..].to_owned()
}

fn unpack_tar(data: &[u8]) -> Result<Unpacked> {
    use tar::EntryType;
    let mut tree = MemTree::empty();
    let mut last_modified = None;
    let mut archive = tar::Archive::new(data);
    for entry in archive.entries().context("reading tar archive")? {
        let mut entry = entry.context("reading tar archive")?;
        let ty = entry.header().entry_type();
        if matches!(
            ty,
            EntryType::XGlobalHeader
                | EntryType::XHeader
                | EntryType::GNULongName
                | EntryType::GNULongLink
        ) {
            continue;
        }
        let path = entry_path(&entry.path_bytes());
        let mtime = pax_mtime(&mut entry)?.unwrap_or(entry.header().mtime()?);
        last_modified = last_modified.max(Some(mtime));
        let node = match ty {
            EntryType::Regular | EntryType::Continuous | EntryType::GNUSparse => {
                let executable = entry.header().mode()? & 0o100 != 0;
                let mut contents = Vec::new();
                entry.read_to_end(&mut contents)?;
                Node::Regular {
                    contents,
                    executable,
                }
            }
            EntryType::Directory => Node::Directory(BTreeSet::new()),
            EntryType::Symlink => {
                let target = entry.link_name_bytes().unwrap_or_default();
                Node::Symlink(target.into_owned())
            }
            EntryType::Link => {
                let target = entry.link_name_bytes().unwrap_or_default();
                let target = entry_path(&target);
                match tree.nodes.get(&target) {
                    Some(Node::Regular {
                        contents,
                        executable,
                    }) => Node::Regular {
                        contents: contents.clone(),
                        executable: *executable,
                    },
                    _ => bail!("hard link '{path}' to '{target}' does not name a file"),
                }
            }
            _ => bail!("archive entry '{path}' has an unsupported type"),
        };
        tree.insert(&path, node);
    }
    Ok(Unpacked {
        tree,
        last_modified,
    })
}

/// The whole seconds of an entry's PAX `mtime`, if it has one.
fn pax_mtime(entry: &mut tar::Entry<'_, &[u8]>) -> Result<Option<u64>> {
    let Some(extensions) = entry.pax_extensions()? else {
        return Ok(None);
    };
    for ext in extensions {
        let ext = ext?;
        if ext.key_bytes() == b"mtime" {
            let value = String::from_utf8_lossy(ext.value_bytes());
            let secs = value.split('.').next().unwrap_or("");
            return Ok(Some(secs.parse::<i64>().unwrap_or(0).max(0) as u64));
        }
    }
    Ok(None)
}

fn unpack_zip(data: &[u8]) -> Result<Unpacked> {
    let mut tree = MemTree::empty();
    let mut last_modified = None;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(data))?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let path = entry_path(file.name_raw());
        last_modified = last_modified.max(zip_mtime(&file));
        let node = if file.is_dir() {
            Node::Directory(BTreeSet::new())
        } else {
            let mut contents = Vec::new();
            file.read_to_end(&mut contents)?;
            if file.is_symlink() {
                Node::Symlink(contents)
            } else {
                Node::Regular {
                    contents,
                    executable: file.unix_mode().is_some_and(|m| m & 0o100 != 0),
                }
            }
        };
        tree.insert(&path, node);
    }
    Ok(Unpacked {
        tree,
        last_modified,
    })
}

/// A zip entry's modification time: from its extended timestamp if it has
/// one, else its DOS time taken as UTC.
fn zip_mtime(file: &zip::read::ZipFile<'_, std::io::Cursor<&[u8]>>) -> Option<u64> {
    for field in file.extra_data_fields() {
        if let zip::extra_fields::ExtraField::ExtendedTimestamp(ts) = field
            && let Some(t) = ts.mod_time()
        {
            return Some(u64::from(t));
        }
    }
    let t = file.last_modified()?;
    let days = days_from_civil(
        i64::from(t.year()),
        i64::from(t.month()),
        i64::from(t.day()),
    );
    let secs = days * 86400
        + i64::from(t.hour()) * 3600
        + i64::from(t.minute()) * 60
        + i64::from(t.second());
    Some(secs.max(0) as u64)
}

/// Days since the epoch of a date in the proleptic Gregorian calendar
/// (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tar archive of `(path, entry type, mode, contents or link target)`,
    /// with the paths stored as given.
    fn tar(entries: &[(&str, tar::EntryType, u32, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for &(path, ty, mode, data) in entries {
            let mut h = tar::Header::new_gnu();
            let gnu = h.as_gnu_mut().unwrap();
            gnu.name[..path.len()].copy_from_slice(path.as_bytes());
            let body: &[u8] = if ty.is_symlink() || ty.is_hard_link() {
                gnu.linkname[..data.len()].copy_from_slice(data);
                &[]
            } else {
                data
            };
            h.set_entry_type(ty);
            h.set_mode(mode);
            h.set_mtime(1000);
            h.set_size(body.len() as u64);
            h.set_cksum();
            b.append(&h, body).unwrap();
        }
        b.into_inner().unwrap()
    }

    fn listing(tree: &MemTree) -> Vec<String> {
        let mut out: Vec<String> = tree
            .nodes
            .iter()
            .map(|(p, n)| match n {
                Node::Regular {
                    contents,
                    executable,
                } => format!(
                    "{p} {}{}",
                    String::from_utf8_lossy(contents),
                    if *executable { " x" } else { "" }
                ),
                Node::Directory(_) => format!("{p}/"),
                Node::Symlink(t) => format!("{p} -> {}", String::from_utf8_lossy(t)),
            })
            .collect();
        out.sort();
        out
    }

    use tar::EntryType::Directory as D;
    use tar::EntryType::Link as H;
    use tar::EntryType::Regular as F;
    use tar::EntryType::Symlink as L;

    #[test]
    fn strips_a_single_top_level_directory() {
        let t = tar(&[("top/", D, 0o755, b""), ("top/a", F, 0o644, b"1")]);
        assert_eq!(listing(&unpack(&t).unwrap().tree), ["/", "a 1"]);
    }

    #[test]
    fn keeps_several_top_level_entries_or_a_lone_file() {
        let t = tar(&[("a/f", F, 0o644, b"1"), ("b", F, 0o644, b"2")]);
        assert_eq!(
            listing(&unpack(&t).unwrap().tree),
            ["/", "a/", "a/f 1", "b 2"]
        );
        let t = tar(&[("f", F, 0o644, b"1")]);
        assert_eq!(listing(&unpack(&t).unwrap().tree), ["/", "f 1"]);
    }

    #[test]
    fn a_lone_symlink_becomes_the_root() {
        let t = tar(&[("l", L, 0o777, b"target")]);
        assert_eq!(listing(&unpack(&t).unwrap().tree), [" -> target"]);
    }

    #[test]
    fn keeps_only_the_owner_executable_bit() {
        let t = tar(&[("o", F, 0o700, b"1"), ("g", F, 0o611, b"2")]);
        assert_eq!(listing(&unpack(&t).unwrap().tree), ["/", "g 2", "o 1 x"]);
    }

    #[test]
    fn copies_hard_links_and_lets_later_entries_win() {
        let t = tar(&[
            ("r/a", F, 0o644, b"1"),
            ("r/h", H, 0o644, b"r/a"),
            ("r/a", F, 0o644, b"2"),
        ]);
        assert_eq!(listing(&unpack(&t).unwrap().tree), ["/", "a 2", "h 1"]);
    }

    #[test]
    fn canonicalises_entry_paths() {
        let t = tar(&[("r/./q//s/f", F, 0o644, b"1")]);
        assert_eq!(
            listing(&unpack(&t).unwrap().tree),
            ["/", "q/", "q/s/", "q/s/f 1"]
        );
    }

    #[test]
    fn an_empty_archive_has_no_modification_time() {
        let unpacked = unpack(&tar(&[])).unwrap();
        assert_eq!(listing(&unpacked.tree), ["/"]);
        assert_eq!(unpacked.last_modified, None);
    }

    #[test]
    fn dates_dos_times_as_utc() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2020, 9, 13), 18518);
    }
}
