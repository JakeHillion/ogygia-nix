//! The `builtins` set and the global scope.

mod attrs;
mod context;
mod derivation;
mod flake;
mod fs;
mod grisu2;
mod json;
mod lists;
mod misc;
pub(crate) mod regex;
mod strings;
mod url;
mod xml;

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::HashMap;

use crate::eval::Eval;
use crate::store::STORE_DIR;
use crate::symbol::Sym;
use crate::value::PrimOp;
use crate::value::PrimOpFn;
use crate::value::PrimOpFn::F1;
use crate::value::PrimOpFn::F2;
use crate::value::PrimOpFn::F3;
use crate::value::Thunk;
use crate::value::ThunkState;
use crate::value::Value;

/// The Nix version this evaluator reports, matching the Nix its equivalence
/// tests run against when built by the flake.
pub const NIX_VERSION: &str = match option_env!("OGYGIA_NIX_EVAL_NIX_VERSION") {
    Some(v) => v,
    None => "2.36.0pre",
};

struct Builtin {
    name: &'static str,
    /// Also in the global scope without the `__` prefix.
    global: bool,
    /// Left out in pure evaluation mode.
    impure: bool,
    kind: Kind,
}

enum Kind {
    PrimOp(PrimOp),
    /// A value built per session, if there is one.
    Const(for<'a> fn(&Eval<'a>) -> Option<Value<'a>>),
    /// The `builtins` set itself.
    Builtins,
}

impl Builtin {
    const fn global(self) -> Builtin {
        Builtin {
            global: true,
            ..self
        }
    }

    const fn impure(self) -> Builtin {
        Builtin {
            impure: true,
            ..self
        }
    }

    fn global_name(&self) -> Cow<'static, str> {
        if self.global {
            Cow::Borrowed(self.name)
        } else {
            Cow::Owned(format!("__{}", self.name))
        }
    }
}

const fn builtin(name: &'static str, kind: Kind) -> Builtin {
    Builtin {
        name,
        global: false,
        impure: false,
        kind,
    }
}

const fn prim(name: &'static str, f: PrimOpFn) -> Builtin {
    builtin(name, Kind::PrimOp(PrimOp { name, f }))
}

