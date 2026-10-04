//! List builtins.

use std::collections::HashMap;

use crate::eval::Eval;
use crate::symbol::Sym;
use crate::value::Entry;
use crate::value::R;
use crate::value::Value;
use crate::value::eval_err;

pub fn head<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    match ev.force_list(args[0])?.first() {
        Some(v) => ev.force(*v),
        None => eval_err("'builtins.head' called on an empty list"),
    }
}

pub fn tail<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    match ev.force_list(args[0])? {
        [] => eval_err("'builtins.tail' called on an empty list"),
        [_, rest @ ..] => Ok(ev.list(rest)),
    }
}

pub fn length<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    Ok(Value::Int(ev.force_list(args[0])?.len() as i64))
}

pub fn elem_at<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let list = ev.force_list(args[0])?;
    let n = ev.force_int(args[1])?;
    match usize::try_from(n).ok().and_then(|i| list.get(i)) {
        Some(v) => ev.force(*v),
        None => eval_err(format!(
            "list index {n} is out of bounds for list of length {}",
            list.len()
        )),
    }
}

pub fn elem<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    for item in ev.force_list(args[1])? {
        if ev.eq(args[0], *item)? {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

pub fn map<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let list = ev.force_list(args[1])?;
    let out: Vec<Value<'a>> = list.iter().map(|x| ev.lazy_app(args[0], *x)).collect();
    Ok(ev.list(&out))
}

pub fn filter<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let f = ev.force_function(args[0])?;
    let list = ev.force_list(args[1])?;
    let mut out = Vec::with_capacity(list.len());
    for item in list {
        if ev.force_bool(ev.call(f, *item)?)? {
            out.push(*item);
        }
    }
    if out.len() == list.len() {
        return ev.force(args[1]);
    }
    Ok(ev.list(&out))
}

pub fn all<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let f = ev.force_function(args[0])?;
    for item in ev.force_list(args[1])? {
        if !ev.force_bool(ev.call(f, *item)?)? {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}

pub fn any<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let f = ev.force_function(args[0])?;
    for item in ev.force_list(args[1])? {
        if ev.force_bool(ev.call(f, *item)?)? {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

pub fn concat_lists<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let mut out = Vec::new();
    for l in ev.force_list(args[0])? {
        out.extend_from_slice(ev.force_list(*l)?);
    }
    Ok(ev.list(&out))
}

pub fn concat_map<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let f = ev.force_function(args[0])?;
    let mut out = Vec::new();
    for item in ev.force_list(args[1])? {
        out.extend_from_slice(ev.force_list(ev.call(f, *item)?)?);
    }
    Ok(ev.list(&out))
}

pub fn foldl<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let f = args[0];
    let mut acc = args[1];
    let list = ev.force_list(args[2])?;
    if list.is_empty() {
        return ev.force(acc);
    }
    let f = ev.force_function(f)?;
    for item in list {
        let g = ev.call(f, acc)?;
        acc = ev.call(g, *item)?;
    }
    Ok(acc)
}

pub fn gen_list<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let n = ev.force_int(args[1])?;
    if n < 0 {
        return eval_err(format!("cannot create list of size {n}"));
    }
    let f = ev.force_function(args[0])?;
    let out: Vec<Value<'a>> = (0..n).map(|i| ev.lazy_app(f, Value::Int(i))).collect();
    Ok(ev.list(&out))
}

pub fn partition<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let f = ev.force_function(args[0])?;
    let mut right = Vec::new();
    let mut wrong = Vec::new();
    for item in ev.force_list(args[1])? {
        if ev.force_bool(ev.call(f, *item)?)? {
            right.push(*item);
        } else {
            wrong.push(*item);
        }
    }
    Ok(ev.attrs(vec![
        ev.entry("right", ev.list(&right)),
        ev.entry("wrong", ev.list(&wrong)),
    ]))
}

