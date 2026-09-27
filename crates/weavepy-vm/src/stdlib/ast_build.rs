//! Native traversal of the value-based AST spec. Constructors, setters, and
//! iterators still use ordinary VM dispatch. The unchanged Python builder owns
//! unusual inputs and observer-visible execution; no user classes are cached.

use crate::error::{name_error, recursion_error, type_error, RuntimeError};
use crate::object::{DictData, DictViewKind, LeafNameProbe, Object, PyDictView, PyFunction};
use crate::sync::{Rc, RefCell};
use crate::Interpreter;

struct Name {
    text: &'static str,
    hash: i64,
}

impl Name {
    fn new(text: &'static str) -> Self {
        Self {
            text,
            hash: crate::object::py_str_hash(text),
        }
    }

    fn matches(&self, function: &PyFunction, expected: &Object) -> bool {
        for dict in [&function.globals, &function.builtins] {
            let probe = LeafNameProbe::new(self.text, self.hash);
            let dict = dict.borrow();
            let value = dict.get(&probe);
            if probe.saw_exotic() {
                return false;
            }
            if let Some(value) = value {
                // Eligibility never needs an owned handle or a callback.
                return value.is_same(expected);
            }
        }
        false
    }

    fn resolve(&self, function: &PyFunction) -> Result<Object, RuntimeError> {
        for dict in [&function.globals, &function.builtins] {
            let probe = LeafNameProbe::new(self.text, self.hash);
            let value = {
                let dict = dict.borrow();
                let value = dict.get(&probe);
                if probe.saw_exotic() {
                    None
                } else {
                    value.cloned()
                }
            };
            let value = if probe.saw_exotic() {
                crate::object::dict_reentrant_get(dict, &Object::from_static(self.text))?
            } else {
                value
            };
            if let Some(value) = value {
                return Ok(value);
            }
        }
        Err(name_error(format!("name '{}' is not defined", self.text)))
    }
}

struct Builder<'a> {
    state: &'a [Object],
    checks: [Name; 3],
    registry: Name,
    setter: Name,
    recursive: Name,
    type_key: Object,
    // A primitive-only list must still enter the normal evaluator regularly
    // for signals, pending calls, async exceptions, and GIL handoff.
    budget: usize,
}

