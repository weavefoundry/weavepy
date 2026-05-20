//! Built-in functions and per-type methods.
//!
//! Two responsibilities live here:
//!
//! 1. [`default_builtins`] returns the dict that lives behind every
//!    module's `__builtins__` — `print`, `len`, `range`, the
//!    type-coercion callables, and so on.
//! 2. [`lookup_method`] resolves an attribute access on a built-in
//!    type (`xs.append`, `s.upper`, `d.get`) to a `BuiltinFn`. The
//!    VM wraps that in a `BoundMethod` so the receiver flows through
//!    as the first arg on call.
//!
//! Builtins close over no state: each takes a `&[Object]` and returns
//! a `Result<Object, RuntimeError>`. Stateful builtins (notably
//! `print`, which needs the interpreter's stdout sink) are installed
//! by [`crate::Interpreter::install_print_into`].

use std::cell::RefCell;
use std::rc::Rc;

use crate::error::{
    index_error, key_error, runtime_error, stop_iteration, type_error, value_error, RuntimeError,
};
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyIterator, Range};

/// Build the dict that backs the `builtins` module.
pub fn default_builtins() -> DictData {
    let mut d = DictData::new();
    macro_rules! reg {
        ($name:literal, $body:expr) => {{
            let f = BuiltinFn {
                name: $name,
                call: Box::new($body),
            };
            d.insert(
                DictKey(Object::from_static($name)),
                Object::Builtin(Rc::new(f)),
            );
        }};
    }

    reg!("len", b_len);
    reg!("range", b_range);
    reg!("str", b_str);
    reg!("repr", b_repr);
    reg!("int", b_int);
    reg!("float", b_float);
    reg!("bool", b_bool);
    reg!("list", b_list);
    reg!("tuple", b_tuple);
    reg!("dict", b_dict);
    reg!("type", b_type);
    reg!("abs", b_abs);
    reg!("min", b_min);
    reg!("max", b_max);
    reg!("sum", b_sum);
    reg!("sorted", b_sorted);
    reg!("reversed", b_reversed);
    reg!("enumerate", b_enumerate);
    reg!("zip", b_zip);
    reg!("map", b_map);
    reg!("filter", b_filter);
    reg!("all", b_all);
    reg!("any", b_any);
    reg!("isinstance", b_isinstance);
    reg!("id", b_id);
    reg!("hex", b_hex);
    reg!("oct", b_oct);
    reg!("bin", b_bin);
    reg!("chr", b_chr);
    reg!("ord", b_ord);
    reg!("input", b_input_unsupported);
    reg!("next", b_next);
    reg!("iter", b_iter);
    reg!("divmod", b_divmod);
    reg!("round", b_round);

    d
}

// ---------- method dispatch ----------

/// Resolve `obj.<name>` to a callable, or `None` if there's no such
/// method. The returned [`Object`] is always a [`Object::Builtin`];
/// the VM wraps it as a [`crate::object::BoundMethod`] so the
/// receiver flows through as the first argument on call.
pub fn lookup_method(obj: &Object, name: &str) -> Option<Object> {
    let f: Option<BuiltinFn> = match obj {
        Object::Str(_) => match name {
            "upper" => Some(method("upper", str_upper)),
            "lower" => Some(method("lower", str_lower)),
            "strip" => Some(method("strip", str_strip)),
            "split" => Some(method("split", str_split)),
            "join" => Some(method("join", str_join)),
            "startswith" => Some(method("startswith", str_startswith)),
            "endswith" => Some(method("endswith", str_endswith)),
            "replace" => Some(method("replace", str_replace)),
            "find" => Some(method("find", str_find)),
            "format" => Some(method("format", str_format_unsupported)),
            _ => None,
        },
        Object::List(_) => match name {
            "append" => Some(method("append", list_append)),
            "pop" => Some(method("pop", list_pop)),
            "extend" => Some(method("extend", list_extend)),
            "insert" => Some(method("insert", list_insert)),
            "remove" => Some(method("remove", list_remove)),
            "index" => Some(method("index", list_index)),
            "count" => Some(method("count", list_count)),
            "sort" => Some(method("sort", list_sort)),
            "reverse" => Some(method("reverse", list_reverse)),
            "clear" => Some(method("clear", list_clear)),
            "copy" => Some(method("copy", list_copy)),
            _ => None,
        },
        Object::Dict(_) => match name {
            "get" => Some(method("get", dict_get)),
            "keys" => Some(method("keys", dict_keys)),
            "values" => Some(method("values", dict_values)),
            "items" => Some(method("items", dict_items)),
            "pop" => Some(method("pop", dict_pop)),
            "update" => Some(method("update", dict_update)),
            "clear" => Some(method("clear", dict_clear)),
            _ => None,
        },
        Object::Tuple(_) => match name {
            "count" => Some(method("count", tuple_count)),
            "index" => Some(method("index", tuple_index)),
            _ => None,
        },
        _ => None,
    };
    f.map(|f| Object::Builtin(Rc::new(f)))
}

