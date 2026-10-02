//! Fetching locked flake inputs that are not in the local Nix store.
//!
//! A fetched input is unpacked into a cache directory, checked against the
//! lock file's `narHash`, and made readable at the store path Nix would give
//! it. Nothing is written to the Nix store.

use std::io::BufRead;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use serde_json::Value as Json;

use crate::io::Io;

/// Where fetched sources are kept, by store path name.
fn cache_dir(io: &Io) -> Result<PathBuf> {
    if let Some(dir) = io.fetch_cache() {
        return Ok(dir.to_owned());
    }
    let base = match std::env::var_os("XDG_CACHE_HOME") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => PathBuf::from(
            std::env::var_os("HOME")
                .ok_or_else(|| anyhow!("neither XDG_CACHE_HOME nor HOME is set"))?,
        )
        .join(".cache"),
    };
    Ok(base.join("ogygia-nix-eval").join("sources"))
}

fn str_attr<'j>(locked: &'j serde_json::Map<String, Json>, name: &str) -> Result<&'j str> {
    locked
        .get(name)
        .and_then(Json::as_str)
        .ok_or_else(|| anyhow!("locked input has no '{name}'"))
}

/// Make the locked input `locked` readable at the logical store path
/// `out_path`, after checking that its NAR hash is `nar_sha256`.
pub fn fetch(
    io: &Io,
    locked: &serde_json::Map<String, Json>,
    out_path: &str,
    nar_sha256: &[u8],
) -> Result<()> {
    let name = out_path
        .rsplit('/')
        .next()
        .expect("store paths have a name");
    let cache = cache_dir(io)?;
    let dest = cache.join(name);
    if !dest.exists() {
        std::fs::create_dir_all(&cache).with_context(|| format!("creating {}", cache.display()))?;
        let staging = cache.join(format!(".{name}.{}", std::process::id()));
        if staging.exists() {
            std::fs::remove_dir_all(&staging)?;
        }
        let ty = str_attr(locked, "type")?;
        let result = match ty {
            "github" => {
                let owner = str_attr(locked, "owner")?;
                let repo = str_attr(locked, "repo")?;
                let rev = str_attr(locked, "rev")?;
                let host = locked
                    .get("host")
                    .and_then(Json::as_str)
                    .unwrap_or("github.com");
                fetch_tarball(
                    &format!("https://{host}/{owner}/{repo}/archive/{rev}.tar.gz"),
                    &staging,
                )
            }
            "gitlab" => {
                let owner = str_attr(locked, "owner")?;
                let repo = str_attr(locked, "repo")?;
                let rev = str_attr(locked, "rev")?;
                let host = locked
                    .get("host")
                    .and_then(Json::as_str)
                    .unwrap_or("gitlab.com");
                fetch_tarball(
                    &format!(
                        "https://{host}/api/v4/projects/{owner}%2F{repo}/repository/archive.tar.gz?sha={rev}"
                    ),
                    &staging,
                )
            }
            "tarball" => fetch_tarball(str_attr(locked, "url")?, &staging),
            "git" => fetch_git(str_attr(locked, "url")?, str_attr(locked, "rev")?, &staging),
            other => Err(anyhow!("fetching {other} inputs is not supported")),
        };
        if let Err(e) = result {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e.context(format!("fetching {out_path}")));
        }
        verify(io, &staging, out_path, nar_sha256)?;
        std::fs::rename(&staging, &dest)
            .with_context(|| format!("moving fetched source to {}", dest.display()))?;
    }
    io.mount(out_path, &dest, None);
    Ok(())
}

fn verify(io: &Io, dir: &Path, out_path: &str, expected: &[u8]) -> Result<()> {
    let staging = format!("/.ogygia-nix-eval/fetch{}", dir.display());
    io.mount(&staging, dir, None);
    let got = io.nar_hash(&staging)?;
    if got.as_slice() != expected {
        let _ = std::fs::remove_dir_all(dir);
        bail!(
            "NAR hash mismatch fetching {out_path}: expected {}, got {}",
            crate::store::format_hash("sha256", expected, "sri")?,
            crate::store::format_hash("sha256", &got, "sri")?
        );
    }
    Ok(())
}