impl Builder<'_> {
    fn child(
        &mut self,
        vm: &mut Interpreter,
        spec: &Object,
        function: &PyFunction,
    ) -> Result<Object, RuntimeError> {
        let builder = self.recursive.resolve(function)?;
        let result = self.walk(vm, spec, &builder, &function.globals);
        if result.is_ok() {
            vm.reap_call_receiver(builder);
        } else {
            crate::gc_trace::mark_maybe_dead();
        }
        result
    }

    fn walk(
        &mut self,
        vm: &mut Interpreter,
        spec: &Object,
        builder: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        let function = match builder {
            Object::Function(function)
                if !crate::gil::free_threading_enabled()
                    && !crate::trace::any_observers_active()
                    && !crate::trace::eval_frame_record_active()
                    && !super::greenlet_native::on_greenlet_stack()
                    && !crate::object::exotic_str_keys_possible()
                    && vm.globals_missing_owner(&function.globals).is_none()
                    && matches!(&self.state[0], Object::Code(code)
                        if Rc::ptr_eq(code, &function.code.borrow())) =>
            {
                Some(function)
            }
            _ => None,
        };
        let ordinary = function.is_some_and(|function| {
            self.checks
                .iter()
                .zip(&self.state[1..])
                .all(|(name, expected)| name.matches(function, expected))
        });
        let simple = matches!(
            spec,
            Object::Dict(_)
                | Object::List(_)
                | Object::None
                | Object::Bool(_)
                | Object::Int(_)
                | Object::Long(_)
                | Object::Float(_)
                | Object::Complex(_)
                | Object::Str(_)
                | Object::WStr(_)
                | Object::Bytes(_)
                | Object::Tuple(_)
                | Object::FrozenSet(_)
        );
        // Subclasses may customize isinstance via __class__, dict.items(),
        // or list iteration. Leave those calls to the Python implementation.
        if !ordinary || !simple || self.budget == 0 {
            self.budget = 256;
            return vm.call(builder, std::slice::from_ref(spec), &[], globals);
        }
        self.budget -= 1;
        let function = function.unwrap();
        // Account for each replaced Python call, including cyclic private
        // specs, and grow the native stack using the evaluator's policy.
        let guard = match crate::recursion::enter() {
            crate::recursion::Enter::Ok(guard) => guard,
            crate::recursion::Enter::Overflow => {
                return Err(recursion_error("maximum recursion depth exceeded"));
            }
        };
        if guard.depth() % 4 == 0 {
            stacker::maybe_grow(512 * 1024, 8 * 1024 * 1024, || {
                self.walk_inner(vm, spec, function)
            })
        } else {
            self.walk_inner(vm, spec, function)
        }
    }

    fn walk_inner(
        &mut self,
        vm: &mut Interpreter,
        spec: &Object,
        function: &PyFunction,
    ) -> Result<Object, RuntimeError> {
        let globals = &function.globals;
        match spec {
            Object::Dict(dict) => {
                // Resolve the registry before looking up the node name, just
                // as the Python subscription expression does. Callbacks may
                // replace it for subsequent nodes.
                let registry = self.registry.resolve(function)?;
                let name = vm.subscr_get_public(spec, &self.type_key)?;
                let cls = vm.subscr_get_public(&registry, &name)?;
                let new = vm.load_attr_public(&cls, "__new__")?;
                let node = vm.call(&new, std::slice::from_ref(&cls), &[], globals);
                let node = match node {
                    Ok(node) => {
                        vm.reap_call_receiver(new);
                        node
                    }
                    Err(error) => {
                        crate::gc_trace::mark_maybe_dead();
                        return Err(error);
                    }
                };
                let outcome = (|| -> Result<(), RuntimeError> {
                    let items = Object::DictView(Rc::new(PyDictView {
                        dict: dict.clone(),
                        kind: DictViewKind::Items,
                        owner: None,
                    }));
                    let iter = vm.make_iter(&items, globals)?;
                    while let Some(item) = vm.iter_next(&iter, globals)? {
                        let Object::Tuple(pair) = item else {
                            unreachable!("dict items yield pairs");
                        };
                        let key = &pair[0];
                        let is_type = match key {
                            Object::Str(name) => &**name == "_type",
                            _ => vm.op_compare(
                                key,
                                &self.type_key,
                                weavepy_compiler::bytecode::CompareKind::Eq,
                            )?,
                        };
                        if is_type {
                            continue;
                        }
                        // Resolve the setter before the child call. A child's
                        // constructor can rebind it, affecting only later fields.
                        let setter = self.setter.resolve(function)?;
                        let value = self.child(vm, &pair[1], function)?;
                        let mut args = [node.clone(), key.clone(), value];
                        let result = vm.call(&setter, &args, &[], globals);
                        // Match CALL's operand cleanup, then POP_TOP for the
                        // setter's unused result. An ignored child can die here.
                        let result = match result {
                            Ok(result) => {
                                vm.reap_call_receiver(setter);
                                vm.reap_call_args(&mut args);
                                result
                            }
                            Err(error) => {
                                crate::gc_trace::mark_maybe_dead();
                                return Err(error);
                            }
                        };
                        crate::gc_trace::note_dropped(&result);
                        if Interpreter::local_needs_prompt_reap(&result) {
                            vm.prompt_reap_dropped(result);
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = outcome {
                    vm.prompt_reap_dropped(node);
                    return Err(error);
                }
                Ok(node)
            }
            Object::List(_) => {
                let iter = vm.make_iter(spec, globals)?;
                // Keep the partial result in a Python list, just like the
                // comprehension. On error its normal cleanup path controls
                // finalizer ordering; dropping a bare Vec is not equivalent.
                let result = Object::new_list(Vec::new());
                let outcome = (|| -> Result<(), RuntimeError> {
                    while let Some(value) = vm.iter_next(&iter, globals)? {
                        let value = self.child(vm, &value, function)?;
                        let Object::List(values) = &result else {
                            unreachable!("partial result is a list");
                        };
                        values.borrow_mut().push(value);
                    }
                    Ok(())
                })();
                if let Err(error) = outcome {
                    vm.prompt_reap_dropped(result);
                    return Err(error);
                }
                Ok(result)
            }
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
            | Object::FrozenSet(_) => Ok(spec.clone()),
            _ => unreachable!("custom inputs use the Python builder"),
        }
    }
}

pub(crate) fn build(
    vm: &mut Interpreter,
    args: &[Object],
    kwargs: &[(String, Object)],
    globals: &Rc<RefCell<DictData>>,
) -> Result<Object, RuntimeError> {
    let [spec, builder, Object::Tuple(state)] = args else {
        return Err(type_error("_ast._build expects a spec, builder, and state"));
    };
    if state.len() != 4 || !kwargs.is_empty() {
        return Err(type_error("invalid _ast._build arguments"));
    }
    // The frozen module can itself be imported with replaced builtins. Only
    // the ordinary operations captured at import justify native type tests.
    let types = crate::builtin_types::builtin_types();
    if !matches!(&state[1], Object::Type(cls) if Rc::ptr_eq(cls, &types.dict_))
        || !matches!(&state[2], Object::Type(cls) if Rc::ptr_eq(cls, &types.list_))
        || !matches!(&state[3], Object::Builtin(b) if b.name == "isinstance"
            && crate::descr_registry::module_of_builtin(b).is_none())
    {
        return vm.call(builder, std::slice::from_ref(spec), &[], globals);
    }
    let Object::Code(code) = &state[0] else {
        return vm.call(builder, std::slice::from_ref(spec), &[], globals);
    };
    // Reuse the Python helper's literal, including its identity when an
    // unusual key observes the operand passed to __eq__.
    let Some(type_key) = crate::code_const_objects(code)
        .iter()
        .find(|value| matches!(value, Object::Str(name) if &**name == "_type"))
        .cloned()
    else {
        return vm.call(builder, std::slice::from_ref(spec), &[], globals);
    };
    Builder {
        state,
        checks: [
            Name::new("dict"),
            Name::new("list"),
            Name::new("isinstance"),
        ],
        registry: Name::new("_NODE_TYPES"),
        setter: Name::new("setattr"),
        recursive: Name::new("_build"),
        type_key,
        budget: 256,
    }
    .walk(vm, spec, builder, globals)
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_builder_and_fallbacks() {
        const CHILD: &str = "WEAVEPY_AST_BUILD_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            for jit in ["0", "1"] {
                let status = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "stdlib::ast_build::tests::native_builder_and_fallbacks",
                    ])
                    .env(CHILD, "1")
                    .env("WEAVEPY_JIT", jit)
                    .status()
                    .expect("spawn AST builder test");
                assert!(status.success(), "AST builder child, JIT={jit}: {status}");
            }
            return;
        }
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let source = include_str!("../../../../tests/regrtest/test_native_ast_build.py");
                let tree = weavepy_parser::parse_module(source).unwrap();
                let code = weavepy_compiler::compile_owned_module_with_options(
                    tree,
                    source,
                    "test_native_ast_build.py",
                    weavepy_compiler::CompileOptions::default(),
                )
                .unwrap();
                crate::Interpreter::new()
                    .run_module(&code)
                    .unwrap_or_else(|error| {
                        panic!("AST builder fixture: {error}");
                    });
                let source =
                    include_str!("../../../../tests/regrtest/test_native_ast_build_cleanup.py");
                let tree = weavepy_parser::parse_module(source).unwrap();
                let code = weavepy_compiler::compile_owned_module_with_options(
                    tree,
                    source,
                    "test_native_ast_build_cleanup.py",
                    weavepy_compiler::CompileOptions::default(),
                )
                .unwrap();
                crate::Interpreter::new()
                    .run_module(&code)
                    .unwrap_or_else(|error| panic!("AST cleanup fixture: {error}"));
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
