//! Flakes: resolving a lock file to input sources and building the flake
//! value that `outputs` receives, as described in the Nix manual's
//! "nix flake" reference.
//!
//! Inputs are located by their locked `narHash`: an input's store path is
//! fully determined by it, so an input that is already in the local store is
//! used in place. A local flake in a Git checkout is read from the working
//! tree, restricted to the files Git tracks, and given the store path Nix
//! would copy it to.

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use serde_json::Value as Json;

use crate::eval::Eval;
use crate::io::Io;
use crate::store;
use crate::value::Entry;
use crate::value::ErrorKind;
use crate::value::R;
use crate::value::Value;
use crate::value::error;

/// A scalar attribute of a source's `sourceInfo`.
#[derive(Clone, Debug)]
pub enum Info {
    Int(i64),
    Str(String),
    Bool(bool),
}

/// A fetched (or located) source tree.
#[derive(Clone, Debug)]
pub struct Source {
    /// The logical store path of the tree.
    pub out_path: String,
    /// `sourceInfo` attributes other than `outPath`.
    pub info: BTreeMap<String, Info>,
    /// Subdirectory holding `flake.nix`.
    pub dir: Option<String>,
}

enum InputRef {
    Node(String),
    Follows(Vec<String>),
}

struct Node {
    inputs: BTreeMap<String, InputRef>,
    locked: Option<serde_json::Map<String, Json>>,
    flake: bool,
    /// For a relative path input, the input path (from the root) of the
    /// node whose source it lies in.
    parent: Option<Vec<String>>,
}

/// A flake whose lock has been resolved: every node's source is known.
pub struct LockedFlake {
    root: String,
    nodes: BTreeMap<String, Node>,
    sources: BTreeMap<String, Source>,
}

fn parse_lock(text: &str) -> Result<(String, BTreeMap<String, Node>)> {
    let json: Json = serde_json::from_str(text).context("parsing flake.lock")?;
    let version = json["version"].as_i64().unwrap_or(0);
    if !(5..=7).contains(&version) {
        bail!("unsupported flake.lock version {version}");
    }
    let root = json["root"]
        .as_str()
        .ok_or_else(|| anyhow!("flake.lock has no root"))?
        .to_owned();
    let mut nodes = BTreeMap::new();
    for (key, node) in json["nodes"]
        .as_object()
        .ok_or_else(|| anyhow!("flake.lock has no nodes"))?
    {
        let mut inputs = BTreeMap::new();
        if let Some(ins) = node["inputs"].as_object() {
            for (name, r) in ins {
                let r = match r {
                    Json::String(s) => InputRef::Node(s.clone()),
                    Json::Array(path) => InputRef::Follows(
                        path.iter()
                            .map(|p| p.as_str().map(str::to_owned))
                            .collect::<Option<_>>()
                            .ok_or_else(|| anyhow!("bad follows path in flake.lock"))?,
                    ),
                    _ => bail!("bad input reference in flake.lock"),
                };
                inputs.insert(name.clone(), r);
            }
        }
        nodes.insert(
            key.clone(),
            Node {
                inputs,
                locked: node["locked"].as_object().cloned(),
                flake: node["flake"].as_bool().unwrap_or(true),
                parent: node["parent"].as_array().map(|p| {
                    p.iter()
                        .filter_map(|s| s.as_str().map(str::to_owned))
                        .collect()
                }),
            },
        );
    }
    Ok((root, nodes))
}

impl LockedFlake {
    /// The node an input of `node` refers to, following `follows` paths
    /// from the root.
    fn input_node(&self, node: &str, name: &str) -> Result<String> {
        let n = self
            .nodes
            .get(node)
            .ok_or_else(|| anyhow!("flake.lock has no node '{node}'"))?;
        match n.inputs.get(name) {
            Some(InputRef::Node(k)) => Ok(k.clone()),
            Some(InputRef::Follows(path)) => self.follow(path),
            None => bail!("input '{name}' of '{node}' is not locked"),
        }
    }

    fn follow(&self, path: &[String]) -> Result<String> {
        let mut node = self.root.clone();
        for name in path {
            node = self.input_node(&node, name)?;
        }
        Ok(node)
    }
}

