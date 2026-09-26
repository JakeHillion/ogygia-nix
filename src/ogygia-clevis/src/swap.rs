//! Atomic replacement of the blob, so a crash never leaves the secret
//! file partial or missing.

use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::Path;

use anyhow::Context;
use anyhow::Result;

/// Replaces the file at `path` with `contents`, keeping its permissions.
pub fn replace(path: &Path, contents: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .context("secret file has no parent directory")?;
    let permissions = fs::metadata(path)
        .with_context(|| format!("reading permissions of {}", path.display()))?
        .permissions();
    let mut tmp = tempfile::Builder::new()
        .prefix(".ogygia-clevis.")
        .tempfile_in(dir)
        .with_context(|| format!("creating temporary file in {}", dir.display()))?;
    tmp.as_file()
        .set_permissions(permissions)
        .context("setting permissions on temporary file")?;
    tmp.write_all(contents).context("writing temporary file")?;
    tmp.as_file().sync_all().context("syncing temporary file")?;
    tmp.persist(path)
        .with_context(|| format!("replacing {}", path.display()))?;
    File::open(dir)
        .and_then(|dir| dir.sync_all())
        .with_context(|| format!("syncing {}", dir.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn replaces_contents_and_keeps_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("disk.jwe");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();

        replace(&path, b"new").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o400
        );
        let entries: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(entries.len(), 1, "temporary file left behind");
    }

    #[test]
    fn requires_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(replace(&dir.path().join("missing.jwe"), b"new").is_err());
    }
}
