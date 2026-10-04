//! A non-owning reference to any heap-backed [`Object`].
//!
//! The cycle collector and the weak-reference registry hold their targets
//! through this type, so neither keeps an object alive: an object dies by
//! reference count the moment its last strong owner goes, as in CPython,
//! and the collector only has to find and break cycles.
//!
//! The weak handle keeps the allocation (not the payload) reserved, so an
//! object's address can't be reused while a handle to it exists. That's
//! what lets the collector and the registry key their tables by address.

use crate::object::Object;
use crate::shared_value::ThinWeak;
use crate::sync::{Rc, Weak};

macro_rules! weak_object {
    ($($variant:ident),* $(,)?) => {
        /// A weak handle on a heap-backed [`Object`]; see the module docs.
        #[allow(missing_debug_implementations)]
        pub enum WeakObject {
            Tuple(ThinWeak<crate::tuple_storage::TupleStorage>),
            $($variant(weak_object!(@payload $variant)),)*
        }

        impl WeakObject {
            /// A weak handle on `obj`, or `None` for a value with no heap
            /// allocation of its own (scalars, strings, bytes).
            pub fn new(obj: &Object) -> Option<Self> {
                Some(match obj {
                    Object::Tuple(t) => Self::Tuple(crate::shared_value::ThinArc::downgrade(t)),
                    $(Object::$variant(rc) => Self::$variant(Rc::downgrade(rc)),)*
                    _ => return None,
                })
            }

            /// The object, while it is alive.
            pub fn upgrade(&self) -> Option<Object> {
                Some(match self {
                    Self::Tuple(w) => Object::Tuple(w.upgrade()?),
                    $(Self::$variant(w) => Object::$variant(w.upgrade()?),)*
                })
            }

            /// The number of strong owners left (0 once the object died).
            pub fn strong_count(&self) -> usize {
                match self {
                    Self::Tuple(w) => w.strong_count(),
                    $(Self::$variant(w) => w.strong_count(),)*
                }
            }

            /// Whether the object has died.
            #[inline]
            pub fn is_dead(&self) -> bool {
                self.strong_count() == 0
            }
        }
    };
    (@payload Long) => { Weak<num_bigint::BigInt> };
    (@payload Complex) => { Weak<crate::object::PyComplex> };
    (@payload List) => { Weak<crate::sync::RefCell<Vec<Object>>> };
    (@payload Dict) => { Weak<crate::sync::RefCell<crate::object::DictData>> };
    (@payload Range) => { Weak<crate::object::Range> };
    (@payload Function) => { Weak<crate::object::PyFunction> };
    (@payload Builtin) => { Weak<crate::object::BuiltinFn> };
    (@payload BoundMethod) => { Weak<crate::object::BoundMethod> };
    (@payload Code) => { Weak<weavepy_compiler::CodeObject> };
    (@payload Cell) => { Weak<crate::sync::RefCell<Object>> };
    (@payload Iter) => { Weak<crate::sync::RefCell<crate::object::PyIterator>> };
    (@payload Slice) => { Weak<crate::object::PySlice> };
    (@payload Type) => { Weak<crate::types::TypeObject> };
    (@payload Instance) => { Weak<crate::types::PyInstance> };
    (@payload Module) => { Weak<crate::object::PyModule> };
    (@payload Generator) => { Weak<crate::object::PyGenerator> };
    (@payload Coroutine) => { Weak<crate::object::PyGenerator> };
    (@payload AsyncGenerator) => { Weak<crate::object::PyGenerator> };
    (@payload AsyncGenAwait) => { Weak<crate::object::AsyncGenAwait> };
    (@payload ByteArray) => { Weak<crate::sync::RefCell<Vec<u8>>> };
    (@payload Set) => { Weak<crate::sync::RefCell<crate::object::SetData>> };
    (@payload FrozenSet) => { Weak<crate::object::FrozenSetObj> };
    (@payload File) => { Weak<crate::object::PyFile> };
    (@payload Property) => { Weak<crate::object::PyProperty> };
    (@payload StaticMethod) => { Weak<crate::object::MethodWrapper> };
    (@payload ClassMethod) => { Weak<crate::object::MethodWrapper> };
    (@payload SlotDescriptor) => { Weak<crate::object::SlotDescriptor> };
    (@payload Frame) => { Weak<crate::object::PyFrame> };
    (@payload Traceback) => { Weak<crate::object::PyTraceback> };
    (@payload MemoryView) => { Weak<crate::object::PyMemoryView> };
    (@payload MappingProxy) => { Weak<crate::sync::RefCell<crate::object::DictData>> };
    (@payload MappingProxyObj) => { Weak<Object> };
    (@payload DictView) => { Weak<crate::object::PyDictView> };
    (@payload SimpleNamespace) => { Weak<crate::sync::RefCell<crate::object::DictData>> };
    (@payload LazyIter) => { Weak<crate::object::PyLazyIter> };
    (@payload Capsule) => { Weak<crate::object::PyCapsuleSoul> };
    (@payload Foreign) => { Weak<crate::foreign::PyForeignSoul> };
}

weak_object!(
    Long,
    Complex,
    List,
    Dict,
    Range,
    Function,
    Builtin,
    BoundMethod,
    Code,
    Cell,
    Iter,
    Slice,
    Type,
    Instance,
    Module,
    Generator,
    Coroutine,
    AsyncGenerator,
    AsyncGenAwait,
    ByteArray,
    Set,
    FrozenSet,
    File,
    Property,
    StaticMethod,
    ClassMethod,
    SlotDescriptor,
    Frame,
    Traceback,
    MemoryView,
    MappingProxy,
    MappingProxyObj,
    DictView,
    SimpleNamespace,
    LazyIter,
    Capsule,
    Foreign,
);