/// Seconds since the epoch as `YYYYMMDDHHMMSS` in UTC.
fn format_date(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}{m:02}{d:02}{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn add_time_info(info: &mut BTreeMap<String, Info>, last_modified: i64) {
    info.insert("lastModified".into(), Info::Int(last_modified));
    info.insert(
        "lastModifiedDate".into(),
        Info::Str(format_date(last_modified)),
    );
}

fn add_rev_info(info: &mut BTreeMap<String, Info>, rev: &str) {
    info.insert("rev".into(), Info::Str(rev.to_owned()));
    info.insert(
        "shortRev".into(),
        Info::Str(rev[..rev.len().min(7)].to_owned()),
    );
}

/// The source of a locked (non-root) input.
fn locked_source(io: &Io, locked: &serde_json::Map<String, Json>) -> Result<Source> {
    let ty = locked
        .get("type")
        .and_then(Json::as_str)
        .ok_or_else(|| anyhow!("locked input has no type"))?;
    let nar_hash = locked
        .get("narHash")
        .and_then(Json::as_str)
        .ok_or_else(|| anyhow!("locked {ty} input has no narHash"))?;
    let (_, digest) = store::parse_hash(nar_hash, Some("sha256"))?;
    let out_path = store::source_path(&digest, "source");
    if !io.in_nix_store(&out_path) {
        crate::fetch::fetch(io, locked, &out_path, &digest)?;
    }
    let mut info = BTreeMap::new();
    if let Some(t) = locked.get("lastModified").and_then(Json::as_i64) {
        add_time_info(&mut info, t);
    }
    info.insert("narHash".into(), Info::Str(nar_hash.to_owned()));
    if let Some(rev) = locked.get("rev").and_then(Json::as_str) {
        add_rev_info(&mut info, rev);
    }
    if let Some(c) = locked.get("revCount").and_then(Json::as_i64) {
        info.insert("revCount".into(), Info::Int(c));
    }
    if ty == "git" {
        let sub = locked
            .get("submodules")
            .and_then(Json::as_bool)
            .unwrap_or(false);
        info.insert("submodules".into(), Info::Bool(sub));
    }
    let dir = locked
        .get("dir")
        .and_then(Json::as_str)
        .filter(|d| !d.is_empty())
        .map(str::to_owned);
    Ok(Source {
        out_path,
        info,
        dir,
    })
}

/// The source of a flake in a local directory: the Git checkout containing
/// it (restricted to tracked files), or the directory itself.
pub fn local_source(io: &Io, dir: &Path) -> Result<Source> {
    let dir = dir
        .canonicalize()
        .with_context(|| format!("resolving flake directory {}", dir.display()))?;
    match gix::discover(&dir) {
        Ok(repo) if repo.workdir().is_some() => git_source(io, &repo, &dir),
        _ => path_source(io, &dir),
    }
}

fn mount_and_hash(io: &Io, root: &Path, filter: Option<HashSet<String>>) -> Result<String> {
    // Hash the tree through a private mount, then alias the resulting store
    // path to it.
    let staging = format!("/.ogygia-nix-eval/local/{}", root.display());
    io.mount(&staging, root, filter);
    let sp = io.add_filtered_to_store(&staging, "source", None)?;
    Ok(sp)
}

/// The newest modification time of anything in the tree at `path`.
fn newest_mtime(path: &Path) -> Result<i64> {
    let meta = path.symlink_metadata()?;
    let mut newest = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as i64;
    if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            newest = newest.max(newest_mtime(&entry?.path())?);
        }
    }
    Ok(newest)
}

fn path_source(io: &Io, dir: &Path) -> Result<Source> {
    let out_path = mount_and_hash(io, dir, None)?;
    let (_, digest) = store::parse_hash(
        &format!("sha256:{}", hex::encode(io.nar_hash(&out_path)?)),
        None,
    )?;
    let mut info = BTreeMap::new();
    info.insert(
        "narHash".into(),
        Info::Str(store::format_hash("sha256", &digest, "sri")?),
    );
    add_time_info(&mut info, newest_mtime(dir)?);
    Ok(Source {
        out_path,
        info,
        dir: None,
    })
}

