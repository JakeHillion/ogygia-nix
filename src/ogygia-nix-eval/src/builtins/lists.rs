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
    let list = ev.force_list(args[1])?;
    if list.is_empty() {
        return ev.force(args[1]);
    }
    let f = ev.force_function(args[0])?;
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
    let f = ev.force_function(args[0])?;
    let mut acc = args[1];
    let list = ev.force_list(args[2])?;
    if list.is_empty() {
        return ev.force(acc);
    }
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
    let mut items = ev.force_list(args[1])?.to_vec();
    if items.is_empty() {
        return Ok(ev.list(&items));
    }
    let cmp = ev.force_function(args[0])?;
    for item in &mut items {
        *item = ev.force(*item)?;
    }
    if items.len() == 1 {
        return Ok(ev.list(&items));
    }
    let less = |a: Value<'a>, b: Value<'a>| -> R<'a, bool> {
        let g = ev.call(cmp, a)?;
        ev.force_bool(ev.call(g, b)?)
    };
    let len = items.len();
    PeekSort {
        buf: items.clone(),
        less: &less,
    }
    .sort_range(&mut items, 0, len, 0, len)?;
    Ok(ev.list(&items))
}

/// The stable natural merge sort (PeekSort) Nix's `builtins.sort` uses. A
/// comparator that is not a strict weak ordering gives an order that depends
/// on exactly which comparisons are made, so this makes Nix's comparisons.
struct PeekSort<'s, 'a> {
    buf: Vec<Value<'a>>,
    less: &'s dyn Fn(Value<'a>, Value<'a>) -> R<'a, bool>,
}

impl<'a> PeekSort<'_, 'a> {
    /// Whether `next` continues a run after `prev`: a weakly increasing run
    /// needs `!(next < prev)`, a strictly decreasing one `next < prev`.
    fn in_run(&self, decreasing: bool, prev: Value<'a>, next: Value<'a>) -> R<'a, bool> {
        Ok((self.less)(next, prev)? == decreasing)
    }

    /// The end of the run starting at `begin`, scanning forwards to `end`.
    fn run_end(
        &self,
        v: &[Value<'a>],
        decreasing: bool,
        mut begin: usize,
        end: usize,
    ) -> R<'a, usize> {
        if begin == end {
            return Ok(begin);
        }
        while begin + 1 != end && self.in_run(decreasing, v[begin], v[begin + 1])? {
            begin += 1;
        }
        Ok(begin + 1)
    }

    /// The start of the run ending at `end`, scanning backwards to `begin`.
    fn run_start(
        &self,
        v: &[Value<'a>],
        decreasing: bool,
        begin: usize,
        mut end: usize,
    ) -> R<'a, usize> {
        if begin == end {
            return Ok(end);
        }
        while end - 1 > begin && self.in_run(decreasing, v[end - 2], v[end - 1])? {
            end -= 1;
        }
        Ok(end - 1)
    }

    fn insertion_sort(&self, v: &mut [Value<'a>]) -> R<'a, ()> {
        for i in 1..v.len() {
            let mut j = i;
            while j > 0 && (self.less)(v[j], v[j - 1])? {
                v.swap(j, j - 1);
                j -= 1;
            }
        }
        Ok(())
    }

    /// Merges the sorted `v[begin..mid]` and `v[mid..end]`.
    fn merge(&mut self, v: &mut [Value<'a>], begin: usize, mid: usize, end: usize) -> R<'a, ()> {
        let (left, n) = (mid - begin, end - begin);
        self.buf[..n].copy_from_slice(&v[begin..end]);
        let (mut l, mut r, mut out) = (0, left, begin);
        while l < left && r < n {
            if (self.less)(self.buf[r], self.buf[l])? {
                v[out] = self.buf[r];
                r += 1;
            } else {
                v[out] = self.buf[l];
                l += 1;
            }
            out += 1;
        }
        let rest = if l < left { l..left } else { r..n };
        v[out..end].copy_from_slice(&self.buf[rest]);
        Ok(())
    }

    /// Sorts `v[begin..end]`, given that `v[begin..left_end]` and
    /// `v[right_begin..end]` are already sorted.
    fn sort_range(
        &mut self,
        v: &mut [Value<'a>],
        begin: usize,
        end: usize,
        left_end: usize,
        right_begin: usize,
    ) -> R<'a, ()> {
        if left_end == end || right_begin == begin {
            return Ok(());
        }
        let n = end - begin;
        if n <= 16 {
            return self.insertion_sort(&mut v[begin..end]);
        }
        let mid = begin + n / 2;
        if mid <= left_end {
            self.sort_range(v, left_end, end, left_end + 1, right_begin)?;
            return self.merge(v, begin, left_end, end);
        }
        if mid >= right_begin {
            self.sort_range(v, begin, right_begin, left_end, right_begin - 1)?;
            return self.merge(v, begin, right_begin, end);
        }
        // The run containing `v[mid - 1]`, reversed if decreasing.
        let decreasing = (self.less)(v[mid], v[mid - 1])?;
        let i = self.run_start(v, decreasing, left_end, mid)?;
        let j = self.run_end(v, decreasing, mid - 1, right_begin)?;
        if decreasing {
            v[i..j].reverse();
        }
        if i == begin && j == end {
            return Ok(());
        }
        if mid - i < j - mid {
            // When `i == begin` the left part is empty and returns at once.
            self.sort_range(v, begin, i, left_end, i.wrapping_sub(1))?;
            self.sort_range(v, i, end, j, right_begin)?;
            self.merge(v, begin, i, end)
        } else {
            self.sort_range(v, begin, j, left_end, i)?;
            self.sort_range(v, j, end, j + 1, right_begin)?;
            self.merge(v, begin, j, end)
        }
    }
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
