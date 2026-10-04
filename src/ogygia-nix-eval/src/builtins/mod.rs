//! The `builtins` set and the global scope.

mod attrs;
mod context;
mod derivation;
mod fs;
mod json;
mod lists;
mod misc;
pub(crate) mod strings;
mod xml;

use std::cell::Cell;
use std::collections::HashMap;

use crate::eval::Eval;
use crate::store::STORE_DIR;
use crate::symbol::Sym;
use crate::value::Entry;
use crate::value::PrimOp;
use crate::value::Thunk;
use crate::value::ThunkState;
use crate::value::Value;

/// The Nix version this evaluator reports, matching the Nix its equivalence
/// tests run against.
pub const NIX_VERSION: &str = "2.34.8";

/// Builtins that are also in the global scope without the `__` prefix.
const GLOBAL: &[&str] = &[
    "abort",
    "baseNameOf",
    "break",
    "builtins",
    "derivation",
    "derivationStrict",
    "dirOf",
    "false",
    "fetchGit",
    "fetchMercurial",
    "fetchTarball",
    "fetchTree",
    "fromTOML",
    "import",
    "isNull",
    "map",
    "null",
    "placeholder",
    "removeAttrs",
    "scopedImport",
    "throw",
    "toString",
    "true",
];

/// Builtins whose values are not primops.
const CONSTANTS: &[&str] = &[
    "builtins",
    "currentSystem",
    "currentTime",
    "derivation",
    "false",
    "langVersion",
    "nixPath",
    "nixVersion",
    "null",
    "storeDir",
    "true",
];

macro_rules! primops {
    ($($name:literal => $arity:literal, $f:path;)*) => {
        pub static PRIMOPS: &[PrimOp] = &[
            $(PrimOp { name: $name, arity: $arity, f: $f },)*
        ];
    };
}

primops! {
    "abort" => 1, misc::abort;
    "add" => 2, misc::add;
    "addDrvOutputDependencies" => 1, context::add_drv_output_dependencies;
    "addErrorContext" => 2, misc::add_error_context;
    "all" => 2, lists::all;
    "any" => 2, lists::any;
    "appendContext" => 2, context::append_context;
    "attrNames" => 1, attrs::attr_names;
    "attrValues" => 1, attrs::attr_values;
    "baseNameOf" => 1, strings::base_name_of;
    "bitAnd" => 2, misc::bit_and;
    "bitOr" => 2, misc::bit_or;
    "bitXor" => 2, misc::bit_xor;
    "break" => 1, misc::break_;
    "catAttrs" => 2, attrs::cat_attrs;
    "ceil" => 1, misc::ceil;
    "compareVersions" => 2, strings::compare_versions;
    "concatLists" => 1, lists::concat_lists;
    "concatMap" => 2, lists::concat_map;
    "concatStringsSep" => 2, strings::concat_strings_sep;
    "convertHash" => 1, strings::convert_hash;
    "deepSeq" => 2, misc::deep_seq;
    "derivationStrict" => 1, derivation::derivation_strict;
    "dirOf" => 1, strings::dir_of;
    "div" => 2, misc::div;
    "elem" => 2, lists::elem;
    "elemAt" => 2, lists::elem_at;
    "fetchGit" => 1, fs::fetch_git;
    "fetchMercurial" => 1, fs::fetch_mercurial;
    "fetchTarball" => 1, fs::fetch_tarball;
    "fetchTree" => 1, fs::fetch_tree;
    "fetchurl" => 1, fs::fetchurl;
    "filter" => 2, lists::filter;
    "filterSource" => 2, fs::filter_source;
    "findFile" => 2, fs::find_file;
    "floor" => 1, misc::floor;
    "foldl'" => 3, lists::foldl;
    "fromJSON" => 1, json::from_json;
    "fromTOML" => 1, json::from_toml;
    "functionArgs" => 1, misc::function_args;
    "genList" => 2, lists::gen_list;
    "genericClosure" => 1, lists::generic_closure;
    "getAttr" => 2, attrs::get_attr;
    "getContext" => 1, context::get_context;
    "getEnv" => 1, misc::get_env;
    "groupBy" => 2, lists::group_by;
    "hasAttr" => 2, attrs::has_attr;
    "hasContext" => 1, context::has_context;
    "hashFile" => 2, fs::hash_file;
    "hashString" => 2, strings::hash_string;
    "head" => 1, lists::head;
    "import" => 1, fs::import;
    "intersectAttrs" => 2, attrs::intersect_attrs;
    "isAttrs" => 1, misc::is_attrs;
    "isBool" => 1, misc::is_bool;
    "isFloat" => 1, misc::is_float;
    "isFunction" => 1, misc::is_function;
    "isInt" => 1, misc::is_int;
    "isList" => 1, misc::is_list;
    "isNull" => 1, misc::is_null;
    "isPath" => 1, misc::is_path;
    "isString" => 1, misc::is_string;
    "length" => 1, lists::length;
    "lessThan" => 2, misc::less_than;
    "listToAttrs" => 1, attrs::list_to_attrs;
    "map" => 2, lists::map;
    "mapAttrs" => 2, attrs::map_attrs;
    "match" => 2, strings::match_;
    "mul" => 2, misc::mul;
    "parseDrvName" => 1, strings::parse_drv_name;
    "partition" => 2, lists::partition;
    "path" => 1, fs::path;
    "pathExists" => 1, fs::path_exists;
    "placeholder" => 1, derivation::placeholder;
    "readDir" => 1, fs::read_dir;
    "readFile" => 1, fs::read_file;
    "readFileType" => 1, fs::read_file_type;
    "removeAttrs" => 2, attrs::remove_attrs;
    "replaceStrings" => 3, strings::replace_strings;
    "scopedImport" => 2, fs::scoped_import;
    "seq" => 2, misc::seq;
    "sort" => 2, lists::sort;
    "split" => 2, strings::split;
    "splitVersion" => 1, strings::split_version;
    "storePath" => 1, fs::store_path;
    "stringLength" => 1, strings::string_length;
    "sub" => 2, misc::sub;
    "substring" => 3, strings::substring;
    "tail" => 1, lists::tail;
    "throw" => 1, misc::throw;
    "toFile" => 2, fs::to_file;
    "toJSON" => 1, json::to_json;
    "toPath" => 1, fs::to_path;
    "toString" => 1, strings::to_string;
    "toXML" => 1, xml::to_xml;
    "trace" => 2, misc::trace;
    "traceVerbose" => 2, misc::trace_verbose;
    "tryEval" => 1, misc::try_eval;
    "typeOf" => 1, misc::type_of;
    "unsafeDiscardOutputDependency" => 1, context::unsafe_discard_output_dependency;
    "unsafeDiscardStringContext" => 1, context::unsafe_discard_string_context;
    "unsafeGetAttrPos" => 2, attrs::unsafe_get_attr_pos;
    "warn" => 2, misc::warn;
    "zipAttrsWith" => 2, attrs::zip_attrs_with;
}