/// The Git blob id of a worktree file, or `None` if it cannot be read.
fn worktree_blob_id(path: &Path) -> Option<String> {
    use sha1::Digest;
    let meta = path.symlink_metadata().ok()?;
    let contents = if meta.file_type().is_symlink() {
        std::os::unix::ffi::OsStrExt::as_bytes(std::fs::read_link(path).ok()?.as_os_str()).to_vec()
    } else {
        std::fs::read(path).ok()?
    };
    let mut h = sha1::Sha1::new();
    h.update(format!("blob {}\0", contents.len()).as_bytes());
    h.update(&contents);
    Some(hex::encode(h.finalize()))
}

/// Whether the tracked files differ from `HEAD`: in the index, or in the
/// working tree relative to the index.
fn is_dirty(
    root: &Path,
    index: &gix::index::File,
    head_files: &std::collections::HashMap<String, (String, u32)>,
) -> bool {
    let mut seen = 0;
    for entry in index.entries() {
        if entry.stage_raw() != 0 {
            return true;
        }
        let path = entry.path(index).to_string();
        let id = entry.id.to_string();
        let mode = entry.mode.bits();
        if head_files.get(&path) != Some(&(id.clone(), mode)) {
            return true;
        }
        seen += 1;
        if entry.mode.is_submodule() {
            continue;
        }
        let file = root.join(&path);
        if worktree_blob_id(&file).as_deref() != Some(id.as_str()) {
            return true;
        }
        let executable = file
            .symlink_metadata()
            .map(|m| m.permissions().mode() & 0o100 != 0)
            .unwrap_or(false);
        if entry.mode == gix::index::entry::Mode::FILE_EXECUTABLE && !executable
            || entry.mode == gix::index::entry::Mode::FILE && executable
        {
            return true;
        }
    }
    seen != head_files.len()
}

fn git_source(io: &Io, repo: &gix::Repository, dir: &Path) -> Result<Source> {
    let root = repo.workdir().expect("checked by caller").canonicalize()?;
    let index = repo.index_or_empty()?;
    let mut tracked = HashSet::new();
    for entry in index.entries() {
        let path = entry.path(&index).to_string();
        // A deleted but still tracked file does not exist.
        if root.join(&path).symlink_metadata().is_err() {
            continue;
        }
        // Every ancestor directory of a tracked file exists too.
        let mut p = path.as_str();
        while let Some((parent, _)) = p.rsplit_once('/') {
            if !tracked.insert(parent.to_owned()) {
                break;
            }
            p = parent;
        }
        tracked.insert(path);
    }

    let out_path = mount_and_hash(io, &root, Some(tracked))?;
    let digest = io.nar_hash(&out_path)?;
    let mut info = BTreeMap::new();
    info.insert(
        "narHash".into(),
        Info::Str(store::format_hash("sha256", &digest, "sri")?),
    );

    match repo.head_commit() {
        Ok(head) => {
            let mut head_files = std::collections::HashMap::new();
            let mut rec = gix::traverse::tree::Recorder::default();
            head.tree()?.traverse().breadthfirst(&mut rec)?;
            for e in rec.records {
                if !e.mode.is_tree() {
                    head_files.insert(
                        e.filepath.to_string(),
                        (e.oid.to_string(), e.mode.value() as u32),
                    );
                }
            }
            add_time_info(&mut info, head.time()?.seconds);
            let rev = head.id.to_string();
            if is_dirty(&root, &index, &head_files) {
                info.insert("dirtyRev".into(), Info::Str(format!("{rev}-dirty")));
                info.insert(
                    "dirtyShortRev".into(),
                    Info::Str(format!("{}-dirty", &rev[..7])),
                );
            } else {
                add_rev_info(&mut info, &rev);
                let count = repo.rev_walk([head.id]).all()?.count();
                info.insert("revCount".into(), Info::Int(count as i64));
            }
        }
        Err(_) => add_time_info(&mut info, 0),
    }
    info.insert("submodules".into(), Info::Bool(false));

    let rel = dir.strip_prefix(&root).unwrap_or(Path::new(""));
    let dir = rel.to_str().filter(|d| !d.is_empty()).map(str::to_owned);
    Ok(Source {
        out_path,
        info,
        dir,
    })
}