const fn constant(name: &'static str, f: for<'a> fn(&Eval<'a>) -> Option<Value<'a>>) -> Builtin {
    builtin(name, Kind::Const(f))
}

static BUILTINS: &[Builtin] = &[
    prim("abort", F1(misc::abort)).global(),
    prim("add", F2(misc::add)),
    prim(
        "addDrvOutputDependencies",
        F1(context::add_drv_output_dependencies),
    ),
    prim("addErrorContext", F2(misc::add_error_context)),
    prim("all", F2(lists::all)),
    prim("any", F2(lists::any)),
    prim("appendContext", F2(context::append_context)),
    prim("attrNames", F1(attrs::attr_names)),
    prim("attrValues", F1(attrs::attr_values)),
    prim("baseNameOf", F1(strings::base_name_of)).global(),
    prim("bitAnd", F2(misc::bit_and)),
    prim("bitOr", F2(misc::bit_or)),
    prim("bitXor", F2(misc::bit_xor)),
    prim("break", F1(misc::break_)).global(),
    builtin("builtins", Kind::Builtins).global(),
    prim("catAttrs", F2(attrs::cat_attrs)),
    prim("ceil", F1(misc::ceil)),
    prim("compareVersions", F2(strings::compare_versions)),
    prim("concatLists", F1(lists::concat_lists)),
    prim("concatMap", F2(lists::concat_map)),
    prim("concatStringsSep", F2(strings::concat_strings_sep)),
    prim("convertHash", F1(strings::convert_hash)),
    constant("currentSystem", |ev| {
        ev.settings.current_system.as_deref().map(|s| ev.string(s))
    })
    .impure(),
    constant("currentTime", |_| Some(Value::Int(current_time()))).impure(),
    prim("deepSeq", F2(misc::deep_seq)),
    constant("derivation", |ev| {
        Some(ev.lazy_native(derivation::derivation_lambda))
    })
    .global(),
    prim("derivationStrict", F1(derivation::derivation_strict)).global(),
    prim("dirOf", F1(strings::dir_of)).global(),
    prim("div", F2(misc::div)),
    prim("elem", F2(lists::elem)),
    prim("elemAt", F2(lists::elem_at)),
    constant("false", |_| Some(Value::Bool(false))).global(),
    prim("fetchGit", F1(fs::fetch_git)).global(),
    prim("fetchMercurial", F1(fs::fetch_mercurial)).global(),
    prim("fetchTarball", F1(fs::fetch_tarball)).global(),
    prim("fetchurl", F1(fs::fetchurl)),
    prim("filter", F2(lists::filter)),
    prim("filterSource", F2(fs::filter_source)),
    prim("findFile", F2(fs::find_file)),
    prim("flakeRefToString", F1(flake::flake_ref_to_string)),
    prim("floor", F1(misc::floor)),
    prim("foldl'", F3(lists::foldl)),
    prim("fromJSON", F1(json::from_json)),
    prim("fromTOML", F1(json::from_toml)).global(),
    prim("functionArgs", F1(misc::function_args)),
    prim("genList", F2(lists::gen_list)),
    prim("genericClosure", F1(lists::generic_closure)),
    prim("getAttr", F2(attrs::get_attr)),
    prim("getContext", F1(context::get_context)),
    prim("getEnv", F1(misc::get_env)),
    prim("getFlake", F1(flake::get_flake)),
    prim("groupBy", F2(lists::group_by)),
    prim("hasAttr", F2(attrs::has_attr)),
    prim("hasContext", F1(context::has_context)),
    prim("hashFile", F2(fs::hash_file)),
    prim("hashString", F2(strings::hash_string)),
    prim("head", F1(lists::head)),
    prim("import", F1(fs::import)).global(),
    prim("intersectAttrs", F2(attrs::intersect_attrs)),
    prim("isAttrs", F1(misc::is_attrs)),
    prim("isBool", F1(misc::is_bool)),
    prim("isFloat", F1(misc::is_float)),
    prim("isFunction", F1(misc::is_function)),
    prim("isInt", F1(misc::is_int)),
    prim("isList", F1(misc::is_list)),
    prim("isNull", F1(misc::is_null)).global(),
    prim("isPath", F1(misc::is_path)),
    prim("isString", F1(misc::is_string)),
    constant("langVersion", |_| Some(Value::Int(6))),
    prim("length", F1(lists::length)),
    prim("lessThan", F2(misc::less_than)),
    prim("listToAttrs", F1(attrs::list_to_attrs)),
    prim("map", F2(lists::map)).global(),
    prim("mapAttrs", F2(attrs::map_attrs)),
    prim("match", F2(regex::match_)),
    prim("mul", F2(misc::mul)),
    constant("nixPath", |ev| Some(nix_path(ev))),
    constant("nixVersion", |ev| Some(ev.string(NIX_VERSION))),
    constant("null", |_| Some(Value::Null)).global(),
    prim("parseDrvName", F1(strings::parse_drv_name)),
    prim("parseFlakeRef", F1(flake::parse_flake_ref)),
    prim("partition", F2(lists::partition)),
    prim("path", F1(fs::path)),
    prim("pathExists", F1(fs::path_exists)),
    prim("placeholder", F1(derivation::placeholder)).global(),
    prim("readDir", F1(fs::read_dir)),
    prim("readFile", F1(fs::read_file)),
    prim("readFileType", F1(fs::read_file_type)),
    prim("removeAttrs", F2(attrs::remove_attrs)).global(),
    prim("replaceStrings", F3(strings::replace_strings)),
    prim("scopedImport", F2(fs::scoped_import)).global(),
    prim("seq", F2(misc::seq)),
    prim("sort", F2(lists::sort)),
    prim("split", F2(regex::split)),
    prim("splitVersion", F1(strings::split_version)),
    constant("storeDir", |ev| Some(ev.string(STORE_DIR))),
    prim("storePath", F1(fs::store_path)),
    prim("stringLength", F1(strings::string_length)),
    prim("sub", F2(misc::sub)),
    prim("substring", F3(strings::substring)),
    prim("tail", F1(lists::tail)),
    prim("throw", F1(misc::throw)).global(),
    prim("toFile", F2(fs::to_file)),
    prim("toJSON", F1(json::to_json)),
    prim("toPath", F1(fs::to_path)),
    prim("toString", F1(strings::to_string)).global(),
    prim("toXML", F1(xml::to_xml)),
    prim("trace", F2(misc::trace)),
    prim("traceVerbose", F2(misc::trace_verbose)),
    constant("true", |_| Some(Value::Bool(true))).global(),
    prim("tryEval", F1(misc::try_eval)),
    prim("typeOf", F1(misc::type_of)),
    prim(
        "unsafeDiscardOutputDependency",
        F1(context::unsafe_discard_output_dependency),
    ),
    prim(
        "unsafeDiscardStringContext",
        F1(context::unsafe_discard_string_context),
    ),
    prim("unsafeGetAttrPos", F2(attrs::unsafe_get_attr_pos)),
    prim("warn", F2(misc::warn)),
    prim("zipAttrsWith", F2(attrs::zip_attrs_with)),
];

fn current_time() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn nix_path<'a>(ev: &Eval<'a>) -> Value<'a> {
    let entries: Vec<Value<'a>> = ev
        .settings
        .nix_path
        .iter()
        .map(|(prefix, path)| {
            ev.attrs(vec![
                ev.entry("prefix", ev.string(prefix)),
                ev.entry("path", ev.string(path)),
            ])
        })
        .collect();
    ev.list(&entries)
}

/// Every name in the global scope.
pub fn global_names(pure: bool) -> impl Iterator<Item = Cow<'static, str>> {
    BUILTINS
        .iter()
        .filter(move |b| !(pure && b.impure))
        .map(Builtin::global_name)
}

/// Build the global scope for a session.
pub fn make_globals<'a>(ev: &Eval<'a>) -> HashMap<Sym, Value<'a>> {
    // `builtins.builtins` is the set itself.
    let self_ref: &'a Thunk<'a> = ev.bump.alloc(Thunk(Cell::new(ThunkState::Blackhole)));
    let mut entries = Vec::new();
    let mut globals = HashMap::new();
    for b in BUILTINS.iter().filter(|b| !(ev.settings.pure && b.impure)) {
        let value = match &b.kind {
            Kind::PrimOp(op) => Value::PrimOp(op),
            Kind::Const(f) => match f(ev) {
                Some(v) => v,
                None => continue,
            },
            Kind::Builtins => Value::Thunk(self_ref),
        };
        entries.push(ev.entry(b.name, value));
        globals.insert(ev.sym(&b.global_name()), value);
    }
    self_ref.0.set(ThunkState::Done(ev.attrs(entries)));
    globals
}
