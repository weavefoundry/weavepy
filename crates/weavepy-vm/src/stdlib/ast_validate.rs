//! Callback-free validation of ordinary Python AST fields and positions.
//!
//! This is an affirmative fast path for `ast._obj2ast_check`, not an error
//! reporter. Unproved attributes, types, cycles, or invalid data return false;
//! the Python validator then preserves its original callbacks and diagnostics.
//! Semantic validation and Rust-AST conversion still run afterward.

use std::collections::HashMap;

use crate::builtin_types::builtin_types;
use crate::error::RuntimeError;
use crate::object::{DictData, LeafNameProbe, Object};
use crate::sync::Rc;
use crate::types::{PyInstance, TypeObject};

struct Field {
    name: String,
    required: bool,
    sum: Option<Rc<TypeObject>>,
}

struct Shape {
    fields: Vec<Field>,
    positions: bool,
}

/// A name probe which cannot invoke an exotic key's equality method.
fn field(dict: &DictData, name: &str) -> Option<Option<Object>> {
    let probe = LeafNameProbe::new(name, crate::object::py_str_hash(name));
    let value = dict.get(&probe).cloned();
    (!probe.saw_exotic()).then_some(value)
}

fn ordinary_type(cls: &TypeObject) -> bool {
    cls.c_ext_ptr.get() == 0 && Rc::ptr_eq(&cls.metaclass_or_type(), &builtin_types().type_)
}

/// The ordinary schema forms are classes, list aliases, and optional unions.
/// Unknown metadata is left to the Python helpers, including its callbacks.
fn rule(value: Option<Object>, sums: &[Rc<TypeObject>]) -> Option<(bool, Option<Rc<TypeObject>>)> {
    let bt = builtin_types();
    let (required, base) = match value {
        None | Some(Object::None) => (false, None),
        Some(Object::Type(cls)) => {
            if !ordinary_type(&cls)
                || cls.lookup("__origin__").is_some()
                || cls.lookup("__args__").is_some()
            {
                return None;
            }
            (!Rc::ptr_eq(&cls, &bt.object_), Some(cls))
        }
        Some(Object::SimpleNamespace(dict)) => {
            let dict = dict.borrow();
            let origin = field(&dict, "__origin__")?;
            let is_list = matches!(&origin, Some(Object::Type(cls)) if Rc::ptr_eq(cls, &bt.list_));
            let Some(Object::Tuple(args)) = field(&dict, "__args__")? else {
                return None;
            };
            let mut types = Vec::with_capacity(args.len());
            for arg in args.iter() {
                let Object::Type(cls) = arg else {
                    return None;
                };
                if !ordinary_type(cls) {
                    return None;
                }
                types.push(cls.clone());
            }
            let optional = types.iter().any(|cls| Rc::ptr_eq(cls, &bt.none_type));
            if !is_list && !optional {
                // Otherwise _sum_base hashes the alias/namespace itself.
                // Leave that operation and any failure to the Python path.
                return None;
            }
            let base = if is_list {
                types.first().cloned()
            } else if optional {
                let mut others = types
                    .into_iter()
                    .filter(|cls| !Rc::ptr_eq(cls, &bt.none_type));
                let first = others.next();
                if others.next().is_none() {
                    first
                } else {
                    None
                }
            } else {
                None
            };
            (!is_list && !optional, base)
        }
        _ => return None,
    };
    let sum = base.filter(|cls| sums.iter().any(|sum| Rc::ptr_eq(cls, sum)));
    Some((required, sum))
}

fn shape(cls: &TypeObject, sums: &[Rc<TypeObject>]) -> Option<Shape> {
    if !ordinary_type(cls)
        || !crate::Interpreter::default_getattribute(cls)
        || cls.lookup("__getattr__").is_some()
    {
        return None;
    }
    let Object::Tuple(attributes) = cls.lookup("_attributes")? else {
        return None;
    };
    let mut positions = false;
    for attribute in attributes.iter() {
        let Object::Str(name) = attribute else {
            return None;
        };
        positions |= &**name == "lineno";
    }
    let Object::Tuple(names) = cls.lookup("_fields")? else {
        return None;
    };
    let types = match cls.lookup("_field_types") {
        None => None,
        Some(Object::Dict(types)) => Some(types),
        _ => return None,
    };
    let mut fields = Vec::with_capacity(names.len());
    for name in names.iter() {
        let Object::Str(name) = name else {
            return None;
        };
        // None defaults are ordinary stored values. Any other class value
        // could be a descriptor; let the existing attribute machinery run.
        if !matches!(cls.lookup(name), None | Some(Object::None)) {
            return None;
        }
        let value = match &types {
            Some(types) => field(&types.borrow(), name)?,
            None => None,
        };
        let (required, sum) = rule(value, sums)?;
        fields.push(Field {
            name: name.to_string(),
            required,
            sum,
        });
    }
    if positions {
        for name in ["lineno", "col_offset", "end_lineno", "end_col_offset"] {
            if !matches!(cls.lookup(name), None | Some(Object::None)) {
                return None;
            }
        }
    }
    Some(Shape { fields, positions })
}

fn integer(value: Option<Object>) -> Option<i64> {
    match value {
        Some(Object::Int(value)) => Some(value),
        Some(Object::Bool(value)) => Some(i64::from(value)),
        _ => None,
    }
}

