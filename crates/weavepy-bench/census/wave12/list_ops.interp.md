# WeavePy profile census: list_ops (work=10000, 4s, WEAVEPY_JIT=0)

interpreter-thread samples: 3135

| samples | share | symbol |
|---|---|---|
| 463 | 14.8% | `<weavepy_vm::Interpreter>::step` |
| 351 | 11.2% | `<weavepy_vm::Interpreter>::run_until_yield_or_return_impl` |
| 132 | 4.2% | `core::ptr::drop_glue::<weavepy_vm::object::Object>` |
| 122 | 3.9% | `_xzm_free` |
| 115 | 3.7% | `<weavepy_vm::object::Object as core::clone::Clone>::clone` |
| 95 | 3.0% | `_tlv_get_addr` |
| 94 | 3.0% | `_xzm_xzone_malloc_tiny` |
| 90 | 2.9% | `<alloc::vec::Vec<weavepy_vm::object::Object>>::push_mut` |
| 83 | 2.6% | `<indexmap::map::IndexMap<u64, (alloc::sync::Arc<weavepy_vm::gc_trace::TrackedHandle>, u8)>>::retain::<weavepy_vm::gc_trace::take_dead_suspects::{closure#0}>::{closure#0}` |
| 79 | 2.5% | `<weavepy_vm::gc_trace::GcState>::reap_dead_finalizable_locked` |
| 71 | 2.3% | `<weavepy_vm::Interpreter>::load_fast_value` |
| 68 | 2.2% | `<weavepy_vm::Interpreter>::specialized_for_iter` |
| 58 | 1.9% | `<weavepy_vm::object::Object>::cmp` |
| 52 | 1.7% | `std::env::_var_os` |
| 48 | 1.5% | `<weavepy_vm::Interpreter>::dispatch_binary_op` |
| 44 | 1.4% | `<weavepy_vm::Interpreter>::dispatch_call` |
| 42 | 1.3% | `<deduplicated_symbol>` |
| 38 | 1.2% | `<weavepy_vm::Interpreter>::dispatch_binary_op::{closure#5}` |
| 38 | 1.2% | `<weavepy_vm::Interpreter>::specialized_binary_op` |
| 37 | 1.2% | `<weavepy_vm::Frame>::pop` |
| 35 | 1.1% | `weavepy_vm::metaclass_method` |
| 34 | 1.1% | `<weavepy_vm::gc_trace::GcState>::handle_for` |
| 33 | 1.1% | `_malloc_zone_malloc` |
| 32 | 1.0% | `__findenv_locked` |
| 31 | 1.0% | `weavepy_vm::builtins::list_append` |
| 27 | 0.9% | `weavepy_vm::gc_trace::take_dead_suspects` |
| 26 | 0.8% | `<weavepy_vm::Interpreter>::recycle_scratch` |
| 25 | 0.8% | `<weavepy_vm::Interpreter>::specialized_binary_subscr` |
| 25 | 0.8% | `core::slice::sort::stable::quicksort::quicksort::<weavepy_vm::object::Object, <[weavepy_vm::object::Object]>::sort_by<<weavepy_vm::Interpreter>::sort_with_key::{closure#5}>::{closure#0}>` |
| 25 | 0.8% | `weavepy_vm::instance_method` |
| 24 | 0.8% | `_free` |
| 23 | 0.7% | `_platform_memset` |
| 22 | 0.7% | `_platform_memmove` |
| 21 | 0.7% | `<weavepy_vm::gc_trace::GcState>::reap_dead_finalizable_locked::{closure#1}` |
| 20 | 0.6% | `<weavepy_vm::Interpreter>::object_is_finalizable` |
| 20 | 0.6% | `_xzm_xzone_malloc` |
| 19 | 0.6% | `weavepy_vm::binary_op` |
| 18 | 0.6% | `<weavepy_vm::Interpreter>::specialized_load_attr` |
| 18 | 0.6% | `<weavepy_vm::sync::GilCell<alloc::vec::Vec<alloc::sync::Arc<weavepy_vm::types::TypeObject>>>>::borrow` |
| 18 | 0.6% | `core::ptr::drop_glue::<[weavepy_vm::object::Object]>` |
| 17 | 0.5% | `<weavepy_vm::gc_trace::GcState>::track` |
| 16 | 0.5% | `weavepy_vm::bignum_op` |
| 16 | 0.5% | `weavepy_vm::dict_view_set_elems` |
| 15 | 0.5% | `<weavepy_vm::Interpreter>::load_attr` |
| 15 | 0.5% | `<weavepy_vm::Interpreter>::reap_dead_subgraph::{closure#0}` |
| 14 | 0.4% | `<weavepy_vm::Interpreter>::call` |
| 14 | 0.4% | `weavepy_vm::numeric_data_attr` |
| 13 | 0.4% | `<weavepy_vm::Interpreter>::reap_call_receiver` |
| 13 | 0.4% | `_realloc` |
| 13 | 0.4% | `weavepy_vm::builtin_needs_interp` |
| 13 | 0.4% | `weavepy_vm::specialize::record_hit` |
| 13 | 0.4% | `weavepy_vm::tier2::note_backedge` |
| 12 | 0.4% | `<weavepy_vm::Interpreter>::pooled_scratch` |
| 11 | 0.4% | `<alloc::raw_vec::RawVec<alloc::vec::Vec<alloc::sync::Arc<weavepy_vm::types::TypeObject>>>>::grow_one` |
| 11 | 0.4% | `__bzero` |
| 11 | 0.4% | `weavepy_vm::native_call_ic_safe` |
| 10 | 0.3% | `<weavepy_vm::Interpreter>::load_attr_inner` |
| 10 | 0.3% | `<weavepy_vm::Interpreter>::maybe_bind` |
| 10 | 0.3% | `core::ptr::drop_glue::<weavepy_vm::object::BuiltinFn>` |
| 9 | 0.3% | `<alloc::vec::Vec<weavepy_vm::object::Object>>::remove` |