fn method(
    name: &'static str,
    body: impl Fn(&[Object]) -> Result<Object, RuntimeError> + 'static,
) -> BuiltinFn {
    BuiltinFn {
        name,
        call: Box::new(body),
    }
}

// ---------- free builtins ----------

fn one<'a>(args: &'a [Object], name: &str) -> Result<&'a Object, RuntimeError> {
    args.first()
        .ok_or_else(|| type_error(format!("{name}() takes 1 argument (0 given)")))
}

fn b_len(args: &[Object]) -> Result<Object, RuntimeError> {
    let v = one(args, "len")?;
    Ok(Object::Int(v.len()? as i64))
}

fn b_range(args: &[Object]) -> Result<Object, RuntimeError> {
    let to_int = |o: &Object| -> Result<i64, RuntimeError> {
        match o {
            Object::Int(i) => Ok(*i),
            Object::Bool(b) => Ok(i64::from(*b)),
            _ => Err(type_error(format!(
                "'{}' object cannot be interpreted as an integer",
                o.type_name()
            ))),
        }
    };
    let (start, stop, step) = match args.len() {
        1 => (0, to_int(&args[0])?, 1),
        2 => (to_int(&args[0])?, to_int(&args[1])?, 1),
        3 => (to_int(&args[0])?, to_int(&args[1])?, to_int(&args[2])?),
        n => {
            return Err(type_error(format!(
                "range expected 1 to 3 arguments, got {n}"
            )))
        }
    };
    if step == 0 {
        return Err(value_error("range() arg 3 must not be zero"));
    }
    Ok(Object::Range(Rc::new(Range { start, stop, step })))
}

fn b_str(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        return Ok(Object::from_static(""));
    }
    Ok(Object::from_str(args[0].to_str()))
}

fn b_repr(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::from_str(one(args, "repr")?.repr()))
}

fn b_int(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        return Ok(Object::Int(0));
    }
    match &args[0] {
        Object::Int(i) => Ok(Object::Int(*i)),
        Object::Bool(b) => Ok(Object::Int(i64::from(*b))),
        Object::Float(f) => Ok(Object::Int(*f as i64)),
        Object::Str(s) => {
            let trimmed = s.trim();
            let base = if args.len() == 2 {
                match &args[1] {
                    Object::Int(i) => *i as u32,
                    _ => 10,
                }
            } else {
                10
            };
            let parsed =
                i64::from_str_radix(trimmed, base).map_err(|e| value_error(e.to_string()))?;
            Ok(Object::Int(parsed))
        }
        _ => Err(type_error(format!(
            "int() argument must be a string or a real number, not '{}'",
            args[0].type_name()
        ))),
    }
}

fn b_float(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        return Ok(Object::Float(0.0));
    }
    match &args[0] {
        Object::Int(i) => Ok(Object::Float(*i as f64)),
        Object::Bool(b) => Ok(Object::Float(f64::from(*b))),
        Object::Float(f) => Ok(Object::Float(*f)),
        Object::Str(s) => {
            let parsed: f64 = s
                .trim()
                .parse()
                .map_err(|e: std::num::ParseFloatError| value_error(e.to_string()))?;
            Ok(Object::Float(parsed))
        }
        _ => Err(type_error(format!(
            "float() argument must be a string or a number, not '{}'",
            args[0].type_name()
        ))),
    }
}