/// Every name in the global scope.
pub fn global_names() -> impl Iterator<Item = String> {
    let all = PRIMOPS
        .iter()
        .map(|p| p.name)
        .chain(CONSTANTS.iter().copied());
    all.flat_map(|n| {
        if GLOBAL.contains(&n) {
            vec![n.to_owned()]
        } else {
            vec![format!("__{n}")]
        }
    })
}

/// Build the global scope for a session.
pub fn make_globals<'a>(ev: &Eval<'a>) -> HashMap<Sym, Value<'a>> {
    let mut entries: Vec<Entry<'a>> = PRIMOPS
        .iter()
        .map(|p| ev.entry(p.name, Value::PrimOp(p)))
        .collect();

    // `builtins.builtins` is the set itself.
    let self_ref: &'a Thunk<'a> = ev.bump.alloc(Thunk(Cell::new(ThunkState::Blackhole)));
    entries.push(ev.entry("builtins", Value::Thunk(self_ref)));
    entries.push(ev.entry("true", Value::Bool(true)));
    entries.push(ev.entry("false", Value::Bool(false)));
    entries.push(ev.entry("null", Value::Null));
    entries.push(ev.entry("nixVersion", ev.string(NIX_VERSION)));
    entries.push(ev.entry("langVersion", Value::Int(6)));
    entries.push(ev.entry("storeDir", ev.string(STORE_DIR)));
    if !ev.settings.pure {
        if let Some(system) = &ev.settings.current_system {
            entries.push(ev.entry("currentSystem", ev.string(system)));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        entries.push(ev.entry("currentTime", Value::Int(now)));
    }
    let nix_path: Vec<Value<'a>> = ev
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
    entries.push(ev.entry("nixPath", ev.list(&nix_path)));
    entries.push(ev.entry("derivation", ev.lazy_native(derivation::derivation_lambda)));

    let builtins = ev.attrs(entries);
    self_ref.0.set(ThunkState::Done(builtins));

    let Value::Attrs(set) = builtins else {
        unreachable!("builtins is a set")
    };
    let mut globals = HashMap::new();
    for e in set.entries {
        let name = ev.name(e.name);
        let global = if GLOBAL.contains(&name) {
            name.to_owned()
        } else {
            format!("__{name}")
        };
        globals.insert(ev.sym(&global), e.value);
    }
    globals
}