/// Read the lock file of the flake at `source` and locate every input.
pub fn lock(io: &Io, source: Source) -> Result<LockedFlake> {
    let flake_dir = match &source.dir {
        Some(d) => crate::path::canon_path(&format!("{}/{d}", source.out_path)),
        None => source.out_path.clone(),
    };
    let lock_path = format!("{flake_dir}/flake.lock");
    let (root, nodes) = if io.exists(&lock_path) {
        parse_lock(&io.read_to_string(&lock_path)?)?
    } else {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            "root".to_owned(),
            Node {
                inputs: BTreeMap::new(),
                locked: None,
                flake: true,
                parent: None,
            },
        );
        ("root".to_owned(), nodes)
    };
    let mut flake = LockedFlake {
        root: root.clone(),
        nodes,
        sources: BTreeMap::new(),
    };
    flake.sources.insert(root, source);
    Ok(flake)
}

impl LockedFlake {
    /// Locate the source of node `key`, and of any node it lies within.
    fn resolve_source(&mut self, io: &Io, key: &str) -> Result<Source> {
        if let Some(s) = self.sources.get(key) {
            return Ok(s.clone());
        }
        let node = &self.nodes[key];
        let locked = node
            .locked
            .as_ref()
            .ok_or_else(|| anyhow!("flake.lock node '{key}' is not locked"))?;
        let relative = locked.get("type").and_then(Json::as_str) == Some("path")
            && locked.get("narHash").is_none();
        let source = if relative {
            // A relative path input is a subdirectory of its parent's source.
            let path = locked
                .get("path")
                .and_then(Json::as_str)
                .ok_or_else(|| anyhow!("path input '{key}' has no path"))?
                .to_owned();
            let parent_path = node.parent.clone().unwrap_or_default();
            let parent = self.follow(&parent_path)?;
            let mut source = self.resolve_source(io, &parent)?;
            source.dir = Some(match source.dir {
                Some(d) => format!("{d}/{path}"),
                None => path,
            });
            source
        } else {
            locked_source(io, locked)?
        };
        self.sources.insert(key.to_owned(), source.clone());
        Ok(source)
    }
}

fn source_info<'a>(ev: &Eval<'a>, src: &Source) -> Value<'a> {
    let mut entries: Vec<Entry<'a>> = src
        .info
        .iter()
        .map(|(k, v)| {
            ev.entry(
                k,
                match v {
                    Info::Int(i) => Value::Int(*i),
                    Info::Str(s) => ev.string(s),
                    Info::Bool(b) => Value::Bool(*b),
                },
            )
        })
        .collect();
    entries.push(ev.entry("outPath", ev.string(&src.out_path)));
    ev.attrs(entries)
}

fn to_eval_err(e: anyhow::Error) -> Box<crate::value::EvalError> {
    error(ErrorKind::Eval, format!("{e:#}"))
}