fn b_bool(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        return Ok(Object::Bool(false));
    }
    Ok(Object::Bool(args[0].is_truthy()))
}

fn b_list(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        return Ok(Object::new_list(Vec::new()));
    }
    let mut it = args[0].make_iter()?;
    let mut out = Vec::new();
    while let Some(v) = it.next_value() {
        out.push(v);
    }
    Ok(Object::new_list(out))
}

fn b_tuple(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        return Ok(Object::new_tuple(Vec::new()));
    }
    let mut it = args[0].make_iter()?;
    let mut out = Vec::new();
    while let Some(v) = it.next_value() {
        out.push(v);
    }
    Ok(Object::new_tuple(out))
}

fn b_dict(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        return Ok(Object::new_dict());
    }
    // Accept an iterable of (k, v) pairs.
    let mut it = args[0].make_iter()?;
    let mut d = DictData::new();
    while let Some(pair) = it.next_value() {
        match pair {
            Object::Tuple(items) if items.len() == 2 => {
                d.insert(DictKey(items[0].clone()), items[1].clone());
            }
            _ => {
                return Err(value_error(
                    "dictionary update sequence element is not a 2-tuple",
                ))
            }
        }
    }
    Ok(Object::Dict(Rc::new(RefCell::new(d))))
}

fn b_type(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::from_static(one(args, "type")?.type_name()))
}

fn b_abs(args: &[Object]) -> Result<Object, RuntimeError> {
    match one(args, "abs")? {
        Object::Int(i) => Ok(Object::Int(i.abs())),
        Object::Float(f) => Ok(Object::Float(f.abs())),
        Object::Bool(b) => Ok(Object::Int(i64::from(*b))),
        other => Err(type_error(format!(
            "bad operand type for abs(): '{}'",
            other.type_name()
        ))),
    }
}

fn min_or_max(args: &[Object], is_min: bool) -> Result<Object, RuntimeError> {
    let pool: Vec<Object> = if args.len() == 1 {
        let mut out = Vec::new();
        let mut it = args[0].make_iter()?;
        while let Some(v) = it.next_value() {
            out.push(v);
        }
        out
    } else {
        args.to_vec()
    };
    if pool.is_empty() {
        return Err(value_error("min/max arg is an empty sequence"));
    }
    let mut best = pool[0].clone();
    for v in pool.into_iter().skip(1) {
        let ord = v.cmp(&best)?;
        if (is_min && ord.is_lt()) || (!is_min && ord.is_gt()) {
            best = v;
        }
    }
    Ok(best)
}

fn b_min(args: &[Object]) -> Result<Object, RuntimeError> {
    min_or_max(args, true)
}

fn b_max(args: &[Object]) -> Result<Object, RuntimeError> {
    min_or_max(args, false)
}

fn b_sum(args: &[Object]) -> Result<Object, RuntimeError> {
    let iterable = one(args, "sum")?;
    let mut total = Object::Int(0);
    let mut it = iterable.make_iter()?;
    while let Some(v) = it.next_value() {
        total = crate::binary_op(&total, &v, weavepy_compiler::BinOpKind::Add)?;
    }
    Ok(total)
}

fn b_sorted(args: &[Object]) -> Result<Object, RuntimeError> {
    let iterable = one(args, "sorted")?;
    let mut it = iterable.make_iter()?;
    let mut buf: Vec<Object> = Vec::new();
    while let Some(v) = it.next_value() {
        buf.push(v);
    }
    let mut err: Option<RuntimeError> = None;
    buf.sort_by(|a: &Object, b: &Object| match a.cmp(b) {
        Ok(o) => o,
        Err(e) => {
            err = Some(e);
            std::cmp::Ordering::Equal
        }
    });
    if let Some(e) = err {
        return Err(e);
    }
    Ok(Object::new_list(buf))
}

