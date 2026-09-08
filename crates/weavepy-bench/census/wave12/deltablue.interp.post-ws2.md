# WeavePy profile census: deltablue (work=50, 4s, WEAVEPY_JIT=0)

interpreter-thread samples: 3043

| samples | share | symbol |
|---|---|---|
| 278 | 9.1% | `<weavepy_vm::Interpreter>::run_until_yield_or_return_impl` |
| 234 | 7.7% | `<weavepy_vm::Interpreter>::step` |
| 150 | 4.9% | `<weavepy_vm::Interpreter>::push_frame_shell` |
| 149 | 4.9% | `core::ptr::drop_glue::<weavepy_vm::object::Object>` |
| 142 | 4.7% | `_xzm_free` |
| 127 | 4.2% | `<weavepy_vm::types::TypeObject>::lookup` |
| 115 | 3.8% | `_tlv_get_addr` |
| 111 | 3.6% | `<alloc::vec::Vec<weavepy_vm::object::Object>>::push_mut` |
| 100 | 3.3% | `_xzm_xzone_malloc_tiny` |
| 93 | 3.1% | `<weavepy_vm::Interpreter>::recycle_frame_shell` |
| 93 | 3.1% | `<weavepy_vm::object::Object as core::clone::Clone>::clone` |
| 67 | 2.2% | `_platform_memmove` |
| 60 | 2.0% | `<weavepy_vm::Interpreter>::specialized_load_attr` |
| 59 | 1.9% | `<weavepy_vm::gc_trace::GcState>::handle_for` |
| 58 | 1.9% | `<weavepy_vm::types::TypeObject>::metaclass_or_type` |
| 53 | 1.7% | `<indexmap::map::IndexMap<weavepy_vm::object::DictKey, weavepy_vm::object::Object, weavepy_vm::fasthash::FxBuildHasher>>::get_index_of::<weavepy_vm::object::StrKey>` |
| 53 | 1.7% | `_platform_memcmp` |
| 46 | 1.5% | `<weavepy_vm::Interpreter>::recycle_frame_allocs` |
| 41 | 1.3% | `<deduplicated_symbol>` |
| 40 | 1.3% | `<weavepy_vm::Interpreter>::load_attr_instance_default` |
| 38 | 1.2% | `<weavepy_vm::Interpreter>::specialized_store_attr` |
| 36 | 1.2% | `<weavepy_vm::Interpreter>::dispatch_call` |
| 36 | 1.2% | `<weavepy_vm::Interpreter>::run_frame` |
| 36 | 1.2% | `weavepy_vm::object::py_hash_bytes_slice` |
| 33 | 1.1% | `<weavepy_vm::Interpreter>::recycle_scratch` |
| 31 | 1.0% | `<weavepy_vm::Frame>::pop` |
| 31 | 1.0% | `<weavepy_vm::Interpreter>::load_fast_value` |
| 26 | 0.9% | `<weavepy_vm::Interpreter>::load_attr_inner` |
| 24 | 0.8% | `<weavepy_vm::Interpreter>::load_attr_type` |
| 22 | 0.7% | `_malloc_zone_malloc` |
| 21 | 0.7% | `<indexmap::inner::Core<weavepy_vm::object::DictKey, weavepy_vm::object::Object>>::insert_full` |
| 18 | 0.6% | `<weavepy_vm::sync::GilCell<alloc::vec::Vec<alloc::sync::Arc<weavepy_vm::types::TypeObject>>>>::borrow` |
| 18 | 0.6% | `core::ptr::drop_glue::<weavepy_vm::Frame>` |
| 18 | 0.6% | `weavepy_vm::object::py_str_hash` |
| 17 | 0.6% | `<weavepy_vm::Interpreter>::pooled_locals_from_args` |
| 17 | 0.6% | `weavepy_vm::gc_trace::note_dropped` |
| 17 | 0.6% | `weavepy_vm::weakref_registry::id_of` |
| 16 | 0.5% | `<weavepy_vm::Interpreter>::reap_dead_subgraph::{closure#0}` |
| 16 | 0.5% | `weavepy_vm::builtin_types::builtin_types::{closure#0}` |
| 15 | 0.5% | `_platform_memset` |
