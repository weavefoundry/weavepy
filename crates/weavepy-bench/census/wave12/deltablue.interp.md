# WeavePy profile census: deltablue (work=50, 4s, WEAVEPY_JIT=0)

interpreter-thread samples: 2826

| samples | share | symbol |
|---|---|---|
| 316 | 11.2% | `<weavepy_vm::Interpreter>::run_until_yield_or_return_impl` |
| 221 | 7.8% | `<weavepy_vm::Interpreter>::step` |
| 115 | 4.1% | `_tlv_get_addr` |
| 87 | 3.1% | `<weavepy_vm::Interpreter>::recycle_frame_shell` |
| 86 | 3.0% | `_xzm_free` |
| 82 | 2.9% | `core::ptr::drop_glue::<weavepy_vm::object::Object>` |
| 80 | 2.8% | `<indexmap::map::IndexMap<u64, (alloc::sync::Arc<weavepy_vm::gc_trace::TrackedHandle>, u8)>>::retain::<weavepy_vm::gc_trace::take_dead_suspects::{closure#0}>::{closure#0}` |
| 79 | 2.8% | `_xzm_xzone_malloc_tiny` |
| 72 | 2.5% | `<weavepy_vm::Interpreter>::specialized_load_attr` |
| 71 | 2.5% | `<weavepy_vm::Interpreter>::push_frame_shell` |
| 61 | 2.2% | `<weavepy_vm::types::TypeObject>::lookup` |
| 57 | 2.0% | `<weavepy_vm::object::Object as core::clone::Clone>::clone` |
| 53 | 1.9% | `<weavepy_vm::gc_trace::GcState>::reap_dead_finalizable_locked` |
| 52 | 1.8% | `<weavepy_vm::Interpreter>::dispatch_call` |
| 49 | 1.7% | `_platform_memcmp` |
| 47 | 1.7% | `<weavepy_vm::Interpreter>::recycle_frame_allocs` |
| 47 | 1.7% | `<weavepy_vm::Interpreter>::run_frame` |
| 46 | 1.6% | `weavepy_vm::object::py_hash_bytes_slice` |
| 44 | 1.6% | `<indexmap::map::IndexMap<weavepy_vm::object::DictKey, weavepy_vm::object::Object, weavepy_vm::fasthash::FxBuildHasher>>::get_index_of::<weavepy_vm::object::StrKey>` |
| 43 | 1.5% | `_platform_memmove` |
| 37 | 1.3% | `<weavepy_vm::gc_trace::GcState>::handle_for` |
| 36 | 1.3% | `_xzm_xzone_malloc` |
| 34 | 1.2% | `<deduplicated_symbol>` |
| 33 | 1.2% | `<alloc::vec::Vec<weavepy_vm::object::Object>>::push_mut` |
| 30 | 1.1% | `weavepy_vm::gc_trace::take_dead_suspects` |
| 30 | 1.1% | `weavepy_vm::object::py_str_hash` |
| 28 | 1.0% | `<weavepy_vm::Interpreter>::load_attr_inner` |
| 27 | 1.0% | `<weavepy_vm::Interpreter>::recycle_scratch` |
| 26 | 0.9% | `<weavepy_vm::Frame>::pop` |
| 24 | 0.8% | `_malloc_zone_malloc` |
| 22 | 0.8% | `<weavepy_vm::Interpreter>::load_fast_value` |
| 21 | 0.7% | `_free` |
| 21 | 0.7% | `core::ptr::drop_glue::<weavepy_vm::Frame>` |
| 20 | 0.7% | `<weavepy_vm::Interpreter>::load_attr_instance_default` |
| 18 | 0.6% | `<weavepy_vm::Interpreter>::call_python_owned` |
| 18 | 0.6% | `<weavepy_vm::Interpreter>::pooled_locals_from_args` |
| 17 | 0.6% | `<weavepy_vm::Interpreter>::specialized_store_attr` |
| 17 | 0.6% | `<weavepy_vm::Interpreter>::sync_py_locals` |
| 17 | 0.6% | `<weavepy_vm::types::TypeObject>::metaclass_or_type` |
| 17 | 0.6% | `__findenv_locked` |
| 17 | 0.6% | `indexmap::inner::equivalent::<weavepy_vm::object::DictKey, weavepy_vm::object::Object, weavepy_vm::object::StrKey>::{closure#0}` |
| 16 | 0.6% | `<weavepy_vm::Interpreter>::pop_frame_shell` |
| 16 | 0.6% | `<weavepy_vm::sync::GilCell<alloc::vec::Vec<alloc::sync::Arc<weavepy_vm::types::TypeObject>>>>::borrow` |
| 15 | 0.5% | `<weavepy_vm::Interpreter>::run_py_exact_nofree` |
| 15 | 0.5% | `weavepy_vm::builtin_types::builtin_types::{closure#0}` |
| 14 | 0.5% | `<weavepy_vm::Interpreter>::pooled_scratch` |
| 14 | 0.5% | `<weavepy_vm::sync::GilCell<alloc::sync::Arc<weavepy_vm::types::TypeObject>>>::borrow` |
| 14 | 0.5% | `_platform_memset` |
| 13 | 0.5% | `<weavepy_vm::gc_trace::GcState>::collect_generation` |
| 12 | 0.4% | `weavepy_vm::object::py_hash_value` |
| 11 | 0.4% | `<weavepy_vm::gc_trace::GcState>::reap_dead_finalizable_locked::{closure#1}` |
| 11 | 0.4% | `<weavepy_vm::object::PyFunction>::code` |
| 11 | 0.4% | `__bzero` |
| 11 | 0.4% | `core::ptr::drop_glue::<[weavepy_vm::object::Object]>` |
| 10 | 0.4% | `<std::hash::random::RandomState as core::hash::BuildHasher>::hash_one::<&u64>` |
| 10 | 0.4% | `<weavepy_vm::Interpreter>::specialized_compare_op` |
| 10 | 0.4% | `<weavepy_vm::Interpreter>::specialized_load_global` |
| 10 | 0.4% | `<weavepy_vm::sync::GilCell<u32>>::get` |
| 10 | 0.4% | `DYLD-STUB$$free` |
| 9 | 0.3% | `<core::hash::sip::Hasher<core::hash::sip::Sip13Rounds> as core::hash::Hasher>::write` |