fn b_reversed(args: &[Object]) -> Result<Object, RuntimeError> {
    let iterable = one(args, "reversed")?;
    let mut it = iterable.make_iter()?;
    let mut buf = Vec::new();
    while let Some(v) = it.next_value() {
        buf.push(v);
    }
    buf.reverse();
    Ok(Object::new_list(buf))
}

fn b_enumerate(args: &[Object]) -> Result<Object, RuntimeError> {
    let iterable = one(args, "enumerate")?;
    let start = if args.len() >= 2 {
        match &args[1] {
            Object::Int(i) => *i,
            _ => return Err(type_error("enumerate() start must be an int")),
        }
    } else {
        0
    };
    let mut it = iterable.make_iter()?;
    let mut buf = Vec::new();
    let mut i = start;
    while let Some(v) = it.next_value() {
        buf.push(Object::new_tuple(vec![Object::Int(i), v]));
        i += 1;
    }
    Ok(Object::new_list(buf))
}

fn b_zip(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut iters: Vec<PyIterator> = args
        .iter()
        .map(|a| a.make_iter())
        .collect::<Result<_, _>>()?;
    let mut out = Vec::new();
    loop {
        let mut tup = Vec::with_capacity(iters.len());
        for it in iters.iter_mut() {
            match it.next_value() {
                Some(v) => tup.push(v),
                None => return Ok(Object::new_list(out)),
            }
        }
        out.push(Object::new_tuple(tup));
    }
}

fn b_map(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(type_error(
        "map() requires call-into-interpreter support; use a list comprehension instead",
    ))
}

fn b_filter(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(type_error(
        "filter() requires call-into-interpreter support; use a list comprehension instead",
    ))
}

fn b_all(args: &[Object]) -> Result<Object, RuntimeError> {
    let iterable = one(args, "all")?;
    let mut it = iterable.make_iter()?;
    while let Some(v) = it.next_value() {
        if !v.is_truthy() {
            return Ok(Object::Bool(false));
        }
    }
    Ok(Object::Bool(true))
}

fn b_any(args: &[Object]) -> Result<Object, RuntimeError> {
    let iterable = one(args, "any")?;
    let mut it = iterable.make_iter()?;
    while let Some(v) = it.next_value() {
        if v.is_truthy() {
            return Ok(Object::Bool(true));
        }
    }
    Ok(Object::Bool(false))
}

fn b_isinstance(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error("isinstance expected 2 arguments"));
    }
    let want = args[1].to_str();
    let got = args[0].type_name();
    Ok(Object::Bool(got == want))
}

fn b_id(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(one(args, "id")?.repr().len() as i64))
}

fn b_hex(args: &[Object]) -> Result<Object, RuntimeError> {
    match one(args, "hex")? {
        Object::Int(i) => {
            if *i < 0 {
                Ok(Object::from_str(format!("-0x{:x}", -i)))
            } else {
                Ok(Object::from_str(format!("0x{i:x}")))
            }
        }
        other => Err(type_error(format!(
            "'{}' object cannot be interpreted as an integer",
            other.type_name()
        ))),
    }
}

fn b_oct(args: &[Object]) -> Result<Object, RuntimeError> {
    match one(args, "oct")? {
        Object::Int(i) => {
            if *i < 0 {
                Ok(Object::from_str(format!("-0o{:o}", -i)))
            } else {
                Ok(Object::from_str(format!("0o{i:o}")))
            }
        }
        _ => Err(type_error("expected int")),
    }
}

fn b_bin(args: &[Object]) -> Result<Object, RuntimeError> {
    match one(args, "bin")? {
        Object::Int(i) => {
            if *i < 0 {
                Ok(Object::from_str(format!("-0b{:b}", -i)))
            } else {
                Ok(Object::from_str(format!("0b{i:b}")))
            }
        }
        _ => Err(type_error("expected int")),
    }
}

