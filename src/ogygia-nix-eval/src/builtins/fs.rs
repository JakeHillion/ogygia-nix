//! Builtins that read files or compute store paths of them.

use std::cell::Cell;

use crate::eval::Coerce;
use crate::eval::Eval;
use crate::value::Ctx;
use crate::value::Entry;
use crate::value::Env;
use crate::value::ErrorKind;
use crate::value::R;
use crate::value::Value;
use crate::value::error;
use crate::value::eval_err;

fn io_err(e: anyhow::Error) -> Box<crate::value::EvalError> {
    error(ErrorKind::Eval, format!("{e:#}"))
}

pub fn import<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let path = ev.coerce_to_path(args[0])?;
    ev.import(&path)
}

pub fn scoped_import<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let scope = ev.force_attrs(args[0])?;
    let path = ev.coerce_to_path(args[1])?;
    let file = ev.ctx.io.resolve_import(&path).map_err(io_err)?;
    let names: Vec<_> = scope.entries.iter().map(|e| e.name).collect();
    let expr = ev
        .ctx
        .compile_file_scoped(&file, ev.settings.pure, &names)
        .map_err(io_err)?;
    let slots = ev
        .bump
        .alloc_slice_fill_iter(scope.entries.iter().map(|e| Cell::new(e.value)));
    let env = ev.alloc(Env {
        parent: None,
        slots,
    });
    ev.eval(expr, env)
}

pub fn read_file<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let path = ev.coerce_to_path(args[0])?;
    let bytes = ev.ctx.io.read(&path).map_err(io_err)?;
    Ok(ev.str_val(&bytes, &[]))
}

pub fn read_dir<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let path = ev.coerce_to_path(args[0])?;
    let entries = ev.ctx.io.read_dir(&path).map_err(io_err)?;
    Ok(ev.attrs(
        entries
            .into_iter()
            .map(|(name, ty)| Entry {
                name: ev.sym(&name),
                value: ev.string(ty.name()),
                pos: None,
            })
            .collect(),
    ))
}

pub fn read_file_type<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let path = ev.coerce_to_path(args[0])?;
    match ev.ctx.io.file_type(&path) {
        Some(t) => Ok(ev.string(t.name())),
        None => eval_err(format!("path '{path}' does not exist")),
    }
}

pub fn path_exists<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let v = ev.force(args[0])?;
    let (raw, _) = ev.coerce_to_string(v, Coerce::PLAIN)?;
    let path = ev.coerce_to_path(v)?;
    let io = &ev.ctx.io;
    // A trailing slash requires a directory.
    if raw.ends_with(b"/") && path != "/" {
        return Ok(Value::Bool(io.is_dir(&path)));
    }
    Ok(Value::Bool(io.exists(&path)))
}

pub fn hash_file<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let algo = ev.force_str_no_ctx(args[0])?;
    let path = ev.coerce_to_path(args[1])?;
    let bytes = ev.ctx.io.read(&path).map_err(io_err)?;
    match super::strings::hash_bytes(algo, &bytes) {
        Some(h) => Ok(ev.string(&hex::encode(h))),
        None => eval_err(format!(
            "unknown hash algorithm '{}'",
            String::from_utf8_lossy(algo)
        )),
    }
}

pub fn find_file<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let search = ev.force_list(args[0])?;
    let name = ev.force_str_no_ctx(args[1])?;
    let name = String::from_utf8_lossy(name).into_owned();
    let syms = &ev.ctx.syms;
    for entry in search {
        let entry = ev.force_attrs(*entry)?;
        let prefix = match entry.get(syms.prefix) {
            Some(p) => String::from_utf8_lossy(ev.force_str_no_ctx(p)?).into_owned(),
            None => String::new(),
        };
        let Some(path) = entry.get(syms.path) else {
            return eval_err("attribute 'path' missing in a search path entry");
        };
        let (path, _) = ev.coerce_to_string(path, Coerce::PLAIN)?;
        let path = String::from_utf8_lossy(&path).into_owned();
        let rest = if prefix.is_empty() {
            Some(name.as_str())
        } else if name == prefix {
            Some("")
        } else {
            name.strip_prefix(&format!("{prefix}/"))
        };
        let Some(rest) = rest else { continue };
        let candidate = if rest.is_empty() {
            path
        } else {
            format!("{path}/{rest}")
        };
        let candidate = crate::path::canon_path(&candidate);
        if ev.ctx.io.exists(&candidate) {
            return Ok(ev.path_val(&candidate));
        }
    }
    eval_err(format!(
        "file '{name}' was not found in the Nix search path (add it using $NIX_PATH or -I)"
    ))
}