fn http() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(concat!("ogygia-nix-eval/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

fn download(url: &str) -> Result<Vec<u8>> {
    if let Some(path) = url.strip_prefix("file://") {
        return std::fs::read(path).with_context(|| format!("reading {url}"));
    }
    let resp = http()?
        .get(url)
        .send()
        .with_context(|| format!("downloading {url}"))?;
    if !resp.status().is_success() {
        bail!("downloading {url}: HTTP {}", resp.status());
    }
    Ok(resp.bytes()?.to_vec())
}

/// Download an archive and unpack it into `dest`, dropping the single
/// top-level directory archives usually have.
fn fetch_tarball(url: &str, dest: &Path) -> Result<()> {
    let data = download(url)?;
    let reader: Box<dyn Read> = if data.starts_with(&[0x1f, 0x8b]) {
        Box::new(flate2::read::GzDecoder::new(&data[..]))
    } else if data.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0]) {
        let mut out = Vec::new();
        lzma_rs::xz_decompress(&mut std::io::BufReader::new(&data[..]), &mut out)
            .map_err(|e| anyhow!("decompressing {url}: {e:?}"))?;
        Box::new(std::io::Cursor::new(out))
    } else {
        Box::new(&data[..])
    };
    unpack_tar(reader, dest).with_context(|| format!("unpacking {url}"))
}

fn unpack_tar(reader: impl Read, dest: &Path) -> Result<()> {
    let unpacked = dest.with_extension("unpack");
    if unpacked.exists() {
        std::fs::remove_dir_all(&unpacked)?;
    }
    std::fs::create_dir_all(&unpacked)?;
    let mut archive = tar::Archive::new(reader);
    let mut links = Vec::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if path
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            bail!("archive entry {} escapes the archive", path.display());
        }
        let target = unpacked.join(&path);
        let kind = entry.header().entry_type();
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match kind {
            tar::EntryType::Directory => std::fs::create_dir_all(&target)?,
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                let mode = entry.header().mode()?;
                let mut contents = Vec::new();
                entry.read_to_end(&mut contents)?;
                write_file(&target, &contents, mode & 0o100 != 0)?;
            }
            tar::EntryType::Symlink => {
                let link = entry
                    .link_name()?
                    .ok_or_else(|| anyhow!("symlink {} has no target", path.display()))?;
                std::os::unix::fs::symlink(link, &target)?;
            }
            tar::EntryType::Link => {
                let link = entry
                    .link_name()?
                    .ok_or_else(|| anyhow!("hard link {} has no target", path.display()))?
                    .into_owned();
                links.push((target, link));
            }
            // Pax and GNU metadata entries carry no files.
            _ => {}
        }
    }
    for (target, link) in links {
        std::fs::copy(unpacked.join(link), target)?;
    }
    // Strip a single top-level directory.
    let top: Vec<_> = std::fs::read_dir(&unpacked)?.collect::<std::io::Result<_>>()?;
    let root = match top.as_slice() {
        [only] if only.file_type()?.is_dir() => only.path(),
        _ => unpacked.clone(),
    };
    std::fs::rename(&root, dest)?;
    if unpacked.exists() {
        std::fs::remove_dir_all(&unpacked)?;
    }
    Ok(())
}