fn b_chr(args: &[Object]) -> Result<Object, RuntimeError> {
    match one(args, "chr")? {
        Object::Int(i) => {
            let ch = char::from_u32(*i as u32)
                .ok_or_else(|| value_error("chr() arg not in range(0x110000)"))?;
            Ok(Object::from_str(ch.to_string()))
        }
        _ => Err(type_error("chr() expected int")),
    }
}

fn b_ord(args: &[Object]) -> Result<Object, RuntimeError> {
    match one(args, "ord")? {
        Object::Str(s) => {
            let mut chars = s.chars();
            let c = chars
                .next()
                .ok_or_else(|| type_error("ord() expected a character, but empty string given"))?;
            if chars.next().is_some() {
                return Err(type_error(
                    "ord() expected a character, but multi-character string given",
                ));
            }
            Ok(Object::Int(i64::from(u32::from(c))))
        }
        _ => Err(type_error("ord() expected string")),
    }
}

fn b_input_unsupported(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(runtime_error("input() is not supported in this build"))
}

fn b_next(args: &[Object]) -> Result<Object, RuntimeError> {
    let it = one(args, "next")?;
    let default = args.get(1).cloned();
    if let Object::Iter(it) = it {
        match it.borrow_mut().next_value() {
            Some(v) => Ok(v),
            None => default.ok_or_else(stop_iteration),
        }
    } else {
        Err(type_error(format!(
            "'{}' object is not an iterator",
            it.type_name()
        )))
    }
}

fn b_iter(args: &[Object]) -> Result<Object, RuntimeError> {
    let it = one(args, "iter")?.make_iter()?;
    Ok(Object::Iter(Rc::new(RefCell::new(it))))
}

fn b_divmod(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error("divmod expected 2 arguments"));
    }
    let q = crate::binary_op(&args[0], &args[1], weavepy_compiler::BinOpKind::FloorDiv)?;
    let r = crate::binary_op(&args[0], &args[1], weavepy_compiler::BinOpKind::Mod)?;
    Ok(Object::new_tuple(vec![q, r]))
}

fn b_round(args: &[Object]) -> Result<Object, RuntimeError> {
    match one(args, "round")? {
        Object::Int(i) => Ok(Object::Int(*i)),
        Object::Float(f) => Ok(Object::Float(f.round())),
        _ => Err(type_error("round() argument must be int or float")),
    }
}

// ---------- str methods ----------

fn str_self(args: &[Object]) -> Result<&str, RuntimeError> {
    match args.first() {
        Some(Object::Str(s)) => Ok(s),
        _ => Err(type_error("expected str method receiver")),
    }
}

fn str_upper(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::from_str(str_self(args)?.to_uppercase()))
}

fn str_lower(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::from_str(str_self(args)?.to_lowercase()))
}

fn str_strip(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::from_str(str_self(args)?.trim().to_owned()))
}

fn str_split(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let out: Vec<Object> = if args.len() == 1 {
        s.split_whitespace().map(Object::from_str).collect()
    } else {
        match &args[1] {
            Object::Str(sep) => s.split(&**sep).map(Object::from_str).collect(),
            _ => return Err(type_error("split() argument must be str")),
        }
    };
    Ok(Object::new_list(out))
}

fn str_join(args: &[Object]) -> Result<Object, RuntimeError> {
    let sep = str_self(args)?.to_owned();
    if args.len() != 2 {
        return Err(type_error("join() expected 1 argument"));
    }
    let mut it = args[1].make_iter()?;
    let mut parts = Vec::new();
    while let Some(v) = it.next_value() {
        match v {
            Object::Str(s) => parts.push(s.to_string()),
            other => {
                return Err(type_error(format!(
                    "sequence item: expected str instance, {} found",
                    other.type_name()
                )))
            }
        }
    }
    Ok(Object::from_str(parts.join(&sep)))
}

fn str_startswith(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let prefix = match args.get(1) {
        Some(Object::Str(p)) => p,
        _ => return Err(type_error("startswith() expected str")),
    };
    Ok(Object::Bool(s.starts_with(&**prefix)))
}