pub fn group_by<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let f = ev.force_function(args[0])?;
    let mut groups: HashMap<Sym, Vec<Value<'a>>> = HashMap::new();
    for item in ev.force_list(args[1])? {
        let key = ev.force_str_no_ctx(ev.call(f, *item)?)?;
        let key = ev.ctx.interner.intern_bytes(key);
        groups.entry(key).or_default().push(*item);
    }
    let out = groups
        .into_iter()
        .map(|(name, items)| Entry {
            name,
            value: ev.list(&items),
            pos: None,
        })
        .collect();
    Ok(ev.attrs(out))
}

/// A stable merge sort whose comparator may fail.
pub fn sort<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let cmp = ev.force_function(args[0])?;
    let mut items = ev.force_list(args[1])?.to_vec();
    if items.len() <= 1 {
        return Ok(ev.list(&items));
    }
    let less = |a: Value<'a>, b: Value<'a>| -> R<'a, bool> {
        let g = ev.call(cmp, a)?;
        ev.force_bool(ev.call(g, b)?)
    };
    let mut buf = items.clone();
    merge_sort(&mut items, &mut buf, &less)?;
    Ok(ev.list(&items))
}

fn merge_sort<'a>(
    v: &mut [Value<'a>],
    buf: &mut [Value<'a>],
    less: &dyn Fn(Value<'a>, Value<'a>) -> R<'a, bool>,
) -> R<'a, ()> {
    let n = v.len();
    if n <= 1 {
        return Ok(());
    }
    if n <= 16 {
        // Insertion sort: stable, and few comparisons for short runs.
        for i in 1..n {
            let mut j = i;
            while j > 0 && less(v[j], v[j - 1])? {
                v.swap(j, j - 1);
                j -= 1;
            }
        }
        return Ok(());
    }
    let mid = n / 2;
    {
        let (l, r) = v.split_at_mut(mid);
        let (bl, br) = buf.split_at_mut(mid);
        merge_sort(l, bl, less)?;
        merge_sort(r, br, less)?;
    }
    let (mut i, mut j, mut k) = (0, mid, 0);
    while i < mid && j < n {
        if less(v[j], v[i])? {
            buf[k] = v[j];
            j += 1;
        } else {
            buf[k] = v[i];
            i += 1;
        }
        k += 1;
    }
    while i < mid {
        buf[k] = v[i];
        i += 1;
        k += 1;
    }
    while j < n {
        buf[k] = v[j];
        j += 1;
        k += 1;
    }
    v.copy_from_slice(&buf[..n]);
    Ok(())
}

pub fn generic_closure<'a>(ev: &Eval<'a>, args: &[Value<'a>]) -> R<'a> {
    let s = &ev.ctx.syms;
    let set = ev.force_attrs(args[0])?;
    let Some(start) = set.get(s.start_set) else {
        return eval_err("attribute 'startSet' required");
    };
    let start = ev.force_list(start)?;
    if start.is_empty() {
        return Ok(ev.list(&[]));
    }
    let Some(op) = set.get(s.operator) else {
        return eval_err("attribute 'operator' required");
    };
    let mut queue: std::collections::VecDeque<Value<'a>> = start.iter().copied().collect();
    let mut seen: Vec<Value<'a>> = Vec::new();
    let mut out = Vec::new();
    while let Some(item) = queue.pop_front() {
        let attrs = ev.force_attrs(item)?;
        let Some(key) = attrs.get(s.key) else {
            return eval_err("attribute 'key' required");
        };
        let key = ev.force(key)?;
        let mut dup = false;
        for k in &seen {
            if ev.eq(*k, key)? {
                dup = true;
                break;
            }
        }
        if dup {
            continue;
        }
        seen.push(key);
        out.push(item);
        let next = ev.call(op, item)?;
        queue.extend(ev.force_list(next)?.iter().copied());
    }
    Ok(ev.list(&out))
}