impl<'a> Eval<'a> {
    /// Register a locked flake with this session and return its value.
    pub fn flake_value(&self, flake: LockedFlake) -> R<'a> {
        let id = {
            let mut flakes = self.flakes.borrow_mut();
            flakes.push(flake);
            flakes.len() - 1
        };
        let root: &'a str = self.bump.alloc_str(&self.flakes.borrow()[id].root);
        self.force(self.flake_node(id, root))
    }

    /// The (lazy) value of node `key` of registered flake `id`.
    fn flake_node(&self, id: usize, key: &'a str) -> Value<'a> {
        let cache_key = (id, key.to_owned());
        if let Some(v) = self.flake_cache.borrow().get(&cache_key) {
            return *v;
        }
        let v = self.lazy_native(move |ev| ev.build_flake_node(id, key));
        self.flake_cache.borrow_mut().insert(cache_key, v);
        v
    }

    fn build_flake_node(&self, id: usize, key: &'a str) -> R<'a> {
        let (src, is_flake, inputs) = {
            let mut flakes = self.flakes.borrow_mut();
            let flake = &mut flakes[id];
            // Inputs are located, and fetched if need be, only once
            // evaluation reaches them.
            let src = flake
                .resolve_source(&self.ctx.io, key)
                .with_context(|| format!("locating flake input '{key}'"))
                .map_err(to_eval_err)?;
            let node = &flake.nodes[key];
            let inputs = node
                .inputs
                .keys()
                .map(|name| Ok((name.clone(), flake.input_node(key, name)?)))
                .collect::<Result<Vec<_>>>()
                .map_err(to_eval_err)?;
            (src, node.flake, inputs)
        };
        let info = source_info(self, &src);
        let info_attrs = self.force_attrs(info)?;
        // An input in a subdirectory of its source keeps the directory as
        // written, `./` and all.
        let out_path = match &src.dir {
            Some(d) => format!("{}/{d}", src.out_path),
            None => src.out_path.clone(),
        };
        if !is_flake {
            let extra = self.attrs(vec![
                self.entry("sourceInfo", info),
                self.entry("outPath", self.string(&out_path)),
            ]);
            return Ok(self.update(info_attrs, self.force_attrs(extra)?));
        }
        let flake_nix = crate::path::canon_path(&format!("{out_path}/flake.nix"));
        let flake_def = self.import(&flake_nix)?;
        let flake_def = self.force_attrs(flake_def)?;
        let Some(outputs_fn) = flake_def.get(self.sym("outputs")) else {
            return crate::value::eval_err(format!(
                "flake '{flake_nix}' lacks attribute 'outputs'"
            ));
        };
        let input_entries: Vec<Entry<'a>> = inputs
            .into_iter()
            .map(|(name, node)| {
                let node: &'a str = self.bump.alloc_str(&node);
                self.entry(&name, self.flake_node(id, node))
            })
            .collect();
        let inputs = self.attrs(input_entries);
        let this = self.flake_node(id, key);
        let args = self.update(
            self.force_attrs(inputs)?,
            self.force_attrs(self.attrs(vec![self.entry("self", this)]))?,
        );
        let outputs = self.call(outputs_fn, args)?;
        let outputs_attrs = self.force_attrs(outputs)?;
        let mut extra = vec![
            self.entry("inputs", inputs),
            self.entry("outputs", outputs),
            self.entry("sourceInfo", info),
            self.entry("_type", self.string("flake")),
        ];
        extra.push(self.entry("outPath", self.string(&out_path)));
        let with_info = self.update(outputs_attrs, info_attrs);
        let Value::Attrs(with_info) = with_info else {
            unreachable!("update returns a set")
        };
        Ok(self.update(with_info, self.force_attrs(self.attrs(extra))?))
    }
}

/// Open the flake in `dir` and lock its inputs.
pub fn open_local(io: &Io, dir: &Path) -> Result<LockedFlake> {
    let source = local_source(io, dir)?;
    lock(io, source)
}

/// `builtins.getFlake`: a flake reference that is a local path.
pub fn get_flake<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let r = ev.force_str_no_ctx(args[0])?;
    let r = String::from_utf8_lossy(r).into_owned();
    let path = r
        .strip_prefix("path:")
        .or_else(|| r.strip_prefix("git+file://"))
        .unwrap_or(&r);
    if !path.starts_with('/') {
        return crate::value::eval_err(format!(
            "flake reference '{r}' is not supported; only local absolute paths are"
        ));
    }
    let physical = ev.ctx.io.physical(path).map_err(to_eval_err)?;
    let locked = open_local(&ev.ctx.io, &physical).map_err(to_eval_err)?;
    ev.flake_value(locked)
}

/// Split an attribute path such as `a."b.c".d` into its components.
pub fn parse_attr_path(s: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' if quoted => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            '.' if !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    if quoted {
        bail!("unterminated quote in attribute path '{s}'");
    }
    if !s.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

/// The physical directory of a flake reference naming a local flake.
pub fn local_flake_dir(flake_ref: &str) -> Result<PathBuf> {
    let path = flake_ref
        .strip_prefix("path:")
        .or_else(|| flake_ref.strip_prefix("git+file://"))
        .unwrap_or(flake_ref);
    if path.contains(':') {
        bail!("flake reference '{flake_ref}' is not a local flake");
    }
    Ok(PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(format_date(1790450599), "20260926192319");
        assert_eq!(format_date(0), "19700101000000");
    }

    #[test]
    fn attr_paths() {
        assert_eq!(
            parse_attr_path(r#"nixosConfigurations."a.b.c".config"#).unwrap(),
            ["nixosConfigurations", "a.b.c", "config"]
        );
        assert!(parse_attr_path("").unwrap().is_empty());
    }
}