fn str_endswith(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let suffix = match args.get(1) {
        Some(Object::Str(p)) => p,
        _ => return Err(type_error("endswith() expected str")),
    };
    Ok(Object::Bool(s.ends_with(&**suffix)))
}

fn str_replace(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let from = match args.get(1) {
        Some(Object::Str(p)) => p,
        _ => return Err(type_error("replace() expected str")),
    };
    let to = match args.get(2) {
        Some(Object::Str(p)) => p,
        _ => return Err(type_error("replace() expected str")),
    };
    Ok(Object::from_str(s.replace(&**from, to)))
}

fn str_find(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = str_self(args)?;
    let sub = match args.get(1) {
        Some(Object::Str(p)) => p,
        _ => return Err(type_error("find() expected str")),
    };
    Ok(Object::Int(s.find(&**sub).map_or(-1, |i| i as i64)))
}

fn str_format_unsupported(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(runtime_error(
        "str.format is not supported in this build (RFC 0005)",
    ))
}

// ---------- list methods ----------

fn list_self(args: &[Object]) -> Result<Rc<RefCell<Vec<Object>>>, RuntimeError> {
    match args.first() {
        Some(Object::List(l)) => Ok(l.clone()),
        _ => Err(type_error("expected list method receiver")),
    }
}

fn list_append(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error("list.append() expected 1 argument"));
    }
    list_self(args)?.borrow_mut().push(args[1].clone());
    Ok(Object::None)
}

fn list_pop(args: &[Object]) -> Result<Object, RuntimeError> {
    let l = list_self(args)?;
    let mut l = l.borrow_mut();
    let idx = if args.len() > 1 {
        match &args[1] {
            Object::Int(i) => {
                if l.is_empty() {
                    return Err(index_error("pop from empty list"));
                }
                let len = l.len() as i64;
                let n = if *i < 0 { i + len } else { *i };
                if n < 0 || n >= len {
                    return Err(index_error("pop index out of range"));
                }
                n as usize
            }
            _ => return Err(type_error("pop index must be int")),
        }
    } else {
        if l.is_empty() {
            return Err(index_error("pop from empty list"));
        }
        l.len() - 1
    };
    Ok(l.remove(idx))
}

fn list_extend(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error("list.extend() expected 1 argument"));
    }
    let l = list_self(args)?;
    let mut it = args[1].make_iter()?;
    while let Some(v) = it.next_value() {
        l.borrow_mut().push(v);
    }
    Ok(Object::None)
}

fn list_insert(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 3 {
        return Err(type_error("list.insert() expected 2 arguments"));
    }
    let i = match &args[1] {
        Object::Int(i) => *i,
        _ => return Err(type_error("insert index must be int")),
    };
    let l = list_self(args)?;
    let mut l = l.borrow_mut();
    let len = l.len() as i64;
    let idx = if i < 0 {
        (i + len).max(0) as usize
    } else if i > len {
        l.len()
    } else {
        i as usize
    };
    l.insert(idx, args[2].clone());
    Ok(Object::None)
}

fn list_remove(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error("list.remove() expected 1 argument"));
    }
    let l = list_self(args)?;
    let mut l = l.borrow_mut();
    let pos = l
        .iter()
        .position(|x| x.eq_value(&args[1]))
        .ok_or_else(|| value_error("list.remove(x): x not in list"))?;
    l.remove(pos);
    Ok(Object::None)
}

fn list_index(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() < 2 {
        return Err(type_error("list.index() expected at least 1 argument"));
    }
    let l = list_self(args)?;
    let l = l.borrow();
    let pos = l
        .iter()
        .position(|x| x.eq_value(&args[1]))
        .ok_or_else(|| value_error("x not in list"))?;
    Ok(Object::Int(pos as i64))
}

fn list_count(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error("list.count() expected 1 argument"));
    }
    let l = list_self(args)?;
    let l = l.borrow();
    let n = l.iter().filter(|x| x.eq_value(&args[1])).count();
    Ok(Object::Int(n as i64))
}

