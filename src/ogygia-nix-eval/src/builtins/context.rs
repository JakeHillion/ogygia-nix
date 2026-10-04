//! String context builtins.

use std::collections::BTreeMap;

use crate::eval::Eval;
use crate::value::Ctx;
use crate::value::Entry;
use crate::value::R;
use crate::value::Value;
use crate::value::eval_err;

pub fn has_context<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let s = ev.force_str(args[0])?;
    Ok(Value::Bool(!s.ctx.is_empty()))
}

pub fn unsafe_discard_string_context<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let (s, _) = ev.coerce_to_string(args[0], crate::eval::Coerce::INTERP)?;
    Ok(ev.str_val(&s, &[]))
}

pub fn unsafe_discard_output_dependency<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let (s, ctx) = ev.coerce_to_string(args[0], crate::eval::Coerce::INTERP)?;
    let ctx = ctx
        .into_iter()
        .map(|c| match c {
            Ctx::DrvDeep(p) => Ctx::Opaque(p),
            c => c,
        })
        .collect();
    Ok(ev.str_with_ctx(&s, ctx))
}

pub fn add_drv_output_dependencies<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let (s, ctx) = ev.coerce_to_string(args[0], crate::eval::Coerce::INTERP)?;
    let [Ctx::Opaque(p)] = ctx.as_slice() else {
        return eval_err(
            "context of string passed to 'addDrvOutputDependencies' must refer to exactly one derivation",
        );
    };
    if !p.ends_with(".drv") {
        return eval_err(format!(
            "path '{p}' is not a derivation, so 'addDrvOutputDependencies' cannot be applied"
        ));
    }
    Ok(ev.str_with_ctx(&s, vec![Ctx::DrvDeep(p)]))
}

#[derive(Default)]
struct Info<'a> {
    path: bool,
    all_outputs: bool,
    outputs: Vec<&'a str>,
}

pub fn get_context<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let s = ev.force_str(args[0])?;
    let mut infos: BTreeMap<&'a str, Info<'a>> = BTreeMap::new();
    for c in s.ctx {
        match *c {
            Ctx::Opaque(p) => infos.entry(p).or_default().path = true,
            Ctx::DrvDeep(p) => infos.entry(p).or_default().all_outputs = true,
            Ctx::Built { drv, output } => infos.entry(drv).or_default().outputs.push(output),
        }
    }
    let entries = infos
        .into_iter()
        .map(|(p, info)| {
            let mut fields = Vec::new();
            if info.path {
                fields.push(ev.entry("path", Value::Bool(true)));
            }
            if info.all_outputs {
                fields.push(ev.entry("allOutputs", Value::Bool(true)));
            }
            if !info.outputs.is_empty() {
                let mut outs = info.outputs;
                outs.sort();
                let outs: Vec<Value<'a>> = outs.into_iter().map(|o| ev.string(o)).collect();
                fields.push(ev.entry("outputs", ev.list(&outs)));
            }
            Entry {
                name: ev.sym(p),
                value: ev.attrs(fields),
                pos: None,
            }
        })
        .collect();
    Ok(ev.attrs(entries))
}

pub fn append_context<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let s = ev.force_str(args[0])?;
    let set = ev.force_attrs(args[1])?;
    let mut ctx = s.ctx.to_vec();
    let syms = &ev.ctx.syms;
    for e in set.entries {
        let key = ev.name(e.name);
        let Some(path) = crate::store::parse_store_path(key) else {
            return eval_err(format!("context key '{key}' is not a store path"));
        };
        let path: &'a str = ev.bump.alloc_str(&path);
        let info = ev.force_attrs(e.value)?;
        if let Some(p) = info.get(syms.path)
            && ev.force_bool(p)?
        {
            ctx.push(Ctx::Opaque(path));
        }
        if let Some(a) = info.get(syms.all_outputs)
            && ev.force_bool(a)?
        {
            ctx.push(Ctx::DrvDeep(path));
        }
        if let Some(outs) = info.get(syms.outputs) {
            for o in ev.force_list(outs)? {
                let o = ev.force_str_no_ctx(*o)?;
                let output: &'a str = ev.bump.alloc_str(&String::from_utf8_lossy(o));
                ctx.push(Ctx::Built { drv: path, output });
            }
        }
    }
    Ok(ev.str_with_ctx(s.s, ctx))
}