fn valid_positions(dict: &DictData) -> Option<bool> {
    let line = integer(field(dict, "lineno")?)?;
    let col = integer(field(dict, "col_offset")?)?;
    let end_line = match field(dict, "end_lineno")? {
        None | Some(Object::None) => line,
        value => integer(value)?,
    };
    let end_col = match field(dict, "end_col_offset")? {
        None | Some(Object::None) => col,
        value => integer(value)?,
    };
    Some(
        line <= end_line
            && (line >= 0 || end_line == line)
            && (col >= 0 || end_col == col)
            && (line != end_line || col <= end_col),
    )
}

enum Work {
    Enter(Rc<PyInstance>, usize),
    Leave(*const PyInstance),
}

fn child(value: &Object, ast: &TypeObject, work: &mut Vec<Work>, depth: usize) -> Option<()> {
    match value {
        Object::Instance(inst) if inst.cls().is_subclass_of(ast) => {
            work.push(Work::Enter(inst.clone(), depth));
        }
        // In particular, decline subclasses with customized __class__ or
        // list iteration, rather than bypassing isinstance/iteration hooks.
        Object::None
        | Object::Bool(_)
        | Object::Int(_)
        | Object::Long(_)
        | Object::Float(_)
        | Object::Complex(_)
        | Object::Str(_)
        | Object::WStr(_)
        | Object::Bytes(_)
        | Object::Tuple(_)
        | Object::FrozenSet(_) => {}
        _ => return None,
    }
    Some(())
}

fn check(tree: &Object, ast: &Rc<TypeObject>, sums: &[Rc<TypeObject>]) -> Option<()> {
    let Object::Instance(root) = tree else {
        return None;
    };
    if !ordinary_type(ast) || !root.cls().is_subclass_of(ast) {
        return None;
    }
    // These caches live for one callback-free, GIL-held walk only. They never
    // hide class/schema mutations between compile calls or pin user classes.
    let mut shapes = HashMap::new();
    let mut states = HashMap::new();
    let mut work = vec![Work::Enter(root.clone(), 0)];
    let mut visited = 0usize;
    while let Some(item) = work.pop() {
        let (inst, depth) = match item {
            Work::Leave(ptr) => {
                states.insert(ptr, 2u8);
                continue;
            }
            Work::Enter(inst, depth) => (inst, depth),
        };
        let ptr = Rc::as_ptr(&inst);
        match states.get(&ptr) {
            Some(1) => return None,
            Some(2) => continue,
            _ => {}
        }
        visited += 1;
        if depth > 512 || visited > 1_000_000 {
            return None;
        }
        states.insert(ptr, 1u8);
        work.push(Work::Leave(ptr));
        let cls = inst.cls();
        let cls_ptr = Rc::as_ptr(&cls);
        if let std::collections::hash_map::Entry::Vacant(entry) = shapes.entry(cls_ptr) {
            entry.insert(shape(&cls, sums)?);
        }
        let info = shapes.get(&cls_ptr)?;
        // Don't materialize a dictionary for empty context/operator nodes.
        let dict = inst.dict.get().map(|dict| dict.borrow());
        if info.positions && !valid_positions(dict.as_deref()?)? {
            return None;
        }
        for item in &info.fields {
            let value = match dict.as_deref() {
                Some(dict) => field(dict, &item.name)?,
                None => None,
            };
            let Some(value) = value.filter(|value| !matches!(value, Object::None)) else {
                if item.required {
                    return None;
                }
                continue;
            };
            let values = match value {
                Object::List(items) => items.borrow().clone(),
                value => vec![value],
            };
            for value in &values {
                if let Some(base) = &item.sum {
                    if !matches!(value, Object::None) {
                        let Object::Instance(inst) = value else {
                            return None;
                        };
                        let cls = inst.cls();
                        if Rc::ptr_eq(&cls, base) || !cls.is_subclass_of(base) {
                            return None;
                        }
                    }
                }
                child(value, ast, &mut work, depth + 1)?;
            }
        }
    }
    Some(())
}

pub(super) fn validate_fields(args: &[Object]) -> Result<Object, RuntimeError> {
    let [tree, Object::Type(ast), Object::FrozenSet(sums)] = args else {
        return Ok(Object::Bool(false));
    };
    // The local schema cache needs a stable view. Free-threaded execution
    // retains the Python path, as do potentially callback-bearing class keys.
    if crate::gil::free_threading_enabled() || crate::object::exotic_str_keys_possible() {
        return Ok(Object::Bool(false));
    }
    let mut types = Vec::with_capacity(sums.len());
    for key in sums.iter() {
        let Object::Type(cls) = &key.0 else {
            return Ok(Object::Bool(false));
        };
        if !ordinary_type(cls) {
            return Ok(Object::Bool(false));
        }
        types.push(cls.clone());
    }
    Ok(Object::Bool(check(tree, ast, &types).is_some()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_fields_and_fallbacks() {
        const CHILD: &str = "WEAVEPY_AST_FIELDS_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "stdlib::ast_validate::tests::native_fields_and_fallbacks",
                ])
                .env(CHILD, "1")
                .env("WEAVEPY_JIT", "0")
                .status()
                .expect("spawn AST validation test");
            assert!(status.success(), "AST validation child: {status}");
            return;
        }
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let source = include_str!("../../../../tests/regrtest/test_native_ast_fields.py");
                let tree = weavepy_parser::parse_module(source).unwrap();
                let code = weavepy_compiler::compile_owned_module_with_options(
                    tree,
                    source,
                    "test_native_ast_fields.py",
                    weavepy_compiler::CompileOptions::default(),
                )
                .unwrap();
                crate::Interpreter::new()
                    .run_module(&code)
                    .unwrap_or_else(|error| {
                        panic!("AST validation fixture: {error}");
                    });
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
