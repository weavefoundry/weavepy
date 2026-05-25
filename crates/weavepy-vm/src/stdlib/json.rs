//! The `json` built-in module.
//!
//! Encode and decode JSON. We use `serde_json` for the wire format
//! and then transform the resulting tree of values into Python
//! objects (and vice versa). This sidesteps the substantial amount
//! of state machine code that would otherwise live in pure Rust.

use crate::sync::Rc;
use crate::sync::RefCell;

use serde_json::{Map, Number, Value};

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("json"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("JSON encoder and decoder."),
        );
        d.insert(
            DictKey(Object::from_static("loads")),
            b("loads", json_loads),
        );
        d.insert(
            DictKey(Object::from_static("dumps")),
            b("dumps", json_dumps),
        );
        d.insert(DictKey(Object::from_static("load")), b("load", json_load));
        d.insert(DictKey(Object::from_static("dump")), b("dump", json_dump));
        d.insert(
            DictKey(Object::from_static("JSONDecodeError")),
            Object::Type(crate::builtin_types::builtin_types().value_error.clone()),
        );
    }
    Rc::new(PyModule {
        name: "json".to_owned(),
        filename: None,
        dict,
    })
}

fn b(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(body),
    }))
}

fn json_loads(args: &[Object]) -> Result<Object, RuntimeError> {
    let text = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        Some(Object::Bytes(b)) => {
            String::from_utf8(b.to_vec()).map_err(|_| value_error("invalid UTF-8 in JSON bytes"))?
        }
        _ => return Err(type_error("loads() expects str or bytes")),
    };
    let value: Value =
        serde_json::from_str(&text).map_err(|e| value_error(format!("invalid JSON: {e}")))?;
    Ok(json_to_object(value))
}

fn json_dumps(args: &[Object]) -> Result<Object, RuntimeError> {
    let value = args
        .first()
        .ok_or_else(|| type_error("dumps() missing argument"))?;
    let indent = match args.get(1) {
        Some(Object::Int(n)) => Some(*n),
        _ => None,
    };
    let sort_keys = matches!(args.get(2), Some(Object::Bool(true)));
    let json = object_to_json(value)?;
    let serialised = if let Some(n) = indent {
        if n <= 0 {
            serde_json::to_string(&maybe_sort(json, sort_keys))
        } else {
            serde_json::to_string_pretty(&maybe_sort(json, sort_keys))
        }
    } else {
        serde_json::to_string(&maybe_sort(json, sort_keys))
    }
    .map_err(|e| value_error(format!("JSON encoding failed: {e}")))?;
    Ok(Object::from_str(serialised))
}

fn json_load(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = args
        .first()
        .ok_or_else(|| type_error("load() expects a file"))?;
    let file = match f {
        Object::File(file) => file.clone(),
        _ => return Err(type_error("load() expects a file-like object")),
    };
    let text = file.read_text_all()?;
    let value: Value =
        serde_json::from_str(&text).map_err(|e| value_error(format!("invalid JSON: {e}")))?;
    Ok(json_to_object(value))
}

fn json_dump(args: &[Object]) -> Result<Object, RuntimeError> {
    let payload = args
        .first()
        .ok_or_else(|| type_error("dump() missing payload"))?;
    let f = args
        .get(1)
        .ok_or_else(|| type_error("dump() missing file"))?;
    let file = match f {
        Object::File(file) => file.clone(),
        _ => return Err(type_error("dump() expects a file-like object")),
    };
    let s = match json_dumps(std::slice::from_ref(payload))? {
        Object::Str(s) => s.to_string(),
        _ => return Err(value_error("dump() failed")),
    };
    file.write_text(&s)?;
    Ok(Object::None)
}

fn maybe_sort(value: Value, sort: bool) -> Value {
    if !sort {
        return value;
    }
    match value {
        Value::Object(map) => {
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort();
            let mut new = Map::new();
            for k in keys {
                if let Some(v) = map.get(&k) {
                    new.insert(k, maybe_sort(v.clone(), true));
                }
            }
            Value::Object(new)
        }
        Value::Array(arr) => Value::Array(arr.into_iter().map(|v| maybe_sort(v, true)).collect()),
        other => other,
    }
}

fn json_to_object(value: Value) -> Object {
    match value {
        Value::Null => Object::None,
        Value::Bool(b) => Object::Bool(b),
        Value::Number(n) => json_number(&n),
        Value::String(s) => Object::from_str(s),
        Value::Array(items) => Object::new_list(items.into_iter().map(json_to_object).collect()),
        Value::Object(map) => {
            let mut d = DictData::new();
            for (k, v) in map {
                d.insert(DictKey(Object::from_str(k)), json_to_object(v));
            }
            Object::Dict(Rc::new(RefCell::new(d)))
        }
    }
}

fn json_number(n: &Number) -> Object {
    if let Some(i) = n.as_i64() {
        return Object::Int(i);
    }
    if let Some(f) = n.as_f64() {
        return Object::Float(f);
    }
    if let Some(u) = n.as_u64() {
        return Object::Int(u as i64);
    }
    Object::Float(0.0)
}

fn object_to_json(obj: &Object) -> Result<Value, RuntimeError> {
    Ok(match obj {
        Object::None => Value::Null,
        Object::Bool(b) => Value::Bool(*b),
        Object::Int(i) => Value::Number((*i).into()),
        Object::Float(f) => Number::from_f64(*f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Object::Str(s) => Value::String(s.to_string()),
        Object::List(items) => {
            let items = items.borrow();
            let mut arr = Vec::with_capacity(items.len());
            for item in items.iter() {
                arr.push(object_to_json(item)?);
            }
            Value::Array(arr)
        }
        Object::Tuple(items) => {
            let mut arr = Vec::with_capacity(items.len());
            for item in items.iter() {
                arr.push(object_to_json(item)?);
            }
            Value::Array(arr)
        }
        Object::Dict(d) => {
            let d = d.borrow();
            let mut map = Map::new();
            for (k, v) in d.iter() {
                let key = match &k.0 {
                    Object::Str(s) => s.to_string(),
                    Object::Int(i) => i.to_string(),
                    Object::Float(f) => f.to_string(),
                    Object::Bool(b) => {
                        if *b {
                            "true".to_owned()
                        } else {
                            "false".to_owned()
                        }
                    }
                    Object::None => "null".to_owned(),
                    _ => return Err(type_error("keys must be str, int, float, bool, or None")),
                };
                map.insert(key, object_to_json(v)?);
            }
            Value::Object(map)
        }
        _ => {
            return Err(type_error(format!(
                "Object of type {} is not JSON serializable",
                obj.type_name()
            )))
        }
    })
}