fn write_file(path: &Path, contents: &[u8], executable: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, contents)?;
    let mode = if executable { 0o755 } else { 0o644 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

/// Convert a gitoxide result, whose errors are not all `std::error::Error`.
fn git<T, E: std::fmt::Debug>(r: std::result::Result<T, E>) -> Result<T> {
    r.map_err(|e| anyhow!("git: {e:?}"))
}

/// Encode one Git pkt-line.
fn pkt_line(s: &str) -> Vec<u8> {
    let mut v = format!("{:04x}", s.len() + 4).into_bytes();
    v.extend_from_slice(s.as_bytes());
    v
}

/// Fetch commit `rev` of the repository at `url` over Git's smart HTTP
/// protocol (version 2) and write its tree to `dest`.
fn fetch_git(url: &str, rev: &str, dest: &Path) -> Result<()> {
    if let Some(path) = url.strip_prefix("file://") {
        let repo = git(gix::open(path))?;
        return checkout(&repo, rev, dest);
    }
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        bail!("only http(s) and file Git URLs can be fetched, not {url}");
    }
    let url = url.trim_end_matches('/');
    let client = http()?;
    // Protocol version 2 needs a capability request first.
    client
        .get(format!("{url}/info/refs?service=git-upload-pack"))
        .header("Git-Protocol", "version=2")
        .send()
        .and_then(|r| r.error_for_status())
        .with_context(|| format!("contacting {url}"))?;
    let mut req = pkt_line("command=fetch\n");
    req.extend(pkt_line("object-format=sha1\n"));
    req.extend(b"0001");
    req.extend(pkt_line(&format!("want {rev}\n")));
    req.extend(pkt_line("deepen 1\n"));
    req.extend(pkt_line("no-progress\n"));
    req.extend(pkt_line("done\n"));
    req.extend(b"0000");
    let resp = client
        .post(format!("{url}/git-upload-pack"))
        .header("Git-Protocol", "version=2")
        .header("Content-Type", "application/x-git-upload-pack-request")
        .body(req)
        .send()
        .and_then(|r| r.error_for_status())
        .with_context(|| format!("fetching {rev} from {url}"))?;
    let pack = read_packfile(std::io::BufReader::new(resp))?;

    let repo_dir = dest.with_extension("git");
    if repo_dir.exists() {
        std::fs::remove_dir_all(&repo_dir)?;
    }
    git(gix::init_bare(&repo_dir))?;
    gix_pack::Bundle::write_to_directory(
        &mut std::io::BufReader::new(&pack[..]),
        Some(&repo_dir.join("objects/pack")),
        &mut gix::progress::Discard,
        &std::sync::atomic::AtomicBool::new(false),
        None::<gix::odb::Cache<gix::odb::store::Handle<std::sync::Arc<gix::odb::Store>>>>,
        gix::hash::Kind::Sha1,
        Default::default(),
    )
    .map_err(|e| anyhow!("indexing pack from {url}: {e:?}"))?;
    let repo = git(gix::open(&repo_dir))?;
    checkout(&repo, rev, dest)?;
    std::fs::remove_dir_all(&repo_dir)?;
    Ok(())
}

/// Write the tree of commit `rev` to `dest`.
fn checkout(repo: &gix::Repository, rev: &str, dest: &Path) -> Result<()> {
    let id = git(gix::ObjectId::from_hex(rev.as_bytes()))?;
    let commit = git(git(repo.find_object(id))?.try_into_commit())?;
    let tree = git(commit.tree())?;
    std::fs::create_dir_all(dest)?;
    write_tree(repo, &tree, dest)
}

/// The pack data of a protocol version 2 fetch response.
fn read_packfile(mut r: impl BufRead) -> Result<Vec<u8>> {
    let mut pack = Vec::new();
    let mut in_pack = false;
    loop {
        let mut len = [0u8; 4];
        if r.read_exact(&mut len).is_err() {
            break;
        }
        let len = usize::from_str_radix(std::str::from_utf8(&len)?, 16)?;
        if len < 4 {
            // Flush, delimiter or response-end packet.
            continue;
        }
        let mut line = vec![0u8; len - 4];
        r.read_exact(&mut line)?;
        if !in_pack {
            if line == b"packfile\n" {
                in_pack = true;
            } else if let Some(msg) = line.strip_prefix(b"ERR ") {
                bail!("git server error: {}", String::from_utf8_lossy(msg));
            }
            continue;
        }
        match line.first() {
            Some(1) => pack.extend_from_slice(&line[1..]),
            Some(3) => bail!("git server error: {}", String::from_utf8_lossy(&line[1..])),
            _ => {}
        }
    }
    if pack.is_empty() {
        bail!("git server sent no pack");
    }
    Ok(pack)
}

fn write_tree(repo: &gix::Repository, tree: &gix::Tree<'_>, dir: &Path) -> Result<()> {
    for entry in tree.iter() {
        let entry = git(entry)?;
        let path = dir.join(gix::path::from_bstr(entry.filename()).as_ref());
        let mode = entry.mode();
        if mode.is_tree() {
            std::fs::create_dir_all(&path)?;
            let sub = git(git(repo.find_object(entry.oid()))?.try_into_tree())?;
            write_tree(repo, &sub, &path)?;
        } else if mode.is_link() {
            let target = git(repo.find_object(entry.oid()))?;
            let target: &std::ffi::OsStr = std::os::unix::ffi::OsStrExt::from_bytes(&target.data);
            std::os::unix::fs::symlink(target, &path)?;
        } else if mode.is_commit() {
            // Submodules are not fetched; Nix leaves an empty directory.
            std::fs::create_dir_all(&path)?;
        } else {
            let blob = git(repo.find_object(entry.oid()))?;
            write_file(&path, &blob.data, mode.is_executable())?;
        }
    }
    Ok(())
}
