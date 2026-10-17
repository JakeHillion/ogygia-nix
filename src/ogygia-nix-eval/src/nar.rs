//! Nix Archive serialisation, as specified in the Nix manual's "Nix Archive"
//! section.

use std::io::Write;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::bail;

use crate::accessor::Accessor;
use crate::accessor::FileType;

/// Write one NAR string: its length, the bytes, and padding to 8 bytes.
fn write_str(w: &mut impl Write, s: &[u8]) -> std::io::Result<()> {
    w.write_all(&(s.len() as u64).to_le_bytes())?;
    w.write_all(s)?;
    let pad = (8 - s.len() % 8) % 8;
    w.write_all(&[0u8; 8][..pad])
}

/// Write the NAR serialisation of `path` in `acc`, including each directory
/// entry only if `keep(entry path, type)` accepts it.
pub fn dump(
    w: &mut impl Write,
    acc: &dyn Accessor,
    path: &str,
    keep: &mut dyn FnMut(&str, FileType) -> Result<bool>,
) -> Result<()> {
    write_str(w, b"nix-archive-1")?;
    dump_node(w, acc, path, keep)
}

fn dump_node(
    w: &mut impl Write,
    acc: &dyn Accessor,
    path: &str,
    keep: &mut dyn FnMut(&str, FileType) -> Result<bool>,
) -> Result<()> {
    let Some(ft) = acc.file_type(path) else {
        bail!("path '{path}' does not exist");
    };
    write_str(w, b"(")?;
    write_str(w, b"type")?;
    match ft {
        FileType::Symlink => {
            write_str(w, b"symlink")?;
            write_str(w, b"target")?;
            let target = acc
                .read_link(path)
                .with_context(|| format!("reading '{path}'"))?;
            write_str(w, &target)?;
        }
        FileType::Directory => {
            write_str(w, b"directory")?;
            let entries = acc
                .read_dir(path)
                .with_context(|| format!("reading directory '{path}'"))?;
            for (name, ty) in entries {
                let child = format!("{}/{name}", path.trim_end_matches('/'));
                if !keep(&child, ty)? {
                    continue;
                }
                write_str(w, b"entry")?;
                write_str(w, b"(")?;
                write_str(w, b"name")?;
                write_str(w, name.as_bytes())?;
                write_str(w, b"node")?;
                dump_node(w, acc, &child, keep)?;
                write_str(w, b")")?;
            }
        }
        FileType::Regular => {
            write_str(w, b"regular")?;
            if acc
                .is_executable(path)
                .with_context(|| format!("reading '{path}'"))?
            {
                write_str(w, b"executable")?;
                write_str(w, b"")?;
            }
            write_str(w, b"contents")?;
            let contents = acc
                .read_file(path)
                .with_context(|| format!("reading file '{path}'"))?;
            write_str(w, &contents)?;
        }
        FileType::Unknown => bail!("file '{path}' has an unsupported type"),
    }
    write_str(w, b")")?;
    Ok(())
}