pub fn to_file<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let name = ev.force_str_no_ctx(args[0])?;
    let name = String::from_utf8_lossy(name).into_owned();
    crate::store::check_name(&name).map_err(io_err)?;
    let contents = ev.force_str(args[1])?;
    let mut refs = Vec::new();
    for c in contents.ctx {
        match c {
            Ctx::Opaque(p) => refs.push(*p),
            _ => {
                return eval_err(format!(
                    "files created by builtins.toFile may not reference derivations, but {name} references one"
                ));
            }
        }
    }
    let path = crate::store::text_path(contents.s, &name, &refs);
    let path: &'a str = ev.bump.alloc_str(&path);
    Ok(ev.str_val(path.as_bytes(), &[Ctx::Opaque(path)]))
}

pub fn to_path<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let path = ev.coerce_to_path(args[0])?;
    Ok(ev.string(&path))
}

pub fn store_path<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    if ev.settings.pure {
        return eval_err("'builtins.storePath' is not allowed in pure evaluation mode");
    }
    let path = ev.coerce_to_path(args[0])?;
    let prefix = format!("{}/", crate::store::STORE_DIR);
    let Some(rest) = path.strip_prefix(&prefix) else {
        return eval_err(format!("path '{path}' is not in the Nix store"));
    };
    let base = &path[..prefix.len() + rest.find('/').unwrap_or(rest.len())];
    let base: &'a str = ev.bump.alloc_str(base);
    Ok(ev.str_val(path.as_bytes(), &[Ctx::Opaque(base)]))
}

/// Shared by `builtins.path` and `filterSource`.
fn add_path<'a>(
    ev: &Eval<'a>,
    path: &str,
    name: Option<String>,
    filter: Option<Value<'a>>,
) -> R<'a> {
    let name = match name {
        Some(n) => n,
        None => String::from_utf8_lossy(crate::path::base_name_of(path.as_bytes())).into_owned(),
    };
    let io = &ev.ctx.io;
    let sp = match filter {
        None => io
            .add_filtered_to_store(path, &name, None)
            .map_err(io_err)?,
        Some(f) => {
            let err: Cell<Option<Box<crate::value::EvalError>>> = Cell::new(None);
            let keep = |p: &str, ty: crate::io::FileType| -> anyhow::Result<bool> {
                let r = (|| {
                    let g = ev.call(f, ev.string(p))?;
                    ev.force_bool(ev.call(g, ev.string(ty.name()))?)
                })();
                match r {
                    Ok(b) => Ok(b),
                    Err(e) => {
                        err.set(Some(e));
                        anyhow::bail!("filter failed")
                    }
                }
            };
            match io.add_filtered_to_store(path, &name, Some(&keep)) {
                Ok(sp) => sp,
                Err(e) => return Err(err.take().unwrap_or_else(|| io_err(e))),
            }
        }
    };
    let sp: &'a str = ev.bump.alloc_str(&sp);
    Ok(ev.str_val(sp.as_bytes(), &[Ctx::Opaque(sp)]))
}

pub fn path<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let set = ev.force_attrs(args[0])?;
    let get = |n: &str| set.get(ev.sym(n));
    let Some(p) = get("path") else {
        return eval_err(
            "missing required 'path' attribute in the first argument to builtins.path",
        );
    };
    let path = ev.coerce_to_path(p)?;
    let name = match get("name") {
        Some(n) => Some(String::from_utf8_lossy(ev.force_str_no_ctx(n)?).into_owned()),
        None => None,
    };
    let filter = match get("filter") {
        Some(f) => Some(ev.force_function(f)?),
        None => None,
    };
    if let Some(r) = get("recursive")
        && !ev.force_bool(r)?
    {
        return eval_err("non-recursive builtins.path is not supported");
    }
    add_path(ev, &path, name, filter)
}

pub fn filter_source<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let filter = ev.force_function(args[0])?;
    let path = ev.coerce_to_path(args[1])?;
    add_path(ev, &path, None, Some(filter))
}

fn unsupported<'a>(name: &str) -> R<'a> {
    eval_err(format!(
        "'builtins.{name}' is not supported by this evaluator"
    ))
}

pub fn fetch_git<'a>(_ev: &Eval<'a>, _args: &[Value<'a>]) -> R<'a> {
    unsupported("fetchGit")
}

pub fn fetch_mercurial<'a>(_ev: &Eval<'a>, _args: &[Value<'a>]) -> R<'a> {
    unsupported("fetchMercurial")
}

pub fn fetch_tarball<'a>(_ev: &Eval<'a>, _args: &[Value<'a>]) -> R<'a> {
    unsupported("fetchTarball")
}

pub fn fetch_tree<'a>(_ev: &Eval<'a>, _args: &[Value<'a>]) -> R<'a> {
    unsupported("fetchTree")
}

pub fn fetchurl<'a>(_ev: &Eval<'a>, _args: &[Value<'a>]) -> R<'a> {
    unsupported("fetchurl")
}
