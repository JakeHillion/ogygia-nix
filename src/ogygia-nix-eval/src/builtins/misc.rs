//! Arithmetic, type predicates, control flow and other small builtins.

use crate::eval::Coerce;
use crate::eval::Eval;
use crate::ir::BinOp;
use crate::ir::Param;
use crate::value::Entry;
use crate::value::ErrorKind;
use crate::value::R;
use crate::value::Value;
use crate::value::error;
use crate::value::eval_err;

fn arith<'a>(ev: &Eval<'a>, op: BinOp, args: &[Value<'a>]) -> R<'a> {
    let a = ev.force(args[0])?;
    let b = ev.force(args[1])?;
    match (a, b) {
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
            ev.arith_nopos(op, a, b)
        }
        (Value::Int(_) | Value::Float(_), other) | (other, _) => ev.type_error("an integer", other),
    }
}

pub fn add<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    arith(ev, BinOp::Add, args)
}

pub fn sub<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    arith(ev, BinOp::Sub, args)
}

pub fn mul<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    arith(ev, BinOp::Mul, args)
}

pub fn div<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    arith(ev, BinOp::Div, args)
}

fn bits<'a>(ev: &Eval<'a>, args: &[Value<'a>], f: fn(i64, i64) -> i64) -> R<'a> {
    let a = ev.force_int(args[0])?;
    let b = ev.force_int(args[1])?;
    Ok(Value::Int(f(a, b)))
}

pub fn bit_and<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    bits(ev, args, |a, b| a & b)
}

pub fn bit_or<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    bits(ev, args, |a, b| a | b)
}

pub fn bit_xor<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    bits(ev, args, |a, b| a ^ b)
}

fn round<'a>(ev: &Eval<'a>, v: Value<'a>, f: fn(f64) -> f64, name: &str) -> R<'a> {
    match ev.force(v)? {
        Value::Int(i) => Ok(Value::Int(i)),
        Value::Float(x) => {
            let r = f(x);
            if !(-9.223_372_036_854_776e18..9.223_372_036_854_776e18).contains(&r) {
                return eval_err(format!(
                    "{name} of {} is out of the range of an integer",
                    crate::print::float_g(x)
                ));
            }
            Ok(Value::Int(r as i64))
        }
        other => ev.type_error("a float", other),
    }
}

pub fn ceil<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    round(ev, args[0], f64::ceil, "ceil")
}

pub fn floor<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    round(ev, args[0], f64::floor, "floor")
}

pub fn less_than<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    Ok(Value::Bool(ev.less_than(args[0], args[1])?))
}

pub fn seq<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    ev.force(args[0])?;
    ev.force(args[1])
}

pub fn deep_seq<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    ev.deep_force(args[0])?;
    ev.force(args[1])
}

fn message<'a>(ev: &Eval<'a>, v: Value<'a>, c: Coerce) -> R<'a, String> {
    let (s, _) = ev.coerce_to_string(v, c)?;
    Ok(String::from_utf8_lossy(&s).into_owned())
}

pub fn throw<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let msg = message(ev, args[0], Coerce::INTERP)?;
    Err(error(ErrorKind::Throw, msg))
}

pub fn abort<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let msg = message(ev, args[0], Coerce::INTERP)?;
    Err(error(
        ErrorKind::Abort,
        format!("evaluation aborted with the following error message: '{msg}'"),
    ))
}

pub fn add_error_context<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    ev.force(args[1]).or_else(|mut e| {
        e.trace.push(message(ev, args[0], Coerce::PLAIN)?);
        Err(e)
    })
}

pub fn try_eval<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let (success, value) = match ev.force(args[0]) {
        Ok(v) => (true, v),
        Err(e) if matches!(e.kind, ErrorKind::Throw | ErrorKind::Assert) => {
            (false, Value::Bool(false))
        }
        Err(e) => return Err(e),
    };
    Ok(ev.attrs(vec![
        ev.entry("success", Value::Bool(success)),
        ev.entry("value", value),
    ]))
}

pub fn trace<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let v = ev.force(args[0])?;
    let text = match v {
        Value::Str(s) => s.as_str_lossy().into_owned(),
        other => {
            let p = crate::print::print_strict(ev, other)?;
            String::from_utf8_lossy(&p).into_owned()
        }
    };
    eprintln!("trace: {text}");
    ev.force(args[1])
}

pub fn trace_verbose<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    ev.force(args[1])
}

pub fn warn<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let msg = ev.force_str(args[0])?;
    eprintln!("evaluation warning: {}", msg.as_str_lossy());
    ev.force(args[1])
}

pub fn break_<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    ev.force(args[0])
}

pub fn type_of<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let v = ev.force(args[0])?;
    Ok(ev.string(v.type_of()))
}

fn is<'a>(ev: &Eval<'a>, v: Value<'a>, f: fn(&Value<'a>) -> bool) -> R<'a> {
    let v = ev.force(v)?;
    Ok(Value::Bool(f(&v)))
}

pub fn is_attrs<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    is(ev, args[0], |v| matches!(v, Value::Attrs(_)))
}

pub fn is_bool<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    is(ev, args[0], |v| matches!(v, Value::Bool(_)))
}

pub fn is_float<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    is(ev, args[0], |v| matches!(v, Value::Float(_)))
}

pub fn is_function<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    is(ev, args[0], |v| v.is_function())
}

pub fn is_int<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    is(ev, args[0], |v| matches!(v, Value::Int(_)))
}

pub fn is_list<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    is(ev, args[0], |v| matches!(v, Value::List(_)))
}

pub fn is_null<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    is(ev, args[0], |v| matches!(v, Value::Null))
}

pub fn is_path<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    is(ev, args[0], |v| matches!(v, Value::Path(_)))
}

pub fn is_string<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    is(ev, args[0], |v| matches!(v, Value::Str(_)))
}

pub fn function_args<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    match ev.force(args[0])? {
        Value::Lambda(c) => match &c.def.param {
            Param::Ident(_) => Ok(Value::Attrs(ev.empty_attrs)),
            Param::Pattern { formals, .. } => Ok(ev.attrs(
                formals
                    .iter()
                    .map(|f| Entry {
                        name: f.name,
                        value: Value::Bool(f.default.is_some()),
                        pos: Some(c.def.pos),
                    })
                    .collect(),
            )),
        },
        Value::PrimOp(_) | Value::PrimOpApp(_) => Ok(Value::Attrs(ev.empty_attrs)),
        other => ev.type_error("a function", other),
    }
}

pub fn get_env<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let name = ev.force_str_no_ctx(args[0])?;
    if ev.settings.pure {
        return Ok(ev.string(""));
    }
    let name = String::from_utf8_lossy(name);
    Ok(ev.string(&std::env::var(&*name).unwrap_or_default()))
}