fn list_sort(args: &[Object]) -> Result<Object, RuntimeError> {
    let l = list_self(args)?;
    let mut err: Option<RuntimeError> = None;
    l.borrow_mut()
        .sort_by(|a: &Object, b: &Object| match a.cmp(b) {
            Ok(o) => o,
            Err(e) => {
                err = Some(e);
                std::cmp::Ordering::Equal
            }
        });
    if let Some(e) = err {
        return Err(e);
    }
    Ok(Object::None)
}

fn list_reverse(args: &[Object]) -> Result<Object, RuntimeError> {
    list_self(args)?.borrow_mut().reverse();
    Ok(Object::None)
}

fn list_clear(args: &[Object]) -> Result<Object, RuntimeError> {
    list_self(args)?.borrow_mut().clear();
    Ok(Object::None)
}

fn list_copy(args: &[Object]) -> Result<Object, RuntimeError> {
    let l = list_self(args)?;
    let v = l.borrow().clone();
    Ok(Object::new_list(v))
}

// ---------- dict methods ----------

fn dict_self(args: &[Object]) -> Result<Rc<RefCell<DictData>>, RuntimeError> {
    match args.first() {
        Some(Object::Dict(d)) => Ok(d.clone()),
        _ => Err(type_error("expected dict method receiver")),
    }
}

fn dict_get(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    let key = args
        .get(1)
        .ok_or_else(|| type_error("dict.get() expected at least 1 argument"))?;
    let default = args.get(2).cloned().unwrap_or(Object::None);
    let value = d
        .borrow()
        .get(&DictKey(key.clone()))
        .cloned()
        .unwrap_or(default);
    Ok(value)
}

fn dict_keys(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    let keys: Vec<Object> = d.borrow().keys().map(|k| k.0.clone()).collect();
    Ok(Object::new_list(keys))
}

fn dict_values(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    let vs: Vec<Object> = d.borrow().values().cloned().collect();
    Ok(Object::new_list(vs))
}

fn dict_items(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    let items: Vec<Object> = d
        .borrow()
        .iter()
        .map(|(k, v)| Object::new_tuple(vec![k.0.clone(), v.clone()]))
        .collect();
    Ok(Object::new_list(items))
}

fn dict_pop(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    let key = args
        .get(1)
        .ok_or_else(|| type_error("dict.pop() expected at least 1 argument"))?;
    let mut d = d.borrow_mut();
    if let Some(v) = d.shift_remove(&DictKey(key.clone())) {
        Ok(v)
    } else if let Some(default) = args.get(2).cloned() {
        Ok(default)
    } else {
        Err(key_error(key.repr()))
    }
}

fn dict_update(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = dict_self(args)?;
    if let Some(other) = args.get(1) {
        match other {
            Object::Dict(o) => {
                for (k, v) in o.borrow().iter() {
                    d.borrow_mut().insert(k.clone(), v.clone());
                }
            }
            _ => return Err(type_error("dict.update() expected dict")),
        }
    }
    Ok(Object::None)
}

fn dict_clear(args: &[Object]) -> Result<Object, RuntimeError> {
    dict_self(args)?.borrow_mut().clear();
    Ok(Object::None)
}

// ---------- tuple methods ----------

fn tuple_count(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error("tuple.count() expected 1 argument"));
    }
    let t = match args.first() {
        Some(Object::Tuple(t)) => t.clone(),
        _ => return Err(type_error("expected tuple")),
    };
    let n = t.iter().filter(|x| x.eq_value(&args[1])).count();
    Ok(Object::Int(n as i64))
}

fn tuple_index(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() < 2 {
        return Err(type_error("tuple.index() expected at least 1 argument"));
    }
    let t = match args.first() {
        Some(Object::Tuple(t)) => t.clone(),
        _ => return Err(type_error("expected tuple")),
    };
    let pos = t
        .iter()
        .position(|x| x.eq_value(&args[1]))
        .ok_or_else(|| value_error("x not in tuple"))?;
    Ok(Object::Int(pos as i64))
}
