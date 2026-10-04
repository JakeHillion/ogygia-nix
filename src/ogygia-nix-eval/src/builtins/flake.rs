//! The flake builtins, which Nix provides but refuses to run while the
//! `flakes` experimental feature is disabled, as it is here.

use crate::eval::Eval;
use crate::value::R;
use crate::value::Value;
use crate::value::eval_err;

fn flakes_disabled<'a>() -> R<'a> {
    eval_err(
        "experimental Nix feature 'flakes' is disabled; add '--extra-experimental-features flakes' to enable it",
    )
}

pub fn get_flake<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    ev.force_str_no_ctx(args[0])?;
    flakes_disabled()
}

pub fn parse_flake_ref<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    ev.force_str_no_ctx(args[0])?;
    flakes_disabled()
}

pub fn flake_ref_to_string<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let set = ev.force_attrs(args[0])?;
    for e in set.entries {
        match ev.force(e.value)? {
            Value::Str(_) | Value::Int(_) | Value::Bool(_) => {}
            v => {
                return eval_err(format!(
                    "flake reference attribute sets may only contain integers, Booleans, and strings, but attribute '{}' is {}",
                    ev.ctx.name(e.name),
                    v.show_type()
                ));
            }
        }
    }
    match set.get(ev.sym("type")).map(|v| ev.force(v)).transpose()? {
        None => eval_err("'type' attribute to specify input scheme is required but not provided"),
        Some(Value::Str(_)) => flakes_disabled(),
        Some(_) => eval_err("input attribute 'type' is not a string"),
    }
}
