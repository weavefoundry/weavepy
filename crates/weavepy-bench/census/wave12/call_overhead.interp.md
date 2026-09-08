# WeavePy profile census: call_overhead (work=150000, 4s, WEAVEPY_JIT=0)

interpreter-thread samples: 3106

| samples | share | symbol |
|---|---|---|
| 438 | 14.1% | `<weavepy_vm::Interpreter>::run_until_yield_or_return_impl` |
| 374 | 12.0% | `<weavepy_vm::Interpreter>::step` |
| 129 | 4.2% | `<weavepy_vm::Interpreter>::push_frame_shell` |
| 109 | 3.5% | `<weavepy_vm::Interpreter>::recycle_frame_shell` |
| 105 | 3.4% | `<weavepy_vm::object::Object as core::clone::Clone>::clone` |
| 92 | 3.0% | `_tlv_get_addr` |
| 87 | 2.8% | `<weavepy_vm::gc_trace::GcState>::reap_dead_finalizable_locked` |
| 87 | 2.8% | `core::ptr::drop_glue::<weavepy_vm::object::Object>` |
| 86 | 2.8% | `<weavepy_vm::Interpreter>::run_frame` |
| 80 | 2.6% | `<weavepy_vm::Interpreter>::recycle_frame_allocs` |
| 77 | 2.5% | `<alloc::vec::Vec<weavepy_vm::object::Object>>::push_mut` |
| 75 | 2.4% | `_xzm_free` |
| 65 | 2.1% | `<weavepy_vm::Interpreter>::dispatch_call` |
| 64 | 2.1% | `_xzm_xzone_malloc_tiny` |
| 61 | 2.0% | `<weavepy_vm::Interpreter>::load_fast_value` |
| 52 | 1.7% | `<indexmap::map::IndexMap<u64, (alloc::sync::Arc<weavepy_vm::gc_trace::TrackedHandle>, u8)>>::retain::<weavepy_vm::gc_trace::take_dead_suspects::{closure#0}>::{closure#0}` |
| 50 | 1.6% | `_malloc_zone_malloc` |
| 41 | 1.3% | `<weavepy_vm::Interpreter>::recycle_scratch` |
| 38 | 1.2% | `<weavepy_vm::Interpreter>::specialized_binary_op` |
| 34 | 1.1% | `<weavepy_vm::Interpreter>::call_python_owned` |
| 34 | 1.1% | `_platform_memmove` |
| 33 | 1.1% | `<weavepy_vm::Interpreter>::pooled_scratch` |
| 33 | 1.1% | `_platform_memcmp` |
| 31 | 1.0% | `<weavepy_vm::Interpreter>::specialized_load_global` |
| 31 | 1.0% | `core::ptr::drop_glue::<weavepy_vm::Frame>` |
| 30 | 1.0% | `<weavepy_vm::object::PyFunction>::code` |
| 29 | 0.9% | `<weavepy_vm::sync::GilCell<alloc::vec::Vec<alloc::sync::Arc<weavepy_vm::types::TypeObject>>>>::borrow` |
| 28 | 0.9% | `<hashbrown::raw::RawTable<usize>>::reserve_rehash::<indexmap::inner::get_hash<weavepy_vm::object::DictKey, weavepy_vm::object::Object>::{closure#0}>` |
| 26 | 0.8% | `weavepy_vm::builtins::dict_lookup` |
| 24 | 0.8% | `<weavepy_vm::Frame>::pop` |
| 24 | 0.8% | `_xzm_xzone_malloc` |
| 22 | 0.7% | `<weavepy_vm::Interpreter>::pop_frame_shell` |
| 22 | 0.7% | `<weavepy_vm::Interpreter>::run_py_exact_nofree` |
| 21 | 0.7% | `<weavepy_vm::Interpreter>::reap_call_receiver` |
| 19 | 0.6% | `<weavepy_vm::Interpreter>::pooled_locals_from_args` |
| 19 | 0.6% | `<weavepy_vm::Interpreter>::specialized_load_attr` |
| 19 | 0.6% | `<weavepy_vm::Interpreter>::specialized_store_attr` |
| 19 | 0.6% | `_free` |
| 19 | 0.6% | `_platform_memset` |
| 18 | 0.6% | `<deduplicated_symbol>` |
| 16 | 0.5% | `<weavepy_vm::gc_trace::GcState>::reap_dead_finalizable_locked::{closure#1}` |
| 15 | 0.5% | `<weavepy_vm::Interpreter>::run_until_yield_or_return` |
| 15 | 0.5% | `weavepy_vm::gc_trace::take_dead_suspects` |
| 15 | 0.5% | `weavepy_vm::weakref_registry::id_of` |
| 14 | 0.5% | `weavepy_vm::code_const_objects` |
| 14 | 0.5% | `weavepy_vm::tier2::materialize_parked` |
| 14 | 0.5% | `weavepy_vm::trace::monitoring_union_mask::{closure#0}` |
| 12 | 0.4% | `<weavepy_vm::Interpreter>::call_c_profiled` |
| 12 | 0.4% | `core::ptr::drop_glue::<[weavepy_vm::object::Object]>` |
| 12 | 0.4% | `weavepy_vm::object::py_hash_bytes_slice` |
| 12 | 0.4% | `weavepy_vm::specialize::record_hit` |
| 11 | 0.4% | `<alloc::raw_vec::RawVecInner>::finish_grow` |
| 11 | 0.4% | `__bzero` |
| 10 | 0.3% | `<alloc::vec::Vec<(alloc::string::String, weavepy_vm::object::Object)> as alloc::vec::spec_from_iter_nested::SpecFromIterNested<(alloc::string::String, weavepy_vm::object::Object), core::iter::adapters::zip::Zip<alloc::vec::into_iter::IntoIter<alloc::string::String>, alloc::vec::into_iter::IntoIter<weavepy_vm::object::Object>>>>::from_iter` |
| 10 | 0.3% | `<weavepy_vm::Interpreter>::anchors_tracked_child::walk::{closure#0}` |
| 10 | 0.3% | `<weavepy_vm::Interpreter>::pooled_stack` |
| 9 | 0.3% | `<weavepy_vm::Interpreter>::make_frame` |
| 8 | 0.3% | `<alloc::vec::Vec<weavepy_vm::object::Object>>::extend_with` |
| 8 | 0.3% | `weavepy_vm::builtin_needs_interp` |
| 7 | 0.2% | `<weavepy_vm::Interpreter>::call` |
