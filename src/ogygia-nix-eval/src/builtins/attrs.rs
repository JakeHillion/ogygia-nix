//! Attribute set builtins.

use std::collections::HashMap;

use crate::eval::Eval;
use crate::symbol::Sym;
use crate::value::Entry;
use crate::value::R;
use crate::value::Value;
use crate::value::eval_err;

fn sym_arg<'a>(ev: &Eval<'a>, v: Value<'a>) -> R<'a, Sym> {
    let s = ev.force_str_no_ctx(v)?;
    Ok(ev.ctx.interner.intern_bytes(s))
}

pub fn attr_names<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let set = ev.force_attrs(args[0])?;
    let names: Vec<Value<'a>> = set
        .sorted(ev.ctx)
        .iter()
        .map(|e| ev.string(ev.name(e.name)))
        .collect();
    Ok(ev.list(&names))
}

pub fn attr_values<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let set = ev.force_attrs(args[0])?;
    let values: Vec<Value<'a>> = set.sorted(ev.ctx).iter().map(|e| e.value).collect();
    Ok(ev.list(&values))
}

pub fn get_attr<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let name = sym_arg(ev, args[0])?;
    let set = ev.force_attrs(args[1])?;
    match set.get(name) {
        Some(v) => ev.force(v),
        None => eval_err(format!("attribute '{}' missing", ev.name(name))),
    }
}

pub fn has_attr<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let name = sym_arg(ev, args[0])?;
    let set = ev.force_attrs(args[1])?;
    Ok(Value::Bool(set.get(name).is_some()))
}

pub fn cat_attrs<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let name = sym_arg(ev, args[0])?;
    let list = ev.force_list(args[1])?;
    let mut out = Vec::new();
    for item in list {
        if let Some(v) = ev.force_attrs(*item)?.get(name) {
            out.push(v);
        }
    }
    Ok(ev.list(&out))
}

pub fn intersect_attrs<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let a = ev.force_attrs(args[0])?;
    let b = ev.force_attrs(args[1])?;
    let out: Vec<Entry<'a>> = b
        .entries
        .iter()
        .filter(|e| a.get(e.name).is_some())
        .copied()
        .collect();
    Ok(ev.attrs_sorted(&out))
}

pub fn remove_attrs<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let set = ev.force_attrs(args[0])?;
    let names = ev.force_list(args[1])?;
    let mut remove = Vec::with_capacity(names.len());
    for n in names {
        remove.push(sym_arg(ev, *n)?);
    }
    let out: Vec<Entry<'a>> = set
        .entries
        .iter()
        .filter(|e| !remove.contains(&e.name))
        .copied()
        .collect();
    if out.len() == set.len() {
        return Ok(Value::Attrs(set));
    }
    Ok(ev.attrs_sorted(&out))
}

pub fn list_to_attrs<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let list = ev.force_list(args[0])?;
    let s = &ev.ctx.syms;
    let mut seen: HashMap<Sym, ()> = HashMap::with_capacity(list.len());
    let mut out = Vec::with_capacity(list.len());
    for item in list {
        let set = ev.force_attrs(*item)?;
        let Some(name) = set.get(s.name) else {
            return eval_err("attribute 'name' missing in a call to 'listToAttrs'");
        };
        let name = sym_arg(ev, name)?;
        // The first definition of a name wins.
        if seen.insert(name, ()).is_some() {
            continue;
        }
        let Some(value) = set.get(s.value) else {
            return eval_err("attribute 'value' missing in a call to 'listToAttrs'");
        };
        out.push(Entry {
            name,
            value,
            pos: set.entry(s.value).and_then(|e| e.pos),
        });
    }
    out.sort_by_key(|e| e.name);
    Ok(ev.attrs_sorted(&out))
}

pub fn map_attrs<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let f = args[0];
    let set = ev.force_attrs(args[1])?;
    let out: Vec<Entry<'a>> = set
        .entries
        .iter()
        .map(|e| {
            let partial = ev.lazy_app(f, ev.string(ev.name(e.name)));
            Entry {
                name: e.name,
                value: ev.lazy_app(partial, e.value),
                pos: e.pos,
            }
        })
        .collect();
    Ok(ev.attrs_sorted(&out))
}

pub fn zip_attrs_with<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let f = ev.force_function(args[0])?;
    let list = ev.force_list(args[1])?;
    let mut groups: HashMap<Sym, Vec<Value<'a>>> = HashMap::new();
    let mut order = Vec::new();
    for item in list {
        for e in ev.force_attrs(*item)?.entries {
            groups
                .entry(e.name)
                .or_insert_with(|| {
                    order.push(e.name);
                    Vec::new()
                })
                .push(e.value);
        }
    }
    let out: Vec<Entry<'a>> = order
        .into_iter()
        .map(|name| {
            let values = ev.list(&groups[&name]);
            let partial = ev.lazy_app(f, ev.string(ev.name(name)));
            Entry {
                name,
                value: ev.lazy_app(partial, values),
                pos: None,
            }
        })
        .collect();
    Ok(ev.attrs(out))
}

pub fn unsafe_get_attr_pos<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let name = sym_arg(ev, args[0])?;
    let set = ev.force_attrs(args[1])?;
    match set.entry(name).and_then(|e| e.pos) {
        Some(pos) => Ok(ev.pos_value(pos)),
        None => Ok(Value::Null),
    }
}
